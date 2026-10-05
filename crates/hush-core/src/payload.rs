use serde::{Deserialize, Serialize};

/// Max file bytes inlined in one Signal message (relay cap is 1 MiB total wire).
pub const MAX_INLINE_FILE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum MessagePayload {
    Text { body: String },
    File {
        name: String,
        data: Vec<u8>,
    },
}

impl MessagePayload {
    pub fn encode(&self) -> Result<Vec<u8>, crate::error::Error> {
        serde_json::to_vec(self).map_err(|_| crate::error::Error::Corrupt)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, crate::error::Error> {
        serde_json::from_slice(bytes).map_err(|_| crate::error::Error::Corrupt)
    }
}
