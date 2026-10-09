use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    Key, XChaCha20Poly1305, XNonce,
};
use rand::{RngCore, TryRngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::Error;

pub const MASTER_KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const SALT_LEN: usize = 16;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rngs::OsRng.unwrap_err().fill_bytes(&mut buf);
    buf
}

pub fn argon2_params() -> Result<Params, Error> {
    Ok(Params::new(19456, 2, 1, Some(MASTER_KEY_LEN))?)
}

pub fn derive_master_key(
    passphrase: &str,
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, Error> {
    let params = Params::new(m_cost, t_cost, p_cost, Some(MASTER_KEY_LEN))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    argon2.hash_password_into(passphrase.as_bytes(), salt, &mut *out)?;
    Ok(out)
}

pub fn encrypt(master: &[u8; MASTER_KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(master));
    let nonce_bytes = random_bytes::<NONCE_LEN>();
    let nonce = XNonce::from_slice(&nonce_bytes);
    let mut out = nonce_bytes.to_vec();
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| Error::Crypto)?;
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(master: &[u8; MASTER_KEY_LEN], packed: &[u8]) -> Result<Vec<u8>, Error> {
    if packed.len() < NONCE_LEN {
        return Err(Error::WrongPassphrase);
    }
    let (nonce_bytes, ct) = packed.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(master));
    let nonce = XNonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| Error::WrongPassphrase)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Context string mixed into the queue delivery token.
///
/// A domain separator: the same read token must never produce a value that
/// collides with a token derived for any other purpose.
const QUEUE_DELIVERY_CONTEXT: &[u8] = b"hush-queue-delivery-v1";

/// The *public* credential for a device's relay queue, derived from the private
/// read token.
///
/// A queue needs two credentials that must not be interchangeable. A peer uses
/// the delivery token to put a message in the inbox, and that token travels in
/// invites, so it is public by construction. The device itself uses the read
/// token to list, read, delete and burn, and that must never be public: a
/// published credential that authorizes burning lets anyone permanently destroy
/// the inbox.
///
/// Deriving the public one as `HMAC(read_token, context)` gives both properties
/// with a single stored secret. A peer holding the delivery token cannot invert
/// the MAC to recover the read token, while the relay — which stores only a hash
/// of the delivery token — can still recognise a read request by recomputing the
/// MAC from the presented read token. So the relay keeps one hash per queue and
/// there is no second credential to register or rotate.
pub fn queue_delivery_token(read_token: &str) -> String {
    hex::encode(hmac_sha256(read_token.as_bytes(), QUEUE_DELIVERY_CONTEXT))
}

/// Domain separators for a per-contact queue's two credentials.
///
/// A device mints one queue per contact it receives from, so deleting a contact
/// can burn that one queue without touching any other. The queue's id and its
/// read token are both derived from the device's own read token and the
/// contact's account id, which means the device stores no per-contact secret: it
/// recomputes the id and the read token to read and to burn, and hands the
/// derived *delivery* token to the contact in a grant.
///
/// The two contexts are distinct so the id of a contact's queue can never equal
/// the read token of any queue, or vice versa.
const CONTACT_QUEUE_ID_CONTEXT: &[u8] = b"hush-contact-queue-id-v1";
const CONTACT_QUEUE_READ_CONTEXT: &[u8] = b"hush-contact-queue-read-v1";

/// MAC a device read token against a context and an account id.
///
/// The account id is appended rather than folded into the key so that the same
/// device read token yields an independent credential for every contact.
fn contact_queue_mac(read_token: &str, context: &[u8], account_id: &str) -> String {
    let mut message = Vec::with_capacity(context.len() + account_id.len());
    message.extend_from_slice(context);
    message.extend_from_slice(account_id.as_bytes());
    hex::encode(hmac_sha256(read_token.as_bytes(), &message))
}

/// The relay queue id of the queue this device mints for one contact.
pub fn contact_queue_id(read_token: &str, account_id: &str) -> String {
    contact_queue_mac(read_token, CONTACT_QUEUE_ID_CONTEXT, account_id)
}

/// The private read token for the queue this device mints for one contact.
///
/// Kept out of the vault and out of any invite or grant, like the device read
/// token it is derived from: it authorizes listing, deleting and burning.
pub fn contact_queue_read_token(read_token: &str, account_id: &str) -> String {
    contact_queue_mac(read_token, CONTACT_QUEUE_READ_CONTEXT, account_id)
}

/// HMAC-SHA256 (RFC 2104) over the `sha2` block size.
///
/// Implemented here rather than pulled in as a dependency: the tree already has
/// `sha2` at the version this needs, and the crate's other candidate (`hmac`)
/// would arrive on a second `sha2` major and duplicate the hash implementation.
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;

    // A key longer than the block is hashed down; a shorter one is zero-padded.
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&sha256(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// HKDF-SHA256 (RFC 5869), extract-then-expand, filling `out` from `ikm`,
/// `salt` and `info`.
///
/// Hand-rolled for the same reason HMAC is: no new dependency for one call
/// site. An empty salt means HMAC with a 32-byte zero block, per the RFC.
/// Checked against the RFC 5869 vectors in [`crate::drop`]'s tests.
pub(crate) fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    // extract
    let mut salt_block = [0u8; 32];
    if !salt.is_empty() {
        let take = salt.len().min(32);
        salt_block[..take].copy_from_slice(&salt[..take]);
    }
    let prk = hmac_sha256(&salt_block, ikm);

    // expand: block i = HMAC(prk, previous block || info || i)
    let mut t = [0u8; 32];
    let mut done = 0usize;
    let mut counter = 1u8;
    while done < out.len() {
        let mut input = Vec::with_capacity(32 + info.len() + 1);
        input.extend_from_slice(&t[..done.min(32)]);
        input.extend_from_slice(info);
        input.push(counter);
        t = hmac_sha256(&prk, &input);
        let take = (out.len() - done).min(32);
        out[done..done + take].copy_from_slice(&t[..take]);
        done += take;
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 test vectors. The derivation is load-bearing for queue
    /// authorization, so it is checked against the specification rather than
    /// only against itself.
    #[test]
    fn hmac_sha256_matches_rfc4231() {
        // Case 1: 20-byte key of 0x0b, "Hi There".
        assert_eq!(
            hex::encode(hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        // Case 2: key "Jefe", "what do ya want for nothing?".
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        // Case 6: a key longer than the 64-byte block must be hashed first.
        let long_key = [0xaau8; 131];
        assert_eq!(
            hex::encode(hmac_sha256(
                &long_key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// The property the whole split rests on: the public token must not reveal
    /// the private one.
    #[test]
    fn the_delivery_token_is_not_the_read_token() {
        let read = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
        let delivery = queue_delivery_token(read);
        assert_ne!(delivery, read);
        assert_eq!(delivery.len(), 64, "hex of a 32-byte MAC");
        assert_eq!(
            queue_delivery_token(read),
            delivery,
            "derivation must be deterministic"
        );
        assert_ne!(
            queue_delivery_token("a different read token"),
            delivery,
            "different read tokens must not collide"
        );
    }
}
