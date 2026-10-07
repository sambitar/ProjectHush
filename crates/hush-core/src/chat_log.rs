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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatRecordPayload {
    Text { body: String },
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
    },
}

#[derive(Default, Serialize, Deserialize)]
struct ChatLogFile {
    messages: Vec<ChatRecord>,
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

    pub fn messages(&self) -> &[ChatRecord] {
        &self.file.messages
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
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Mail {
                    thread_id: thread_id.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                },
            },
        )
    }

    /// Mail records only, in log order. The Mail rail filters the same encrypted
    /// log the chat rail reads rather than keeping a second store.
    pub fn mail_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Mail { .. }))
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
        )
        .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        assert_eq!(reloaded.mail_records().count(), 1);
        assert_eq!(reloaded.messages().len(), 1);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
