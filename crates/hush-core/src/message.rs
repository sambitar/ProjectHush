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
    /// Live chat lines that expire, from contacts.
    ///
    /// Kept out of `messages` for the same reason as `papers`, `voices` and
    /// `signals`: these are not filed in the chat log. The caller owns the
    /// in-memory copy and must drop it at `expires_ms`; a restart or a missed
    /// poll loses it, which is the promise the feature makes.
    pub ephemeral: Vec<ReceivedEphemeral>,
    /// Paper payloads from peers, for the caller to apply to its paper store.
    ///
    /// Kept out of `messages` deliberately: a paper payload is not a chat
    /// message and must never be rendered as one, and the document itself is
    /// the daemon's business (`paper_store`), not the chat log's.
    pub papers: Vec<ReceivedPaper>,
    /// Voice payloads from conclave trustees (Task 23), for the caller to
    /// apply to its voice manager.
    ///
    /// Kept out of `messages` for the same reason as `papers`: a voice payload
    /// is not a chat message. The manager is in-memory — a voice session is
    /// live-only — so these are applied and then gone.
    pub voices: Vec<ReceivedVoice>,
    /// 1:1 call-signalling payloads from contacts (Task 17).
    ///
    /// Taken out of `messages` and consumed by the caller's call state machine;
    /// nothing here is written to the chat log.
    pub calls: Vec<ReceivedCall>,
    /// Trustee signals accepted during this poll, newest last.
    ///
    /// Kept out of `messages` for the same reason as `papers`, `voices` and
    /// `zine_grants`: a signal is not a chat message and must never render as
    /// one. The envelope has already been policy-checked, expired signals have
    /// been dropped, duplicates have been suppressed and the audit log has been
    /// written; the caller owns what the body *means*.
    pub signals: Vec<crate::signals::ReceivedSignal>,
    /// Acknowledgements of signals this vault sent during this poll.
    ///
    /// Carried separately from `signals` because an ack is not a signal kind:
    /// it belongs to the envelope, and the caller uses it to settle whatever
    /// pending state the original send created.
    pub signal_acks: Vec<crate::signals::ReceivedSignalAck>,
    /// Contacts this vault should (re)announce its wallet addresses to because
    /// the handshake just moved: their address was newly stored, or they just
    /// allowed payments.
    ///
    /// The address exchange is otherwise one-shot — whoever enables first
    /// announces, and the other side has nothing to answer with — so a peer can
    /// allow payments and still never receive ours. The daemon answers this list
    /// with an announcement to exactly those contacts, which also makes a lost
    /// announcement self-healing.
    pub wallet_announce_to: Vec<String>,
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
    /// Read keys for zines, handed over by contacts during this poll.
    ///
    /// Kept out of `messages` for the same reason as `papers`: a grant is not a
    /// chat message. The daemon applies each to its zines sidecar and then sweeps
    /// the bulletin for blobs that have just become readable, which is the
    /// epidemic's second half.
    pub zine_grants: Vec<ReceivedZineGrant>,
    pub warnings: Vec<String>,
}

/// A zine read key received from a contact.
///
/// Authority-free by design: any holder of a key may re-share it, so there is
/// nothing to verify and nothing here records who vouched for what. The sender
/// is carried for diagnostics only.
#[derive(Debug, Clone)]
pub struct ReceivedZineGrant {
    pub from_account: String,
    pub from_nick: String,
    /// The post this key opens. One grant, one post.
    pub zine_id: String,
    /// The symmetric read key.
    pub key: Vec<u8>,
    /// Content hash of the newest revision the sender held.
    pub latest_hash: String,
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

/// An expiring chat line received from a contact.
///
/// The payload was consumed at the relay, validated, and deliberately **not**
/// written to the chat log. The daemon hands it to the UI once; if the UI is not
/// there to take it, it is gone.
#[derive(Debug, Clone)]
pub struct ReceivedEphemeral {
    pub from_account: String,
    pub from_nick: String,
    /// The sender's message id, used to key the line on both sides.
    pub id: String,
    pub sent_ms: u64,
    pub expires_ms: u64,
    pub body: String,
}

/// A voice payload received from a conclave trustee (Task 23).
///
/// Kept out of `messages` for the same reason as `papers` and `zine_grants`:
/// a voice payload is not a chat message and must never render as one. The
/// daemon applies each to its in-memory voice manager — the session itself is
/// live-only and stores nothing — so this channel carries the room's truth and
/// the SDP of its handshakes, already authority-checked for identity by
/// `recv`.
#[derive(Debug, Clone)]
pub struct ReceivedVoice {
    /// The sending account's id, for the authority checks.
    pub from_account: String,
    /// The sender's nickname, for diagnostics.
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
}

/// One 1:1 call-signalling payload received from a contact (Task 17).
///
/// Kept out of `messages` for the same reason as `voices`: call signalling is
/// not chat and must never render as a row. It is consumed by the caller's
/// call state machine and never written to the vault.
#[derive(Debug, Clone)]
pub struct ReceivedCall {
    pub from_account: String,
    pub from_nick: String,
    /// One of `CallOffer`, `CallAnswer`, `CallEnd`.
    pub payload: crate::payload::MessagePayload,
}
