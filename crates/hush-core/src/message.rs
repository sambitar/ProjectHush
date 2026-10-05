#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
}

#[derive(Debug, Clone, Default)]
pub struct InboxResult {
    pub messages: Vec<ReceivedMessage>,
    pub warnings: Vec<String>,
}
