use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    Key, XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::Error;

pub const MASTER_KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const SALT_LEN: usize = 16;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut buf);
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
