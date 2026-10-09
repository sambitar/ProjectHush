use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{
    decrypt, derive_master_key, encrypt, random_bytes, sha256, MASTER_KEY_LEN, SALT_LEN,
};
use crate::error::Error;
use crate::lockfile::LockFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityPub(pub [u8; 32]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobId(pub [u8; 32]);

const LOCK_FILE_NAME: &str = "vault.lock.json";

/// Read and parse the vault unlock header.
fn read_lock(dir: &Path) -> Result<LockFile, Error> {
    let path = dir.join(LOCK_FILE_NAME);
    if !path.exists() {
        return Err(Error::NotFound);
    }
    serde_json::from_slice(&fs::read(&path)?).map_err(|_| Error::Corrupt)
}

/// Write the unlock header via write-then-rename.
///
/// A crash partway through must not leave an unreadable header, which would lock
/// the owner out of their own vault with no way back.
fn write_lock(dir: &Path, lock: &LockFile) -> Result<(), Error> {
    let tmp = dir.join("vault.lock.json.tmp");
    let bytes = serde_json::to_vec_pretty(lock).map_err(|_| Error::Corrupt)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dir.join(LOCK_FILE_NAME))?;
    Ok(())
}

/// Derive the passphrase key (KEK) described by the header.
fn derive_kek(lock: &LockFile, passphrase: &str) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, Error> {
    if lock.kdf != "argon2id" {
        return Err(Error::Corrupt);
    }
    let salt = hex::decode(&lock.salt_hex).map_err(|_| Error::Corrupt)?;
    derive_master_key(passphrase, &salt, lock.m_cost, lock.t_cost, lock.p_cost)
}

/// Recover the master key from a passphrase.
///
/// Format v2 unwraps it; format v1 derived it directly, so the derived key *is*
/// the master key.
fn unlock_master(
    lock: &LockFile,
    passphrase: &str,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, Error> {
    let kek = derive_kek(lock, passphrase)?;
    let Some(wrapped_hex) = lock.wrapped_master.as_deref() else {
        return Ok(kek);
    };
    let packed = hex::decode(wrapped_hex).map_err(|_| Error::Corrupt)?;
    let bytes = decrypt(&kek, &packed)?;
    if bytes.len() != MASTER_KEY_LEN {
        return Err(Error::Corrupt);
    }
    let mut master = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    master.copy_from_slice(&bytes);
    Ok(master)
}

/// Build a fresh v2 header wrapping `master` under `passphrase`.
fn wrap_master(master: &[u8; MASTER_KEY_LEN], passphrase: &str) -> Result<LockFile, Error> {
    let salt = random_bytes::<SALT_LEN>();
    let params = crate::crypto::argon2_params()?;
    let mut lock = LockFile {
        kdf: "argon2id".into(),
        salt_hex: hex::encode(salt),
        m_cost: params.m_cost(),
        t_cost: params.t_cost(),
        p_cost: params.p_cost(),
        wrapped_master: None,
    };
    let kek = derive_kek(&lock, passphrase)?;
    lock.wrapped_master = Some(hex::encode(encrypt(&kek, master)?));
    Ok(lock)
}

pub struct Vault {
    dir: PathBuf,
    master: Zeroizing<[u8; MASTER_KEY_LEN]>,
    identity_public: IdentityPub,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Vault {
    pub fn create(dir: &Path, passphrase: &str) -> Result<Self, Error> {
        if passphrase.is_empty() {
            return Err(Error::EmptyPassphrase);
        }
        fs::create_dir_all(dir)?;
        if dir.join(LOCK_FILE_NAME).exists() {
            return Err(Error::AlreadyExists);
        }

        // The master key is random and only *wrapped* by the passphrase, so a
        // later passphrase change rewrites the header and nothing else.
        let master = Zeroizing::new(random_bytes::<MASTER_KEY_LEN>());
        write_lock(dir, &wrap_master(&master, passphrase)?)?;

        let signing = SigningKey::from_bytes(&random_bytes::<32>());
        let verifying: VerifyingKey = signing.verifying_key();
        let identity_public = IdentityPub(verifying.to_bytes());

        fs::write(dir.join("identity.pub"), identity_public.0)?;

        let mut secret = signing.to_bytes();
        let wrapped = encrypt(&master, &secret)?;
        secret.zeroize();
        fs::write(dir.join("identity.secret"), wrapped)?;
        fs::create_dir_all(dir.join("blobs"))?;

        Ok(Self {
            dir: dir.to_path_buf(),
            master,
            identity_public,
        })
    }

    pub fn open_with_master(dir: &Path, master: Zeroizing<[u8; MASTER_KEY_LEN]>) -> Result<Self, Error> {
        let lock_path = dir.join(LOCK_FILE_NAME);
        if !lock_path.exists() {
            return Err(Error::NotFound);
        }
        let packed = fs::read(dir.join("identity.secret"))?;
        let secret_bytes = decrypt(&master, &packed)?;
        if secret_bytes.len() != 32 {
            return Err(Error::Corrupt);
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&secret_bytes);
        let signing = SigningKey::from_bytes(&sk);
        sk.zeroize();
        let identity_public = IdentityPub(signing.verifying_key().to_bytes());
        Ok(Self {
            dir: dir.to_path_buf(),
            master,
            identity_public,
        })
    }

    pub fn open(dir: &Path, passphrase: &str) -> Result<Self, Error> {
        if passphrase.is_empty() {
            return Err(Error::EmptyPassphrase);
        }
        let lock = read_lock(dir)?;
        let master = unlock_master(&lock, passphrase)?;
        Self::open_with_master(dir, master)
    }

    /// Re-wrap the master key under a new passphrase.
    ///
    /// The master key itself does not change, so every blob, `account.enc`,
    /// `chat.enc` and the recovery wrap stay valid. Only the unlock header is
    /// rewritten, which keeps this atomic and independent of vault size.
    ///
    /// A v1 vault (master derived from the passphrase) is upgraded to v2 here:
    /// the master key it already has is kept and re-wrapped, so existing
    /// ciphertext is untouched.
    pub fn change_passphrase(&self, current: &str, new: &str) -> Result<(), Error> {
        if new.is_empty() {
            return Err(Error::EmptyPassphrase);
        }
        let lock = read_lock(&self.dir)?;
        if *unlock_master(&lock, current)? != *self.master {
            return Err(Error::WrongPassphrase);
        }
        write_lock(&self.dir, &wrap_master(&self.master, new)?)
    }

    pub fn identity_public(&self) -> IdentityPub {
        self.identity_public
    }

    pub(crate) fn master_key(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.master
    }

    pub fn put_blob(&self, plaintext: &[u8]) -> Result<BlobId, Error> {
        let id = BlobId(sha256(plaintext));
        let packed = encrypt(&self.master, plaintext)?;
        let path = self.blob_path(&id);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, packed)?;
        Ok(id)
    }

    pub fn get_blob(&self, id: &BlobId) -> Result<Vec<u8>, Error> {
        let path = self.blob_path(id);
        let packed = fs::read(path).map_err(|_| Error::BlobNotFound)?;
        decrypt(&self.master, &packed)
    }

    fn blob_path(&self, id: &BlobId) -> PathBuf {
        self.dir.join("blobs").join(hex::encode(id.0))
    }

    /// Mint the vault's published drop key.
    ///
    /// The published string is the address outsiders hold, so creation is a
    /// deliberate act and a second create is refused rather than rotating
    /// silently — rotation is its own operation, with the old key archived.
    pub fn create_drop_keypair(&self) -> Result<String, Error> {
        if self.read_drop_keys()?.is_some() {
            return Err(Error::DropKeyExists);
        }
        let kp = crate::drop::drop_keypair();
        self.write_drop_keys(&DropKeysFile {
            current: DropKeyEntry::from(&kp),
            retired: Vec::new(),
        })?;
        Ok(crate::drop::encode_drop_pk(&kp.pk))
    }

    /// Rotate: the current key retires — the daemon keeps draining its
    /// rendezvous slot until the relay TTL expires it, so drops sealed in
    /// flight are never lost — a new key takes its place, and the retired list
    /// is capped at the three newest.
    pub fn rotate_drop_keypair(&self) -> Result<String, Error> {
        let mut keys = self.read_drop_keys()?.ok_or(Error::DropKeyMissing)?;
        // Clone, not move: the file type zeroizes on drop.
        let mut retired = keys.current.clone();
        retired.retired_unix = now_secs();
        keys.retired.insert(0, retired);
        keys.retired.truncate(RETIRED_DROP_KEYS);
        let kp = crate::drop::drop_keypair();
        keys.current = DropKeyEntry::from(&kp);
        self.write_drop_keys(&keys)?;
        Ok(crate::drop::encode_drop_pk(&kp.pk))
    }

    /// The pasteable public drop key, when one exists. This is the string a
    /// member publishes; nothing else identifies the drop address.
    pub fn drop_public_key(&self) -> Option<String> {
        let file = self.read_drop_keys().ok()??;
        let pk = hex::decode(&file.current.pk_hex).ok()?;
        let pk: [u8; 32] = pk.try_into().ok()?;
        Some(crate::drop::encode_drop_pk(&pk))
    }

    /// Every drop key still worth draining: the current one first, then the
    /// retired ones, newest first.
    pub(crate) fn drop_keys_for_drain(&self) -> Vec<DropDrainKey> {
        let file = match self.read_drop_keys() {
            Ok(Some(file)) => file,
            _ => return Vec::new(),
        };
        let mut keys = Vec::with_capacity(file.retired.len() + 1);
        if let Some(k) = file.current.to_drain_key() {
            keys.push(k);
        }
        for entry in &file.retired {
            if let Some(k) = entry.to_drain_key() {
                keys.push(k);
            }
        }
        keys
    }

    fn read_drop_keys(&self) -> Result<Option<DropKeysFile>, Error> {
        let path = self.dir.join(DROP_KEYS_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let plain = decrypt(&self.master, &fs::read(path)?)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| Error::Corrupt)
    }

    fn write_drop_keys(&self, keys: &DropKeysFile) -> Result<(), Error> {
        let plain = serde_json::to_vec(keys).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(&self.master, &plain)?;
        fs::write(self.dir.join(DROP_KEYS_FILE), packed)?;
        Ok(())
    }
}

/// How many retired drop keys keep draining after a rotation. The relay's TTL
/// bounds a drop's lifetime, so anything older than that cannot carry a drop
/// in flight anyway.
const RETIRED_DROP_KEYS: usize = 3;

const DROP_KEYS_FILE: &str = "drop-keys.enc";

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Serialize, Deserialize, zeroize::ZeroizeOnDrop)]
struct DropKeyEntry {
    sk_hex: String,
    pk_hex: String,
    #[serde(default)]
    retired_unix: u64,
}

impl From<&crate::drop::DropKeyPair> for DropKeyEntry {
    fn from(kp: &crate::drop::DropKeyPair) -> Self {
        Self {
            sk_hex: hex::encode(kp.sk),
            pk_hex: hex::encode(kp.pk),
            retired_unix: 0,
        }
    }
}

impl DropKeyEntry {
    fn to_drain_key(&self) -> Option<DropDrainKey> {
        let sk: [u8; 32] = hex::decode(&self.sk_hex).ok()?.try_into().ok()?;
        let pk: [u8; 32] = hex::decode(&self.pk_hex).ok()?.try_into().ok()?;
        Some(DropDrainKey {
            public_key: crate::drop::encode_drop_pk(&pk),
            sk,
            pk,
        })
    }
}

#[derive(Serialize, Deserialize, zeroize::ZeroizeOnDrop)]
struct DropKeysFile {
    current: DropKeyEntry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<DropKeyEntry>,
}

/// One drop key the daemon drains: raw key bytes for [`crate::drop`], plus the
/// pasteable public string for logs and provenance.
#[derive(Clone, Debug)]
pub struct DropDrainKey {
    pub public_key: String,
    pub pk: [u8; 32],
    pub sk: [u8; 32],
}

impl DropDrainKey {
    pub fn keypair(&self) -> crate::drop::DropKeyPair {
        crate::drop::DropKeyPair { sk: self.sk, pk: self.pk }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hush-vault-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn lock_json(vault_dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(vault_dir.join(LOCK_FILE_NAME)).unwrap()).unwrap()
    }

    #[test]
    fn new_vaults_wrap_the_master_key() {
        let d = scratch("v2");
        Vault::create(&d, "pw").unwrap();
        assert!(
            lock_json(&d)["wrapped_master"].is_string(),
            "new vaults must use the wrapped-master header"
        );
    }

    #[test]
    fn master_key_is_random_not_passphrase_derived() {
        // Same passphrase, different vaults: the master keys must differ, or a
        // passphrase change would still be a re-encryption of everything.
        let a = Vault::create(&scratch("rand-a"), "same").unwrap();
        let b = Vault::create(&scratch("rand-b"), "same").unwrap();
        assert_ne!(*a.master, *b.master);
    }

    #[test]
    fn change_passphrase_reopens_with_new_only() {
        let d = scratch("change");
        let vault = Vault::create(&d, "old-pw").unwrap();
        let identity = vault.identity_public();
        let blob = vault.put_blob(b"survives rotation").unwrap();

        vault.change_passphrase("old-pw", "new-pw").unwrap();

        assert!(matches!(
            Vault::open(&d, "old-pw").unwrap_err(),
            Error::WrongPassphrase
        ));
        let reopened = Vault::open(&d, "new-pw").unwrap();
        assert_eq!(reopened.identity_public(), identity);
        assert_eq!(reopened.get_blob(&blob).unwrap(), b"survives rotation");
    }

    #[test]
    fn change_passphrase_keeps_the_master_key() {
        let d = scratch("master-stable");
        let vault = Vault::create(&d, "old-pw").unwrap();
        let before = *vault.master;
        vault.change_passphrase("old-pw", "new-pw").unwrap();
        assert_eq!(*vault.master, before, "in-memory master must not change");
        assert_eq!(
            *Vault::open(&d, "new-pw").unwrap().master,
            before,
            "reopened master must match, or every blob is lost"
        );
    }

    #[test]
    fn change_passphrase_rejects_wrong_current() {
        let d = scratch("wrong-current");
        let vault = Vault::create(&d, "old-pw").unwrap();
        assert!(matches!(
            vault.change_passphrase("not-the-pw", "new-pw").unwrap_err(),
            Error::WrongPassphrase
        ));
        // A rejected change must leave the vault untouched.
        Vault::open(&d, "old-pw").unwrap();
    }

    #[test]
    fn change_passphrase_rejects_empty_new() {
        let d = scratch("empty-new");
        let vault = Vault::create(&d, "old-pw").unwrap();
        assert!(matches!(
            vault.change_passphrase("old-pw", "").unwrap_err(),
            Error::EmptyPassphrase
        ));
    }

    #[test]
    fn the_drop_key_survives_a_reload() {
        let d = scratch("drop-key");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(vault.drop_public_key().is_none(), "a fresh vault has no drop key");
        let pk = vault.create_drop_keypair().unwrap();
        assert!(pk.starts_with("hush-drop-pk-v1:"));
        assert_eq!(
            vault.drop_public_key().unwrap(),
            pk,
            "the minted key is the published one"
        );
        assert!(matches!(
            vault.create_drop_keypair().unwrap_err(),
            Error::DropKeyExists
        ));

        let reloaded = Vault::open(&d, "pw").unwrap();
        assert_eq!(reloaded.drop_public_key().unwrap(), pk);
        assert_eq!(reloaded.drop_keys_for_drain().len(), 1);
        assert_eq!(reloaded.drop_keys_for_drain()[0].public_key, pk);
    }

    #[test]
    fn rotating_the_drop_key_archives_the_old_one_and_caps_the_history() {
        let d = scratch("drop-rotate");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(matches!(
            vault.rotate_drop_keypair().unwrap_err(),
            Error::DropKeyMissing
        ));

        let first = vault.create_drop_keypair().unwrap();
        let second = vault.rotate_drop_keypair().unwrap();
        assert_ne!(first, second);
        let keys = vault.drop_keys_for_drain();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].public_key, second, "the current key drains first");
        assert_eq!(keys[1].public_key, first, "the retired key still drains");

        // Three more rotations: the retired list stays capped at three.
        for _ in 0..3 {
            vault.rotate_drop_keypair().unwrap();
        }
        let keys = vault.drop_keys_for_drain();
        assert_eq!(keys.len(), 4, "current plus the three newest retired");
        assert!(
            !keys.iter().any(|k| k.public_key == first),
            "the oldest retired key has fallen off and is no longer drained"
        );

        let reloaded = Vault::open(&d, "pw").unwrap();
        assert_eq!(reloaded.drop_public_key().unwrap(), vault.drop_public_key().unwrap());
    }

    /// Vaults written before format v2 derived the master key from the
    /// passphrase. They must keep opening, and upgrading must keep that same
    /// master key so existing ciphertext stays readable.
    #[test]
    fn v1_vault_opens_and_upgrades_without_re_encrypting() {
        let d = scratch("v1-compat");
        let salt = random_bytes::<SALT_LEN>();
        let params = crate::crypto::argon2_params().unwrap();
        let master = derive_master_key(
            "legacy-pw",
            &salt,
            params.m_cost(),
            params.t_cost(),
            params.p_cost(),
        )
        .unwrap();

        let legacy = LockFile {
            kdf: "argon2id".into(),
            salt_hex: hex::encode(salt),
            m_cost: params.m_cost(),
            t_cost: params.t_cost(),
            p_cost: params.p_cost(),
            wrapped_master: None,
        };
        fs::write(
            d.join(LOCK_FILE_NAME),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let signing = SigningKey::from_bytes(&random_bytes::<32>());
        fs::write(d.join("identity.pub"), signing.verifying_key().to_bytes()).unwrap();
        fs::write(
            d.join("identity.secret"),
            encrypt(&master, &signing.to_bytes()).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(d.join("blobs")).unwrap();

        let vault = Vault::open(&d, "legacy-pw").unwrap();
        let blob = vault.put_blob(b"pre-existing blob").unwrap();
        let identity = vault.identity_public();
        assert!(
            lock_json(&d)["wrapped_master"].is_null(),
            "opening must not silently rewrite the header"
        );

        vault.change_passphrase("legacy-pw", "fresh-pw").unwrap();

        assert!(
            lock_json(&d)["wrapped_master"].is_string(),
            "changing the passphrase should upgrade the header to v2"
        );
        let reopened = Vault::open(&d, "fresh-pw").unwrap();
        assert_eq!(reopened.identity_public(), identity);
        assert_eq!(reopened.get_blob(&blob).unwrap(), b"pre-existing blob");
    }

    #[test]
    fn lock_file_rewrite_leaves_no_temp_behind() {
        let d = scratch("atomic");
        let vault = Vault::create(&d, "pw").unwrap();
        vault.change_passphrase("pw", "pw2").unwrap();
        assert!(
            !d.join("vault.lock.json.tmp").exists(),
            "temporary header must not survive the rename"
        );
    }
}
