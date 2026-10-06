use std::fs;
use std::path::{Path, PathBuf};

use libsignal_protocol::{IdentityKeyStore, InMemSignalProtocolStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backup::{export_vault, import_vault};
use crate::chat_log::ChatLog;
use crate::recovery::{ensure_recovery_files, generate_recovery_code, recovery_files_exist, unwrap_master_with_recovery};
use crate::crypto::{decrypt, encrypt, random_bytes};
use crate::error::Error;
use crate::invite::InvitePayload;
use crate::message::{InboxResult, ReceivedMessage};
use crate::payload::{MessagePayload, MAX_INLINE_FILE_BYTES};
use crate::relay_client::{normalize_relay_base, RelayClient};
use crate::signal::{
    decrypt_message, encrypt_message, invite_from_bundle, mint_prekey_bundle,
    process_contact_bundle, protocol_name_from_identity,
};
use crate::signal_persist::{export_store, import_store};
use crate::vault::{BlobId, Vault};
use crate::wire::{decode_relay_blob, encode_relay_blob};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contact {
    pub nickname: String,
    pub protocol_name: String,
    pub queue_id: String,
    pub queue_token: String,
    /// True after we successfully received at least one message from them (they added our invite).
    #[serde(default)]
    pub inbound_ready: bool,
}

#[derive(Serialize, Deserialize)]
struct AccountState {
    relay: String,
    protocol_name: String,
    receive_queue_id: String,
    receive_queue_token: String,
    contacts: Vec<Contact>,
    /// Stable invite string so "Copy my invite" does not rotate keys every click.
    #[serde(default)]
    invite_cache: Option<String>,
    #[serde(default)]
    recovery_acknowledged: bool,
    #[serde(default)]
    last_backup_unix: u64,
}

pub struct Account {
    vault: Vault,
    store: InMemSignalProtocolStore,
    state: AccountState,
    dir: PathBuf,
    chat_log: ChatLog,
    pending_recovery_code: Option<String>,
}

impl Account {
    pub async fn create(dir: &Path, passphrase: &str, relay: &str) -> Result<Self, Error> {
        let relay = normalize_relay_base(relay)?;
        let vault = Vault::create(dir, passphrase)?;
        let mut store = crate::signal::new_protocol_store()?;
        let _bundle = mint_prekey_bundle(&mut store).await?;
        let protocol_name = protocol_name_from_identity(
            &store
                .get_identity_key_pair()
                .await
                .map_err(|_| Error::Corrupt)?,
        );
        let receive_queue_id = Uuid::new_v4().to_string();
        let receive_queue_token = hex::encode(random_bytes::<32>());
        let state = AccountState {
            relay,
            protocol_name,
            receive_queue_id,
            receive_queue_token,
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: false,
            last_backup_unix: 0,
        };
        let master = *vault.master_key();
        let recovery_code = generate_recovery_code();
        ensure_recovery_files(dir, &master, &recovery_code)?;
        let chat_log = ChatLog::load(dir, &master)?;
        let account = Self {
            vault,
            store,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code: Some(recovery_code),
        };
        account.save().await?;
        Ok(account)
    }

    pub async fn open(dir: &Path, passphrase: &str) -> Result<Self, Error> {
        let vault = Vault::open(dir, passphrase)?;
        Self::finish_open(dir, vault).await
    }

    pub async fn open_with_recovery(dir: &Path, recovery_code: &str) -> Result<Self, Error> {
        let master = unwrap_master_with_recovery(dir, recovery_code)?;
        let vault = Vault::open_with_master(dir, master)?;
        Self::finish_open(dir, vault).await
    }

    async fn finish_open(dir: &Path, vault: Vault) -> Result<Self, Error> {
        let master = *vault.master_key();
        let packed = fs::read(dir.join("account.enc")).map_err(|_| Error::NotFound)?;
        let state_bytes = decrypt(&master, &packed)?;
        let mut state: AccountState =
            serde_json::from_slice(&state_bytes).map_err(|_| Error::Corrupt)?;
        let signal_bytes = fs::read(dir.join("signal.enc")).map_err(|_| Error::NotFound)?;
        let signal_plain = decrypt(&master, &signal_bytes)?;
        let store = import_store(&signal_plain).await?;
        let chat_log = ChatLog::load(dir, &master)?;

        let mut pending_recovery_code = None;
        if !recovery_files_exist(dir) {
            let code = generate_recovery_code();
            ensure_recovery_files(dir, &master, &code)?;
            state.recovery_acknowledged = false;
            pending_recovery_code = Some(code);
        }

        let needs_save = pending_recovery_code.is_some();
        let account = Self {
            vault,
            store,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code,
        };
        if needs_save {
            account.save().await?;
        }
        Ok(account)
    }

    pub fn recovery_acknowledged(&self) -> bool {
        self.state.recovery_acknowledged
    }

    pub fn take_pending_recovery_code(&mut self) -> Option<String> {
        self.pending_recovery_code.take()
    }

    pub async fn acknowledge_recovery(&mut self) -> Result<(), Error> {
        self.state.recovery_acknowledged = true;
        self.pending_recovery_code = None;
        self.save().await
    }

    pub async fn export_backup(&mut self) -> Result<Vec<u8>, Error> {
        let archive = export_vault(&self.dir)?;
        self.state.last_backup_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.save().await?;
        Ok(archive)
    }

    pub fn last_backup_unix(&self) -> u64 {
        self.state.last_backup_unix
    }

    pub fn import_backup(dir: &Path, archive: &[u8]) -> Result<(), Error> {
        import_vault(dir, archive)
    }

    pub fn chat_history(&self) -> &[crate::chat_log::ChatRecord] {
        self.chat_log.messages()
    }

    pub fn read_blob(&self, id: &BlobId) -> Result<Vec<u8>, Error> {
        self.vault.get_blob(id)
    }

    pub fn vault_dir(&self) -> &Path {
        &self.dir
    }

    pub fn relay(&self) -> &str {
        &self.state.relay
    }

    pub fn protocol_name(&self) -> &str {
        &self.state.protocol_name
    }

    pub fn contacts(&self) -> &[Contact] {
        &self.state.contacts
    }

    pub fn vault_bytes_on_disk(&self) -> u64 {
        dir_size(&self.dir)
    }

    pub async fn invite(&mut self) -> Result<String, Error> {
        if let Some(cached) = &self.state.invite_cache {
            return Ok(cached.clone());
        }
        let bundle = mint_prekey_bundle(&mut self.store).await?;
        let payload = invite_from_bundle(
            &self.state.relay,
            &self.state.protocol_name,
            &self.state.receive_queue_id,
            &self.state.receive_queue_token,
            &bundle,
        )?;
        let encoded = payload.encode()?;
        self.state.invite_cache = Some(encoded.clone());
        self.save().await?;
        Ok(encoded)
    }

    pub async fn add_contact(&mut self, nickname: &str, invite_str: &str) -> Result<(), Error> {
        let payload = InvitePayload::decode(invite_str)?;
        if normalize_relay(&payload.relay) != normalize_relay(&self.state.relay) {
            return Err(Error::RelayMismatch(payload.relay.clone(), self.state.relay.clone()));
        }
        if self
            .state
            .contacts
            .iter()
            .any(|c| c.protocol_name == payload.protocol_name)
        {
            return Err(Error::ContactExists);
        }
        let bundle = payload.to_prekey_bundle()?;
        process_contact_bundle(
            &mut self.store,
            &self.state.protocol_name,
            &payload.protocol_name,
            &bundle,
        )
        .await?;
        self.state.contacts.push(Contact {
            nickname: nickname.to_string(),
            protocol_name: payload.protocol_name.clone(),
            queue_id: payload.queue_id,
            queue_token: payload.queue_token,
            inbound_ready: false,
        });
        self.save().await?;
        Ok(())
    }

    pub async fn send_text(&mut self, nickname: &str, text: &str) -> Result<(), Error> {
        self.send_payload(
            nickname,
            MessagePayload::Text {
                body: text.to_string(),
            },
        )
        .await?;
        let master = *self.vault.master_key();
        self.chat_log
            .append_text(&master, nickname, true, text)?;
        Ok(())
    }

    pub async fn send_file(&mut self, nickname: &str, path: &Path) -> Result<(), Error> {
        let data = fs::read(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        self.send_file_bytes(nickname, &name, &data).await
    }

    pub async fn send_file_bytes(
        &mut self,
        nickname: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), Error> {
        if data.len() > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let blob_id = self.vault.put_blob(data)?;
        self.send_payload(
            nickname,
            MessagePayload::File {
                name: name.to_string(),
                data: data.to_vec(),
            },
        )
        .await?;
        let master = *self.vault.master_key();
        self.chat_log.append_file(
            &master,
            nickname,
            true,
            name,
            &blob_id,
            data.len() as u64,
        )?;
        Ok(())
    }

    async fn send_payload(&mut self, nickname: &str, payload: MessagePayload) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        let plain = payload.encode()?;
        let ct = encrypt_message(
            &mut self.store,
            &self.state.protocol_name,
            &contact.protocol_name,
            plain.as_slice(),
        )
        .await?;
        let body = encode_relay_blob(&ct);
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .put(&contact.queue_id, &contact.queue_token, &body)
            .await?;
        self.save().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<InboxResult, Error> {
        let relay = RelayClient::try_new(&self.state.relay)?;
        let ids = relay
            .list(&self.state.receive_queue_id, &self.state.receive_queue_token)
            .await?;
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let mut dirty = false;
        for blob_id in ids {
            let body = relay
                .get(
                    &self.state.receive_queue_id,
                    &self.state.receive_queue_token,
                    &blob_id,
                )
                .await?;
            let (ct, legacy_from) = decode_relay_blob(&body)?;
            let (plain, from_protocol) = match legacy_from {
                Some(from) => match decrypt_message(
                    &mut self.store,
                    &self.state.protocol_name,
                    &from,
                    &ct,
                )
                .await
                {
                    Ok(p) => (p, from),
                    Err(Error::Session) => {
                        warnings.push(format!(
                            "Could not read a message from {} — they may not have added your invite yet (use Copy my invite on your side, they paste it under Add contact).",
                            short_name(&from)
                        ));
                        continue;
                    }
                    Err(e) => return Err(e),
                },
                None => match self.try_decrypt_from_any_contact(&ct).await {
                    Ok(pair) => pair,
                    Err(Error::Session) => {
                        warnings.push(
                            "Could not decrypt a queued message — unknown sender or session not ready.".into(),
                        );
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            let payload = decode_legacy_payload(&plain)?;
            let file_blob_id = if let MessagePayload::File { data, name } = &payload {
                let id = self.vault.put_blob(data)?;
                let master = *self.vault.master_key();
                let nick = self
                    .contact_nick_for_protocol(&from_protocol)
                    .unwrap_or_else(|| short_name(&from_protocol));
                self.chat_log.append_file(
                    &master,
                    &nick,
                    false,
                    name,
                    &id,
                    data.len() as u64,
                )?;
                Some(id)
            } else {
                let master = *self.vault.master_key();
                let nick = self
                    .contact_nick_for_protocol(&from_protocol)
                    .unwrap_or_else(|| short_name(&from_protocol));
                if let MessagePayload::Text { body } = &payload {
                    self.chat_log
                        .append_text(&master, &nick, false, body)?;
                }
                None
            };
            if let Some(c) = self
                .state
                .contacts
                .iter_mut()
                .find(|c| c.protocol_name == from_protocol)
            {
                c.inbound_ready = true;
            }
            let nick = self
                .contact_nick_for_protocol(&from_protocol)
                .unwrap_or_else(|| short_name(&from_protocol));
            out.push(ReceivedMessage {
                from_nick: nick,
                payload,
                file_blob_id,
            });
            relay
                .delete(
                    &self.state.receive_queue_id,
                    &self.state.receive_queue_token,
                    &blob_id,
                )
                .await?;
            dirty = true;
        }
        if dirty {
            self.save().await?;
        }
        Ok(InboxResult {
            messages: out,
            warnings,
        })
    }

    fn find_contact(&self, nickname: &str) -> Option<&Contact> {
        self.state.contacts.iter().find(|c| c.nickname == nickname)
    }

    fn contact_nick_for_protocol(&self, protocol_name: &str) -> Option<String> {
        self.state
            .contacts
            .iter()
            .find(|c| c.protocol_name == protocol_name)
            .map(|c| c.nickname.clone())
    }

    async fn try_decrypt_from_any_contact(
        &mut self,
        ct: &[u8],
    ) -> Result<(Vec<u8>, String), Error> {
        let names: Vec<String> = self
            .state
            .contacts
            .iter()
            .map(|c| c.protocol_name.clone())
            .collect();
        for name in names {
            if let Ok(plain) = decrypt_message(
                &mut self.store,
                &self.state.protocol_name,
                &name,
                ct,
            )
            .await
            {
                return Ok((plain, name));
            }
        }
        Err(Error::Session)
    }

    async fn save(&self) -> Result<(), Error> {
        let master = self.vault.master_key();
        let state_bytes = serde_json::to_vec(&self.state).map_err(|_| Error::Corrupt)?;
        let state_enc = encrypt(master, &state_bytes)?;
        fs::write(self.dir.join("account.enc"), state_enc)?;

        let session_names: Vec<String> = self
            .state
            .contacts
            .iter()
            .map(|c| c.protocol_name.clone())
            .collect();
        let signal_plain = export_store(&self.store, &session_names).await?;
        let signal_enc = encrypt(master, &signal_plain)?;
        fs::write(self.dir.join("signal.enc"), signal_enc)?;
        Ok(())
    }
}

fn decode_legacy_payload(plain: &[u8]) -> Result<MessagePayload, Error> {
    if let Ok(p) = MessagePayload::decode(plain) {
        return Ok(p);
    }
    let text = String::from_utf8(plain.to_vec()).map_err(|_| Error::Corrupt)?;
    Ok(MessagePayload::Text { body: text })
}

fn normalize_relay(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn short_name(protocol_name: &str) -> String {
    if protocol_name.len() > 12 {
        format!("{}…", &protocol_name[..12])
    } else {
        protocol_name.to_string()
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}
