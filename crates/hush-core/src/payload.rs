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

/// A trustee's standing in a conclave.
///
/// The state is per trustee and converges across peers: every change is
/// broadcast to the joined trustees, and each member's own vault is the only
/// place the state is stored (there is no relay-side group object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrusteeState {
    /// Invited but has not accepted. Receives the invite and nothing else.
    Invited,
    /// In the conclave and receiving its messages.
    #[default]
    Joined,
    /// Was a trustee and left voluntarily.
    Left,
    /// Removed by the convener.
    Kicked,
}

/// One trustee in a conclave descriptor.
///
/// `account_id` is the stable identity: it is what lets a membership change
/// converge across vaults, where a nickname would not (two peers can call the
/// same person different things). `nick` is the sender's display hint and is
/// never authoritative.
///
/// A trustee written before this rename was a bare nickname string in the
/// record's `members` list; the custom `Deserialize` below still reads that
/// shape, so an existing vault loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConclaveTrustee {
    /// Stable identity: the trustee's account id (hex of the identity key).
    pub account_id: String,
    /// The sender's nickname for this trustee; a display hint only.
    pub nick: String,
    pub state: TrusteeState,
}

impl<'de> Deserialize<'de> for ConclaveTrustee {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            /// The pre-rename shape: `members: ["bob"]`.
            Nick(String),
            Full {
                #[serde(default)]
                account_id: String,
                #[serde(default)]
                nick: String,
                #[serde(default)]
                state: TrusteeState,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Nick(nick) => ConclaveTrustee {
                account_id: String::new(),
                nick,
                state: TrusteeState::Joined,
            },
            Repr::Full {
                account_id,
                nick,
                state,
            } => ConclaveTrustee {
                account_id,
                nick,
                state,
            },
        })
    }
}

/// The membership descriptor carried by every conclave control payload.
///
/// It is the whole of a conclave's shared state: the name, the convener (the
/// only trustee who may invite or kick, and the only one who may end), the
/// trustees with per-member state, and whether the conclave has ended. It rides
/// on invite, accept, leave, update and end so the trustee list converges on
/// every peer without a server object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveDescriptor {
    pub conclave_id: String,
    pub name: String,
    /// The convener's account id. Only the convener may invite, kick, or end.
    #[serde(default)]
    pub convener: String,
    /// The trustees. `alias` reads the pre-rename `members` field name.
    #[serde(default, alias = "members")]
    pub trustees: Vec<ConclaveTrustee>,
    #[serde(default)]
    pub ended: bool,
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
    /// A message in a local conclave, fanned out to each member over their own
    /// 1:1 session.
    ///
    /// There is no relay-side room and no group object: the sender encrypts this
    /// once per member and puts N blobs. The descriptor (`conclave_id`, `name`,
    /// `members`) travels with every message so a recipient that has never seen
    /// the conclave can materialise it locally; once the conclave exists, later
    /// messages just append. Membership is the sender's local view, so the names
    /// carried here are the sender's nicknames and may not match the recipient's.
    ///
    /// The wire tag and the id field are renamed from the original `meeting` /
    /// `meeting_id`. `alias` keeps every already-queued or already-sent message
    /// decodable, so the rename does not strand anything on the relay.
    #[serde(alias = "meeting")]
    Conclave {
        #[serde(alias = "meeting_id")]
        conclave_id: String,
        name: String,
        /// The sender's trustee nicknames. `alias` reads the pre-rename
        /// `members` field so an already-queued message still decodes.
        #[serde(default, alias = "members")]
        trustees: Vec<String>,
        body: String,
    },
    /// An invitation to join a conclave.
    ///
    /// This replaces the old silent materialisation: a message for a conclave a
    /// vault has never seen no longer creates it. The invite is stored as a
    /// pending record the recipient must explicitly accept or decline, and only
    /// on acceptance does the conclave exist locally. Sent by the convener only.
    ConclaveInvite { descriptor: ConclaveDescriptor },
    /// An acknowledgement that an invited trustee joined.
    ///
    /// Sent to the convener, who re-broadcasts the authoritative trustee list so
    /// the convener's record distinguishes invited from joined on every peer.
    ConclaveAccept { descriptor: ConclaveDescriptor },
    /// A declined invitation.
    ///
    /// Deliberately silent to the convener beyond the state change: no further
    /// notification is sent and the trustee is not carried to anyone else.
    ConclaveDecline {
        conclave_id: String,
        /// The account id of the trustee who declined.
        member: String,
    },
    /// A trustee left the conclave voluntarily.
    ///
    /// Sent by the leaver to the convener, who applies it and re-broadcasts the
    /// trustee list, so every joined trustee converges from one authority rather
    /// than a peer-to-peer flood.
    ConclaveLeave { descriptor: ConclaveDescriptor },
    /// The convener's authoritative trustee snapshot, sent to every joined
    /// trustee.
    ///
    /// This is the single convergence path: invites, joins, kicks, and leaves all
    /// reach the other trustees as a convener-originated update, so there is
    /// exactly one writer whose view everyone else adopts. Receivers accept it
    /// only from the convener named in the descriptor.
    ConclaveUpdate { descriptor: ConclaveDescriptor },
    /// The convener ended the conclave for everyone.
    ///
    /// The record and its message log stay in every vault; the descriptor's
    /// `ended` flag makes it read-only on every peer rather than only on the
    /// convener's.
    ConclaveEnd { descriptor: ConclaveDescriptor },
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
    fn a_conclave_payload_round_trips_with_its_descriptor() {
        let p = MessagePayload::Conclave {
            conclave_id: "m-1".into(),
            name: "Standup".into(),
            trustees: vec!["bob".into(), "carol".into()],
            body: "morning".into(),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// The wire tag is now `conclave` and the id field `conclave_id`.
    #[test]
    fn a_conclave_payload_writes_the_new_tag_and_field() {
        let p = MessagePayload::Conclave {
            conclave_id: "m-1".into(),
            name: "Standup".into(),
            trustees: vec!["bob".into()],
            body: "hi".into(),
        };
        let json = String::from_utf8(p.encode().unwrap()).unwrap();
        assert!(json.contains(r#""t":"conclave""#), "json was {json}");
        assert!(json.contains(r#""conclave_id":"m-1""#), "json was {json}");
    }

    /// A message written by the old build (`t: "meeting"`, `meeting_id`) must
    /// still decode, so a rename does not strand queued or in-flight messages.
    #[test]
    fn a_legacy_meeting_payload_still_decodes() {
        let json = r#"{"t":"meeting","meeting_id":"m-1","name":"Standup","members":["bob"],"body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave {
                conclave_id, trustees, ..
            } => {
                assert_eq!(conclave_id, "m-1");
                assert_eq!(trustees, vec!["bob".to_string()]);
            }
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    #[test]
    fn a_conclave_payload_without_a_member_list_still_decodes() {
        let json = r#"{"t":"conclave","conclave_id":"m-1","name":"Standup","body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave { trustees, .. } => assert!(trustees.is_empty()),
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    /// A queued message written before the trustees rename carries `members`; it
    /// must still decode.
    #[test]
    fn a_pre_rename_conclave_payload_still_decodes_its_members() {
        let json = r#"{"t":"conclave","conclave_id":"m-1","name":"Standup","members":["bob"],"body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave { trustees, .. } => {
                assert_eq!(trustees, vec!["bob".to_string()]);
            }
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    fn trustee(account_id: &str, nick: &str, state: TrusteeState) -> ConclaveTrustee {
        ConclaveTrustee {
            account_id: account_id.to_string(),
            nick: nick.to_string(),
            state,
        }
    }

    #[test]
    fn a_conclave_invite_round_trips_with_its_trustees() {
        let p = MessagePayload::ConclaveInvite {
            descriptor: ConclaveDescriptor {
                conclave_id: "c-1".into(),
                name: "Trip".into(),
                convener: "aa".into(),
                trustees: vec![
                    trustee("aa", "alice", TrusteeState::Joined),
                    trustee("bb", "bob", TrusteeState::Invited),
                ],
                ended: false,
            },
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_accept_round_trips_and_moves_the_trustee_to_joined() {
        let p = MessagePayload::ConclaveAccept {
            descriptor: ConclaveDescriptor {
                conclave_id: "c-1".into(),
                name: "Trip".into(),
                convener: "aa".into(),
                trustees: vec![trustee("bb", "bob", TrusteeState::Joined)],
                ended: false,
            },
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_decline_round_trips() {
        let p = MessagePayload::ConclaveDecline {
            conclave_id: "c-1".into(),
            member: "bb".into(),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A trustee written before `state` existed decodes as joined, which is what
    /// a member was before consent existed.
    #[test]
    fn a_trustee_without_a_state_decodes_as_joined() {
        let json = r#"{"account_id":"bb","nick":"bob"}"#;
        let m: ConclaveTrustee = serde_json::from_str(json).unwrap();
        assert_eq!(m.state, TrusteeState::Joined);
    }

    /// The pre-rename record shape was a bare nickname list under `members`; a
    /// descriptor written that way must still decode.
    #[test]
    fn a_descriptor_with_pre_rename_members_still_decodes() {
        let json = r#"{"conclave_id":"c-1","name":"Trip","members":["bob"]}"#;
        let d: ConclaveDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(d.trustees.len(), 1);
        assert_eq!(d.trustees[0].nick, "bob");
        assert_eq!(d.trustees[0].state, TrusteeState::Joined);
    }
}
