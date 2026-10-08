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
    /// A message in a local meeting, fanned out to each member over their own
    /// 1:1 session.
    ///
    /// There is no relay-side room and no group object: the sender encrypts this
    /// once per member and puts N blobs. The descriptor (`meeting_id`, `name`,
    /// `members`) travels with every message so a recipient that has never seen
    /// the meeting can materialise it locally; once the meeting exists, later
    /// messages just append. Membership is the sender's local view, so the names
    /// carried here are the sender's nicknames and may not match the recipient's.
    Meeting {
        meeting_id: String,
        name: String,
        #[serde(default)]
        members: Vec<String>,
        body: String,
    },
    /// A grant of the sender's per-contact receive queue.
    ///
    /// Delivered into the recipient's inbox (the grant channel) and encrypted by
    /// the same libsignal session as any other payload, so the relay cannot read
    /// which account is granting to whom. The signature is what the recipient
    /// actually trusts: it is verified against the identity key the sender
    /// pinned in their invite, so an unauthenticated write to the grant channel
    /// cannot introduce a queue the sender does not own. See
    /// `Account::apply_queue_grant`.
    QueueGrant {
        /// The queue id the recipient should deliver this sender's account into.
        queue_id: String,
        /// The delivery token for that queue. Public by design: it authorizes
        /// putting a message in the queue and nothing else.
        delivery_token: String,
        /// Signature over the canonical grant bytes by the granting account's
        /// identity key.
        signature: Vec<u8>,
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

    #[test]
    fn a_meeting_payload_round_trips_with_its_descriptor() {
        let p = MessagePayload::Meeting {
            meeting_id: "m-1".into(),
            name: "Standup".into(),
            members: vec!["bob".into(), "carol".into()],
            body: "morning".into(),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A message from a build that predates the descriptor must still decode, so
    /// the member list is defaulted rather than required.
    #[test]
    fn a_meeting_payload_without_a_member_list_still_decodes() {
        let json = r#"{"t":"meeting","meeting_id":"m-1","name":"Standup","body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Meeting { members, .. } => assert!(members.is_empty()),
            other => panic!("expected a meeting, got {other:?}"),
        }
    }
}
