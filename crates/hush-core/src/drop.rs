//! The `hush-drop-v1` container: one encrypted package for someone who has no
//! Hush account.
//!
//! A drop is not member-to-member mail. The recipient has no keys, so there is
//! no session to establish and nothing to ratchet. The whole of the secrecy is a
//! PIN the sender shares out of band, stretched into a key, over an AEAD. That is
//! a weaker bound than mail between members, and the module is written so the
//! bound is visible rather than implied.
//!
//! The cipher is XChaCha20-Poly1305 and the KDF is PBKDF2-HMAC-SHA256, chosen
//! against the vault's Argon2id on purpose: the recipient decrypts in a browser,
//! where PBKDF2 is native to WebCrypto and XChaCha20-Poly1305 is available as
//! pure JS. Argon2 in a browser would mean shipping WASM.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use libsignal_protocol::{KeyPair, PrivateKey, PublicKey};
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{hkdf_sha256, pbkdf2_sha256, random_bytes};
use crate::error::Error;

pub const DROP_MAGIC: [u8; 4] = *b"HSHD";
pub const DROP_VERSION: u8 = 1;

/// PBKDF2 is the only KDF id version 1 defines. It is in the header so a future
/// version can name a different one without the old containers becoming
/// unreadable.
const KDF_PBKDF2_SHA256: u8 = 1;

/// Version 2 of the container: sealed to the recipient's X25519 public key
/// instead of stretched from a shared PIN.
///
/// A new version byte rather than a mode tag inside v1. A mode byte cannot be
/// added to the v1 header without either changing its length — which would make
/// every v1 container already on the relay or in an email unreadable — or
/// overloading an existing v1 field, which is ambiguous. Version 2 keeps v1
/// byte-for-byte identical and gives the second, structurally different header
/// its own layout.
pub const DROP_VERSION_V2: u8 = 2;

/// The only mode version 2 defines: a sealed box over X25519.
///
/// The mode byte is explicit so a v2 container says which crypto it uses rather
/// than the reader inferring it from the layout. A v2 container claiming any
/// other mode is refused rather than guessed at. v1's mode (PBKDF2 from a PIN) is
/// implicit in its version and is not repeated here.
pub const DROP_MODE_PUBKEY: u8 = 2;

/// X25519 public and private keys are both 32 bytes.
const X25519_KEY_LEN: usize = 32;

/// magic + version + mode + ephemeral public key + nonce.
pub const DROP_V2_HEADER_LEN: usize = 4 + 1 + 1 + X25519_KEY_LEN + NONCE_LEN;

/// Domain separator for the HKDF that turns the X25519 shared secret into an
/// AEAD key. Versioned so a future construction cannot collide with this one.
const PUBKEY_KDF_CONTEXT: &[u8] = b"hush-drop-v2/x25519-hkdf-sha256";

/// Prefix for a pasteable drop public key. Versioned and distinctive, so pasting
/// an invite, a recovery code, or a truncated key is refused with a clear reason
/// rather than base64-decoded into nonsense.
pub const DROP_PUBLIC_KEY_PREFIX: &str = "hush-drop-pk-v1:";

/// Prefix for the private-key file `drop-keygen` writes. A different prefix from
/// the public one, so pasting the wrong half is rejected rather than used.
pub const DROP_PRIVATE_KEY_PREFIX: &str = "hush-drop-sk-v1:";

/// Deliberately high: the PIN is the only secret, and an attacker who holds the
/// blob can guess offline. See the module note.
pub const DROP_ITERATIONS: u32 = 600_000;

/// The PIN is the only secret, so the stretch must stay expensive. Asserted at
/// compile time rather than left as a comment someone can quietly lower.
const _: () = assert!(DROP_ITERATIONS >= 600_000);

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// magic + version + kdf + iterations + salt + nonce.
pub const DROP_HEADER_LEN: usize = 4 + 1 + 1 + 4 + SALT_LEN + NONCE_LEN;

/// The container carries attachments, so it is allowed to be larger than a
/// single relay blob. The relay's drop route uses this as its own cap.
pub const MAX_DROP_BYTES: usize = 8 * 1024 * 1024;

/// One file inside a package. `data` is standard base64, so the whole package is
/// plain JSON and a non-Rust reader needs nothing but a base64 decoder.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DropAttachment {
    pub name: String,
    pub data: String,
}

/// What the recipient sees once the container opens.
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

/// Whether `bytes` begins with the container magic.
///
/// This is what the `drop@` ingress keys on: a mail attachment that does not
/// start with `HSHD` is not a container and is refused before anything else
/// looks at it.
pub fn is_container(bytes: &[u8]) -> bool {
    bytes.len() > DROP_HEADER_LEN && bytes[..4] == DROP_MAGIC
}

/// Stretch a PIN into a key. Empty is refused here rather than producing a
/// deterministic key from nothing.
fn pin_key(pin: &str, salt: &[u8], iterations: u32) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
    if pin.is_empty() {
        return Err(Error::EmptyPin);
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    pbkdf2_sha256(pin.as_bytes(), salt, iterations, &mut *key);
    Ok(key)
}

/// Seal a package under `pin`.
///
/// The header is passed as associated data, so the version, KDF id, iteration
/// count, salt and nonce are all authenticated: an attacker cannot lower the
/// iteration count or swap the salt without the tag check failing.
pub fn seal(pkg: &DropPackage, pin: &str) -> Result<Vec<u8>, Error> {
    seal_with_iterations(pkg, pin, DROP_ITERATIONS)
}

/// The iteration count is a parameter rather than a constant read inside, so a
/// test can seal cheaply. Production always goes through [`seal`], which passes
/// [`DROP_ITERATIONS`]; the count is recorded in the header either way, so
/// [`open`] needs no hint.
fn seal_with_iterations(
    pkg: &DropPackage,
    pin: &str,
    iterations: u32,
) -> Result<Vec<u8>, Error> {
    let salt = random_bytes::<SALT_LEN>();
    let nonce_bytes = random_bytes::<NONCE_LEN>();

    let mut header = Vec::with_capacity(DROP_HEADER_LEN);
    header.extend_from_slice(&DROP_MAGIC);
    header.push(DROP_VERSION);
    header.push(KDF_PBKDF2_SHA256);
    header.extend_from_slice(&iterations.to_be_bytes());
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce_bytes);

    let key = pin_key(pin, &salt, iterations)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = serde_json::to_vec(pkg).map_err(|_| Error::BadDrop)?;
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| Error::Crypto)?;

    header.extend_from_slice(&ct);
    Ok(header)
}

/// Open a container with `pin`. A wrong PIN and a tampered container are
/// indistinguishable here, which is the intended behaviour.
pub fn open(bytes: &[u8], pin: &str) -> Result<DropPackage, Error> {
    if bytes.len() < 5 || bytes[..4] != DROP_MAGIC {
        return Err(Error::BadDrop);
    }
    // A v2 container is a public-key drop. Refusing it here, by version, is what
    // makes "a pubkey drop cannot be opened with a PIN" structural rather than a
    // hope that the wrong crypto happens to fail.
    if bytes[4] == DROP_VERSION_V2 {
        return Err(Error::DropNeedsKey);
    }
    if bytes[4] != DROP_VERSION || bytes[5] != KDF_PBKDF2_SHA256 {
        return Err(Error::UnsupportedDrop);
    }
    if bytes.len() <= DROP_HEADER_LEN {
        return Err(Error::BadDrop);
    }
    let iterations = u32::from_be_bytes(bytes[6..10].try_into().map_err(|_| Error::BadDrop)?);
    // A zero count would derive the key from the first PBKDF2 block alone, which
    // is not what any sender meant to produce.
    if iterations == 0 {
        return Err(Error::BadDrop);
    }
    let salt = &bytes[10..10 + SALT_LEN];
    let nonce_bytes = &bytes[10 + SALT_LEN..DROP_HEADER_LEN];
    let header = &bytes[..DROP_HEADER_LEN];

    let key = pin_key(pin, salt, iterations)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce_bytes),
            Payload {
                msg: &bytes[DROP_HEADER_LEN..],
                aad: header,
            },
        )
        .map_err(|_| Error::WrongPassphrase)?;
    serde_json::from_slice(&plaintext).map_err(|_| Error::BadDrop)
}

/// A drop keypair: the recipient's long-lived X25519 key.
///
/// The public half is shared out of band (like a PIN is today); the private half
/// never leaves the recipient and is what opens the drop.
#[derive(Clone)]
pub struct DropKeypair {
    pub public_key: [u8; X25519_KEY_LEN],
    pub private_key: [u8; X25519_KEY_LEN],
}

/// Generate a fresh X25519 keypair for receiving public-key drops.
///
/// The private key is returned to the caller, which is responsible for storing it
/// where the user controls it. Nothing here logs it.
pub fn generate_keypair() -> DropKeypair {
    let mut csprng = OsRng.unwrap_err();
    let pair = KeyPair::generate(&mut csprng);
    let mut public_key = [0u8; X25519_KEY_LEN];
    public_key.copy_from_slice(pair.public_key.public_key_bytes());
    let mut private_key = [0u8; X25519_KEY_LEN];
    private_key.copy_from_slice(&pair.private_key.serialize());
    DropKeypair {
        public_key,
        private_key,
    }
}

/// Encode a drop public key as the pasteable, versioned string the sender types.
pub fn encode_public_key(key: &[u8; X25519_KEY_LEN]) -> String {
    format!("{DROP_PUBLIC_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(key))
}

/// Decode a pasteable drop public key, or refuse it with a reason.
///
/// Rejects the wrong prefix (an invite, a private key, anything else), a bad
/// base64 body, the wrong length, and a point that is not a canonical X25519
/// public key. That is what lets the UI say plainly what is wrong.
pub fn decode_public_key(text: &str) -> Result<[u8; X25519_KEY_LEN], Error> {
    let body = text
        .trim()
        .strip_prefix(DROP_PUBLIC_KEY_PREFIX)
        .ok_or(Error::InvalidDropPublicKey)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| Error::InvalidDropPublicKey)?;
    let key: [u8; X25519_KEY_LEN] = bytes.try_into().map_err(|_| Error::InvalidDropPublicKey)?;
    let point =
        PublicKey::from_djb_public_key_bytes(&key).map_err(|_| Error::InvalidDropPublicKey)?;
    if !point.is_canonical() {
        return Err(Error::InvalidDropPublicKey);
    }
    Ok(key)
}

/// Encode a drop private key for the file `drop-keygen` writes.
pub fn encode_private_key(key: &[u8; X25519_KEY_LEN]) -> String {
    format!("{DROP_PRIVATE_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(key))
}

/// Decode a stored drop private key, or refuse it. The prefix is distinct from
/// the public one, so a public key pasted where a private key belongs is refused.
pub fn decode_private_key(text: &str) -> Result<[u8; X25519_KEY_LEN], Error> {
    let body = text
        .trim()
        .strip_prefix(DROP_PRIVATE_KEY_PREFIX)
        .ok_or(Error::InvalidDropPrivateKey)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| Error::InvalidDropPrivateKey)?;
    bytes
        .try_into()
        .map_err(|_| Error::InvalidDropPrivateKey)
}

/// Derive the AEAD key for a public-key drop from an X25519 shared secret.
///
/// The ephemeral public key is the HKDF salt and the recipient's public key is
/// bound into the info string, so a key is unique to the (ephemeral, recipient)
/// pair and is not reusable across drops. This is a standard sealed-box
/// construction: ephemeral X25519 ECDH, HKDF-SHA256, XChaCha20-Poly1305.
fn pubkey_aead_key(
    ephemeral_public: &[u8; X25519_KEY_LEN],
    shared: &[u8],
    recipient_public: &[u8],
) -> Zeroizing<[u8; KEY_LEN]> {
    let mut info = Vec::with_capacity(PUBKEY_KDF_CONTEXT.len() + recipient_public.len());
    info.extend_from_slice(PUBKEY_KDF_CONTEXT);
    info.extend_from_slice(recipient_public);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hkdf_sha256(ephemeral_public, shared, &info, &mut *key);
    key
}

/// Seal a package to the recipient's X25519 public key.
///
/// The container carries the ephemeral public key so the recipient can derive the
/// same secret. It carries nothing that identifies the sender: there is no
/// signature and no sender key, which is what makes an anonymous drop anonymous.
pub fn seal_to_key(
    pkg: &DropPackage,
    recipient_public_key: &[u8; X25519_KEY_LEN],
) -> Result<Vec<u8>, Error> {
    let recipient = PublicKey::from_djb_public_key_bytes(recipient_public_key)
        .map_err(|_| Error::InvalidDropPublicKey)?;
    if !recipient.is_canonical() {
        return Err(Error::InvalidDropPublicKey);
    }

    let mut csprng = OsRng.unwrap_err();
    let ephemeral = KeyPair::generate(&mut csprng);
    let mut ephemeral_public = [0u8; X25519_KEY_LEN];
    ephemeral_public.copy_from_slice(ephemeral.public_key.public_key_bytes());

    // Rejects the all-zero shared secret, which is what a low-order recipient key
    // would produce.
    let shared = ephemeral
        .private_key
        .calculate_agreement(&recipient)
        .map_err(|_| Error::InvalidDropPublicKey)?;

    let key = pubkey_aead_key(&ephemeral_public, &shared, recipient.public_key_bytes());
    let nonce_bytes = random_bytes::<NONCE_LEN>();

    let mut header = Vec::with_capacity(DROP_V2_HEADER_LEN);
    header.extend_from_slice(&DROP_MAGIC);
    header.push(DROP_VERSION_V2);
    header.push(DROP_MODE_PUBKEY);
    header.extend_from_slice(&ephemeral_public);
    header.extend_from_slice(&nonce_bytes);

    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = serde_json::to_vec(pkg).map_err(|_| Error::BadDrop)?;
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| Error::Crypto)?;

    header.extend_from_slice(&ct);
    Ok(header)
}

/// Open a public-key container with the recipient's X25519 private key.
///
/// A v1 container, or a v2 container that is not the public-key mode, is refused:
/// the two modes are not interchangeable, and trying the wrong one is an error
/// rather than a decryption failure.
pub fn open_with_key(
    bytes: &[u8],
    private_key: &[u8; X25519_KEY_LEN],
) -> Result<DropPackage, Error> {
    if bytes.len() < 5 || bytes[..4] != DROP_MAGIC {
        return Err(Error::BadDrop);
    }
    if bytes[4] == DROP_VERSION {
        return Err(Error::DropNeedsPin);
    }
    if bytes[4] != DROP_VERSION_V2 {
        return Err(Error::UnsupportedDrop);
    }
    if bytes.len() <= DROP_V2_HEADER_LEN {
        return Err(Error::BadDrop);
    }
    if bytes[5] != DROP_MODE_PUBKEY {
        return Err(Error::DropNeedsPin);
    }

    let mut ephemeral_public = [0u8; X25519_KEY_LEN];
    ephemeral_public.copy_from_slice(&bytes[6..6 + X25519_KEY_LEN]);
    let nonce_bytes = &bytes[6 + X25519_KEY_LEN..DROP_V2_HEADER_LEN];
    let header = &bytes[..DROP_V2_HEADER_LEN];

    let recipient = PrivateKey::deserialize(private_key).map_err(|_| Error::InvalidDropPrivateKey)?;
    let recipient_public = recipient
        .public_key()
        .map_err(|_| Error::InvalidDropPrivateKey)?;
    let ephemeral =
        PublicKey::from_djb_public_key_bytes(&ephemeral_public).map_err(|_| Error::BadDrop)?;
    let shared = recipient
        .calculate_agreement(&ephemeral)
        .map_err(|_| Error::BadDrop)?;

    let key = pubkey_aead_key(&ephemeral_public, &shared, recipient_public.public_key_bytes());
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce_bytes),
            Payload {
                msg: &bytes[DROP_V2_HEADER_LEN..],
                aad: header,
            },
        )
        .map_err(|_| Error::WrongPassphrase)?;
    serde_json::from_slice(&plaintext).map_err(|_| Error::BadDrop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap on purpose: the production count is 600k, and paying it in every
    /// test took the suite from seconds to over a minute. The count is carried
    /// in the header, so this exercises the same code path.
    const TEST_ITERATIONS: u32 = 1_000;

    fn sealed(pkg: &DropPackage, pin: &str) -> Vec<u8> {
        seal_with_iterations(pkg, pin, TEST_ITERATIONS).unwrap()
    }

    fn sample() -> DropPackage {
        DropPackage {
            v: 1,
            subject: "hi".into(),
            body: "<b>hello</b>".into(),
            attachments: vec![DropAttachment {
                name: "a.txt".into(),
                data: "aGk=".into(),
            }],
            created_unix: 1_760_000_000,
        }
    }

    #[test]
    fn a_package_round_trips_under_its_pin() {
        let pkg = sample();
        let bytes = sealed(&pkg, "correct horse battery");
        assert!(is_container(&bytes));
        assert_eq!(open(&bytes, "correct horse battery").unwrap(), pkg);

        // Nothing of the plaintext is visible in the container. This is what the
        // relay and the `drop@` ingress rely on: they only ever hold these bytes.
        let as_text = String::from_utf8_lossy(&bytes);
        assert!(!as_text.contains("hello"), "the body must not be readable");
        assert!(!as_text.contains("hi"), "the subject must not be readable");
    }

    #[test]
    fn the_wrong_pin_does_not_open() {
        let bytes = sealed(&sample(), "1111");
        assert!(open(&bytes, "1112").is_err());
    }

    #[test]
    fn a_tampered_body_or_header_does_not_open() {
        let mut bytes = sealed(&sample(), "1111");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(open(&bytes, "1111").is_err(), "a flipped ciphertext byte must fail");

        // The iteration count is inside the authenticated header, so lowering it
        // must not produce a key that opens the body.
        let mut bytes = sealed(&sample(), "1111");
        bytes[6..10].copy_from_slice(&1u32.to_be_bytes());
        assert!(open(&bytes, "1111").is_err(), "a lowered iteration count must fail");
    }

    #[test]
    fn an_empty_pin_is_rejected_and_an_unknown_version_is_refused() {
        assert!(matches!(seal(&sample(), ""), Err(Error::EmptyPin)));

        let mut bytes = sealed(&sample(), "1111");
        bytes[4] = 9;
        assert!(matches!(open(&bytes, "1111"), Err(Error::UnsupportedDrop)));

        // A zero iteration count is refused before any key is derived.
        let mut bytes = sealed(&sample(), "1111");
        bytes[6..10].copy_from_slice(&0u32.to_be_bytes());
        assert!(matches!(open(&bytes, "1111"), Err(Error::BadDrop)));
    }

    #[test]
    fn the_iteration_count_travels_in_the_header() {
        let bytes = sealed(&sample(), "1111");
        let recorded = u32::from_be_bytes(bytes[6..10].try_into().unwrap());
        assert_eq!(recorded, TEST_ITERATIONS);
    }

    #[test]
    fn a_non_container_is_not_mistaken_for_one() {
        assert!(!is_container(b""));
        assert!(!is_container(b"HSHD"));
        assert!(!is_container(&[0u8; DROP_HEADER_LEN + 8]));
    }

    #[test]
    fn a_public_key_drop_round_trips_and_hides_the_plaintext() {
        let pkg = sample();
        let keys = generate_keypair();
        let bytes = seal_to_key(&pkg, &keys.public_key).unwrap();
        assert!(is_container(&bytes));
        assert_eq!(bytes[4], DROP_VERSION_V2, "a key drop is version 2");
        assert_eq!(bytes[5], DROP_MODE_PUBKEY);
        assert_eq!(open_with_key(&bytes, &keys.private_key).unwrap(), pkg);

        // The ephemeral key is carried so the recipient can derive the same
        // secret; nothing of the plaintext is.
        let as_text = String::from_utf8_lossy(&bytes);
        assert!(!as_text.contains("hello"), "the body must not be readable");
        assert!(!as_text.contains("hi"), "the subject must not be readable");
    }

    #[test]
    fn the_wrong_private_key_does_not_open_a_public_key_drop() {
        let keys = generate_keypair();
        let other = generate_keypair();
        let bytes = seal_to_key(&sample(), &keys.public_key).unwrap();
        assert!(matches!(
            open_with_key(&bytes, &other.private_key),
            Err(Error::WrongPassphrase)
        ));
    }

    #[test]
    fn the_two_modes_do_not_open_each_other() {
        let keys = generate_keypair();
        let key_container = seal_to_key(&sample(), &keys.public_key).unwrap();
        let pin_container = sealed(&sample(), "correct horse battery");

        // A key drop has no PIN to try, and the reader says so by version rather
        // than deriving a key that happens to fail.
        assert!(matches!(
            open(&key_container, "correct horse battery"),
            Err(Error::DropNeedsKey)
        ));
        // A PIN drop is not a sealed box and has no ephemeral key to use.
        assert!(matches!(
            open_with_key(&pin_container, &keys.private_key),
            Err(Error::DropNeedsPin)
        ));
    }

    #[test]
    fn a_tampered_public_key_container_does_not_open() {
        let keys = generate_keypair();

        // A flipped ciphertext byte fails the tag.
        let mut bytes = seal_to_key(&sample(), &keys.public_key).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(open_with_key(&bytes, &keys.private_key).is_err());

        // The ephemeral key is inside the authenticated header, so swapping it
        // must fail rather than silently derive a different key.
        let mut bytes = seal_to_key(&sample(), &keys.public_key).unwrap();
        bytes[6] ^= 1;
        assert!(open_with_key(&bytes, &keys.private_key).is_err());

        // An unknown version or mode is refused.
        let mut bytes = seal_to_key(&sample(), &keys.public_key).unwrap();
        bytes[4] = 9;
        assert!(matches!(
            open_with_key(&bytes, &keys.private_key),
            Err(Error::UnsupportedDrop)
        ));
        let mut bytes = seal_to_key(&sample(), &keys.public_key).unwrap();
        bytes[5] = 1;
        assert!(matches!(
            open_with_key(&bytes, &keys.private_key),
            Err(Error::DropNeedsPin)
        ));
    }

    #[test]
    fn a_public_key_encoding_round_trips_and_refuses_the_wrong_shape() {
        let keys = generate_keypair();
        let encoded = encode_public_key(&keys.public_key);
        assert!(encoded.starts_with(DROP_PUBLIC_KEY_PREFIX));
        assert_eq!(decode_public_key(&encoded).unwrap(), keys.public_key);

        // Surrounding whitespace from a paste is tolerated.
        assert_eq!(
            decode_public_key(&format!("  {encoded}\n")).unwrap(),
            keys.public_key
        );

        for bad in [
            "",
            "not a key",
            // An invite is base64 but not prefixed, and must be refused.
            "eyJ2IjoxfQ",
            // The wrong half of the pair.
            &encode_private_key(&keys.private_key),
            // Right prefix, wrong body.
            "hush-drop-pk-v1:not-base64!!",
            // Right prefix, wrong length.
            "hush-drop-pk-v1:AAAA",
        ] {
            assert!(
                matches!(decode_public_key(bad), Err(Error::InvalidDropPublicKey)),
                "{bad:?} should have been refused"
            );
        }
    }

    #[test]
    fn a_private_key_encoding_is_distinct_from_the_public_one() {
        let keys = generate_keypair();
        let encoded = encode_private_key(&keys.private_key);
        assert!(encoded.starts_with(DROP_PRIVATE_KEY_PREFIX));
        assert_eq!(decode_private_key(&encoded).unwrap(), keys.private_key);

        // Pasting a public key where a private key belongs is refused, and vice
        // versa: the prefixes differ.
        assert!(matches!(
            decode_private_key(&encode_public_key(&keys.public_key)),
            Err(Error::InvalidDropPrivateKey)
        ));
        assert!(matches!(
            decode_public_key(&encoded),
            Err(Error::InvalidDropPublicKey)
        ));
    }
}
