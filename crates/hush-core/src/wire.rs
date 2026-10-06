//! Relay queue blob format (Task 5c.1).
//!
//! v1 wire: **opaque Signal ciphertext bytes only** — no JSON, no cleartext sender id on the relay.
//! Legacy: JSON `{"from":"…","ct":[…]}` still accepted on read for in-flight blobs.

use serde::Deserialize;

#[derive(Deserialize)]
struct LegacyWireEnvelope {
    from: String,
    ct: Vec<u8>,
}

/// Bytes to store on the relay for one message.
pub fn encode_relay_blob(signal_ciphertext: &[u8]) -> Vec<u8> {
    signal_ciphertext.to_vec()
}

/// Parse a relay blob into Signal ciphertext and optional legacy sender name.
pub fn decode_relay_blob(body: &[u8]) -> Result<(Vec<u8>, Option<String>), crate::error::Error> {
    if body.is_empty() {
        return Err(crate::error::Error::Corrupt);
    }
    if body.first() == Some(&b'{') {
        if let Ok(legacy) = serde_json::from_slice::<LegacyWireEnvelope>(body) {
            return Ok((legacy.ct, Some(legacy.from)));
        }
    }
    Ok((body.to_vec(), None))
}
