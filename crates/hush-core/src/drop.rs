//! Incoming drops: sealed boxes an outsider addresses to a *published drop key*.
//!
//! The drop key is the address. A member publishes
//! `hush-drop-pk-v1:<base64url>` anywhere — a forum bio, a website — and anyone
//! holding that string and the relay's drop page can send a drop. There is no
//! sender key, no signature, and no reply path: the sender stays anonymous, and
//! the drop key deliberately says nothing about which vault it belongs to.
//!
//! The container is a sealed box (`hush-drop-v2`): an ephemeral X25519 keypair,
//! ECDH against the recipient's drop key, HKDF-SHA256 to a symmetric key, then
//! XChaCha20-Poly1305 over the JSON package. The full header — magic, version,
//! mode, ephemeral key, nonce — is the AEAD associated data, so a tampered
//! header cannot survive opening. The crypto primitives are the ones already in
//! the tree ([`libsignal_protocol::KeyPair`] for X25519,
//! [`chacha20poly1305::XChaCha20Poly1305`] for the AEAD); the only new code is
//! the HKDF, hand-rolled in [`crate::crypto`] beside the existing HMAC.
//!
//! A drop is *found* by rendezvous rather than by any server-side directory:
//! [`rendezvous_id`] derives a 16-byte delivery id from the recipient's public
//! key alone. The dropper uploads the container to `PUT /drop/{id}` under that
//! id; the member computes the same id from their own key and drains it. The
//! relay cannot link an id to a person, and a person who never publishes their
//! key has no address at all.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use libsignal_protocol::{PrivateKey as KeyPrivate, PublicKey as KeyPublic};
use libsignal_protocol::KeyPair;
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{hkdf_sha256, hmac_sha256, random_bytes};
use crate::error::Error;

pub const DROP_MAGIC: [u8; 4] = *b"HSHD";
/// Container format version. Version 1 of the removed outsider-mail slice was a
/// PIN container; this plan builds version 2 only, the sealed box.
pub const DROP_VERSION: u8 = 2;
const MODE_SEALED_BOX: u8 = 2;
const EPHEMERAL_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// magic(4) + version(1) + mode(1) + ephemeral pk(32) + nonce(24)
pub const DROP_V2_HEADER_LEN: usize = 4 + 1 + 1 + EPHEMERAL_LEN + NONCE_LEN;
pub const MAX_DROP_BYTES: usize = 8 * 1024 * 1024;

/// Pasteable encodings. The suffix version is the *key encoding* version, not
/// the container version; it stays v1 so already-published keys never change
/// meaning. The two prefixes differ so pasting the wrong half is refused
/// rather than used.
pub const DROP_KEY_PREFIX_PK: &str = "hush-drop-pk-v1:";
pub const DROP_KEY_PREFIX_SK: &str = "hush-drop-sk-v1:";

/// The domain-separation string HKDF binds the recipient key with, and the
/// message [`rendezvous_id`] MACs. Versioned so a future container change
/// cannot silently reuse old ids.
const HKDF_INFO: &[u8] = b"hush-drop-v2";
const RENDEZVOUS_MSG: &[u8] = b"hush-drop-rendezvous-v1";

/// One file inside a drop. `data` is base64 (standard alphabet), matching how
/// mail attachments ride inside the sealed JSON.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DropAttachment {
    pub name: String,
    pub data: String,
}

/// The plaintext a sealed drop carries. Plain text only — the recipient renders
/// it unstyled, so there is nothing here to sanitize.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DropPackage {
    pub v: u32,
    #[serde(default)]
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub attachments: Vec<DropAttachment>,
    #[serde(default)]
    pub created_unix: u64,
}

/// An X25519 drop keypair as raw bytes. The public half is what gets published;
/// both halves together are what opens drops.
#[derive(Clone, Debug)]
pub struct DropKeyPair {
    pub sk: [u8; KEY_LEN],
    pub pk: [u8; KEY_LEN],
}

pub fn is_drop_container(bytes: &[u8]) -> bool {
    bytes.len() > DROP_V2_HEADER_LEN && bytes[..4] == DROP_MAGIC
}

/// Generate a fresh drop keypair.
pub fn drop_keypair() -> DropKeyPair {
    let pair = KeyPair::generate(&mut OsRng.unwrap_err());
    DropKeyPair {
        sk: private_bytes(&pair.private_key),
        pk: public_bytes(&pair.public_key),
    }
}

/// The raw 32-byte X25519 point, not [`KeyPublic::serialize`]'s type-prefixed form.
fn public_bytes(pk: &KeyPublic) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(pk.public_key_bytes());
    out
}

fn private_bytes(sk: &KeyPrivate) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&sk.serialize());
    out
}

/// The 16-byte delivery id a drop to `recipient_pk` uploads under, hex-encoded.
/// Both the drop page and the member's daemon derive it from the public key
/// alone; the relay sees only an opaque id.
pub fn rendezvous_id(recipient_pk: &[u8; KEY_LEN]) -> String {
    let mac = hmac_sha256(recipient_pk, RENDEZVOUS_MSG);
    hex::encode(&mac[..16])
}

/// The HKDF-derived key, unique to the (ephemeral, recipient) pair: the
/// ephemeral public key is the salt and the recipient key is bound into the
/// info string, so a key is never reusable across drops.
fn drop_key(
    recipient_pk: &[u8; KEY_LEN],
    ephemeral_pk: &[u8],
    shared: &[u8],
) -> Zeroizing<[u8; KEY_LEN]> {
    let mut info = Vec::with_capacity(HKDF_INFO.len() + recipient_pk.len());
    info.extend_from_slice(HKDF_INFO);
    info.extend_from_slice(recipient_pk);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hkdf_sha256(shared, ephemeral_pk, &info, &mut *key);
    key
}

fn recipient_public_key(recipient_pk: &[u8; KEY_LEN]) -> Result<KeyPublic, Error> {
    KeyPublic::from_djb_public_key_bytes(recipient_pk).map_err(|_| Error::Crypto)
}

/// Seal `pkg` to the holder of `recipient_pk`. The sender contributes only an
/// ephemeral key: nothing in the container identifies them or permits a reply.
pub fn seal_to_key(pkg: &DropPackage, recipient_pk: &[u8; KEY_LEN]) -> Result<Vec<u8>, Error> {
    let recipient = recipient_public_key(recipient_pk)?;
    let eph = KeyPair::generate(&mut OsRng.unwrap_err());
    let eph_pk = public_bytes(&eph.public_key);
    // calculate_agreement rejects the all-zero shared secret a low-order
    // recipient point would produce, so a malicious paste fails to seal.
    let shared = eph
        .private_key
        .calculate_agreement(&recipient)
        .map_err(|_| Error::Crypto)?;

    let nonce_bytes = random_bytes::<NONCE_LEN>();
    let mut header = Vec::with_capacity(DROP_V2_HEADER_LEN);
    header.extend_from_slice(&DROP_MAGIC);
    header.push(DROP_VERSION);
    header.push(MODE_SEALED_BOX);
    header.extend_from_slice(&eph_pk);
    header.extend_from_slice(&nonce_bytes);

    let key = drop_key(recipient_pk, &eph_pk, &shared);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = serde_json::to_vec(pkg).map_err(|_| Error::BadDrop)?;
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload { msg: &plaintext, aad: &header },
        )
        .map_err(|_| Error::Crypto)?;
    header.extend_from_slice(&ct);
    Ok(header)
}

/// Open a container sealed to this keypair. A wrong or unrelated key fails the
/// AEAD tag check; a tampered header or body fails with it.
pub fn open_with_key(bytes: &[u8], key: &DropKeyPair) -> Result<DropPackage, Error> {
    if bytes.len() <= DROP_V2_HEADER_LEN || bytes[..4] != DROP_MAGIC {
        return Err(Error::BadDrop);
    }
    if bytes[4] != DROP_VERSION || bytes[5] != MODE_SEALED_BOX {
        return Err(Error::UnsupportedDrop);
    }
    let eph_pk: [u8; KEY_LEN] =
        bytes[6..6 + EPHEMERAL_LEN].try_into().map_err(|_| Error::BadDrop)?;
    let nonce_bytes: [u8; NONCE_LEN] = bytes[6 + EPHEMERAL_LEN..DROP_V2_HEADER_LEN]
        .try_into()
        .map_err(|_| Error::BadDrop)?;
    let header = &bytes[..DROP_V2_HEADER_LEN];

    let eph_public = recipient_public_key(&eph_pk)?;
    let private = KeyPrivate::deserialize(&key.sk).map_err(|_| Error::Crypto)?;
    let shared = private.calculate_agreement(&eph_public).map_err(|_| Error::Crypto)?;

    let key = drop_key(&key.pk, &eph_pk, &shared);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload { msg: &bytes[DROP_V2_HEADER_LEN..], aad: header },
        )
        .map_err(|_| Error::DropNotForThisKey)?;
    serde_json::from_slice(&plaintext).map_err(|_| Error::BadDrop)
}

pub fn encode_drop_pk(pk: &[u8; KEY_LEN]) -> String {
    format!("{}{}", DROP_KEY_PREFIX_PK, URL_SAFE_NO_PAD.encode(pk))
}

pub fn encode_drop_sk(sk: &[u8; KEY_LEN]) -> String {
    format!("{}{}", DROP_KEY_PREFIX_SK, URL_SAFE_NO_PAD.encode(sk))
}

pub fn decode_drop_pk(pasteable: &str) -> Result<[u8; KEY_LEN], Error> {
    decode_drop_key(pasteable, DROP_KEY_PREFIX_PK)
}

pub fn decode_drop_sk(pasteable: &str) -> Result<[u8; KEY_LEN], Error> {
    decode_drop_key(pasteable, DROP_KEY_PREFIX_SK)
}

fn decode_drop_key(pasteable: &str, expected_prefix: &str) -> Result<[u8; KEY_LEN], Error> {
    let encoded = pasteable
        .strip_prefix(expected_prefix)
        .ok_or(Error::InvalidDropKey)?;
    let raw = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Error::InvalidDropKey)?;
    <[u8; KEY_LEN]>::try_from(raw.as_slice()).map_err(|_| Error::InvalidDropKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DropPackage {
        DropPackage {
            v: 2,
            subject: "hi".into(),
            body: "plain text only".into(),
            attachments: vec![DropAttachment { name: "a.txt".into(), data: "aGk=".into() }],
            created_unix: 1_760_000_000,
        }
    }

    #[test]
    fn a_package_round_trips_under_its_drop_key() {
        let recipient = drop_keypair();
        let bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
        assert!(is_drop_container(&bytes));
        let back = open_with_key(&bytes, &recipient).unwrap();
        assert_eq!(back.subject, "hi");
        assert_eq!(back.body, "plain text only");
        assert_eq!(back.attachments[0].name, "a.txt");
    }

    #[test]
    fn the_wrong_drop_key_does_not_open() {
        let recipient = drop_keypair();
        let other = drop_keypair();
        let bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
        assert!(matches!(
            open_with_key(&bytes, &other),
            Err(Error::DropNotForThisKey)
        ));
    }

    #[test]
    fn a_tampered_header_or_body_does_not_open() {
        let recipient = drop_keypair();
        // Flip a ciphertext byte.
        let mut bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(open_with_key(&bytes, &recipient).is_err());
        // An unknown mode byte, authenticated via the header AAD.
        let mut bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
        bytes[5] = 9;
        assert!(matches!(
            open_with_key(&bytes, &recipient),
            Err(Error::UnsupportedDrop)
        ));
        // The right version with the wrong mode is still refused.
        let mut bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
        bytes[4] = 2;
        bytes[5] = 1;
        assert!(matches!(
            open_with_key(&bytes, &recipient),
            Err(Error::UnsupportedDrop)
        ));
    }

    #[test]
    fn a_garbage_container_is_refused_cleanly() {
        assert!(matches!(
            open_with_key(b"nope", &drop_keypair()),
            Err(Error::BadDrop)
        ));
        let mut short = vec![b'H', b'S', b'H', b'D', 2, 2];
        short.extend_from_slice(&[0u8; 40]);
        assert!(matches!(
            open_with_key(&short, &drop_keypair()),
            Err(Error::BadDrop)
        ));
    }

    #[test]
    fn hkdf_sha256_matches_the_rfc5869_vectors() {
        // Test Case 1: 22 octets of 0x0b, salt 0x00..0x0c, info 0xf0..0xf9, 42 out.
        let ikm = [0x0b; 22];
        let salt: [u8; 13] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info: [u8; 10] = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let mut out = [0u8; 42];
        hkdf_sha256(&ikm, &salt, &info, &mut out);
        assert_eq!(
            hex::encode(out),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
        // Test Case 3: empty salt and info, 82 octets out — exercises the
        // zero-salt path and multi-block expand in one vector.
        let mut long = [0u8; 82];
        hkdf_sha256(&ikm, b"", b"", &mut long);
        assert_eq!(
            hex::encode(long),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8b2fb61057244b36c6ddd287f634795e7d80d5fe26bfc36def6dc129c29271a0eb7ab14bd2ca88259"
        );
    }

    #[test]
    fn the_rendezvous_id_is_stable_and_keyed_by_the_public_key() {
        let a = drop_keypair();
        let b = drop_keypair();
        assert_eq!(rendezvous_id(&a.pk), rendezvous_id(&a.pk));
        assert_ne!(rendezvous_id(&a.pk), rendezvous_id(&b.pk));
        assert_eq!(rendezvous_id(&a.pk).len(), 32);
        assert!(rendezvous_id(&a.pk).chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn pasteable_keys_refuse_the_wrong_half_and_bad_lengths() {
        let kp = drop_keypair();
        let pk = encode_drop_pk(&kp.pk);
        let sk = encode_drop_sk(&kp.sk);
        assert!(pk.starts_with("hush-drop-pk-v1:"));
        assert!(sk.starts_with("hush-drop-sk-v1:"));
        assert_eq!(decode_drop_pk(&pk).unwrap(), kp.pk);
        assert_eq!(decode_drop_sk(&sk).unwrap(), kp.sk);
        assert!(matches!(decode_drop_pk(&sk), Err(Error::InvalidDropKey)));
        assert!(matches!(decode_drop_sk(&pk), Err(Error::InvalidDropKey)));
        assert!(matches!(
            decode_drop_pk("hush-drop-pk-v1:!!!"),
            Err(Error::InvalidDropKey)
        ));
        assert!(matches!(
            decode_drop_pk("hush-drop-pk-v1:AAAA"),
            Err(Error::InvalidDropKey)
        ));
    }

    #[test]
    fn opens_a_node_sealed_container() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let bytes =
            std::fs::read(dir.join("tests/fixtures/drop-v2-node.hshd"))
                .expect("run scripts/drops-vector.mjs seal to generate the fixture");
        let sk_hex = std::fs::read_to_string(dir.join("tests/fixtures/drop-v2-node.sk"))
            .expect("fixture sidecar");
        let pk_hex = std::fs::read_to_string(dir.join("tests/fixtures/drop-v2-node.pk"))
            .expect("fixture sidecar");
        let mut sk = [0u8; KEY_LEN];
        sk.copy_from_slice(&hex::decode(sk_hex.trim()).unwrap());
        let mut pk = [0u8; KEY_LEN];
        pk.copy_from_slice(&hex::decode(pk_hex.trim()).unwrap());
        let pkg = open_with_key(&bytes, &DropKeyPair { sk, pk }).unwrap();
        assert_eq!(pkg.subject, "from the page");
        assert_eq!(pkg.body, "sealed by the noble stack, opened by hush-core");
        assert_eq!(pkg.attachments[0].name, "note.txt");
    }
}
