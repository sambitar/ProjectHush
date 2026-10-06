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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatRecordPayload {
    Text { body: String },
    File {
        name: String,
        blob_id_hex: String,
        size: u64,
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

    fn push(&mut self, master: &[u8; 32], record: ChatRecord) -> Result<(), Error> {
        self.file.messages.push(record);
        self.save(master)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
