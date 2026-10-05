use sha2::{Digest, Sha256};

pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn tokens_match(expected_hash: &[u8], token: &str) -> bool {
    let got = hash_token(token);
    expected_hash.len() == 32 && got.as_ref() == expected_hash
}
