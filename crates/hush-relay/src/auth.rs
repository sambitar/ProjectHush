use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Compare a presented token against a stored hash in constant time.
///
/// Both sides are hashed first, so the comparison is always over 32 bytes and
/// does not depend on the length of the presented token. A short-circuiting `==`
/// would let a caller learn the stored hash one byte at a time from response
/// timing, which matters because this runs on a network-facing route.
pub fn tokens_match(expected_hash: &[u8], token: &str) -> bool {
    if expected_hash.len() != 32 {
        return false;
    }
    let got = hash_token(token);
    got.ct_eq(expected_hash).into()
}
