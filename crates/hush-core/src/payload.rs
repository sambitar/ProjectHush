use serde::{Deserialize, Serialize};

/// Max file bytes inlined in one Signal message (relay cap is 1 MiB total wire).
pub const MAX_INLINE_FILE_BYTES: usize = 256 * 1024;

/// Max attachments in one mail message. The total byte cap is
/// `MAX_INLINE_FILE_BYTES`, shared across all of them.
pub const MAX_MAIL_ATTACHMENTS: usize = 8;

/// One attachment on the wire: a display name and its bytes, base64 in JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailAttachment {
    pub name: String,
    #[serde(with = "b64_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum MessagePayload {
    Text { body: String },
    File {
        name: String,
        #[serde(with = "b64_bytes")]
        data: Vec<u8>,
    },
    /// An internal mail message. Distinct from `Text`/`File` so the Mail rail can
    /// render threads, subjects and multiple attachments while riding the same
    /// encrypted session and relay queue.
    Mail {
        thread_id: String,
        #[serde(default)]
        subject: Option<String>,
        body: String,
        #[serde(default)]
        attachments: Vec<MailAttachment>,
    },
    /// A copy of a message this account sent, fanned out to its own other
    /// devices so a linked device shows it in the right conversation.
    ///
    /// The recipient's account id travels with it because the receiving device
    /// cannot recover it from the ciphertext: the session it arrived on names
    /// *this* account, not the party the message was addressed to. Guessing from
    /// the chat log would misfile the message whenever two conversations are
    /// open.
    OwnCopy {
        to_account_id: String,
        inner: Box<MessagePayload>,
    },
}

mod b64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

impl MessagePayload {
    pub fn encode(&self) -> Result<Vec<u8>, crate::error::Error> {
        serde_json::to_vec(self).map_err(|_| crate::error::Error::Corrupt)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, crate::error::Error> {
        if let Ok(p) = serde_json::from_slice::<Self>(bytes) {
            return Ok(p);
        }
        // Legacy file payloads used JSON byte arrays before base64.
        #[derive(Deserialize)]
        struct LegacyFile {
            t: String,
            name: String,
            data: Vec<u8>,
        }
        if let Ok(legacy) = serde_json::from_slice::<LegacyFile>(bytes) {
            if legacy.t == "file" {
                return Ok(MessagePayload::File {
                    name: legacy.name,
                    data: legacy.data,
                });
            }
        }
        serde_json::from_slice(bytes).map_err(|_| crate::error::Error::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_payload_round_trips_with_attachments() {
        let p = MessagePayload::Mail {
            thread_id: "t-1".into(),
            subject: Some("hi".into()),
            body: "body".into(),
            attachments: vec![MailAttachment {
                name: "a.txt".into(),
                data: vec![1, 2, 3],
            }],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_mail_payload_without_subject_or_attachments_round_trips() {
        let p = MessagePayload::Mail {
            thread_id: "t-2".into(),
            subject: None,
            body: "body".into(),
            attachments: vec![],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }
}
