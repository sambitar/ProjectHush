use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{SigningKey, VerifyingKey};
use k256::schnorr::SigningKey as SchnorrSigningKey;
use sha2::{Digest, Sha256};
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
        // A chunked blob is a directory (large, streamed writes); a plain blob
        // is a single file. Both live under `blobs/<hex>`.
        if path.is_dir() {
            let mut out = Vec::new();
            self.read_blob_stream(id, &mut out)?;
            return Ok(out);
        }
        let packed = fs::read(path).map_err(|_| Error::BlobNotFound)?;
        decrypt(&self.master, &packed)
    }

    fn blob_path(&self, id: &BlobId) -> PathBuf {
        self.dir.join("blobs").join(hex::encode(id.0))
    }

    /// Stream a large plaintext into the vault as a chunked, content-addressed
    /// blob, without ever holding the whole file in memory.
    ///
    /// This is the large-file path (torrent downloads). Each chunk is encrypted
    /// on its own with the master key and written under `blobs/<hex>/chunk-N`;
    /// the id is the SHA-256 of the full plaintext, computed as the bytes pass
    /// through. The single-file [`Self::put_blob`] format is untouched, so
    /// existing blobs keep reading exactly as before.
    ///
    /// Content-addressing makes this idempotent: storing the same bytes twice
    /// keeps the first copy. The write goes to a temp directory first and is
    /// renamed into place only when complete, so a crash never leaves a
    /// half-blob under a real id.
    pub fn put_blob_stream<R: Read>(&self, reader: &mut R) -> Result<BlobId, Error> {
        let blobs = self.dir.join("blobs");
        fs::create_dir_all(&blobs)?;
        let tmp = blobs.join(format!(".tmp-{}", hex::encode(random_bytes::<16>())));
        fs::create_dir_all(&tmp)?;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK_BYTES];
        let mut index: u32 = 0;
        loop {
            let n = read_full(reader, &mut buf)?;
            hasher.update(&buf[..n]);
            let packed = encrypt(&self.master, &buf[..n])?;
            fs::write(tmp.join(chunk_name(index)), packed)?;
            index += 1;
            if n < buf.len() {
                break;
            }
        }

        let id = BlobId(hasher.finalize().into());
        let dest = self.blob_path(&id);
        if dest.exists() {
            // Same content already stored; drop the temp copy.
            let _ = fs::remove_dir_all(&tmp);
            return Ok(id);
        }
        fs::rename(&tmp, &dest)?;
        Ok(id)
    }

    /// Reassemble a blob into `w`, streaming, and verify it against its id.
    ///
    /// A single-file blob decrypts in one step; a chunked blob decrypts chunk by
    /// chunk. Either way the plaintext is hashed as it is written, and a
    /// mismatch (truncation, reordering, tampering) is an error rather than
    /// silently corrupt output. Returns the number of plaintext bytes written.
    pub fn read_blob_stream<W: Write>(&self, id: &BlobId, w: &mut W) -> Result<u64, Error> {
        let path = self.blob_path(id);
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        if path.is_dir() {
            let mut names: Vec<_> = fs::read_dir(&path)?
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .filter(|n| n.to_string_lossy().starts_with("chunk-"))
                .collect();
            names.sort();
            if names.is_empty() {
                return Err(Error::BlobNotFound);
            }
            for name in names {
                let packed = fs::read(path.join(name))?;
                let plain = decrypt(&self.master, &packed)?;
                hasher.update(&plain);
                w.write_all(&plain)?;
                total += plain.len() as u64;
            }
        } else {
            let packed = fs::read(&path).map_err(|_| Error::BlobNotFound)?;
            let plain = decrypt(&self.master, &packed)?;
            hasher.update(&plain);
            w.write_all(&plain)?;
            total += plain.len() as u64;
        }
        if hasher.finalize().as_slice() != id.0 {
            return Err(Error::Corrupt);
        }
        Ok(total)
    }

    /// Remove a blob (single file or chunked directory). Returns whether
    /// anything was there.
    pub fn delete_blob(&self, id: &BlobId) -> Result<bool, Error> {
        let path = self.blob_path(id);
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
            return Ok(true);
        }
        if path.exists() {
            fs::remove_file(&path)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether a blob is stored (either format).
    pub fn has_blob(&self, id: &BlobId) -> bool {
        self.blob_path(id).exists()
    }

    /// Read a named encrypted sidecar file in the vault directory.
    ///
    /// For feature-owned state that must be encrypted at rest but does not
    /// belong in the message log — reading-library state, saved filters. The
    /// caller owns the file name and the JSON schema inside; the vault owns
    /// the key. Returns `None` when the file does not exist yet.
    pub fn read_encrypted_file(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let path = self.sidecar_path(name)?;
        if !path.exists() {
            return Ok(None);
        }
        let plain = decrypt(&self.master, &fs::read(path)?)?;
        Ok(Some(plain))
    }

    /// Write a named encrypted sidecar file (write-then-rename, like the
    /// unlock header, so a crash partway through keeps the previous copy).
    pub fn write_encrypted_file(&self, name: &str, plaintext: &[u8]) -> Result<(), Error> {
        let path = self.sidecar_path(name)?;
        let packed = encrypt(&self.master, plaintext)?;
        let tmp = self.dir.join(format!("{name}.tmp"));
        fs::write(&tmp, packed)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Delete a named encrypted sidecar file, reporting whether one was there.
    ///
    /// The counterpart to [`Vault::write_encrypted_file`]: a feature that mints
    /// a credential can also forget it, instead of leaving a secret in the vault
    /// with no path to removal. `Ok(false)` means there was nothing to delete,
    /// which is not an error for a caller that wanted it gone either way.
    pub fn remove_encrypted_file(&self, name: &str) -> Result<bool, Error> {
        let path = self.sidecar_path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Sidecar names come from daemon code, but treat them as untrusted
    /// anyway: no separators, no traversal, nothing to escape the vault dir.
    fn sidecar_path(&self, name: &str) -> Result<PathBuf, Error> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            || name.contains("..")
        {
            return Err(Error::Corrupt);
        }
        Ok(self.dir.join(name))
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

    /// The pen name this vault publishes under, if it has one yet.
    pub fn pen_name(&self) -> Option<PenName> {
        let file = self.read_pen_names().ok()??;
        Some(public_pen_name(&file.current))
    }

    /// The current pen name's seal keypair, for opening what is sealed to it.
    ///
    /// The seal public half is published with every zine, so anyone can seal a
    /// comment to it; only this vault holds the private half that opens one.
    /// `None` when there is no pen name — a caller cannot address an identity
    /// that does not exist, and should not silently create one.
    pub fn pen_name_seal_keypair(&self) -> Option<crate::drop::DropKeyPair> {
        let file = self.read_pen_names().ok()??;
        let sk: [u8; 32] = hex::decode(&file.current.seal_sk_hex).ok()?.try_into().ok()?;
        let pk: [u8; 32] = hex::decode(&file.current.seal_pk_hex).ok()?.try_into().ok()?;
        Some(crate::drop::DropKeyPair { sk, pk })
    }

    /// Sign `msg` as the current pen name.
    ///
    /// Provenance, not secrecy. The signature travels *inside* the sealed blob,
    /// so it is checked by a reader rather than by anyone who merely holds the
    /// bytes — a signing key in the clear would be a stable pseudonym for its
    /// author, and the bulletin is meant to be one flat opaque log. What it buys
    /// is the property the read key cannot: every reader holds that key, so
    /// without a signature any reader could forge an edit. `None` when there is
    /// no pen name, so nothing is ever published unsigned under a byline that
    /// does not exist.
    pub fn pen_name_sign(&self, msg: &[u8]) -> Option<Vec<u8>> {
        use ed25519_dalek::Signer;
        let file = self.read_pen_names().ok()??;
        let sk: [u8; 32] = hex::decode(&file.current.sign_sk_hex).ok()?.try_into().ok()?;
        let signing = SigningKey::from_bytes(&sk);
        Some(signing.sign(msg).to_bytes().to_vec())
    }

    /// Mint a pen name. A second call is refused rather than silently absorbed,
    /// like the drop key: a pen name is an identity, and quietly reusing one
    /// when the caller expected a new one is the kind of thing that ends up
    /// published under the wrong byline.
    pub fn create_pen_name(&self, name: &str) -> Result<PenName, Error> {
        if self.read_pen_names()?.is_some() {
            return Err(Error::PenNameExists);
        }
        let entry = PenNameEntry::new(name);
        let out = public_pen_name(&entry);
        self.write_pen_names(&PenNamesFile {
            current: entry,
            others: Vec::new(),
        })?;
        Ok(out)
    }

    /// Rename the current pen name.
    ///
    /// A display name is not an identity, so this does not touch either key and
    /// nothing already published changes its `zine_id`. Refused when there is
    /// no pen name to rename, so a rename cannot create one by accident.
    pub fn rename_pen_name(&self, name: &str) -> Result<PenName, Error> {
        let mut file = self.read_pen_names()?.ok_or(Error::PenNameMissing)?;
        file.current.name = name.trim().to_string();
        let out = public_pen_name(&file.current);
        self.write_pen_names(&file)?;
        Ok(out)
    }

    fn read_pen_names(&self) -> Result<Option<PenNamesFile>, Error> {
        let path = self.dir.join(PEN_NAMES_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let plain = decrypt(&self.master, &fs::read(path)?)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| Error::Corrupt)
    }

    fn write_pen_names(&self, file: &PenNamesFile) -> Result<(), Error> {
        if file.others.len() + 1 > PEN_NAMES_CAP {
            return Err(Error::PenNameCap);
        }
        let plain = serde_json::to_vec(file).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(&self.master, &plain)?;
        fs::write(self.dir.join(PEN_NAMES_FILE), packed)?;
        Ok(())
    }

    /// The public identity this vault publishes and comments under on the
    /// public Nostr network, if it has one yet.
    pub fn public_identity(&self) -> Option<PublicIdentity> {
        let entry = self.read_public_identity().ok()??;
        public_identity_of(&entry)
    }

    /// Mint the public identity.
    ///
    /// A second call is refused rather than absorbed, like a pen name: it is an
    /// identity, and quietly reusing one when the caller expected a new one
    /// would publish under a key nobody chose.
    pub fn create_public_identity(&self) -> Result<PublicIdentity, Error> {
        if self.read_public_identity()?.is_some() {
            return Err(Error::PublicIdentityExists);
        }
        let entry = PublicIdentityEntry::new();
        let out = public_identity_of(&entry).ok_or(Error::Crypto)?;
        self.write_public_identity(&entry)?;
        Ok(out)
    }

    /// Sign a Nostr event id as the public identity.
    ///
    /// `msg` is the 32-byte event id, and the signature is BIP340 over it —
    /// which is what `msg` already being a SHA-256 digest means. Signing the
    /// *digest* rather than hashing again is the whole subtlety here: Nostr's
    /// signature is over `sha256(serialized_event)`, so a signature computed
    /// over `sha256(sha256(serialized_event))` would be a well-formed Schnorr
    /// signature that no relay and no other client accepts.
    ///
    /// `None` when there is no public identity, so nothing is ever published
    /// under a key that does not exist. The secret never leaves this method.
    pub fn public_identity_sign(&self, msg: &[u8]) -> Option<Vec<u8>> {
        use k256::schnorr::signature::hazmat::PrehashSigner;
        let entry = self.read_public_identity().ok()??;
        let signing = entry.signing_key()?;
        // The default `aux_rand` (all zeroes) is BIP340's deterministic mode.
        // It costs nothing a reader can detect — verification depends only on
        // `(r, s)` — and it keeps signing reproducible for a given event.
        let signature = signing.sign_prehash(msg).ok()?;
        Some(signature.to_bytes().to_vec())
    }

    fn read_public_identity(&self) -> Result<Option<PublicIdentityEntry>, Error> {
        let path = self.dir.join(PUBLIC_IDENTITY_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let plain = decrypt(&self.master, &fs::read(path)?)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| Error::Corrupt)
    }

    fn write_public_identity(&self, entry: &PublicIdentityEntry) -> Result<(), Error> {
        let plain = serde_json::to_vec(entry).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(&self.master, &plain)?;
        fs::write(self.dir.join(PUBLIC_IDENTITY_FILE), packed)?;
        Ok(())
    }
}

/// Strip the secrets out of a stored pen-name entry.
fn public_pen_name(entry: &PenNameEntry) -> PenName {
    PenName {
        id: entry.zine_id(),
        name: entry.name.clone(),
        sign_pk: entry.sign_pk_hex.clone(),
    }
}

/// How many retired drop keys keep draining after a rotation. The relay's TTL
/// bounds a drop's lifetime, so anything older than that cannot carry a drop
/// in flight anyway.
const RETIRED_DROP_KEYS: usize = 3;

const DROP_KEYS_FILE: &str = "drop-keys.enc";

/// The zine pen names this vault publishes under.
///
/// Its own file rather than a field in the vault record, for the same reason
/// the drop keys have one: a pen name is a **key**, and a key belongs in
/// ciphertext that is decrypted only while the vault is unlocked. Every file in
/// the vault directory is carried by [`crate::backup::export_vault`], so a
/// pen name is backed up by existing, without an entry in the manifest.
const PEN_NAMES_FILE: &str = "pen-names.enc";

/// How many pen names one vault may hold. Multiple pen names are a later
/// feature; the cap exists now so the sidecar cannot grow without bound.
const PEN_NAMES_CAP: usize = 16;

/// A pen name: the identity a zine is published under, and the two keys that
/// make it usable.
///
/// Two keys, not one, because a pen name has to do two different things and no
/// single curve does both:
///
/// - **seal** — X25519, so a stranger can seal a comment to it. This is the
///   **address**: `zine_id` is this key in hex, because the id is what a
///   commenter encrypts to and what a `ZineGrant` names, so it has to be
///   derivable from the key the crypto actually uses.
/// - **sign** — Ed25519, so authorship can be proven. Verification material,
///   not an address, so it travels with the blob rather than in the id.
///
/// Both are generated here and never derived from the messaging identity, which
/// is what makes a pen name *isolated*: nothing in this file can be computed
/// from the Signal keys, so publishing under a pen name cannot be tied to a
/// member's account even by someone holding their messaging private key.
#[derive(Clone, Serialize, Deserialize, zeroize::ZeroizeOnDrop)]
struct PenNameEntry {
    /// The display name the author chose. Free text, local, and only ever
    /// published inside an encrypted descriptor.
    name: String,
    seal_sk_hex: String,
    seal_pk_hex: String,
    sign_sk_hex: String,
    sign_pk_hex: String,
    created_unix: u64,
}

impl PenNameEntry {
    fn new(name: &str) -> Self {
        let seal = crate::drop::drop_keypair();
        // `from_bytes` over the crate's own `random_bytes`, the same way the
        // vault's device and identity keys are minted: `SigningKey::generate`
        // wants a `rand_core` that is a version behind the one in this tree.
        let signing = SigningKey::from_bytes(&random_bytes::<32>());
        Self {
            name: name.trim().to_string(),
            seal_sk_hex: hex::encode(seal.sk),
            seal_pk_hex: hex::encode(seal.pk),
            sign_sk_hex: hex::encode(signing.to_bytes()),
            sign_pk_hex: hex::encode(signing.verifying_key().to_bytes()),
            created_unix: now_secs(),
        }
    }

    /// The address a zine is named by: the seal public key, hex.
    fn zine_id(&self) -> String {
        self.seal_pk_hex.clone()
    }
}

#[derive(Serialize, Deserialize, zeroize::ZeroizeOnDrop)]
struct PenNamesFile {
    /// The pen name new work is published under.
    current: PenNameEntry,
    /// Other pen names this vault holds. Multiple pen names are a later
    /// feature; the list exists so adding one is additive, not a migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    others: Vec<PenNameEntry>,
}

/// A pen name as the rest of the program sees it: the display name and the two
/// public keys, with no secret material in the struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PenName {
    /// `zine_id`: the seal public key, hex. What a comment is sealed to.
    pub id: String,
    pub name: String,
    /// The signing public key, hex. Provenance, published with a blob.
    pub sign_pk: String,
}

/// Check a pen name's signature over the bytes it was made for.
///
/// Free rather than a method because the verifier is not the signer: a reader
/// checks a blob against the signing key that travelled inside it, and holds no
/// pen name of its own in the case that matters. False for anything malformed —
/// a bad hex key, a short signature, a wrong length — because a blob that cannot
/// be checked is a blob that is not verified.
pub fn verify_pen_signature(sign_pk_hex: &str, message: &[u8], signature: &[u8]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Ok(pk) = hex::decode(sign_pk_hex) else {
        return false;
    };
    let Ok(pk) = <[u8; 32]>::try_from(pk.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(signature) else {
        return false;
    };
    vk.verify(message, &sig).is_ok()
}

/// The public identity this vault publishes and comments under on the public
/// Nostr network (neighborhoods, Task 22).
///
/// One secp256k1 key, because a Nostr identity *is* one keypair: it signs events
/// and it is the address events name, so there is no second curve to split it
/// across the way a pen name needs. It is generated here and never derived from
/// the messaging identity or from any pen name, which is what keeps a public
/// post from being tied back to the member's account or to the private byline.
const PUBLIC_IDENTITY_FILE: &str = "public-identity.enc";

#[derive(Clone, Serialize, Deserialize, zeroize::ZeroizeOnDrop)]
struct PublicIdentityEntry {
    /// secp256k1 secret key, hex. The `nsec`; never leaves the vault.
    secret_hex: String,
    created_unix: u64,
}

impl PublicIdentityEntry {
    fn new() -> Self {
        // `from_bytes` over the crate's own `random_bytes`, the same way the
        // pen name's signing key is minted: the RNG trait versions in this tree
        // do not line up with the ones `k256` re-exports. A random 32-byte
        // string is a valid secp256k1 scalar with overwhelming probability; the
        // loop is the honest handling of the case where it is not.
        let signing = loop {
            if let Ok(k) = SchnorrSigningKey::from_bytes(&random_bytes::<32>()) {
                break k;
            }
        };
        Self {
            secret_hex: hex::encode(signing.to_bytes()),
            created_unix: now_secs(),
        }
    }

    fn signing_key(&self) -> Option<SchnorrSigningKey> {
        let sk: [u8; 32] = hex::decode(&self.secret_hex).ok()?.try_into().ok()?;
        SchnorrSigningKey::from_bytes(&sk).ok()
    }
}

/// The public identity as the rest of the program sees it: the public key only,
/// with no secret material in the struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicIdentity {
    /// The x-only secp256k1 public key, hex. A Nostr event's `pubkey`.
    pub pubkey_hex: String,
    /// The bech32 `npub` encoding of the same key — the human-facing handle.
    pub npub: String,
}

/// The public identity a stored entry stands for: the x-only public key in hex
/// and its bech32 `npub`.
fn public_identity_of(entry: &PublicIdentityEntry) -> Option<PublicIdentity> {
    let signing = entry.signing_key()?;
    let xonly = signing.verifying_key().to_bytes();
    let xonly = xonly.as_slice();
    Some(PublicIdentity {
        pubkey_hex: hex::encode(xonly),
        npub: npub_of(xonly)?,
    })
}

/// The bech32 `npub` for a 32-byte x-only secp256k1 public key.
///
/// Nostr's human-facing handle. bech32, not bech32m: `npub`/`nsec` predate the
/// bech32m checksum and other clients expect the original.
fn npub_of(xonly: &[u8]) -> Option<String> {
    if xonly.len() != 32 {
        return None;
    }
    let hrp = bech32::Hrp::parse("npub").ok()?;
    bech32::encode::<bech32::Bech32>(hrp, xonly).ok()
}

/// Plaintext bytes per encrypted chunk for [`Vault::put_blob_stream`]. Large
/// enough that per-chunk overhead is negligible, small enough that peak memory
/// stays flat regardless of file size.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

fn chunk_name(index: u32) -> String {
    format!("chunk-{index:04}")
}

/// Fill `buf` as far as the reader allows, returning the byte count. `Read::read`
/// may return short reads for a healthy stream, so one call is not enough.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(filled)
}

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
    fn encrypted_sidecars_roundtrip_and_stay_opaque() {
        let d = scratch("sidecar");
        let vault = Vault::create(&d, "pw").unwrap();

        assert_eq!(
            vault.read_encrypted_file("reads-state.enc").unwrap(),
            None,
            "a missing sidecar reads as None, not an error"
        );

        vault
            .write_encrypted_file("reads-state.enc", b"{\"saved\":[]}")
            .unwrap();

        // On disk: not plaintext.
        let raw = fs::read(d.join("reads-state.enc")).unwrap();
        assert_ne!(raw, b"{\"saved\":[]}".to_vec());
        assert!(
            !String::from_utf8_lossy(&raw).contains("saved"),
            "sidecar bytes must not leak plaintext"
        );

        let reopened = Vault::open(&d, "pw").unwrap();
        assert_eq!(
            reopened.read_encrypted_file("reads-state.enc").unwrap(),
            Some(b"{\"saved\":[]}".to_vec()),
            "a sidecar survives reopen"
        );

        // Rewrites replace, not append or duplicate.
        reopened
            .write_encrypted_file("reads-state.enc", b"{\"saved\":[1]}")
            .unwrap();
        assert_eq!(
            reopened.read_encrypted_file("reads-state.enc").unwrap(),
            Some(b"{\"saved\":[1]}".to_vec())
        );

        // Removal forgets it, and removing it twice is not an error.
        assert!(reopened.remove_encrypted_file("reads-state.enc").unwrap());
        assert_eq!(reopened.read_encrypted_file("reads-state.enc").unwrap(), None);
        assert!(!reopened.remove_encrypted_file("reads-state.enc").unwrap());
        assert!(!d.join("reads-state.enc").exists(), "the file is gone");
    }

    #[test]
    fn encrypted_sidecar_names_are_confined() {
        let d = scratch("sidecar-names");
        let vault = Vault::create(&d, "pw").unwrap();
        let before: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        for bad in ["../x.enc", "a/b.enc", "..", "", ".enc/.."] {
            assert!(
                vault.write_encrypted_file(bad, b"x").is_err(),
                "name {bad:?} must be refused"
            );
            assert!(vault.read_encrypted_file(bad).is_err());
            assert!(
                vault.remove_encrypted_file(bad).is_err(),
                "removal must refuse {bad:?} too"
            );
        }
        // Nothing written, nothing escaped: the dir is untouched.
        let after: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(before, after);
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

    #[test]
    fn a_pen_name_is_minted_once_and_survives_a_reopen() {
        let d = scratch("pen-name");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(vault.pen_name().is_none(), "a fresh vault has no pen name");

        let pen = vault.create_pen_name("  Ada  ").unwrap();
        assert_eq!(pen.name, "Ada", "the display name is trimmed");
        assert_eq!(pen.id.len(), 64, "the id is a 32-byte key in hex");
        assert_eq!(pen.sign_pk.len(), 64);
        assert_ne!(pen.id, pen.sign_pk, "the address and the signing key differ");

        assert!(matches!(
            vault.create_pen_name("someone else").unwrap_err(),
            Error::PenNameExists
        ));

        let reloaded = Vault::open(&d, "pw").unwrap();
        assert_eq!(reloaded.pen_name().unwrap(), pen);
    }

    #[test]
    fn renaming_a_pen_name_keeps_both_keys() {
        let d = scratch("pen-name-rename");
        let vault = Vault::create(&d, "pw").unwrap();
        let before = vault.create_pen_name("Ada").unwrap();
        let after = vault.rename_pen_name("Ada Lovelace").unwrap();
        assert_eq!(after.name, "Ada Lovelace");
        assert_eq!(after.id, before.id, "a display name is not an identity");
        assert_eq!(after.sign_pk, before.sign_pk);
        assert_eq!(Vault::open(&d, "pw").unwrap().pen_name().unwrap(), after);
    }

    #[test]
    fn renaming_without_a_pen_name_is_refused_rather_than_creating_one() {
        let d = scratch("pen-name-rename-missing");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(matches!(
            vault.rename_pen_name("nobody").unwrap_err(),
            Error::PenNameMissing
        ));
        assert!(vault.pen_name().is_none());
    }

    #[test]
    fn pen_names_are_isolated_from_the_messaging_identity() {
        // The point of a pen name is that it cannot be tied to the account.
        // Two vaults with the same identity material must still mint unrelated
        // pen names, and a pen name must not be derivable from the drop key.
        let a = scratch("pen-name-a");
        let b = scratch("pen-name-b");
        let va = Vault::create(&a, "pw").unwrap();
        let vb = Vault::create(&b, "pw").unwrap();
        let pa = va.create_pen_name("Ada").unwrap();
        let pb = vb.create_pen_name("Ada").unwrap();
        assert_ne!(pa.id, pb.id);
        assert_ne!(pa.id, hex::encode(va.identity_public().0));
        assert_ne!(pa.id, va.drop_public_key().unwrap_or_default());
        assert_ne!(pa.sign_pk, pa.id);
    }

    #[test]
    fn a_pen_name_is_ciphertext_at_rest() {
        let d = scratch("pen-name-at-rest");
        let vault = Vault::create(&d, "pw").unwrap();
        let pen = vault.create_pen_name("Ada Lovelace").unwrap();
        let raw = fs::read(d.join(PEN_NAMES_FILE)).unwrap();
        let hay = String::from_utf8_lossy(&raw);
        assert!(!hay.contains("Ada Lovelace"), "the display name is not plaintext");
        assert!(!hay.contains(&pen.id), "the address is not plaintext");
        assert!(!hay.contains(&pen.sign_pk), "the signing key is not plaintext");
    }

    /// The pen name's secret halves are what the daemon needs to open a comment
    /// sealed to the byline and to sign a published blob. They must come from
    /// the stored entry, must not exist before a pen name does, and must be the
    /// halves of the keys the world sees.
    #[test]
    fn the_pen_name_secrets_are_the_halves_of_its_public_keys() {
        let d = scratch("pen-name-secrets");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(vault.pen_name_seal_keypair().is_none(), "no pen name yet");
        assert!(vault.pen_name_sign(b"x").is_none(), "nothing to sign with yet");

        let pen = vault.create_pen_name("Ada").unwrap();
        let seal = vault.pen_name_seal_keypair().unwrap();
        assert_eq!(hex::encode(seal.pk), pen.id, "the seal public half is the address");

        // A comment sealed to the published key opens with the stored secret.
        let sealed = crate::drop::seal_bytes_to_key(b"<p>hello</p>", &seal.pk).unwrap();
        let opened = crate::drop::open_bytes_with_key(&sealed, &seal).unwrap();
        assert_eq!(opened, b"<p>hello</p>");

        // And a signature verifies under the published signing key, and only
        // over the bytes it was made for.
        let sig = vault.pen_name_sign(b"a blob").unwrap();
        assert!(verify_pen_signature(&pen.sign_pk, b"a blob", &sig));
        assert!(
            !verify_pen_signature(&pen.sign_pk, b"another blob", &sig),
            "a signature is over the bytes it was made for"
        );
        // Malformed material is a refusal, not a panic.
        assert!(!verify_pen_signature("not-hex", b"a blob", &sig));
        assert!(!verify_pen_signature(&pen.sign_pk, b"a blob", b"short"));
        assert!(!verify_pen_signature(&pen.sign_pk, b"a blob", &[]));
    }

    #[test]
    fn a_backup_carries_the_pen_name_and_the_zines_sidecar() {
        // The acceptance criterion for zines Phase 1 is that a backup carries
        // the pen name. `export_vault` walks the vault directory, so the check
        // that matters is that the pen name and the zines sidecar are *files in
        // it* — a pen name kept anywhere else would be silently dropped from
        // every backup, and the only symptom would be a restored vault that
        // cannot publish under its own byline.
        let d = scratch("pen-name-backup");
        let vault = Vault::create(&d, "pw").unwrap();
        let pen = vault.create_pen_name("Ada").unwrap();
        vault.write_encrypted_file("zines.enc", b"sealed zine state").unwrap();

        let archive = crate::backup::export_vault(&d).unwrap();
        let names: Vec<String> = {
            let mut tar = tar::Archive::new(&archive[..]);
            tar.entries()
                .unwrap()
                .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert!(names.iter().any(|n| n == PEN_NAMES_FILE), "{names:?}");
        assert!(names.iter().any(|n| n == "zines.enc"), "{names:?}");

        let restored = scratch("pen-name-restore");
        crate::backup::import_vault(&restored, &archive).unwrap();
        let back = Vault::open(&restored, "pw").unwrap();
        assert_eq!(back.pen_name().unwrap(), pen);
        assert_eq!(
            back.read_encrypted_file("zines.enc").unwrap().unwrap(),
            b"sealed zine state"
        );
    }

    #[test]
    fn a_public_identity_is_minted_once_and_survives_a_reopen() {
        let d = scratch("public-identity");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(
            vault.public_identity().is_none(),
            "a fresh vault has no public identity"
        );

        let id = vault.create_public_identity().unwrap();
        assert_eq!(id.pubkey_hex.len(), 64, "the pubkey is a 32-byte key in hex");
        assert!(id.npub.starts_with("npub1"), "the handle is a bech32 npub");

        assert!(matches!(
            vault.create_public_identity().unwrap_err(),
            Error::PublicIdentityExists
        ));

        let reloaded = Vault::open(&d, "pw").unwrap();
        assert_eq!(reloaded.public_identity().unwrap(), id);
    }

    /// The whole point of a separate public identity is that a public post
    /// cannot be tied back to the member's account or to the private byline.
    /// Two vaults with the same identity material must mint unrelated public
    /// identities, and the key must not be derivable from the account, the drop
    /// key, or a pen name.
    #[test]
    fn the_public_identity_is_isolated_from_the_account_and_the_pen_name() {
        let a = scratch("public-identity-a");
        let b = scratch("public-identity-b");
        let va = Vault::create(&a, "pw").unwrap();
        let vb = Vault::create(&b, "pw").unwrap();
        let ia = va.create_public_identity().unwrap();
        let ib = vb.create_public_identity().unwrap();
        assert_ne!(ia.pubkey_hex, ib.pubkey_hex);

        assert_ne!(ia.pubkey_hex, hex::encode(va.identity_public().0));
        assert_ne!(ia.pubkey_hex, va.drop_public_key().unwrap_or_default());

        let pen = va.create_pen_name("Ada").unwrap();
        assert_ne!(ia.pubkey_hex, pen.id);
        assert_ne!(ia.pubkey_hex, pen.sign_pk);
    }

    /// The pubkey must be a real Nostr key, not just 32 bytes that look like
    /// one. `k256`'s x-only encoding is checked against the canonical secp256k1
    /// generator, and the `npub` against a decode of the same bytes.
    #[test]
    fn the_public_identity_pubkey_matches_a_known_secp256k1_vector() {
        // secret = 1 -> G.x, the generator's x-coordinate.
        let entry = PublicIdentityEntry {
            secret_hex: "0000000000000000000000000000000000000000000000000000000000000001"
                .to_string(),
            created_unix: 0,
        };
        let id = public_identity_of(&entry).unwrap();
        assert_eq!(
            id.pubkey_hex,
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );

        let (hrp, data) = bech32::decode(&id.npub).unwrap();
        assert_eq!(hrp.to_string(), "npub");
        assert_eq!(hex::encode(data), id.pubkey_hex);
    }

    #[test]
    fn a_public_identity_is_ciphertext_at_rest() {
        let d = scratch("public-identity-at-rest");
        let vault = Vault::create(&d, "pw").unwrap();
        let id = vault.create_public_identity().unwrap();
        let raw = fs::read(d.join(PUBLIC_IDENTITY_FILE)).unwrap();
        let hay = String::from_utf8_lossy(&raw);
        assert!(!hay.contains(&id.pubkey_hex), "the pubkey is not plaintext");
        assert!(!hay.contains(&id.npub), "the npub is not plaintext");
        assert!(!hay.contains("secret_hex"), "the record is not plaintext");
    }

    #[test]
    fn a_backup_carries_the_public_identity() {
        let d = scratch("public-identity-backup");
        let vault = Vault::create(&d, "pw").unwrap();
        let id = vault.create_public_identity().unwrap();

        let archive = crate::backup::export_vault(&d).unwrap();
        let names: Vec<String> = {
            let mut tar = tar::Archive::new(&archive[..]);
            tar.entries()
                .unwrap()
                .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert!(names.iter().any(|n| n == PUBLIC_IDENTITY_FILE), "{names:?}");

        let restored = scratch("public-identity-restore");
        crate::backup::import_vault(&restored, &archive).unwrap();
        let back = Vault::open(&restored, "pw").unwrap();
        assert_eq!(back.public_identity().unwrap(), id);
    }

    /// A signature the vault produces must verify under the very key the event
    /// will name. This is the property an export or a comment depends on: a
    /// relay checks the signature against `pubkey` and drops the event if it
    /// does not hold.
    #[test]
    fn a_public_identity_signature_verifies_under_its_own_pubkey() {
        use k256::schnorr::signature::hazmat::PrehashVerifier;
        let d = scratch("public-identity-sign");
        let vault = Vault::create(&d, "pw").unwrap();
        let id = vault.create_public_identity().unwrap();

        // A stand-in event id: what matters is that it is the 32-byte digest.
        let event_id = Sha256::digest(b"a serialized event");
        let signature = vault.public_identity_sign(&event_id).unwrap();
        assert_eq!(signature.len(), 64, "BIP340 is a 64-byte signature");

        let xonly: [u8; 32] = hex::decode(&id.pubkey_hex).unwrap().try_into().unwrap();
        let verifying = k256::schnorr::VerifyingKey::from_bytes(&xonly).unwrap();
        let signature = k256::schnorr::Signature::try_from(&signature[..]).unwrap();
        verifying
            .verify_prehash(&event_id, &signature)
            .expect("the signature holds under the published key");
    }

    /// The subtlety that would make every event this app signs worthless: Nostr
    /// signs `sha256(serialized_event)`, so the vault must sign the digest it is
    /// handed rather than hashing it a second time. A doubly-hashed signature is
    /// still a valid Schnorr signature — it simply verifies against nothing a
    /// relay computes, so the event is rejected with no clue why.
    #[test]
    fn a_public_identity_signature_is_over_the_digest_and_not_a_hash_of_it() {
        use k256::schnorr::signature::hazmat::PrehashVerifier;
        let d = scratch("public-identity-sign-prehash");
        let vault = Vault::create(&d, "pw").unwrap();
        let id = vault.create_public_identity().unwrap();

        let event_id = Sha256::digest(b"a serialized event");
        let signature = vault.public_identity_sign(&event_id).unwrap();

        let xonly: [u8; 32] = hex::decode(&id.pubkey_hex).unwrap().try_into().unwrap();
        let verifying = k256::schnorr::VerifyingKey::from_bytes(&xonly).unwrap();
        let signature = k256::schnorr::Signature::try_from(&signature[..]).unwrap();

        verifying
            .verify_prehash(&event_id, &signature)
            .expect("verified as a signature over the digest");

        // The same bytes read as a message to hash are a different prehash, and
        // the signature must not verify there. This is what would pass if the
        // vault called `Signer::sign` instead of `PrehashSigner::sign_prehash`.
        use k256::schnorr::signature::Verifier;
        assert!(
            verifying.verify(&event_id, &signature).is_err(),
            "the signature is over the digest, not over a hash of it"
        );
    }

    /// The vault signs with `sign_prehash` and BIP340's default (all-zero)
    /// auxiliary randomness. This is BIP340's own test vector 0 — secret key 3,
    /// all-zero message and `aux_rand` — so a pass here means the call the vault
    /// makes produces signatures the wider Nostr network accepts, not merely
    /// signatures that round-trip against our own verifier.
    #[test]
    fn signing_a_prehash_matches_the_bip340_test_vector() {
        use k256::schnorr::signature::hazmat::PrehashSigner;
        let sk = hex::decode("0000000000000000000000000000000000000000000000000000000000000003")
            .unwrap();
        let signing = SchnorrSigningKey::from_bytes(&sk).unwrap();
        assert_eq!(
            hex::encode(signing.verifying_key().to_bytes().as_slice()),
            "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
            "the vector's public key"
        );

        let signature = signing.sign_prehash(&[0u8; 32]).unwrap();
        assert_eq!(
            hex::encode(signature.to_bytes()),
            concat!(
                "e907831f80848d1069a5371b402410364bdf1c5f8307b0084c55f1ce2dca8215",
                "25f66a4a85ea8b71e482a74f382d2ce5ebeee8fdb2172f477df4900d310536c0"
            )
        );
    }

    /// A read must never mint one. Signing without a public identity is `None`,
    /// and the call leaves the vault without one — a stray export must not be
    /// the thing that creates the key it publishes under.
    #[test]
    fn signing_without_a_public_identity_signs_nothing_and_mints_nothing() {
        let d = scratch("public-identity-sign-absent");
        let vault = Vault::create(&d, "pw").unwrap();
        assert!(vault.public_identity_sign(b"anything").is_none());
        assert!(
            vault.public_identity().is_none(),
            "the failed sign did not mint a key"
        );
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

    #[test]
    fn chunked_blob_roundtrips_across_chunk_boundaries() {
        let d = scratch("chunked");
        let vault = Vault::create(&d, "pw").unwrap();
        // Spans more than one chunk so the chunk loop is exercised, not just a
        // single short read.
        let plain: Vec<u8> = (0..(CHUNK_BYTES * 2 + 12345))
            .map(|i| (i % 251) as u8)
            .collect();

        let id = vault.put_blob_stream(&mut plain.as_slice()).unwrap();
        assert_eq!(id.0, sha256(&plain), "id is the content hash");
        assert!(vault.blob_path(&id).is_dir(), "stored as a chunk directory");

        // Both the streaming and the plain reader return the same bytes.
        let mut out = Vec::new();
        let n = vault.read_blob_stream(&id, &mut out).unwrap();
        assert_eq!(n as usize, plain.len());
        assert_eq!(out, plain);
        assert_eq!(vault.get_blob(&id).unwrap(), plain);

        // Idempotent: the same content does not create a second copy or fail.
        let again = vault.put_blob_stream(&mut plain.as_slice()).unwrap();
        assert_eq!(again.0, id.0);
        assert!(vault.has_blob(&id));

        assert!(vault.delete_blob(&id).unwrap());
        assert!(!vault.has_blob(&id));
        assert!(matches!(vault.get_blob(&id), Err(Error::BlobNotFound)));
    }

    #[test]
    fn chunked_blob_detects_tampering() {
        let d = scratch("chunked-tamper");
        let vault = Vault::create(&d, "pw").unwrap();
        let id = vault
            .put_blob_stream(&mut b"a short chunked blob".as_slice())
            .unwrap();

        // Flip a byte inside the first chunk's ciphertext.
        let chunk = vault.blob_path(&id).join(chunk_name(0));
        let mut bytes = fs::read(&chunk).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        fs::write(&chunk, bytes).unwrap();

        let mut out = Vec::new();
        assert!(
            vault.read_blob_stream(&id, &mut out).is_err(),
            "a tampered chunk must not decrypt to bytes"
        );
    }

    #[test]
    fn single_and_chunked_blobs_coexist() {
        let d = scratch("coexist");
        let vault = Vault::create(&d, "pw").unwrap();
        let single = vault.put_blob(b"small").unwrap();
        let chunked = vault.put_blob_stream(&mut b"streamed".as_slice()).unwrap();
        assert_eq!(vault.get_blob(&single).unwrap(), b"small");
        assert_eq!(vault.get_blob(&chunked).unwrap(), b"streamed");
    }
}
