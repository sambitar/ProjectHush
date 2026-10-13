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
    /// Paper payloads from peers, for the caller to apply to its paper store.
    ///
    /// Kept out of `messages` deliberately: a paper payload is not a chat
    /// message and must never be rendered as one, and the document itself is
    /// the daemon's business (`paper_store`), not the chat log's.
    pub papers: Vec<ReceivedPaper>,
    /// Conclave ids that gained a joined trustee during this poll.
    ///
    /// The paper hub needs this: a trustee that joined after the paper existed
    /// missed the deltas that came before it, and deltas are never replayed, so
    /// the hub hands the newcomer the whole document. Reported here because
    /// only `recv` sees the membership change.
    pub joined: Vec<String>,
    /// `(conclave id, account id)` for every trustee that stopped being joined
    /// during this poll — a leave or a kick.
    ///
    /// The mirror of `joined`, and the paper hub needs it for the same reason:
    /// a paper whose hub is gone has no rendezvous, so the convener inherits it.
    /// Reported here because only `recv` sees the membership change.
    pub departed: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// A paper payload received from a peer.
///
/// The payload is always one of the paper variants (`PaperState`,
/// `PaperDelta`, `PaperSnapshot`, `PaperPresence`, `PaperHub`); it is carried
/// as the enum so the caller can match on which one, and so a future variant
/// needs no change here.
#[derive(Debug, Clone)]
pub struct ReceivedPaper {
    /// The sending account's id, for the creator/convener checks.
    pub from_account: String,
    /// The sender's nickname, for diagnostics.
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
}
