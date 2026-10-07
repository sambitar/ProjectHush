use std::fs;
use std::path::{Path, PathBuf};

use libsignal_protocol::IdentityKeyStore;
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::backup::{export_vault, import_vault};
use crate::chat_log::{ChatLog, MailAttachmentRecord};
use crate::device::{DeviceKeys, LEGACY_DEVICE_FILE};
use crate::recovery::{ensure_recovery_files, generate_recovery_code, recovery_files_exist, unwrap_master_with_recovery};
use crate::crypto::{decrypt, encrypt};
use crate::error::Error;
use crate::invite::InvitePayload;
use crate::message::{InboxResult, ReceivedAttachment, ReceivedMessage};
use crate::payload::{MailAttachment, MessagePayload, MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS};
use crate::relay_client::{normalize_relay_base, RelayClient};
use crate::signal::{
    decrypt_message, encrypt_message, invite_from_bundle, mint_prekey_bundle,
    process_contact_bundle, protocol_name_from_identity,
};
use crate::signal_persist::import_store;
use crate::vault::{BlobId, Vault};
use crate::wire::{decode_relay_blob, encode_relay_blob};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contact {
    pub nickname: String,
    pub protocol_name: String,
    pub queue_id: String,
    pub queue_token: String,
    /// This contact's chosen mail handle (local part only), learned from their
    /// invite. `None` for contacts added before mail, or who never set one.
    #[serde(default)]
    pub mail_handle: Option<String>,
    /// True after we successfully received at least one message from them (they added our invite).
    #[serde(default)]
    pub inbound_ready: bool,
}

#[derive(Serialize, Deserialize)]
struct AccountState {
    relay: String,
    protocol_name: String,
    contacts: Vec<Contact>,
    /// Stable invite string so "Copy my invite" does not rotate keys every click.
    #[serde(default)]
    invite_cache: Option<String>,
    #[serde(default)]
    recovery_acknowledged: bool,
    #[serde(default)]
    last_backup_unix: u64,
    /// Legacy: this device's relay inbox used to live in `account.enc`. Read
    /// once when migrating to `device.enc`, then never written again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receive_queue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receive_queue_token: Option<String>,
    /// This member's chosen mail handle (local part only). Shared vault state,
    /// not device material: it is a label, and a restored vault should keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mail_handle: Option<String>,
}

pub struct Account {
    vault: Vault,
    /// This device's own keys and relay inbox, kept out of the shared vault so
    /// it is never handed to another device.
    device: DeviceKeys,
    state: AccountState,
    dir: PathBuf,
    chat_log: ChatLog,
    pending_recovery_code: Option<String>,
}

impl Account {
    pub async fn create(dir: &Path, passphrase: &str, relay: &str) -> Result<Self, Error> {
        let relay = normalize_relay_base(relay)?;
        let vault = Vault::create(dir, passphrase)?;
        let master = *vault.master_key();
        let mut device = DeviceKeys::create(dir, &master, 1).await?;
        let _bundle = mint_prekey_bundle(device.store_mut()).await?;
        let protocol_name = protocol_name_from_identity(
            &device
                .store_mut()
                .get_identity_key_pair()
                .await
                .map_err(|_| Error::Corrupt)?,
        );
        let state = AccountState {
            relay,
            protocol_name,
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: false,
            last_backup_unix: 0,
            receive_queue_id: None,
            receive_queue_token: None,
            mail_handle: None,
        };
        let recovery_code = generate_recovery_code();
        ensure_recovery_files(dir, &master, &recovery_code)?;
        let chat_log = ChatLog::load(dir, &master)?;
        let account = Self {
            vault,
            device,
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

    /// Rotate the vault passphrase.
    ///
    /// The master key is re-wrapped, not replaced, so `account.enc`,
    /// `signal.enc`, the chat log, every blob and any recovery code keep
    /// working. The account must already be open: the caller proves ownership
    /// with `current`, and a locked vault has no master key to re-wrap.
    pub fn change_passphrase(&self, current: &str, new: &str) -> Result<(), Error> {
        self.vault.change_passphrase(current, new)
    }

    async fn finish_open(dir: &Path, vault: Vault) -> Result<Self, Error> {
        let master = *vault.master_key();
        let packed = fs::read(dir.join("account.enc")).map_err(|_| Error::NotFound)?;
        let state_bytes = decrypt(&master, &packed)?;
        let mut state: AccountState =
            serde_json::from_slice(&state_bytes).map_err(|_| Error::Corrupt)?;

        let mut migrated = false;
        let mut fresh_device = false;
        let device = match DeviceKeys::load(dir, &master).await {
            Ok(device) => device,
            Err(Error::NotFound) if dir.join(LEGACY_DEVICE_FILE).exists() => {
                migrated = true;
                migrate_legacy_device(dir, &master, &mut state).await?
            }
            Err(Error::NotFound) => {
                // A restored backup carries the shared vault but not this
                // device's keys, by design: a backup must never hand our
                // identity private key to another device. Mint a new device
                // identity so the restored vault is usable.
                fresh_device = true;
                DeviceKeys::create(dir, &master, 1).await?
            }
            Err(e) => return Err(e),
        };

        let chat_log = ChatLog::load(dir, &master)?;

        let mut pending_recovery_code = None;
        if !recovery_files_exist(dir) {
            let code = generate_recovery_code();
            ensure_recovery_files(dir, &master, &code)?;
            state.recovery_acknowledged = false;
            pending_recovery_code = Some(code);
        }

        let needs_save = migrated || fresh_device || pending_recovery_code.is_some();
        let account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code,
        };
        if needs_save {
            account.save().await?;
        }
        if migrated {
            // Safe only now that `device.enc` holds the same identity: the
            // legacy file carries the identity private key and would otherwise
            // ride along in the next backup.
            let _ = fs::remove_file(dir.join(LEGACY_DEVICE_FILE));
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

    pub fn mail_handle(&self) -> Option<&str> {
        self.state.mail_handle.as_deref()
    }

    pub fn mail_address(&self) -> Option<String> {
        self.state
            .mail_handle
            .as_deref()
            .map(crate::mail::format_address)
    }

    /// Set or change this member's mail handle.
    ///
    /// The cached invite is dropped so the next `invite()` mints one carrying
    /// the new handle; otherwise "Copy my invite" would keep advertising the old
    /// address until the process restarted.
    pub async fn set_mail_handle(&mut self, handle: &str) -> Result<(), Error> {
        self.state.mail_handle = Some(crate::mail::normalize_handle(handle)?);
        self.state.invite_cache = None;
        self.save().await
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
        let bundle = mint_prekey_bundle(self.device.store_mut()).await?;
        let payload = invite_from_bundle(
            &self.state.relay,
            &self.state.protocol_name,
            self.device.queue_id(),
            self.device.queue_token(),
            self.state.mail_handle.as_deref(),
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
            self.device.store_mut(),
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
            mail_handle: payload.handle.clone(),
            inbound_ready: false,
        });
        self.save().await?;
        Ok(())
    }

    pub async fn remove_contact(&mut self, nickname: &str) -> Result<(), Error> {
        let pos = self
            .state
            .contacts
            .iter()
            .position(|c| c.nickname == nickname)
            .ok_or(Error::ContactNotFound)?;
        self.state.contacts.remove(pos);
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
        self.send_file_bytes(nickname, &name, &data).await?;
        Ok(())
    }

    pub async fn send_file_bytes(
        &mut self,
        nickname: &str,
        name: &str,
        data: &[u8],
    ) -> Result<BlobId, Error> {
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
        Ok(blob_id)
    }

    /// Send an internal mail message, returning the thread id.
    ///
    /// Mail is async by design: unlike `send_text`, this does not check whether
    /// the contact is online. Attachments are inlined on the wire (capped) and
    /// also stored as encrypted vault blobs on our side so the sent message is
    /// openable after a restart.
    pub async fn send_mail(
        &mut self,
        nickname: &str,
        thread_id: Option<&str>,
        subject: Option<&str>,
        body: &str,
        attachments: &[(String, Vec<u8>)],
    ) -> Result<String, Error> {
        if attachments.len() > MAX_MAIL_ATTACHMENTS {
            return Err(Error::TooManyAttachments(MAX_MAIL_ATTACHMENTS));
        }
        let total: usize = attachments.iter().map(|(_, d)| d.len()).sum();
        if total > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let thread_id = thread_id
            .map(|t| t.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let wire: Vec<MailAttachment> = attachments
            .iter()
            .map(|(name, data)| MailAttachment {
                name: name.clone(),
                data: data.clone(),
            })
            .collect();
        self.send_payload_inner(
            nickname,
            MessagePayload::Mail {
                thread_id: thread_id.clone(),
                subject: subject.map(|s| s.to_string()),
                body: body.to_string(),
                attachments: wire,
            },
            false,
        )
        .await?;

        // Store our own copies as blobs, then record the message. `put_blob`
        // borrows the vault, so the ids are collected before the log is written.
        let mut records = Vec::with_capacity(attachments.len());
        for (name, data) in attachments {
            let id = self.vault.put_blob(data)?;
            records.push(MailAttachmentRecord {
                name: name.clone(),
                blob_id_hex: hex::encode(id.0),
                size: data.len() as u64,
            });
        }
        let master = *self.vault.master_key();
        self.chat_log.append_mail(
            &master, nickname, true, &thread_id, subject, body, &records,
        )?;
        Ok(thread_id)
    }

    pub fn mail_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.mail_records().cloned().collect()
    }

    pub async fn heartbeat_presence(&self) -> Result<(), Error> {
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .touch_presence(self.device.queue_id(), self.device.queue_token())
            .await
    }

    pub async fn contact_is_online(&self, nickname: &str) -> Result<bool, Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?;
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .contact_presence_online(&contact.queue_id, &contact.queue_token)
            .await
    }

    async fn send_payload(&mut self, nickname: &str, payload: MessagePayload) -> Result<(), Error> {
        self.send_payload_inner(nickname, payload, true).await
    }

    /// `require_online` is false for mail: mail is delivered into the recipient's
    /// store-and-forward queue while they are away, which is exactly the
    /// difference from live-only chat (5h).
    async fn send_payload_inner(
        &mut self,
        nickname: &str,
        payload: MessagePayload,
        require_online: bool,
    ) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        if require_online && !self.contact_is_online(nickname).await? {
            return Err(Error::ContactOffline);
        }
        let plain = payload.encode()?;
        let ct = encrypt_message(
            self.device.store_mut(),
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
            .list(self.device.queue_id(), self.device.queue_token())
            .await?;
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let mut dirty = false;
        for blob_id in ids {
            let body = relay
                .get(self.device.queue_id(), self.device.queue_token(), &blob_id)
                .await?;
            let (ct, legacy_from) = decode_relay_blob(&body)?;
            let (plain, from_protocol) = match legacy_from {
                Some(from) => match decrypt_message(
                    self.device.store_mut(),
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
            let nick = self
                .contact_nick_for_protocol(&from_protocol)
                .unwrap_or_else(|| short_name(&from_protocol));
            let (file_blob_id, mail_attachments) = match &payload {
                MessagePayload::File { data, name } => {
                    let id = self.vault.put_blob(data)?;
                    let master = *self.vault.master_key();
                    self.chat_log
                        .append_file(&master, &nick, false, name, &id, data.len() as u64)?;
                    (Some(id), Vec::new())
                }
                MessagePayload::Mail {
                    thread_id,
                    subject,
                    body,
                    attachments,
                } => {
                    let mut blobs = Vec::with_capacity(attachments.len());
                    let mut records = Vec::with_capacity(attachments.len());
                    for a in attachments {
                        let id = self.vault.put_blob(&a.data)?;
                        records.push(MailAttachmentRecord {
                            name: a.name.clone(),
                            blob_id_hex: hex::encode(id.0),
                            size: a.data.len() as u64,
                        });
                        blobs.push(ReceivedAttachment {
                            name: a.name.clone(),
                            blob_id: id,
                            size: a.data.len() as u64,
                        });
                    }
                    let master = *self.vault.master_key();
                    self.chat_log.append_mail(
                        &master,
                        &nick,
                        false,
                        thread_id,
                        subject.as_deref(),
                        body,
                        &records,
                    )?;
                    (None, blobs)
                }
                MessagePayload::Text { body } => {
                    let master = *self.vault.master_key();
                    self.chat_log.append_text(&master, &nick, false, body)?;
                    (None, Vec::new())
                }
            };
            if let Some(c) = self
                .state
                .contacts
                .iter_mut()
                .find(|c| c.protocol_name == from_protocol)
            {
                c.inbound_ready = true;
            }
            out.push(ReceivedMessage {
                from_nick: nick,
                payload,
                file_blob_id,
                mail_attachments,
            });
            relay
                .delete(self.device.queue_id(), self.device.queue_token(), &blob_id)
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
                self.device.store_mut(),
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
        self.device.save(&self.dir, master, &session_names).await?;
        Ok(())
    }
}

/// Move a pre-multi-device vault onto `device.enc`.
///
/// The identity key is preserved exactly, so the protocol name, every contact
/// and every session are unaffected. The legacy file is deleted by the caller
/// once the new one is written.
async fn migrate_legacy_device(
    dir: &Path,
    master: &[u8; crate::crypto::MASTER_KEY_LEN],
    state: &mut AccountState,
) -> Result<DeviceKeys, Error> {
    let signal_bytes = fs::read(dir.join(LEGACY_DEVICE_FILE)).map_err(|_| Error::NotFound)?;
    let signal_plain = decrypt(master, &signal_bytes)?;
    let store = import_store(&signal_plain).await?;
    let queue_id = state.receive_queue_id.take().ok_or(Error::Corrupt)?;
    let queue_token = state.receive_queue_token.take().ok_or(Error::Corrupt)?;
    // Device 1 is the identity that already existed, so it keeps its key.
    Ok(DeviceKeys::from_store(1, store, queue_id, queue_token))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DEVICE_FILE;
    use crate::signal_persist::export_store;
    use crate::vault::Vault;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hush-account-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Write the on-disk layout a vault had before the multi-device model: the
    /// device identity in `signal.enc`, and this device's relay inbox in
    /// `account.enc`.
    async fn write_legacy_vault(dir: &Path, passphrase: &str) -> (String, String, Vec<u8>) {
        let vault = Vault::create(dir, passphrase).unwrap();
        let master = *vault.master_key();
        let mut device = DeviceKeys::create(dir, &master, 1).await.unwrap();
        let _ = mint_prekey_bundle(device.store_mut()).await.unwrap();
        let protocol_name = protocol_name_from_identity(
            &device
                .store_mut()
                .get_identity_key_pair()
                .await
                .unwrap(),
        );
        let identity = device.identity_public().await.unwrap();
        let queue_id = device.queue_id().to_string();
        let queue_token = device.queue_token().to_string();

        let store_bytes = export_store(device.store_mut(), &[]).await.unwrap();
        fs::write(
            dir.join(LEGACY_DEVICE_FILE),
            encrypt(&master, &store_bytes).unwrap(),
        )
        .unwrap();

        let state = AccountState {
            relay: "http://127.0.0.1:1".into(),
            protocol_name: protocol_name.clone(),
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: true,
            last_backup_unix: 0,
            receive_queue_id: Some(queue_id.clone()),
            receive_queue_token: Some(queue_token.clone()),
            mail_handle: None,
        };
        let state_bytes = serde_json::to_vec(&state).unwrap();
        fs::write(dir.join("account.enc"), encrypt(&master, &state_bytes).unwrap()).unwrap();

        // The whole point: this vault predates device.enc.
        fs::remove_file(dir.join(DEVICE_FILE)).unwrap();
        (protocol_name, queue_id, identity)
    }

    #[tokio::test]
    async fn legacy_vault_migrates_keeping_identity_name_and_inbox() {
        let dir = scratch("legacy");
        let (protocol_name, queue_id, identity) = write_legacy_vault(&dir, "pw").await;

        let account = Account::open(&dir, "pw").await.unwrap();

        assert_eq!(
            account.protocol_name(),
            protocol_name,
            "migration must not rename the account, or every contact breaks"
        );
        assert_eq!(
            account.device.queue_id(),
            queue_id,
            "the relay inbox must move across, not be regenerated"
        );
        assert!(dir.join(DEVICE_FILE).exists(), "migration must write device.enc");
        assert!(
            !dir.join(LEGACY_DEVICE_FILE).exists(),
            "the legacy key file holds the identity private key and must not linger"
        );

        let master = *account.vault.master_key();
        let reloaded = DeviceKeys::load(&dir, &master).await.unwrap();
        assert_eq!(
            reloaded.identity_public().await.unwrap(),
            identity,
            "the identity key must be preserved exactly, or sessions with existing contacts break"
        );
    }

    #[tokio::test]
    async fn a_restored_backup_gets_a_fresh_device_identity() {
        let dir = scratch("restore-source");
        let account = Account::create(&dir, "pw", "http://127.0.0.1:1")
            .await
            .unwrap();
        let original = account
            .device
            .identity_public()
            .await
            .unwrap();
        let archive = export_vault(&dir).unwrap();
        drop(account);

        let restored = scratch("restore-target");
        import_vault(&restored, &archive).unwrap();
        let account = Account::open(&restored, "pw").await.unwrap();

        assert!(
            restored.join(DEVICE_FILE).exists(),
            "a restored vault must mint a device identity, or it cannot be used"
        );
        assert_ne!(
            account.device.identity_public().await.unwrap(),
            original,
            "the restore must not resurrect the backed-up identity key"
        );
    }

    #[tokio::test]
    async fn a_legacy_vault_does_not_leak_its_identity_through_backup() {
        let dir = scratch("legacy-backup");
        let (_, _, _) = write_legacy_vault(&dir, "pw").await;
        let archive = export_vault(&dir).unwrap();

        let mut tar = tar::Archive::new(&archive[..]);
        let names: Vec<String> = tar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.ends_with(LEGACY_DEVICE_FILE)),
            "an unmigrated vault must not export its identity key: {names:?}"
        );
    }
}
