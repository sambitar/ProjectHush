use hush_core::Account;

/// Live chat requires the recipient to heartbeat their receive queue.
pub async fn recipient_online(recipient: &Account) {
    recipient.heartbeat_presence().await.unwrap();
}
