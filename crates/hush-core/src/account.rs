use std::fs;
use std::path::{Path, PathBuf};

use libsignal_protocol::{IdentityKeyStore, InMemSignalProtocolStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, random_bytes};
use crate::error::Error;
use crate::invite::InvitePayload;
use crate::relay_client::RelayClient;
use crate::signal::{
    decrypt_message, encrypt_message, invite_from_bundle, mint_prekey_bundle,
    process_contact_bundle, protocol_name_from_identity,
};
use crate::signal_persist::{export_store, import_store};
use crate::vault::Vault;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contact {
    pub nickname: String,
    pub protocol_name: String,
    pub queue_id: String,
    pub queue_token: String,
}

#[derive(Serialize, Deserialize)]
struct AccountState {
    relay: String,
    protocol_name: String,
    receive_queue_id: String,
    receive_queue_token: String,
    contacts: Vec<Contact>,
}

#[derive(Serialize, Deserialize)]
struct WireEnvelope {
    from: String,
    ct: Vec<u8>,
}

pub struct Account {
    vault: Vault,
    store: InMemSignalProtocolStore,
    state: AccountState,
    dir: PathBuf,
}

impl Account {
    pub async fn create(dir: &Path, passphrase: &str, relay: &str) -> Result<Self, Error> {
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
            relay: relay.to_string(),
            protocol_name,
            receive_queue_id,
            receive_queue_token,
            contacts: Vec::new(),
        };
        let account = Self {
            vault,
            store,
            state,
            dir: dir.to_path_buf(),
        };
        account.save().await?;
        Ok(account)
    }

    pub async fn open(dir: &Path, passphrase: &str) -> Result<Self, Error> {
        let vault = Vault::open(dir, passphrase)?;
        let packed = fs::read(dir.join("account.enc")).map_err(|_| Error::NotFound)?;
        let master = vault.master_key();
        let state_bytes = decrypt(master, &packed)?;
        let state: AccountState = serde_json::from_slice(&state_bytes).map_err(|_| Error::Corrupt)?;
        let signal_bytes = fs::read(dir.join("signal.enc")).map_err(|_| Error::NotFound)?;
        let signal_plain = decrypt(master, &signal_bytes)?;
        let store = import_store(&signal_plain).await?;
        Ok(Self {
            vault,
            store,
            state,
            dir: dir.to_path_buf(),
        })
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

    pub async fn invite(&mut self) -> Result<String, Error> {
        let bundle = mint_prekey_bundle(&mut self.store).await?;
        let payload = invite_from_bundle(
            &self.state.relay,
            &self.state.protocol_name,
            &self.state.receive_queue_id,
            &self.state.receive_queue_token,
            &bundle,
        )?;
        self.save().await?;
        payload.encode()
    }

    pub async fn add_contact(&mut self, nickname: &str, invite_str: &str) -> Result<(), Error> {
        let payload = InvitePayload::decode(invite_str)?;
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
        });
        self.save().await?;
        Ok(())
    }

    pub async fn send_text(&mut self, nickname: &str, text: &str) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        let ct = encrypt_message(
            &mut self.store,
            &self.state.protocol_name,
            &contact.protocol_name,
            text,
        )
        .await?;
        let wire = WireEnvelope {
            from: self.state.protocol_name.clone(),
            ct,
        };
        let body = serde_json::to_vec(&wire).map_err(|_| Error::Corrupt)?;
        let relay = RelayClient::new(&self.state.relay);
        relay
            .put(&contact.queue_id, &contact.queue_token, &body)
            .await?;
        self.save().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Vec<(String, String)>, Error> {
        let relay = RelayClient::new(&self.state.relay);
        let ids = relay
            .list(&self.state.receive_queue_id, &self.state.receive_queue_token)
            .await?;
        let mut out = Vec::new();
        for blob_id in ids {
            let body = relay
                .get(
                    &self.state.receive_queue_id,
                    &self.state.receive_queue_token,
                    &blob_id,
                )
                .await?;
            let wire: WireEnvelope = serde_json::from_slice(&body).map_err(|_| Error::Corrupt)?;
            let plain = decrypt_message(
                &mut self.store,
                &self.state.protocol_name,
                &wire.from,
                &wire.ct,
            )
            .await?;
            let text = String::from_utf8(plain).map_err(|_| Error::Corrupt)?;
            let nick = self
                .state
                .contacts
                .iter()
                .find(|c| c.protocol_name == wire.from)
                .map(|c| c.nickname.clone())
                .unwrap_or_else(|| wire.from.clone());
            out.push((nick, text));
            relay
                .delete(
                    &self.state.receive_queue_id,
                    &self.state.receive_queue_token,
                    &blob_id,
                )
                .await?;
        }
        if !out.is_empty() {
            self.save().await?;
        }
        Ok(out)
    }

    fn find_contact(&self, nickname: &str) -> Option<&Contact> {
        self.state.contacts.iter().find(|c| c.nickname == nickname)
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
