#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
    /// Set for received files stored in the vault (`put_blob`).
    pub file_blob_id: Option<crate::vault::BlobId>,
}

#[derive(Debug, Clone, Default)]
pub struct InboxResult {
    pub messages: Vec<ReceivedMessage>,
    pub warnings: Vec<String>,
}
