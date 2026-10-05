#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
}
