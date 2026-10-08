use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, MASTER_KEY_LEN};
use crate::error::Error;
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

/// One message inside a meeting's own log.
///
/// Meeting messages do not go through [`ChatRecord`]: a meeting is a local
/// construct with its own log, and keeping its messages here is what keeps them
/// out of the chat and mail projections without a filter that could be forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingMessage {
    pub id: String,
    pub outgoing: bool,
    /// The sender's nickname as this vault knows it; `None` for our own messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub body: String,
    pub ts_ms: u64,
}

/// A meeting: a local record, not a relay-side room.
///
/// It holds the id a descriptor is keyed on, the name, the ordered member list
/// (nicknames as this vault knows them), when it was created, and its own message
/// log. Group chat is sender-side fan-out over the members' 1:1 sessions; there is
/// no group key and no server object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingRecord {
    pub id: String,
    pub name: String,
    pub members: Vec<String>,
    pub created_ms: u64,
    #[serde(default)]
    pub messages: Vec<MeetingMessage>,
}

#[derive(Default, Serialize, Deserialize)]
struct ChatLogFile {
    messages: Vec<ChatRecord>,
    /// Meetings this vault holds. Defaulted so a log written before meetings
    /// existed still loads.
    #[serde(default)]
    meetings: Vec<MeetingRecord>,
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

    /// The meetings this vault holds, in creation order.
    pub fn meetings(&self) -> &[MeetingRecord] {
        &self.file.meetings
    }

    pub fn meeting(&self, id: &str) -> Option<&MeetingRecord> {
        self.file.meetings.iter().find(|m| m.id == id)
    }

    /// Create a meeting with an empty message log, returning its id.
    pub fn create_meeting(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        name: &str,
        members: &[String],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.file.meetings.push(MeetingRecord {
            id: id.clone(),
            name: name.to_string(),
            members: members.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
        });
        self.save(master)?;
        Ok(id)
    }

    /// Materialise a meeting from a received descriptor if this vault does not
    /// have it yet. Returns whether it was created.
    ///
    /// A known meeting is left untouched: a descriptor is how a recipient first
    /// learns a meeting exists, not an instruction to overwrite local membership.
    pub fn ensure_meeting(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        name: &str,
        members: &[String],
    ) -> Result<bool, Error> {
        if self.file.meetings.iter().any(|m| m.id == id) {
            return Ok(false);
        }
        self.file.meetings.push(MeetingRecord {
            id: id.to_string(),
            name: name.to_string(),
            members: members.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
        });
        self.save(master)?;
        Ok(true)
    }

    /// Append one message to a meeting. Returns `false` when the meeting is
    /// unknown, so a caller can tell "no such meeting" from "written".
    pub fn append_meeting_message(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        meeting_id: &str,
        outgoing: bool,
        from: Option<&str>,
        body: &str,
    ) -> Result<bool, Error> {
        let Some(meeting) = self.file.meetings.iter_mut().find(|m| m.id == meeting_id) else {
            return Ok(false);
        };
        meeting.messages.push(MeetingMessage {
            id: Uuid::new_v4().to_string(),
            outgoing,
            from: from.map(|f| f.to_string()),
            body: body.to_string(),
            ts_ms: now_ms(),
        });
        self.save(master)?;
        Ok(true)
    }

    /// Add a member nickname to a meeting if it is not already there. Returns
    /// whether anything changed.
    pub fn add_meeting_member(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        meeting_id: &str,
        nickname: &str,
    ) -> Result<bool, Error> {
        let Some(meeting) = self.file.meetings.iter_mut().find(|m| m.id == meeting_id) else {
            return Ok(false);
        };
        if meeting.members.iter().any(|m| m == nickname) {
            return Ok(false);
        }
        meeting.members.push(nickname.to_string());
        self.save(master)?;
        Ok(true)
    }

    /// Remove a member nickname from a meeting. Returns whether anything changed.
    pub fn remove_meeting_member(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        meeting_id: &str,
        nickname: &str,
    ) -> Result<bool, Error> {
        let Some(meeting) = self.file.meetings.iter_mut().find(|m| m.id == meeting_id) else {
            return Ok(false);
        };
        let before = meeting.members.len();
        meeting.members.retain(|m| m != nickname);
        if meeting.members.len() == before {
            return Ok(false);
        }
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

    #[test]
    fn a_meeting_and_its_messages_survive_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("hush-meeting-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_meeting(&master, "Standup", &["bob".into(), "carol".into()])
            .unwrap();
        log.append_meeting_message(&master, &id, true, None, "morning")
            .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let meeting = reloaded.meeting(&id).expect("the meeting survives a reload");
        assert_eq!(meeting.name, "Standup");
        assert_eq!(meeting.members, vec!["bob".to_string(), "carol".to_string()]);
        assert_eq!(meeting.messages.len(), 1);
        assert_eq!(meeting.messages[0].body, "morning");
        assert!(meeting.messages[0].outgoing);
        // A meeting is not chat history, so the chat rail cannot show it twice.
        assert_eq!(reloaded.chat_records().count(), 0);
    }

    #[test]
    fn a_meeting_materialises_once_from_a_descriptor_and_then_only_appends() {
        let dir = std::env::temp_dir().join(format!("hush-meeting-desc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        assert!(
            log.ensure_meeting(&master, "m-1", "Standup", &["bob".into()])
                .unwrap(),
            "an unknown meeting is created"
        );
        assert!(
            !log.ensure_meeting(&master, "m-1", "Renamed", &["bob".into(), "carol".into()])
                .unwrap(),
            "a descriptor for a known meeting must not overwrite local state"
        );
        assert_eq!(log.meeting("m-1").unwrap().name, "Standup");
        assert_eq!(log.meeting("m-1").unwrap().members.len(), 1);

        log.append_meeting_message(&master, "m-1", false, Some("bob"), "hi")
            .unwrap();
        assert_eq!(log.meeting("m-1").unwrap().messages[0].from.as_deref(), Some("bob"));

        // Appending to an unknown meeting reports "nothing to do" rather than
        // inventing a meeting with no name or members.
        assert!(!log
            .append_meeting_message(&master, "missing", false, None, "x")
            .unwrap());
    }

    #[test]
    fn meeting_membership_can_be_changed() {
        let dir = std::env::temp_dir().join(format!("hush-meeting-mem-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log.create_meeting(&master, "Trip", &["bob".into()]).unwrap();
        assert!(log.add_meeting_member(&master, &id, "carol").unwrap());
        assert!(
            !log.add_meeting_member(&master, &id, "carol").unwrap(),
            "adding the same member twice is a no-op"
        );
        assert!(log.remove_meeting_member(&master, &id, "bob").unwrap());
        assert_eq!(log.meeting(&id).unwrap().members, vec!["carol".to_string()]);
        assert!(!log.remove_meeting_member(&master, &id, "nobody").unwrap());
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
