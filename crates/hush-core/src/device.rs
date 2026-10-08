use std::path::Path;

use libsignal_protocol::{IdentityKeyStore, InMemSignalProtocolStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, random_bytes, MASTER_KEY_LEN};
use crate::error::Error;
use crate::signal_persist::{export_store, import_store};

/// This device's own key material.
pub const DEVICE_FILE: &str = "device.enc";

/// Where this device's identity lived before the multi-device model.
///
/// Read once during migration, then removed. It must stay out of backups for
/// the same reason `device.enc` does: it holds the identity private key.
pub const LEGACY_DEVICE_FILE: &str = "signal.enc";

/// The one device id an account ever has.
///
/// Multi-device is gone: an account is a single identity and a single relay
/// inbox. libsignal still models an address as `(name, device_id)`, so a
/// constant keeps the peer addresses stable rather than threading a value that
/// can only ever be this.
pub const DEVICE_ID: u32 = 1;

const DEVICE_FORMAT_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct DeviceFile {
    v: u32,
    /// Kept so a file written before single-device still reads back, and so the
    /// format does not change shape. Always [`DEVICE_ID`] now, and deliberately
    /// not validated on load: a vault whose file names another device is still
    /// the same account, and refusing to open it would strand the user's data
    /// for no security gain.
    #[allow(dead_code)]
    device_id: u32,
    queue_id: String,
    /// The *private* read credential for this device's relay inbox.
    ///
    /// Only the delivery token derived from it is ever published, because the
    /// read token also authorizes burning the queue.
    read_token: String,
    /// The libsignal store, kept as nested JSON rather than a byte array so the
    /// file stays inspectable without a decoder.
    signal: serde_json::Value,
}

/// The pre-split layout: one token doing both jobs, and no read credential.
///
/// Kept as its own struct because `read_token` is required in v2, so a v1 file
/// cannot be deserialized into `DeviceFile` at all.
#[derive(Deserialize)]
struct DeviceFileV1 {
    v: u32,
    /// Read only to document the old shape; the device id is no longer used.
    #[allow(dead_code)]
    device_id: u32,
    /// Read only to document the old shape; the queue is replaced on migration,
    /// so the abandoned id is deliberately not carried forward.
    #[allow(dead_code)]
    queue_id: String,
    #[allow(dead_code)]
    queue_token: String,
    signal: serde_json::Value,
}

/// Key material belonging to this account's one device.
///
/// Deliberately kept out of backups: a copy of these bytes would hold our
/// identity private key and could impersonate us.
pub struct DeviceKeys {
    store: InMemSignalProtocolStore,
    queue_id: String,
    /// Private. Authorizes reading, deleting and burning this device's own
    /// inbox, so it must never appear in an invite.
    read_token: String,
    /// Set by [`DeviceKeys::load`] when a pre-split queue had to be replaced.
    ///
    /// The caller has to know: the old queue id may already have been handed to
    /// contacts in an invite, so the cached invite has to be dropped and the
    /// new inbox announced. With one device there is no list to republish.
    queue_rotated: bool,
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omits the store, the read token and the queue so a stray
        // `{:?}` in a log line cannot leak key material.
        f.debug_struct("DeviceKeys").finish_non_exhaustive()
    }
}

impl DeviceKeys {
    /// Wrap a store that already exists (used when migrating a vault that
    /// predates the current device model).
    ///
    /// The legacy queue is deliberately *not* carried over. Its token was a
    /// single credential used for both delivery and reading, and it was handed
    /// out in every invite, so it is already public: keeping it would keep the
    /// hole. A fresh queue is minted.
    pub fn from_store(store: InMemSignalProtocolStore) -> Self {
        Self {
            store,
            queue_id: Uuid::new_v4().to_string(),
            read_token: hex::encode(random_bytes::<32>()),
            queue_rotated: true,
        }
    }

    /// Generate a brand-new device identity and persist it.
    pub async fn create(dir: &Path, master: &[u8; MASTER_KEY_LEN]) -> Result<Self, Error> {
        let keys = Self {
            store: crate::signal::new_protocol_store()?,
            queue_id: Uuid::new_v4().to_string(),
            read_token: hex::encode(random_bytes::<32>()),
            queue_rotated: false,
        };
        // A fresh device has no sessions yet, so there is nothing to export.
        keys.save(dir, master, &[]).await?;
        Ok(keys)
    }

    pub async fn load(dir: &Path, master: &[u8; MASTER_KEY_LEN]) -> Result<Self, Error> {
        let path = dir.join(DEVICE_FILE);
        if !path.exists() {
            return Err(Error::NotFound);
        }
        let packed = std::fs::read(&path)?;
        let plain = decrypt(master, &packed)?;
        let value: serde_json::Value =
            serde_json::from_slice(&plain).map_err(|_| Error::Corrupt)?;
        let version = value.get("v").and_then(|v| v.as_u64()).unwrap_or(0);

        // A v1 file has no read token, and its `queue_token` was published in
        // invites, so the queue cannot be salvaged by treating that token as the
        // read credential — the relay would reject it, and accepting it would be
        // the hole this split exists to close. Replace the queue instead. The
        // device identity and every session are untouched, so contacts keep
        // working once they see the new inbox.
        let (store, queue_id, read_token, queue_rotated) = match version {
            1 => {
                let file: DeviceFileV1 =
                    serde_json::from_value(value).map_err(|_| Error::Corrupt)?;
                if file.v != 1 {
                    return Err(Error::Corrupt);
                }
                let signal = serde_json::to_vec(&file.signal).map_err(|_| Error::Corrupt)?;
                let store = import_store(&signal).await?;
                (
                    store,
                    Uuid::new_v4().to_string(),
                    hex::encode(random_bytes::<32>()),
                    true,
                )
            }
            v if v == u64::from(DEVICE_FORMAT_VERSION) => {
                let file: DeviceFile =
                    serde_json::from_value(value).map_err(|_| Error::Corrupt)?;
                let signal = serde_json::to_vec(&file.signal).map_err(|_| Error::Corrupt)?;
                let store = import_store(&signal).await?;
                (store, file.queue_id, file.read_token, false)
            }
            _ => return Err(Error::Corrupt),
        };

        Ok(Self {
            store,
            queue_id,
            read_token,
            queue_rotated,
        })
    }

    /// Whether this device's queue was replaced while loading.
    ///
    /// True for a v1 file and for a migrated legacy vault. The caller must drop
    /// any cached invite, or peers keep delivering to an inbox this device no
    /// longer reads.
    pub fn queue_rotated(&self) -> bool {
        self.queue_rotated
    }

    /// Persist this device's material.
    ///
    /// `session_addresses` is the set of remote account ids whose sessions
    /// should be carried over; the store cannot enumerate them itself. With one
    /// device per account every address is `(account_id, DEVICE_ID)`.
    pub async fn save(
        &self,
        dir: &Path,
        master: &[u8; MASTER_KEY_LEN],
        session_addresses: &[(String, u32)],
    ) -> Result<(), Error> {
        let signal = export_store(&self.store, session_addresses).await?;
        let signal: serde_json::Value =
            serde_json::from_slice(&signal).map_err(|_| Error::Corrupt)?;
        let file = DeviceFile {
            v: DEVICE_FORMAT_VERSION,
            device_id: DEVICE_ID,
            queue_id: self.queue_id.clone(),
            read_token: self.read_token.clone(),
            signal,
        };
        let plain = serde_json::to_vec(&file).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(master, &plain)?;
        std::fs::write(dir.join(DEVICE_FILE), packed)?;
        Ok(())
    }

    pub fn device_id(&self) -> u32 {
        DEVICE_ID
    }

    pub fn queue_id(&self) -> &str {
        &self.queue_id
    }

    /// The *public* credential for this device's inbox: what goes in an invite,
    /// and what a peer uses to deliver.
    pub fn queue_token(&self) -> String {
        crate::crypto::queue_delivery_token(&self.read_token)
    }

    /// The private credential for this device's inbox: what authorizes listing,
    /// reading, deleting and burning it.
    pub fn read_token(&self) -> &str {
        &self.read_token
    }

    /// The relay queue this device mints for one contact to deliver into.
    ///
    /// Per contact: cutting a contact off means burning the one queue it was
    /// handed. Deriving the id from this device's read token and the contact's
    /// account id keeps that per-contact without a table of secrets to store or
    /// migrate.
    pub fn contact_queue_id(&self, account_id: &str) -> String {
        crate::crypto::contact_queue_id(&self.read_token, account_id)
    }

    /// The private read token for a contact's queue on this device.
    ///
    /// Never published and never written to the vault, exactly like the device
    /// read token it is derived from: it is what lets this device drain and burn
    /// that one contact's queue.
    pub fn contact_queue_read_token(&self, account_id: &str) -> String {
        crate::crypto::contact_queue_read_token(&self.read_token, account_id)
    }

    /// The public credential a contact is handed so it can deliver to this
    /// device's queue for it. This is what a queue grant carries.
    pub fn contact_queue_delivery_token(&self, account_id: &str) -> String {
        crate::crypto::queue_delivery_token(&self.contact_queue_read_token(account_id))
    }

    pub fn store_mut(&mut self) -> &mut InMemSignalProtocolStore {
        &mut self.store
    }

    pub async fn registration_id(&self) -> Result<u32, Error> {
        self.store
            .get_local_registration_id()
            .await
            .map_err(|_| Error::Corrupt)
    }

    pub async fn identity_key_pair(&self) -> Result<libsignal_protocol::IdentityKeyPair, Error> {
        self.store
            .get_identity_key_pair()
            .await
            .map_err(|_| Error::Corrupt)
    }

    /// Mint a fresh set of publishable prekeys for this device.
    ///
    /// The private halves stay in the store; only the returned record goes on the
    /// wire.
    pub async fn mint_prekeys(
        &mut self,
        one_time_count: usize,
    ) -> Result<crate::prekey::PreKeyRecord, Error> {
        crate::signal::mint_publishable_prekeys(&mut self.store, DEVICE_ID, one_time_count).await
    }

    pub async fn identity_public(&self) -> Result<Vec<u8>, Error> {
        Ok(self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|_| Error::Corrupt)?
            .identity_key()
            .serialize()
            .to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Vault;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("hush-device-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn device_identity_is_stored_outside_the_shared_vault() {
        let dir = scratch("identity");
        let vault = Vault::create(&dir, "pw").unwrap();
        let device = DeviceKeys::create(&dir, vault.master_key()).await.unwrap();

        assert!(dir.join(DEVICE_FILE).exists());
        assert_eq!(device.device_id(), DEVICE_ID);

        let reloaded = DeviceKeys::load(&dir, vault.master_key()).await.unwrap();
        assert_eq!(reloaded.device_id(), device.device_id());
        assert_eq!(
            reloaded.identity_public().await.unwrap(),
            device.identity_public().await.unwrap(),
            "the device identity must survive a reload"
        );
    }

    #[tokio::test]
    async fn device_file_is_not_plaintext() {
        let dir = scratch("at-rest");
        let vault = Vault::create(&dir, "pw").unwrap();
        DeviceKeys::create(&dir, vault.master_key()).await.unwrap();

        let raw = std::fs::read(dir.join(DEVICE_FILE)).unwrap();
        let identity = DeviceKeys::load(&dir, vault.master_key())
            .await
            .unwrap()
            .identity_public()
            .await
            .unwrap();
        assert!(
            !raw.windows(identity.len())
                .any(|w| w == identity.as_slice()),
            "the identity public key must not appear in the clear"
        );
    }

    #[tokio::test]
    async fn device_identity_survives_a_passphrase_change() {
        let dir = scratch("rotate");
        let vault = Vault::create(&dir, "pw").unwrap();
        let before = DeviceKeys::create(&dir, vault.master_key())
            .await
            .unwrap()
            .identity_public()
            .await
            .unwrap();

        vault.change_passphrase("pw", "pw2").unwrap();

        let reopened = Vault::open(&dir, "pw2").unwrap();
        let after = DeviceKeys::load(&dir, reopened.master_key())
            .await
            .unwrap()
            .identity_public()
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "rotating the passphrase must not touch the device identity"
        );
    }

    #[tokio::test]
    async fn loading_a_missing_device_file_reports_not_found() {
        let dir = scratch("missing");
        let vault = Vault::create(&dir, "pw").unwrap();
        assert!(matches!(
            DeviceKeys::load(&dir, vault.master_key()).await.unwrap_err(),
            Error::NotFound
        ));
    }
}
