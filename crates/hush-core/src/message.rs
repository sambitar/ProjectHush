/// A mail attachment that has been stored in the vault on receipt.
#[derive(Debug, Clone)]
pub struct ReceivedAttachment {
    pub name: String,
    pub blob_id: crate::vault::BlobId,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
    /// Set for received files stored in the vault (`put_blob`).
    pub file_blob_id: Option<crate::vault::BlobId>,
    /// Blob ids for mail attachments, in payload order. Empty for text and file.
    pub mail_attachments: Vec<ReceivedAttachment>,
}

#[derive(Debug, Clone, Default)]
pub struct InboxResult {
    pub messages: Vec<ReceivedMessage>,
    pub warnings: Vec<String>,
}
