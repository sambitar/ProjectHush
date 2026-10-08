use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, MASTER_KEY_LEN};
use crate::error::Error;
use crate::payload::{ConclaveTrustee, TrusteeState};
use crate::vault::BlobId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: String,
    pub contact_nick: String,
    pub outgoing: bool,
    pub ts_ms: u64,
    pub payload: ChatRecordPayload,
}

/// One attachment as stored in the log: the bytes live in a vault blob, and the
/// record keeps only the reference, name and size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailAttachmentRecord {
    pub name: String,
    pub blob_id_hex: String,
    pub size: u64,
}

/// Where an outgoing mail message is in its journey.
///
/// Hush has no delivery receipt, so this is deliberately only two states: the
/// relay accepted the ciphertext (`Sent`), or it did not and we still owe the
/// recipient a copy (`Queued`). Anything finer would be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MailState {
    /// Handed to the relay. This is what the Sent folder shows.
    #[default]
    Sent,
    /// Composed, recorded, but the relay did not accept it. The daemon retries
    /// and flips this to `Sent`; the Outbox folder shows exactly these.
    Queued,
}

/// What an outgoing drop needs to be shown and re-shared.
///
/// A drop has no contact and no thread partner, so the address is kept here
/// rather than inferred from the contact list. `delivery_id` is the relay
/// capability, and the pickup link is composed from it by whoever knows the
/// deployment's base URL — deliberately not stored here, because the base URL is
/// configuration and this record is vault state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalMail {
    /// The typed recipient address.
    pub to: String,
    /// The relay capability id the container was stored under.
    pub delivery_id: String,
    /// When the relay will drop it.
    pub expires_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatRecordPayload {    Text {
        body: String,
    },
    File {
        name: String,
        blob_id_hex: String,
        size: u64,
    },
    Mail {
        thread_id: String,
        subject: Option<String>,
        body: String,
        attachments: Vec<MailAttachmentRecord>,
        /// Defaulted so records written before the Outbox existed decode as
        /// `Sent`, which is what they are.
        #[serde(default)]
        state: MailState,
        /// Set when this went to a non-member address instead of a contact. The
        /// recipient has no keys, so the record carries the capability id rather
        /// than a session: that is the whole difference between a drop and mail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external: Option<ExternalMail>,
    },
    /// A composed message that has not been sent. Unlike mail this is mutable:
    /// it is edited and deleted in place, which is why the log grew
    /// `replace_payload`/`remove_record` alongside it.
    Draft {
        /// Set when the draft is a reply, so sending keeps the conversation.
        #[serde(default)]
        thread_id: Option<String>,
        /// Recipient nickname; empty for a draft that is not addressed yet.
        #[serde(default)]
        to: String,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        body: String,
        #[serde(default)]
        attachments: Vec<MailAttachmentRecord>,
        #[serde(default)]
        updated_ms: u64,
    },
}

/// One message inside a conclave's own log.
///
/// Conclave messages do not go through [`ChatRecord`]: a conclave is a local
/// construct with its own log, and keeping its messages here is what keeps them
/// out of the chat and mail projections without a filter that could be forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveMessage {
    pub id: String,
    pub outgoing: bool,
    /// The sender's nickname as this vault knows it; `None` for our own messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub body: String,
    pub ts_ms: u64,
}

/// A conclave: a local record, not a relay-side room.
///
/// It holds the id a descriptor is keyed on, the name, the trustees (with their
/// per-member state), when it was created, its convener, and its own message
/// log. Group chat is sender-side fan-out over the trustees' 1:1 sessions; there
/// is no group key and no server object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveRecord {
    pub id: String,
    pub name: String,
    /// The trustees, keyed on the stable account id.
    ///
    /// `alias` reads the pre-rename `members` key, whose entries were bare
    /// nickname strings (still accepted by [`ConclaveTrustee`]'s custom
    /// `Deserialize`), so an existing vault loads. New records write `trustees`.
    #[serde(default, alias = "members")]
    pub trustees: Vec<ConclaveTrustee>,
    pub created_ms: u64,
    #[serde(default)]
    pub messages: Vec<ConclaveMessage>,
    /// Set when the conclave ends. The record and its message log stay in the
    /// vault; an ended conclave is read-only on every peer. Defaulted so a
    /// record written before ending existed loads as still active.
    #[serde(default)]
    pub ended: bool,
    /// The convener's account id. The convener is the only trustee who may
    /// invite, kick, or end. Empty for a record written before governance
    /// existed; hydration fills it with the local account.
    #[serde(default, alias = "creator")]
    pub convener: String,
}

impl ConclaveRecord {
    /// Every joined trustee, in order.
    pub fn joined(&self) -> impl Iterator<Item = &ConclaveTrustee> {
        self.trustees
            .iter()
            .filter(|m| m.state == TrusteeState::Joined)
    }

    /// Every invited (not yet accepted) trustee.
    pub fn invited(&self) -> impl Iterator<Item = &ConclaveTrustee> {
        self.trustees
            .iter()
            .filter(|m| m.state == TrusteeState::Invited)
    }

    pub fn trustee(&self, account_id: &str) -> Option<&ConclaveTrustee> {
        self.trustees.iter().find(|m| m.account_id == account_id)
    }

    pub fn is_joined(&self, account_id: &str) -> bool {
        self.trustee(account_id)
            .is_some_and(|m| m.state == TrusteeState::Joined)
    }

    /// Insert or update one trustee's state.
    ///
    /// The merge is monotonic, so a stale snapshot cannot undo a newer fact:
    /// a terminal state (left or kicked) is never revived by a live one, and a
    /// joined trustee is never downgraded back to invited by a peer that had not
    /// yet heard they joined. Returns whether anything changed.
    pub fn upsert_trustee(&mut self, incoming: &ConclaveTrustee) -> bool {
        match self
            .trustees
            .iter_mut()
            .find(|m| m.account_id == incoming.account_id)
        {
            Some(existing) => {
                let terminal = matches!(existing.state, TrusteeState::Left | TrusteeState::Kicked);
                let incoming_live =
                    matches!(incoming.state, TrusteeState::Invited | TrusteeState::Joined);
                let downgrade =
                    existing.state == TrusteeState::Joined && incoming.state == TrusteeState::Invited;
                let mut changed = false;
                if !(terminal && incoming_live) && !downgrade && existing.state != incoming.state {
                    existing.state = incoming.state;
                    changed = true;
                }
                if existing.nick.is_empty() && !incoming.nick.is_empty() {
                    existing.nick = incoming.nick.clone();
                    changed = true;
                }
                changed
            }
            None => {
                self.trustees.push(incoming.clone());
                true
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ChatLogFile {
    messages: Vec<ChatRecord>,
    /// Conclaves this vault holds. Defaulted so a log written before conclaves
    /// existed still loads.
    ///
    /// The on-disk key stays `meetings`: every existing `chat.enc` writes it that
    /// way, and pinning it means an older build can still read a log this build
    /// writes (a rollback must not lose records). `alias` accepts the renamed key
    /// if one is ever written.
    #[serde(default, rename = "meetings", alias = "conclaves")]
    conclaves: Vec<ConclaveRecord>,
}

pub struct ChatLog {
    path: PathBuf,
    file: ChatLogFile,
}

impl ChatLog {
    pub fn load(dir: &Path, master: &[u8; MASTER_KEY_LEN]) -> Result<Self, Error> {
        let path = dir.join("chat.enc");
        let file = if path.exists() {
            let packed = fs::read(&path)?;
            let plain = decrypt(master, &packed)?;
            serde_json::from_slice(&plain).map_err(|_| Error::Corrupt)?
        } else {
            ChatLogFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn save(&self, master: &[u8; MASTER_KEY_LEN]) -> Result<(), Error> {
        let plain = serde_json::to_vec(&self.file).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(master, &plain)?;
        fs::write(&self.path, packed)?;
        Ok(())
    }

    pub fn append_text(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        body: &str,
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Text {
                    body: body.to_string(),
                },
            },
        )
    }

    pub fn append_file(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        name: &str,
        blob_id: &BlobId,
        size: u64,
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::File {
                    name: name.to_string(),
                    blob_id_hex: hex::encode(blob_id.0),
                    size,
                },
            },
        )
    }

    /// Append an outgoing or incoming mail message, returning its record id.
    #[allow(clippy::too_many_arguments)]
    pub fn append_mail(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        thread_id: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
        state: MailState,
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Mail {
                    thread_id: thread_id.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    state,
                    external: None,
                },
            },
        )?;
        Ok(id)
    }

    /// Append an outgoing drop: a message sealed for someone with no account.
    ///
    /// `contact_nick` is set to the typed address so the list can render a row
    /// without unwrapping the payload, and the `external` field carries the
    /// capability id the link is built from. There is no `MailState`: a drop is
    /// either on the relay or the send returned an error, so there is nothing to
    /// retry from an Outbox.
    #[allow(clippy::too_many_arguments)]
    pub fn append_external_mail(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        to: &str,
        delivery_id: &str,
        expires_unix: u64,
        subject: &str,
        body: &str,
        attachments: &[MailAttachmentRecord],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                contact_nick: to.to_string(),
                outgoing: true,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Mail {
                    thread_id: Uuid::new_v4().to_string(),
                    subject: (!subject.is_empty()).then(|| subject.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    state: MailState::Sent,
                    external: Some(ExternalMail {
                        to: to.to_string(),
                        delivery_id: delivery_id.to_string(),
                        expires_unix,
                    }),
                },
            },
        )?;
        Ok(id)
    }

    /// Append a new draft, returning its record id.
    #[allow(clippy::too_many_arguments)]
    pub fn append_draft(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        thread_id: Option<&str>,
        to: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                // A draft has no peer yet when it is unaddressed, so the
                // contact column is filled in when it is sent.
                contact_nick: to.to_string(),
                outgoing: true,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Draft {
                    thread_id: thread_id.map(|t| t.to_string()),
                    to: to.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    updated_ms: now_ms(),
                },
            },
        )?;
        Ok(id)
    }

    /// Replace one record's payload in place, keeping its id and contact.
    ///
    /// Returns `false` when the id is unknown, so callers can tell "nothing to
    /// do" from "written".
    pub fn replace_payload(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        payload: ChatRecordPayload,
        ts_ms: u64,
    ) -> Result<bool, Error> {
        let Some(record) = self.file.messages.iter_mut().find(|r| r.id == id) else {
            return Ok(false);
        };
        record.payload = payload;
        record.ts_ms = ts_ms;
        self.save(master)?;
        Ok(true)
    }

    /// Drop a record. Returns `false` when the id is unknown.
    pub fn remove_record(&mut self, master: &[u8; MASTER_KEY_LEN], id: &str) -> Result<bool, Error> {
        let before = self.file.messages.len();
        self.file.messages.retain(|r| r.id != id);
        if self.file.messages.len() == before {
            return Ok(false);
        }
        self.save(master)?;
        Ok(true)
    }

    /// The conclaves this vault holds, in creation order.
    pub fn conclaves(&self) -> &[ConclaveRecord] {
        &self.file.conclaves
    }

    pub fn conclave(&self, id: &str) -> Option<&ConclaveRecord> {
        self.file.conclaves.iter().find(|m| m.id == id)
    }

    /// Create a conclave with an empty message log, returning its id.
    ///
    /// `convener` is the convener's account id and `trustees` is the initial
    /// roster (the convener as joined, each invitee as invited).
    pub fn create_conclave(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        name: &str,
        convener: &str,
        trustees: &[ConclaveTrustee],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        let record = ConclaveRecord {
            id: id.clone(),
            name: name.to_string(),
            trustees: trustees.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
            ended: false,
            convener: convener.to_string(),
        };
        self.file.conclaves.push(record);
        self.save(master)?;
        Ok(id)
    }

    /// Create a conclave under an id chosen elsewhere (the id an invite carried),
    /// returning whether it was created. Used when accepting an invite, so both
    /// peers key the same conclave.
    pub fn create_conclave_with_id(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        name: &str,
        convener: &str,
        trustees: &[ConclaveTrustee],
    ) -> Result<bool, Error> {
        if self.file.conclaves.iter().any(|m| m.id == id) {
            return Ok(false);
        }
        let record = ConclaveRecord {
            id: id.to_string(),
            name: name.to_string(),
            trustees: trustees.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
            ended: false,
            convener: convener.to_string(),
        };
        self.file.conclaves.push(record);
        self.save(master)?;
        Ok(true)
    }

    /// Apply a change to one conclave in place, then persist it. Returns `false`
    /// when the id is unknown, so a caller can tell "no such conclave" from
    /// "written".
    pub fn update_conclave<F>(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        f: F,
    ) -> Result<bool, Error>
    where
        F: FnOnce(&mut ConclaveRecord),
    {
        let Some(record) = self.file.conclaves.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        f(record);
        self.save(master)?;
        Ok(true)
    }

    /// Fill a legacy record's trustees from its nickname list, resolving each
    /// nickname to an account id with `resolve`. Returns whether anything
    /// changed.
    ///
    /// A record written before consent existed has nicknames only; without this
    /// it would have no trustees and every membership operation would see an
    /// empty set. A nickname that cannot be resolved keeps its nickname as the
    /// id, so it is still one stable entry. The convener is recorded as joined.
    pub fn hydrate_trustees<F>(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        convener: &str,
        mut resolve: F,
    ) -> Result<bool, Error>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let Some(record) = self.file.conclaves.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        if record.trustees.is_empty() || record.trustees.iter().all(|t| !t.account_id.is_empty()) {
            return Ok(false);
        }
        let trustees: Vec<ConclaveTrustee> = record
            .trustees
            .iter()
            .map(|t| {
                // A pre-rename record stored a bare nickname, which decodes with
                // an empty account id; resolve it through contacts. A structured
                // entry already carries its account id.
                if t.account_id.is_empty() {
                    ConclaveTrustee {
                        account_id: resolve(&t.nick).unwrap_or_else(|| t.nick.clone()),
                        nick: t.nick.clone(),
                        state: TrusteeState::Joined,
                    }
                } else {
                    t.clone()
                }
            })
            .collect();
        record.trustees = trustees;
        if record.convener.is_empty() {
            record.convener = convener.to_string();
        }
        self.save(master)?;
        Ok(true)
    }

    /// Append one message to a conclave. Returns `false` when the conclave is
    /// unknown, so a caller can tell "no such conclave" from "written".
    pub fn append_conclave_message(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
        outgoing: bool,
        from: Option<&str>,
        body: &str,
    ) -> Result<bool, Error> {
        let Some(conclave) = self.file.conclaves.iter_mut().find(|m| m.id == conclave_id) else {
            return Ok(false);
        };
        conclave.messages.push(ConclaveMessage {
            id: Uuid::new_v4().to_string(),
            outgoing,
            from: from.map(|f| f.to_string()),
            body: body.to_string(),
            ts_ms: now_ms(),
        });
        self.save(master)?;
        Ok(true)
    }

    /// Mark a conclave as ended. Returns `false` when the id is unknown or it was
    /// already ended.
    ///
    /// The record and its message log stay in the encrypted log; the flag makes
    /// the conclave read-only on every peer it is broadcast to.
    pub fn end_conclave(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
    ) -> Result<bool, Error> {
        let Some(conclave) = self.file.conclaves.iter_mut().find(|m| m.id == conclave_id) else {
            return Ok(false);
        };
        if conclave.ended {
            return Ok(false);
        }
        conclave.ended = true;
        self.save(master)?;
        Ok(true)
    }

    /// Mail records only, in log order. The Mail rail filters the same encrypted
    /// log the chat rail reads rather than keeping a second store.
    pub fn mail_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Mail { .. }))
    }

    /// Drafts only, newest first.
    pub fn draft_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Draft { .. }))
    }

    /// Everything the chat rail renders. Drafts live in this log for the
    /// encryption and backup, but they are not chat history.
    pub fn chat_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| !matches!(r.payload, ChatRecordPayload::Draft { .. }))
    }

    fn push(&mut self, master: &[u8; 32], record: ChatRecord) -> Result<(), Error> {
        self.file.messages.push(record);
        self.save(master)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_record_survives_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("hush-mail-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        log.append_mail(
            &master,
            "alice",
            true,
            "t-1",
            Some("hi"),
            "body",
            &[MailAttachmentRecord {
                name: "a.txt".into(),
                blob_id_hex: "ab".into(),
                size: 3,
            }],
            MailState::Sent,
        )
        .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        assert_eq!(reloaded.mail_records().count(), 1);
        assert_eq!(reloaded.chat_records().count(), 1);
    }

    #[test]
    fn a_queued_message_keeps_its_state_across_a_reload() {
        let dir = std::env::temp_dir().join(format!("hush-queued-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .append_mail(&master, "alice", true, "t-1", None, "waiting", &[], MailState::Queued)
            .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let record = reloaded.mail_records().next().unwrap();
        assert_eq!(record.id, id);
        assert!(matches!(
            record.payload,
            ChatRecordPayload::Mail {
                state: MailState::Queued,
                ..
            }
        ));
    }

    #[test]
    fn a_record_written_before_the_outbox_decodes_as_sent() {
        // The wire shape from before `state` existed. It must keep loading, and
        // it must mean Sent rather than Queued.
        let json = r#"{"messages":[{"id":"r1","contact_nick":"alice","outgoing":true,"ts_ms":1,
            "payload":{"t":"mail","thread_id":"t-1","subject":null,"body":"old","attachments":[]}}]}"#;
        let file: ChatLogFile = serde_json::from_str(json).unwrap();
        assert!(matches!(
            &file.messages[0].payload,
            ChatRecordPayload::Mail {
                state: MailState::Sent,
                ..
            }
        ));
    }

    #[test]
    fn a_draft_is_editable_and_deletable() {
        let dir = std::env::temp_dir().join(format!("hush-draft-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .append_draft(&master, None, "alice", Some("hi"), "first", &[])
            .unwrap();
        assert_eq!(log.draft_records().count(), 1);

        let replaced = log
            .replace_payload(
                &master,
                &id,
                ChatRecordPayload::Draft {
                    thread_id: None,
                    to: "alice".into(),
                    subject: Some("hi".into()),
                    body: "second".into(),
                    attachments: vec![],
                    updated_ms: 5,
                },
                5,
            )
            .unwrap();
        assert!(replaced);

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        match &reloaded.draft_records().next().unwrap().payload {
            ChatRecordPayload::Draft { body, .. } => assert_eq!(body, "second"),
            other => panic!("expected a draft, got {other:?}"),
        }

        let mut log = reloaded;
        assert!(log.remove_record(&master, &id).unwrap());
        assert_eq!(log.draft_records().count(), 0);
        // Removing an unknown id reports "nothing to do" rather than erroring.
        assert!(!log.remove_record(&master, &id).unwrap());
    }

    #[test]
    fn a_draft_is_not_chat_history() {
        let dir = std::env::temp_dir().join(format!("hush-draft-chat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        log.append_text(&master, "alice", true, "hello").unwrap();
        log.append_draft(&master, None, "alice", None, "unsent", &[])
            .unwrap();

        assert_eq!(log.chat_records().count(), 1);
        assert_eq!(log.draft_records().count(), 1);
        // Both live in the one log; only the chat projection hides the draft.
        assert_eq!(log.chat_records().count() + log.draft_records().count(), 2);
    }

    fn trustee(account_id: &str, nick: &str, state: TrusteeState) -> ConclaveTrustee {
        ConclaveTrustee {
            account_id: account_id.to_string(),
            nick: nick.to_string(),
            state,
        }
    }

    #[test]
    fn a_conclave_and_its_messages_survive_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Standup",
                "self",
                &[
                    trustee("self", "", TrusteeState::Joined),
                    trustee("bob-id", "bob", TrusteeState::Invited),
                    trustee("carol-id", "carol", TrusteeState::Invited),
                ],
            )
            .unwrap();
        log.append_conclave_message(&master, &id, true, None, "morning")
            .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let conclave = reloaded.conclave(&id).expect("the conclave survives a reload");
        assert_eq!(conclave.name, "Standup");
        assert_eq!(conclave.convener, "self");
        assert_eq!(conclave.trustees.len(), 3);
        assert_eq!(conclave.invited().count(), 2);
        assert_eq!(conclave.messages.len(), 1);
        assert_eq!(conclave.messages[0].body, "morning");
        assert!(conclave.messages[0].outgoing);
        // A conclave is not chat history, so the chat rail cannot show it twice.
        assert_eq!(reloaded.chat_records().count(), 0);
    }

    #[test]
    fn a_conclave_created_under_an_invite_id_keeps_that_id() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-id-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        assert!(
            log.create_conclave_with_id(
                &master,
                "c-1",
                "Trip",
                "convener",
                &[trustee("convener", "alice", TrusteeState::Joined)]
            )
            .unwrap()
        );
        assert!(
            !log.create_conclave_with_id(&master, "c-1", "Again", "convener", &[]).unwrap(),
            "the same id is not created twice"
        );
        assert_eq!(log.conclave("c-1").unwrap().name, "Trip");
    }

    #[test]
    fn conclave_trustee_updates_converge_and_never_revive_a_terminal_state() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-mem-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Trip",
                "alice",
                &[
                    trustee("alice", "alice", TrusteeState::Joined),
                    trustee("bob", "bob", TrusteeState::Invited),
                ],
            )
            .unwrap();

        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Joined));
        })
        .unwrap();
        assert_eq!(log.conclave(&id).unwrap().joined().count(), 2);
        assert_eq!(log.conclave(&id).unwrap().invited().count(), 0);

        // A stale snapshot that still says invited must not downgrade a joined
        // trustee: a peer that had not yet heard bob joined is not authoritative.
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Invited));
        })
        .unwrap();
        assert_eq!(
            log.conclave(&id).unwrap().trustee("bob").unwrap().state,
            TrusteeState::Joined
        );

        // A kick is terminal: a stale snapshot that still says joined must not
        // revive the trustee.
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Kicked));
        })
        .unwrap();
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Joined));
        })
        .unwrap();
        assert_eq!(
            log.conclave(&id).unwrap().trustee("bob").unwrap().state,
            TrusteeState::Kicked
        );
        assert!(!log.conclave(&id).unwrap().is_joined("bob"));
    }

    #[test]
    fn a_legacy_record_hydrates_its_trustees_from_its_nickname_list() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-hydrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        // A record written before consent existed: a bare nickname, which decodes
        // with an empty account id.
        assert!(
            log.create_conclave_with_id(&master, "m-1", "Standup", "", &[])
                .unwrap()
        );
        log.update_conclave(&master, "m-1", |c| {
            c.trustees = vec![trustee("", "bob", TrusteeState::Joined)];
        })
        .unwrap();

        let hydrated = log
            .hydrate_trustees(&master, "m-1", "self", |nick| {
                (nick == "bob").then(|| "bob-id".to_string())
            })
            .unwrap();
        assert!(hydrated);
        let c = log.conclave("m-1").unwrap();
        assert_eq!(c.trustees.len(), 1);
        assert_eq!(c.trustees[0].account_id, "bob-id");
        assert_eq!(c.trustees[0].state, TrusteeState::Joined);
        assert_eq!(c.convener, "self");
        // A second hydration is a no-op.
        assert!(!log.hydrate_trustees(&master, "m-1", "self", |_| None).unwrap());
    }

    #[test]
    fn ending_a_conclave_keeps_its_record_and_log() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-end-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Trip",
                "alice",
                &[
                    trustee("alice", "alice", TrusteeState::Joined),
                    trustee("bob", "bob", TrusteeState::Joined),
                ],
            )
            .unwrap();
        log.append_conclave_message(&master, &id, true, None, "we leave at six")
            .unwrap();

        assert!(log.end_conclave(&master, &id).unwrap());
        assert!(
            !log.end_conclave(&master, &id).unwrap(),
            "ending the same conclave twice is a no-op"
        );

        // A flag, not a delete: the record and its log survive a reload.
        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let record = reloaded.conclave(&id).expect("the record stays in the log");
        assert!(record.ended, "the record is marked ended");
        assert_eq!(record.messages.len(), 1, "the message log is kept");
        assert_eq!(record.messages[0].body, "we leave at six");
    }

    /// A log written by the build that keyed the list as `meetings` must still
    /// load, so the rename cannot make an existing vault look empty or corrupt.
    #[test]
    fn a_log_written_before_the_rename_still_loads() {
        let json = r#"{"messages":[],"meetings":[{"id":"m-1","name":"Standup","members":["bob"],"created_ms":1,"messages":[]}]}"#;
        let file: ChatLogFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.conclaves.len(), 1, "the old `meetings` key still loads");
        assert_eq!(file.conclaves[0].id, "m-1");
        assert!(!file.conclaves[0].ended, "an old record loads as active");
        // The bare nickname decodes into a trustee with an unknown account id,
        // which hydration resolves on first open.
        assert_eq!(file.conclaves[0].trustees.len(), 1);
        assert_eq!(file.conclaves[0].trustees[0].nick, "bob");
        assert_eq!(file.conclaves[0].trustees[0].account_id, "");
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
