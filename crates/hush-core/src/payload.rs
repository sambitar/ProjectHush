use serde::{Deserialize, Serialize};

/// Max file bytes inlined in one Signal message (relay cap is 1 MiB total wire).
pub const MAX_INLINE_FILE_BYTES: usize = 256 * 1024;

/// Max attachments in one mail message. The total byte cap is
/// `MAX_INLINE_FILE_BYTES`, shared across all of them.
pub const MAX_MAIL_ATTACHMENTS: usize = 8;

/// The largest file a chunked transfer may carry in v1.
///
/// Settled 2026-10-21: 25 MiB over a per-transfer relay queue. Larger files
/// belong on a user-supplied replica, not the relay.
pub const MAX_FILE_TRANSFER_BYTES: u64 = 25 * 1024 * 1024;

/// Plaintext bytes per transfer chunk.
///
/// The relay rejects a queue blob over 1 MiB, and the AEAD tag/nonce add
/// overhead, so 512 KiB leaves generous headroom while keeping the chunk count
/// (and therefore the manifest) small.
pub const MAX_FILE_CHUNK_BYTES: u32 = 512 * 1024;

/// A hard bound on a manifest's chunk list, independent of the size limit.
pub const MAX_FILE_CHUNKS: usize = 64;

/// A transfer file name is a display label, not a path.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// The largest body one expiring chat message may carry.
///
/// Expiring messages are live-only and never stored, so the cap is only there
/// to keep one message from becoming a file transfer. It matches the sideband's
/// 16 KiB envelope rather than the 1 MiB relay blob cap.
pub const MAX_EXPIRE_TEXT_BYTES: usize = 16 * 1024;

/// The shortest lifetime an expiring message may be given.
///
/// A message with no time to arrive is not a message; the daemon enforces this
/// so a forged or buggy sender cannot make one expire before the next poll.
pub const MIN_EXPIRE_TTL_MS: u64 = 1_000;

/// The longest lifetime an expiring message may be given: 24 hours.
///
/// Past this the feature stops meaning "expiring" and starts meaning "saved
/// with a cleanup timer", which is exactly what it promises not to be.
pub const MAX_EXPIRE_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

/// One attachment on the wire: a display name and its bytes, base64 in JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailAttachment {
    pub name: String,
    #[serde(with = "b64_bytes")]
    pub data: Vec<u8>,
}

/// One file attached to a conclave message, inlined base64 like the 1:1 `File`
/// payload. The bytes are encrypted separately to each joined trustee, so a
/// fan-out of N trustees puts N copies on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConclaveFile {
    pub name: String,
    #[serde(with = "b64_bytes")]
    pub data: Vec<u8>,
}

/// One chunk in a `FileOffer` manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkRef {
    /// Zero-based position. The manifest is rejected unless these are contiguous.
    pub index: u32,
    /// SHA-256 of the plaintext chunk, lower-case hex.
    pub sha256: String,
    /// Plaintext length. The last chunk may be shorter than the chunk size.
    pub size: u32,
}

/// A large-file offer: the bytes live in a per-transfer relay queue, sealed
/// under `file_key`, and this manifest only describes them.
///
/// The manifest rides inside the existing Signal session, so the relay never
/// sees the name, the key, or the chunk hashes. The recipient validates every
/// field before allocating anything: see [`FileOffer::validate`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileOffer {
    /// Random id chosen by the sender; also the ack correlation key.
    pub transfer_id: String,
    /// The relay queue holding the ciphertext chunks (`xfer-…`).
    pub queue_id: String,
    /// The capability the recipient uses to drain the transfer queue. It
    /// travels inside the Signal session, never to the relay.
    pub read_token: String,
    /// Display name; never a path.
    pub name: String,
    /// Total plaintext size in bytes.
    pub size: u64,
    /// Plaintext bytes per chunk, except possibly the last.
    pub chunk_size: u32,
    /// One entry per chunk, in order.
    pub chunks: Vec<ChunkRef>,
    /// SHA-256 of the whole plaintext, lower-case hex.
    pub whole_sha256: String,
    /// Random per-file key, 32 bytes, base64 in JSON.
    #[serde(with = "b64_bytes")]
    pub file_key: Vec<u8>,
}

impl FileOffer {
    /// Reject a manifest that is malformed, oversized, or inconsistent before
    /// the receiver allocates or fetches anything.
    pub fn validate(&self) -> Result<(), crate::error::Error> {
        if !safe_id(&self.transfer_id, 64) || !safe_id(&self.queue_id, 128) {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if self.read_token.is_empty()
            || self.read_token.len() > 256
            || !self.read_token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if !safe_file_name(&self.name) {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if self.size == 0 || self.size > MAX_FILE_TRANSFER_BYTES {
            return Err(crate::error::Error::FileTooLarge(
                MAX_FILE_TRANSFER_BYTES as usize,
            ));
        }
        if self.chunk_size == 0 || self.chunk_size > MAX_FILE_CHUNK_BYTES {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if self.chunks.is_empty() || self.chunks.len() > MAX_FILE_CHUNKS {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if self.file_key.len() != 32 {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        if !valid_sha256(&self.whole_sha256) {
            return Err(crate::error::Error::InvalidFileOffer);
        }

        let mut total: u64 = 0;
        for (position, chunk) in self.chunks.iter().enumerate() {
            if chunk.index != position as u32
                || chunk.size == 0
                || chunk.size > self.chunk_size
                || !valid_sha256(&chunk.sha256)
            {
                return Err(crate::error::Error::InvalidFileOffer);
            }
            total = total.saturating_add(u64::from(chunk.size));
        }
        if total != self.size {
            return Err(crate::error::Error::InvalidFileOffer);
        }

        let expected = (self.size + u64::from(self.chunk_size) - 1) / u64::from(self.chunk_size);
        if expected != self.chunks.len() as u64 {
            return Err(crate::error::Error::InvalidFileOffer);
        }
        Ok(())
    }
}

fn safe_id(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A trustee's standing in a conclave.
///
/// The state is per trustee and converges across peers: every change is
/// broadcast to the joined trustees, and each member's own vault is the only
/// place the state is stored (there is no relay-side group object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrusteeState {
    /// Invited but has not accepted. Receives the invite and nothing else.
    Invited,
    /// In the conclave and receiving its messages.
    #[default]
    Joined,
    /// Was a trustee and left voluntarily.
    Left,
    /// Removed by the convener.
    Kicked,
}

/// How far a trustee may reach this vault (Task 24).
///
/// A relationship has two of these — one chosen by each side — and a surface is
/// usable only when both sides allow it, so the effective reach is the narrower
/// of the two (see [`TrusteeReach::narrow`]). `Full` is the default, so a
/// relationship that predates this setting, or a peer that has not announced
/// one, behaves exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrusteeReach {
    /// Mail only: mail both ways, but no live 1:1 chat and no conclaves.
    MailOnly,
    /// Everything the capsule offers: mail, 1:1 chat and conclaves.
    #[default]
    Full,
}

impl TrusteeReach {
    /// The narrower of two reaches.
    ///
    /// A surface is usable only if both sides allow it, so `MailOnly` on either
    /// side makes the whole relationship mail-only. This is the consent-shaped
    /// default: neither side can force a surface the other excluded.
    pub fn narrow(self, other: TrusteeReach) -> TrusteeReach {
        if self == TrusteeReach::MailOnly || other == TrusteeReach::MailOnly {
            TrusteeReach::MailOnly
        } else {
            TrusteeReach::Full
        }
    }

    /// Whether the live surfaces — 1:1 chat and conclaves — are allowed. Mail is
    /// always allowed, so only `Full` opens them.
    pub fn allows_live(self) -> bool {
        self == TrusteeReach::Full
    }
}

/// One trustee in a conclave descriptor.
///
/// `account_id` is the stable identity: it is what lets a membership change
/// converge across vaults, where a nickname would not (two peers can call the
/// same person different things). `nick` is the sender's display hint and is
/// never authoritative.
///
/// A trustee written before this rename was a bare nickname string in the
/// record's `members` list; the custom `Deserialize` below still reads that
/// shape, so an existing vault loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConclaveTrustee {
    /// Stable identity: the trustee's account id (hex of the identity key).
    pub account_id: String,
    /// The sender's nickname for this trustee; a display hint only.
    pub nick: String,
    pub state: TrusteeState,
}

impl<'de> Deserialize<'de> for ConclaveTrustee {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            /// The pre-rename shape: `members: ["bob"]`.
            Nick(String),
            Full {
                #[serde(default)]
                account_id: String,
                #[serde(default)]
                nick: String,
                #[serde(default)]
                state: TrusteeState,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Nick(nick) => ConclaveTrustee {
                account_id: String::new(),
                nick,
                state: TrusteeState::Joined,
            },
            Repr::Full {
                account_id,
                nick,
                state,
            } => ConclaveTrustee {
                account_id,
                nick,
                state,
            },
        })
    }
}

/// The membership descriptor carried by every conclave control payload.
///
/// It is the whole of a conclave's shared state: the name, the convener (the
/// only trustee who may invite or kick, and the only one who may end), the
/// trustees with per-member state, and whether the conclave has ended. It rides
/// on invite, accept, leave, update and end so the trustee list converges on
/// every peer without a server object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveDescriptor {
    pub conclave_id: String,
    pub name: String,
    /// The convener's account id. Only the convener may invite, kick, or end.
    #[serde(default)]
    pub convener: String,
    /// The trustees. `alias` reads the pre-rename `members` field name.
    #[serde(default, alias = "members")]
    pub trustees: Vec<ConclaveTrustee>,
    #[serde(default)]
    pub ended: bool,
}

/// The descriptor carried by every paper payload.
///
/// One paper per conclave. This is both the wire shape and the at-rest record
/// (the daemon's paper sidecar stores it), so a paper is defined once — the
/// same arrangement as [`ConclaveDescriptor`], and the reason there is not a
/// second, drifting copy in the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperDescriptor {
    /// Random id. Never a name: the relay sees opaque ciphertext only.
    pub paper_id: String,
    /// The conclave this paper belongs to.
    pub conclave_id: String,
    /// Account id of the trustee who created the paper.
    pub creator: String,
    /// Account id of the current hub — the DataChannel rendezvous, not
    /// authority over content (a CRDT merges anywhere). It is the convener
    /// whose word settles a `PaperHub` claim.
    pub hub: String,
    /// Display title. Absent from the first wire shape, so it defaults rather
    /// than failing to decode.
    #[serde(default = "default_paper_title")]
    pub title: String,
    pub created_ms: u64,
}

/// The title a paper falls back to when its creator did not give one. Single
/// source: the wire default and the daemon's create path both read it, so the
/// two cannot drift.
pub const DEFAULT_PAPER_TITLE: &str = "Paper";

/// The descriptor carried by every voice payload (Task 23).
///
/// Unlike a paper's descriptor this is **not** an at-rest record — a voice
/// session is live-only and stores nothing — so it exists for the wire and the
/// in-memory state machine, and it is self-contained (conclave id and opener
/// ride with it) so a receiver can check authority without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceDescriptor {
    /// Random id, generated by the opener. Never a name: the relay sees
    /// opaque ciphertext only.
    pub session_id: String,
    /// The conclave this voice session belongs to. Carried so authority is
    /// checkable on receipt: only a joined trustee of *this* conclave may open.
    pub conclave_id: String,
    /// Account id of the trustee who opened the session. The opener's leaving
    /// ends the session for everyone — the one authority voice carries.
    pub opener: String,
    /// When the session was opened, for display only (no persistence).
    pub started_ms: u64,
}

fn default_paper_title() -> String {
    DEFAULT_PAPER_TITLE.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum MessagePayload {
    Text {
        body: String,
    },
    /// A chat line that is deleted from both sides at `expires_ms`.
    ///
    /// Unlike [`MessagePayload::Text`], this payload is **never** written to
    /// either vault: `recv` hands it to the caller out of band and `send`
    /// records no chat row. It is live-only for the same reason chat is, so an
    /// away peer never has a copy parked on the relay waiting to be delivered
    /// after its deadline. The id is the sender's, so both sides and a retried
    /// delivery can name the same line.
    ExpireText {
        /// Random id chosen by the sender: UI key and duplicate suppression.
        id: String,
        /// When the sender produced it, Unix ms. Advisory only.
        sent_ms: u64,
        /// When both sides delete it, Unix ms.
        expires_ms: u64,
        body: String,
    },
    File {
        name: String,
        #[serde(with = "b64_bytes")]
        data: Vec<u8>,
    },
    /// A large file offered by reference. The bytes are in a per-transfer relay
    /// queue, sealed with `file_key`; this payload only carries the manifest.
    ///
    /// A build that does not know this tag decodes it as `Unknown` and ignores
    /// it. Senders must check the contact's `supports_file_transfer` capability
    /// first; they must not assume every peer can receive this.
    FileOffer {
        offer: FileOffer,
    },
    /// The recipient has verified and stored a `FileOffer`; the sender may
    /// burn the transfer queue.
    FileReceived {
        transfer_id: String,
    },
    /// A 1:1 contact request: the sender wants to connect and has already
    /// consumed the recipient's invite.
    ///
    /// This is the one payload an account that is not yet a contact may send.
    /// It carries the sender's own standing invite, so the recipient never has
    /// to obtain it out of band: accepting stores the sender and answers over
    /// the session this first message opened. The relay still learns nothing —
    /// the invite rides inside the Signal session like any chat line.
    ContactRequest {
        /// The sender's standing invite (pointer or full v2). The recipient
        /// uses it only on accept, and never consumes its prekey bundle: the
        /// session was already built by this message.
        invite: String,
        /// How the sender would like to be named on the recipient's device.
        /// A hint, not identity.
        display_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// Whether the sender's build can receive a chunked `FileOffer`.
        #[serde(default)]
        supports_file_transfer: bool,
    },
    /// The recipient accepted a [`Self::ContactRequest`]. The sender now stores
    /// the contact and grants its per-contact queue.
    ContactAccept,
    /// The recipient declined a [`Self::ContactRequest`]. The sender keeps the
    /// account id for routing but creates no contact.
    ContactDecline,
    /// An internal mail message. Distinct from `Text`/`File` so the Mail rail can
    /// render threads, subjects and multiple attachments while riding the same
    /// encrypted session and relay queue.
    Mail {
        thread_id: String,
        #[serde(default)]
        subject: Option<String>,
        body: String,
        #[serde(default)]
        attachments: Vec<MailAttachment>,
    },
    /// A message in a local conclave, fanned out to each member over their own
    /// 1:1 session.
    ///
    /// There is no relay-side room and no group object: the sender encrypts this
    /// once per member and puts N blobs. The descriptor (`conclave_id`, `name`,
    /// `members`) travels with every message so a recipient that has never seen
    /// the conclave can materialise it locally; once the conclave exists, later
    /// messages just append. Membership is the sender's local view, so the names
    /// carried here are the sender's nicknames and may not match the recipient's.
    ///
    /// The wire tag and the id field are renamed from the original `meeting` /
    /// `meeting_id`. `alias` keeps every already-queued or already-sent message
    /// decodable, so the rename does not strand anything on the relay.
    #[serde(alias = "meeting")]
    Conclave {
        #[serde(alias = "meeting_id")]
        conclave_id: String,
        name: String,
        /// The sender's trustee nicknames. `alias` reads the pre-rename
        /// `members` field so an already-queued message still decodes.
        #[serde(default, alias = "members")]
        trustees: Vec<String>,
        body: String,
        /// A file shared into the conclave, absent for a text message.
        ///
        /// This is an optional field on the existing payload rather than a new
        /// variant: a file is just a conclave message with an attachment, so it
        /// rides the same descriptor and fan-out, and a message written before
        /// files existed still decodes (the field defaults to `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<ConclaveFile>,
    },
    /// An invitation to join a conclave.
    ///
    /// This replaces the old silent materialisation: a message for a conclave a
    /// vault has never seen no longer creates it. The invite is stored as a
    /// pending record the recipient must explicitly accept or decline, and only
    /// on acceptance does the conclave exist locally. Sent by the convener only.
    ConclaveInvite {
        descriptor: ConclaveDescriptor,
    },
    /// An acknowledgement that an invited trustee joined.
    ///
    /// Sent to the convener, who re-broadcasts the authoritative trustee list so
    /// the convener's record distinguishes invited from joined on every peer.
    ConclaveAccept {
        descriptor: ConclaveDescriptor,
    },
    /// A declined invitation.
    ///
    /// Deliberately silent to the convener beyond the state change: no further
    /// notification is sent and the trustee is not carried to anyone else.
    ConclaveDecline {
        conclave_id: String,
        /// The account id of the trustee who declined.
        member: String,
    },
    /// A trustee left the conclave voluntarily.
    ///
    /// Sent by the leaver to the convener, who applies it and re-broadcasts the
    /// trustee list, so every joined trustee converges from one authority rather
    /// than a peer-to-peer flood.
    ConclaveLeave {
        descriptor: ConclaveDescriptor,
    },
    /// The convener's authoritative trustee snapshot, sent to every joined
    /// trustee.
    ///
    /// This is the single convergence path: invites, joins, kicks, and leaves all
    /// reach the other trustees as a convener-originated update, so there is
    /// exactly one writer whose view everyone else adopts. Receivers accept it
    /// only from the convener named in the descriptor.
    ConclaveUpdate {
        descriptor: ConclaveDescriptor,
    },
    /// The convener ended the conclave for everyone.
    ///
    /// The record and its message log stay in every vault; the descriptor's
    /// `ended` flag makes it read-only on every peer rather than only on the
    /// convener's.
    ConclaveEnd {
        descriptor: ConclaveDescriptor,
    },
    /// A request for an introduction inside a conclave.
    ///
    /// Two joined trustees who are not each other's contacts cannot exchange
    /// anything directly — conclave fan-out only rides pairwise 1:1 sessions —
    /// so the mesh is completed by a member who knows them both. The `seeker`
    /// wants the `target`'s invite; whoever receives this request either *is*
    /// the target (consent is asked) or a member with a direct channel to the
    /// target (the courier, who relays it on the seeker's behalf).
    ///
    /// Like every conclave control payload it rides an existing 1:1 session,
    /// so the sender is authenticated by the session itself; receivers still
    /// verify the claim matches the sender (see `Account::receive_intro_req`).
    ConclaveIntroReq {
        conclave_id: String,
        /// The conclave's name, so prompts can say where the request came from.
        name: String,
        /// The account that wants to meet the target.
        seeker: String,
        /// The seeker's nickname as the sender knows it. Display hint only:
        /// the sender's nicknames need not match the receiver's.
        seeker_nick: String,
        /// The account whose invite is wanted.
        target: String,
        /// The target's nickname as the sender knows it.
        target_nick: String,
    },
    /// An introduction consented to by its owner: their standing invite,
    /// addressed to one seeker, carried by a member who knows them both.
    ///
    /// The invite is the same signed capability an out-of-band swap would use;
    /// the conclave merely replaces the meeting place. The `target` field must
    /// equal the sender's account id — an invite is only ever delivered in the
    /// name of the account that sent it — and `seeker` names the one member
    /// allowed to consume it.
    ConclaveIntro {
        conclave_id: String,
        name: String,
        /// The account the invite belongs to; must equal the sender.
        target: String,
        /// The target's nickname as the sender knows it.
        target_nick: String,
        /// The one member this introduction is addressed to.
        seeker: String,
        /// The target's standing invite (`Account::invite`), minted with their
        /// explicit consent.
        invite: String,
        /// True when this card is the acceptance half: the sender already
        /// consumed the receiver's invite and opened their session, so the
        /// receiver must add the sender as a contact *without* consuming the
        /// attached bundle. Both sides consuming would leave each holding an
        /// outbound session that shadows the other's first prekey message,
        /// and every message in both directions would be undecryptable.
        #[serde(default)]
        reverse: bool,
    },
    /// A grant of the sender's per-contact receive queue.
    ///
    /// Delivered into the recipient's inbox (the grant channel) and encrypted by
    /// the same libsignal session as any other payload, so the relay cannot read
    /// which account is granting to whom. The signature is what the recipient
    /// actually trusts: it is verified against the identity key the sender
    /// pinned in their invite, so an unauthenticated write to the grant channel
    /// cannot introduce a queue the sender does not own. See
    /// `Account::apply_queue_grant`.
    QueueGrant {
        /// The queue id the recipient should deliver this sender's account into.
        queue_id: String,
        /// The delivery token for that queue. Public by design: it authorizes
        /// putting a message in the queue and nothing else.
        delivery_token: String,
        /// Signature over the canonical grant bytes by the granting account's
        /// identity key.
        signature: Vec<u8>,
    },
    /// The sender's reach preference for the recipient (Task 24).
    ///
    /// A consent announcement, not authority: it says which surfaces the sender
    /// will use with the recipient, so the recipient's UI can mirror it and the
    /// pair never dead-ends on a chat or a conclave the sender would refuse. It
    /// only ever narrows — the receiver's own choice is never widened by it —
    /// and it is applied only from a known contact, because the session is the
    /// authentication. Forged, the worst it can do is hide a surface the sender
    /// was not going to use anyway, so there is no signature.
    Reach {
        scope: TrusteeReach,
    },
    /// A paper's descriptor, sent when a paper is created or adopted.
    ///
    /// Creator-authoritative at creation: the creator names themselves as hub.
    /// Later hub changes are convener-authoritative and ride `PaperHub`.
    PaperState {
        paper: PaperDescriptor,
    },
    /// One opaque yrs update.
    ///
    /// Authority-free by design: a CRDT merges an update anywhere, in any
    /// order, so a receiver applies this without checking who sent it.
    PaperDelta {
        paper_id: String,
        #[serde(with = "b64_bytes")]
        update: Vec<u8>,
    },
    /// The whole document, for a trustee that has just joined or a peer too far
    /// behind for replaying deltas to be worth it. Sent by the hub.
    ///
    /// The descriptor travels with the state because the peer that needs this
    /// is exactly the one that never saw `PaperState`: without the descriptor
    /// the payload names a paper this vault has no record of, so it could not
    /// be placed in a conclave even though its state merges perfectly well.
    PaperSnapshot {
        paper: PaperDescriptor,
        #[serde(with = "b64_bytes")]
        state: Vec<u8>,
    },
    /// Coarse presence over the slow path.
    ///
    /// v1 sends presence on the DataChannel only; the variant is reserved so the
    /// relay path can carry a coarse version later without a format change.
    PaperPresence {
        paper_id: String,
        peer: String,
        #[serde(with = "b64_bytes")]
        state: Vec<u8>,
    },
    /// Hub succession, or the convener explicitly re-homing a paper.
    ///
    /// Accepted only when the sender is the convener of `paper.conclave_id`,
    /// which is why the descriptor has to carry that id: the check is
    /// `sender == conclave(paper.conclave_id).convener`.
    PaperHub {
        paper: PaperDescriptor,
    },
    /// One half of a paper's DataChannel handshake, sent by the peer that
    /// offers.
    ///
    /// Signalling rides the existing 1:1 session rather than a channel of its
    /// own: the relay already delivers it reliably and in order, and it is the
    /// only path that exists before the DataChannel does. Only meaningful for a
    /// paper both ends already hold — a hub ignores an offer naming a paper it
    /// does not have — so no descriptor travels with it.
    ///
    /// `sdp` is the connection description as the JSON both WebRTC stacks
    /// already agree on, which keeps this module free of any WebRTC dependency.
    /// It carries the gathered ICE candidates, so there is deliberately no
    /// separate candidate variant: the handshake is one round trip, and the
    /// relay path is already live while it happens, so waiting for gathering to
    /// finish costs the user nothing.
    PaperOffer {
        paper_id: String,
        sdp: String,
    },
    /// The hub's reply to `PaperOffer`.
    ///
    /// Sent only by the paper's hub, and only to the peer that offered: the
    /// topology is a star, so a peer never answers another peer.
    PaperAnswer {
        paper_id: String,
        sdp: String,
    },
    /// A trustee opened a voice room in this conclave (Task 23).
    ///
    /// Authority-checked on receipt against the conclave roster the receiver
    /// already holds: only a joined trustee may open. A room is joined
    /// explicitly per trustee — joining is the consent, so there is no ring.
    VoiceOpen {
        session: VoiceDescriptor,
    },
    /// One half of a voice session's WebRTC handshake, sent by the trustee that
    /// joins, to each trustee already in the room (the mesh has no hub to offer
    /// to).
    ///
    /// Same shape and same reasoning as [`MessagePayload::PaperOffer`]: the SDP
    /// is the JSON both WebRTC stacks agree on, it carries the gathered ICE
    /// candidates so the handshake is one round trip, and there is deliberately
    /// no separate candidate variant unless renegotiation proves to need one.
    /// Meaningful only for a session the sender has seen `VoiceOpen` for.
    VoiceOffer {
        session_id: String,
        sdp: String,
    },
    /// The reply to a `VoiceOffer`, from a trustee already in the room.
    ///
    /// One per joining pair, never re-broadcast: a mesh peer answers only the
    /// peer that offered.
    VoiceAnswer {
        session_id: String,
        sdp: String,
    },
    /// Local capture muted or unmuted.
    ///
    /// Advisory only: the sender stops capturing *before* the flag is sent, so
    /// silence is the truth and this is the label. Applied only for
    /// `who == sender` — you cannot mute someone else. Low-frequency control
    /// traffic (a user action, not voice activity), which is why it rides the
    /// relay cadence while speaking indicators stay local.
    VoiceMute {
        session_id: String,
        who: String,
        muted: bool,
    },
    /// A trustee left the voice session.
    ///
    /// Applied only for `who == sender`. When the sender is the session's
    /// opener this is the end of the session for everyone (the opener may end
    /// it for all); otherwise the room continues, and ends by itself when fewer
    /// than two members remain.
    VoiceLeave {
        session_id: String,
        /// The account id of the trustee who left.
        who: String,
    },
    /// A zine's read key, handed to one recipient over their existing session.
    ///
    /// This is the key epidemic in one payload: delivery of a zine is broadcast
    /// on the bulletin, but readability is passed hand to hand, because there is
    /// no "all members" key to encrypt a post to a stranger with. The recipient
    /// learns a post exists and can then find its blob on the bulletin by trying
    /// the key.
    ///
    /// Authority-free on receipt, unlike `QueueGrant`: any holder of a read key
    /// may re-share it to their own contacts, and a grant confers read, never
    /// write. So there is nothing to verify and no signature — the only trust is
    /// that the session itself is authenticated to a known contact, and the
    /// worst a forged grant can do is hand over a key the sender already holds.
    /// One grant covers one post, which is the price of a key per post.
    ZineGrant {
        /// The post's id, not the pen name: a grant names the one post it opens.
        zine_id: String,
        /// The post's symmetric read key, so the recipient can open its blob
        /// without trial decryption.
        #[serde(with = "b64_bytes")]
        key: Vec<u8>,
        /// The content hash of the newest revision the sender holds, so the
        /// recipient can pick that blob out of the lane without opening the
        /// chain from the start.
        latest_hash: String,
    },
    /// The minting Place telling a newly adopted Place about the relationship.
    ///
    /// Sent by the Place that scanned the invite (Settings → Add place). The
    /// recipient verifies the vouch chain, adds the sender as a contact from
    /// `invite`, and records a MintedMe place entry. See
    /// `docs/plans/2026-10-19-places-desktop.md`.
    PlaceAdopt {
        /// The minter's standing invite (pointer or legacy).
        invite: String,
        /// Vouch chain ending at the recipient.
        chain: crate::place::VouchChain,
        /// Suggested label stamped at adopt time ("phone", …).
        label: String,
    },
    /// The adopted Place confirming the vouch.
    ///
    /// Sent back by the vault that received [`Self::PlaceAdopt`], carrying the
    /// same chain with that vault's counter-signature on the hop naming it. The
    /// minter cannot produce this signature, so it is the evidence that the
    /// adopted vault agreed to the relationship rather than merely being named
    /// in it. Until it arrives the minter's hop is a claim, and a peer badge
    /// requires an accepted chain — see `VouchChain::verify_accepted`.
    ///
    /// Sent by the Place that received the adopt, so the recipient here is the
    /// minter. No further reply: the exchange ends at two signatures.
    PlaceAdoptAck {
        /// The chain back, with the sender's counter-signature on the last hop.
        chain: crate::place::VouchChain,
    },
    /// Opt-in introduction of one of the sender's Places to a contact.
    ///
    /// The recipient verifies the chain against contacts they already trust,
    /// adds the place as a contact from `invite`, and may show a "place of …"
    /// badge when a trusted anchor is found.
    PlaceIntroduce {
        invite: String,
        chain: crate::place::VouchChain,
        label: String,
    },
    /// A typed, non-chat message between two trustees.
    ///
    /// Signals are the sideband: they ride the same encrypted 1:1 session and
    /// the same store-and-forward queue as chat and mail, but they are never
    /// rendered as either. The envelope owns delivery concerns — identity,
    /// expiry, duplicate suppression and acknowledgement — while `body` names
    /// what the signal means. A build that does not know a kind decodes it as
    /// `SignalBody::Unknown` and ignores it.
    Signal {
        /// Random id chosen by the sender: dedupe key and ack correlation.
        id: String,
        /// When the sender produced it, Unix ms. Advisory only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent_ms: Option<u64>,
        /// When the signal stops being meaningful, Unix ms. A receiver drops
        /// it after this instant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_ms: Option<u64>,
        /// Ask the receiver for a `SignalAck`.
        #[serde(default)]
        ack: bool,
        body: crate::signals::SignalBody,
    },
    /// A 1:1 call offer (Task 17), sent inside the contact's Signal session.
    ///
    /// Same SDP-only, non-trickle shape as [`MessagePayload::VoiceOffer`]: the
    /// sender gathers ICE candidates before offering, so one payload carries
    /// the whole handshake half. The session id is the caller's random id and
    /// names the call on both sides.
    CallOffer {
        session_id: String,
        sdp: String,
    },
    /// The callee's answer to [`MessagePayload::CallOffer`].
    CallAnswer {
        session_id: String,
        sdp: String,
    },
    /// Either side ended the call.
    ///
    /// Consumed by `recv` and handed to the caller out of band; never a chat
    /// row. `reason` is diagnostic (`hangup`, `declined`, `busy`, `failed`) and
    /// has no protocol authority.
    CallEnd {
        session_id: String,
        reason: String,
    },
    /// One acknowledgement of a [`MessagePayload::Signal`].
    ///
    /// A control payload: it is consumed by `recv` and never surfaced as an
    /// inbox item.
    SignalAck {
        /// The `id` of the signal being acknowledged.
        id: String,
        status: crate::signals::SignalStatus,
    },
    /// A payload whose tag this build does not know.
    ///
    /// Decoded rather than rejected so a peer running a newer build can send
    /// something this build cannot read without the whole message being thrown
    /// away as corrupt. Never constructed locally and never sent: it exists so
    /// that `decode` has somewhere honest to land.
    #[serde(other)]
    Unknown,
}
mod b64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

impl MessagePayload {
    pub fn encode(&self) -> Result<Vec<u8>, crate::error::Error> {
        serde_json::to_vec(self).map_err(|_| crate::error::Error::Corrupt)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, crate::error::Error> {
        if let Ok(p) = serde_json::from_slice::<Self>(bytes) {
            return Ok(p);
        }
        // Legacy file payloads used JSON byte arrays before base64.
        #[derive(Deserialize)]
        struct LegacyFile {
            t: String,
            name: String,
            data: Vec<u8>,
        }
        if let Ok(legacy) = serde_json::from_slice::<LegacyFile>(bytes) {
            if legacy.t == "file" {
                return Ok(MessagePayload::File {
                    name: legacy.name,
                    data: legacy.data,
                });
            }
        }
        serde_json::from_slice(bytes).map_err(|_| crate::error::Error::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expiring_message_round_trips_with_its_deadline() {
        let p = MessagePayload::ExpireText {
            id: "e-1".into(),
            sent_ms: 1_700_000_000_000,
            expires_ms: 1_700_000_030_000,
            body: "burn after reading".into(),
        };
        let encoded = p.encode().unwrap();
        let as_text = String::from_utf8(encoded.clone()).unwrap();
        assert!(
            as_text.contains("\"t\":\"expire_text\""),
            "the wire tag names the variant: {as_text}"
        );
        assert_eq!(MessagePayload::decode(&encoded).unwrap(), p);
    }

    #[test]
    fn a_mail_payload_round_trips_with_attachments() {
        let p = MessagePayload::Mail {
            thread_id: "t-1".into(),
            subject: Some("hi".into()),
            body: "body".into(),
            attachments: vec![MailAttachment {
                name: "a.txt".into(),
                data: vec![1, 2, 3],
            }],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A zine grant carries key bytes, which must survive the base64 hop as
    /// bytes and must not be readable in the encoded form.
    #[test]
    fn a_zine_grant_round_trips_with_its_key() {
        let p = MessagePayload::ZineGrant {
            zine_id: "3f2a".into(),
            key: vec![0u8, 1, 2, 250, 255],
            latest_hash: "ab12".into(),
        };
        let encoded = p.encode().unwrap();
        assert_eq!(MessagePayload::decode(&encoded).unwrap(), p);
        let as_text = String::from_utf8(encoded).unwrap();
        assert!(
            !as_text.contains("[0,1,2"),
            "the key is base64, not a JSON byte array: {as_text}"
        );
    }

    #[test]
    fn a_mail_payload_without_subject_or_attachments_round_trips() {
        let p = MessagePayload::Mail {
            thread_id: "t-2".into(),
            subject: None,
            body: "body".into(),
            attachments: vec![],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_payload_round_trips_with_its_descriptor() {
        let p = MessagePayload::Conclave {
            conclave_id: "m-1".into(),
            name: "Standup".into(),
            trustees: vec!["bob".into(), "carol".into()],
            body: "morning".into(),
            file: None,
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_payload_round_trips_with_a_file() {
        let p = MessagePayload::Conclave {
            conclave_id: "m-1".into(),
            name: "Standup".into(),
            trustees: vec!["bob".into()],
            body: String::new(),
            file: Some(ConclaveFile {
                name: "notes.txt".into(),
                data: vec![9, 8, 7],
            }),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A conclave message written before files existed has no `file` field and
    /// must still decode.
    #[test]
    fn a_conclave_payload_without_a_file_decodes_as_none() {
        let json = r#"{"t":"conclave","conclave_id":"m-1","name":"Standup","body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave { file, .. } => assert!(file.is_none()),
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    /// The wire tag is now `conclave` and the id field `conclave_id`.
    #[test]
    fn a_conclave_payload_writes_the_new_tag_and_field() {
        let p = MessagePayload::Conclave {
            conclave_id: "m-1".into(),
            name: "Standup".into(),
            trustees: vec!["bob".into()],
            body: "hi".into(),
            file: None,
        };
        let json = String::from_utf8(p.encode().unwrap()).unwrap();
        assert!(json.contains(r#""t":"conclave""#), "json was {json}");
        assert!(json.contains(r#""conclave_id":"m-1""#), "json was {json}");
    }

    /// A message written by the old build (`t: "meeting"`, `meeting_id`) must
    /// still decode, so a rename does not strand queued or in-flight messages.
    #[test]
    fn a_legacy_meeting_payload_still_decodes() {
        let json =
            r#"{"t":"meeting","meeting_id":"m-1","name":"Standup","members":["bob"],"body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave {
                conclave_id,
                trustees,
                ..
            } => {
                assert_eq!(conclave_id, "m-1");
                assert_eq!(trustees, vec!["bob".to_string()]);
            }
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    #[test]
    fn a_conclave_payload_without_a_member_list_still_decodes() {
        let json = r#"{"t":"conclave","conclave_id":"m-1","name":"Standup","body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave { trustees, .. } => assert!(trustees.is_empty()),
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    /// A queued message written before the trustees rename carries `members`; it
    /// must still decode.
    #[test]
    fn a_pre_rename_conclave_payload_still_decodes_its_members() {
        let json = r#"{"t":"conclave","conclave_id":"m-1","name":"Standup","members":["bob"],"body":"hi"}"#;
        let p: MessagePayload = serde_json::from_str(json).unwrap();
        match p {
            MessagePayload::Conclave { trustees, .. } => {
                assert_eq!(trustees, vec!["bob".to_string()]);
            }
            other => panic!("expected a conclave, got {other:?}"),
        }
    }

    fn trustee(account_id: &str, nick: &str, state: TrusteeState) -> ConclaveTrustee {
        ConclaveTrustee {
            account_id: account_id.to_string(),
            nick: nick.to_string(),
            state,
        }
    }

    #[test]
    fn a_conclave_invite_round_trips_with_its_trustees() {
        let p = MessagePayload::ConclaveInvite {
            descriptor: ConclaveDescriptor {
                conclave_id: "c-1".into(),
                name: "Trip".into(),
                convener: "aa".into(),
                trustees: vec![
                    trustee("aa", "alice", TrusteeState::Joined),
                    trustee("bb", "bob", TrusteeState::Invited),
                ],
                ended: false,
            },
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_accept_round_trips_and_moves_the_trustee_to_joined() {
        let p = MessagePayload::ConclaveAccept {
            descriptor: ConclaveDescriptor {
                conclave_id: "c-1".into(),
                name: "Trip".into(),
                convener: "aa".into(),
                trustees: vec![trustee("bb", "bob", TrusteeState::Joined)],
                ended: false,
            },
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_conclave_decline_round_trips() {
        let p = MessagePayload::ConclaveDecline {
            conclave_id: "c-1".into(),
            member: "bb".into(),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A trustee written before `state` existed decodes as joined, which is what
    /// a member was before consent existed.
    #[test]
    fn a_trustee_without_a_state_decodes_as_joined() {
        let json = r#"{"account_id":"bb","nick":"bob"}"#;
        let m: ConclaveTrustee = serde_json::from_str(json).unwrap();
        assert_eq!(m.state, TrusteeState::Joined);
    }

    /// The pre-rename record shape was a bare nickname list under `members`; a
    /// descriptor written that way must still decode.
    #[test]
    fn a_descriptor_with_pre_rename_members_still_decodes() {
        let json = r#"{"conclave_id":"c-1","name":"Trip","members":["bob"]}"#;
        let d: ConclaveDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(d.trustees.len(), 1);
        assert_eq!(d.trustees[0].nick, "bob");
        assert_eq!(d.trustees[0].state, TrusteeState::Joined);
    }

    fn paper_descriptor() -> PaperDescriptor {
        PaperDescriptor {
            paper_id: "p-1".into(),
            conclave_id: "c-1".into(),
            creator: "aa".into(),
            hub: "aa".into(),
            title: "Trip notes".into(),
            created_ms: 1_791_837_993_291,
        }
    }

    #[test]
    fn a_paper_state_round_trips_with_its_descriptor() {
        let p = MessagePayload::PaperState {
            paper: paper_descriptor(),
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// A yrs update is binary. Bytes that a JSON number array would still carry
    /// but a reader could mangle (0x00, 0xff) must survive the wire.
    #[test]
    fn a_paper_delta_round_trips_binary_bytes() {
        let update = vec![0u8, 255, 1, 128, 0];
        let p = MessagePayload::PaperDelta {
            paper_id: "p-1".into(),
            update,
        };
        let decoded = MessagePayload::decode(&p.encode().unwrap()).unwrap();
        assert_eq!(decoded, p);
        match decoded {
            MessagePayload::PaperDelta { update, .. } => {
                assert_eq!(update, vec![0u8, 255, 1, 128, 0])
            }
            other => panic!("expected a paper delta, got {other:?}"),
        }
    }

    #[test]
    fn a_paper_snapshot_round_trips_binary_bytes() {
        let p = MessagePayload::PaperSnapshot {
            paper: paper_descriptor(),
            state: vec![0, 0],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    #[test]
    fn a_paper_presence_round_trips() {
        let p = MessagePayload::PaperPresence {
            paper_id: "p-1".into(),
            peer: "bb".into(),
            state: vec![7],
        };
        assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
    }

    /// Signalling is one round trip and the offer carries the gathered
    /// candidates, so there is no separate candidate payload that could be
    /// dropped while the handshake is in flight.
    #[test]
    fn paper_signalling_round_trips() {
        let sdp =
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\na=candidate:1 1 udp 1 127.0.0.1 9 typ host\r\n";
        for p in [
            MessagePayload::PaperOffer {
                paper_id: "p-1".into(),
                sdp: sdp.into(),
            },
            MessagePayload::PaperAnswer {
                paper_id: "p-1".into(),
                sdp: sdp.into(),
            },
        ] {
            let decoded = MessagePayload::decode(&p.encode().unwrap()).unwrap();
            assert_eq!(decoded, p);
            match decoded {
                MessagePayload::PaperOffer { sdp, .. }
                | MessagePayload::PaperAnswer { sdp, .. } => {
                    assert!(
                        sdp.contains("a=candidate:"),
                        "candidates must survive the wire"
                    )
                }
                other => panic!("expected paper signalling, got {other:?}"),
            }
        }
    }

    /// `PaperHub` is accepted only when the convener sent it, so the descriptor
    /// must carry the conclave id the receiver resolves that convener through:
    /// the check is `sender == conclave(paper.conclave_id).convener`.
    #[test]
    fn a_paper_hub_round_trips_and_names_the_conclave_to_check_against() {
        let p = MessagePayload::PaperHub {
            paper: paper_descriptor(),
        };
        let decoded = MessagePayload::decode(&p.encode().unwrap()).unwrap();
        assert_eq!(decoded, p);
        match decoded {
            MessagePayload::PaperHub { paper } => {
                assert_eq!(paper.conclave_id, "c-1");
                assert_eq!(paper.hub, "aa");
            }
            other => panic!("expected a paper hub, got {other:?}"),
        }
    }

    /// The wire descriptor omits `title`; a payload written that way still has
    /// to name the paper, so it defaults rather than failing to decode.
    #[test]
    fn a_paper_descriptor_without_a_title_decodes_as_paper() {
        let json =
            r#"{"paper_id":"p-1","conclave_id":"c-1","creator":"aa","hub":"aa","created_ms":5}"#;
        let d: PaperDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(d.title, "Paper");
    }

    /// A payload from a newer build must be ignored, not treated as corrupt.
    #[test]
    fn an_unknown_payload_tag_decodes_to_unknown() {
        let json = br#"{"t":"something_this_build_has_never_heard_of","x":1}"#;
        assert_eq!(
            MessagePayload::decode(json).unwrap(),
            MessagePayload::Unknown
        );
    }

    /// The legacy fallback turns undecodable bytes into a chat message, which
    /// would surface a future payload as a bogus text message in someone's
    /// chat. An unknown tag must not ride that path.
    #[test]
    fn an_unknown_payload_tag_is_not_decoded_as_text() {
        let json = br#"{"t":"something_this_build_has_never_heard_of","x":1}"#;
        assert!(
            !matches!(
                MessagePayload::decode(json).unwrap(),
                MessagePayload::Text { .. }
            ),
            "an unknown tag must not become a text message"
        );
    }

    /// Tolerating an unknown *tag* must not tolerate a malformed *body*: a
    /// `text` payload missing its body is real corruption, not a newer format,
    /// and reading it as `Unknown` would hide a broken peer.
    #[test]
    fn a_known_tag_with_a_malformed_body_is_still_an_error() {
        assert!(MessagePayload::decode(br#"{"t":"text"}"#).is_err());
    }

    fn voice_descriptor() -> VoiceDescriptor {
        VoiceDescriptor {
            session_id: "v-1".into(),
            conclave_id: "c-1".into(),
            opener: "aa".into(),
            started_ms: 7,
        }
    }

    /// Every voice variant round-trips, and the tag names are the snake_case
    /// ones the tolerant-decode discipline implies.
    #[test]
    fn voice_payloads_round_trip() {
        let payloads = vec![
            MessagePayload::VoiceOpen {
                session: voice_descriptor(),
            },
            MessagePayload::VoiceOffer {
                session_id: "v-1".into(),
                sdp: "v=0\r\no=- …".into(),
            },
            MessagePayload::VoiceAnswer {
                session_id: "v-1".into(),
                sdp: "v=0\r\no=- …".into(),
            },
            MessagePayload::VoiceMute {
                session_id: "v-1".into(),
                who: "bb".into(),
                muted: true,
            },
            MessagePayload::VoiceLeave {
                session_id: "v-1".into(),
                who: "bb".into(),
            },
        ];
        for p in payloads {
            assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
        }
    }

    /// The descriptor is self-contained — conclave id and opener ride with it —
    /// so a receiver can check authority without a second lookup the way the
    /// late-joiner `PaperSnapshot` bug taught.
    #[test]
    fn a_voice_open_carries_the_conclave_and_the_opener() {
        let p = MessagePayload::VoiceOpen {
            session: voice_descriptor(),
        };
        match MessagePayload::decode(&p.encode().unwrap()).unwrap() {
            MessagePayload::VoiceOpen { session } => {
                assert_eq!(session.conclave_id, "c-1");
                assert_eq!(session.opener, "aa");
            }
            other => panic!("expected a voice open, got {other:?}"),
        }
    }

    /// A `voice_ice` tag from a future build decodes to `Unknown`, not corrupt —
    /// the renegotiation variant the spec deferred must be tolerable when it
    /// arrives.
    #[test]
    fn an_unknown_voice_tag_decodes_to_unknown() {
        let json = br#"{"t":"voice_ice","session_id":"v-1","candidate":"x"}"#;
        assert_eq!(
            MessagePayload::decode(json).unwrap(),
            MessagePayload::Unknown
        );
    }

    /// A reach announcement round-trips, and the tag is the snake_case one the
    /// tolerant-decode discipline implies.
    #[test]
    fn a_reach_payload_round_trips() {
        for scope in [TrusteeReach::MailOnly, TrusteeReach::Full] {
            let p = MessagePayload::Reach { scope };
            assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
        }
        let json = String::from_utf8(
            MessagePayload::Reach {
                scope: TrusteeReach::MailOnly,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
        assert!(json.contains(r#""t":"reach""#), "json was {json}");
        assert!(json.contains(r#""scope":"mail_only""#), "json was {json}");
    }

    /// A surface is usable only if both sides allow it, so the narrower reach
    /// wins, and only `Full` opens the live surfaces.
    #[test]
    fn reach_narrows_to_mail_only_when_either_side_says_so() {
        use TrusteeReach::{Full, MailOnly};
        assert_eq!(Full.narrow(Full), Full);
        assert_eq!(Full.narrow(MailOnly), MailOnly);
        assert_eq!(MailOnly.narrow(Full), MailOnly);
        assert_eq!(MailOnly.narrow(MailOnly), MailOnly);
        assert!(Full.allows_live());
        assert!(!MailOnly.allows_live());
    }

    /// `Full` is the default, so a relationship that predates this setting — or
    /// a peer that has not announced one — behaves exactly as before.
    #[test]
    fn reach_defaults_to_full() {
        assert_eq!(TrusteeReach::default(), TrusteeReach::Full);
    }

    /// A contact request round-trips with its note and capability bit, and the
    /// accept/decline tags are the snake_case names older builds will decode as
    /// `Unknown` (and therefore ignore) rather than corrupt.
    #[test]
    fn contact_request_payloads_round_trip() {
        let request = MessagePayload::ContactRequest {
            invite: "hush1qqqq".into(),
            display_name: "sam".into(),
            note: Some("from the office".into()),
            supports_file_transfer: true,
        };
        assert_eq!(
            MessagePayload::decode(&request.encode().unwrap()).unwrap(),
            request
        );
        let json = String::from_utf8(request.encode().unwrap()).unwrap();
        assert!(json.contains(r#""t":"contact_request""#), "json was {json}");
        assert!(
            json.contains(r#""supports_file_transfer":true"#),
            "json was {json}"
        );

        for payload in [
            MessagePayload::ContactAccept,
            MessagePayload::ContactDecline,
        ] {
            assert_eq!(
                MessagePayload::decode(&payload.encode().unwrap()).unwrap(),
                payload
            );
        }
    }

    fn file_offer() -> FileOffer {
        let hash = "a".repeat(64);
        FileOffer {
            transfer_id: "transfer-1".into(),
            queue_id: "xfer-transfer-1".into(),
            read_token: "read-token".into(),
            name: "lease.pdf".into(),
            size: 2048,
            chunk_size: 1024,
            chunks: vec![
                ChunkRef {
                    index: 0,
                    sha256: hash.clone(),
                    size: 1024,
                },
                ChunkRef {
                    index: 1,
                    sha256: hash,
                    size: 1024,
                },
            ],
            whole_sha256: "b".repeat(64),
            file_key: vec![7u8; 32],
        }
    }

    #[test]
    fn a_file_offer_round_trips_and_validates() {
        let payload = MessagePayload::FileOffer {
            offer: file_offer(),
        };
        let decoded = MessagePayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(decoded, payload);
        match decoded {
            MessagePayload::FileOffer { offer } => offer.validate().unwrap(),
            other => panic!("expected a file offer, got {other:?}"),
        }
    }

    #[test]
    fn a_file_offer_rejects_an_oversized_file() {
        let mut offer = file_offer();
        offer.size = MAX_FILE_TRANSFER_BYTES + 1;
        assert!(matches!(
            offer.validate(),
            Err(crate::error::Error::FileTooLarge(_))
        ));
    }

    #[test]
    fn a_file_offer_rejects_a_gap_in_the_chunk_indexes() {
        let mut offer = file_offer();
        offer.chunks[1].index = 5;
        assert!(matches!(
            offer.validate(),
            Err(crate::error::Error::InvalidFileOffer)
        ));
    }

    #[test]
    fn a_file_offer_rejects_a_wrong_key_length() {
        let mut offer = file_offer();
        offer.file_key = vec![7u8; 31];
        assert!(matches!(
            offer.validate(),
            Err(crate::error::Error::InvalidFileOffer)
        ));
    }

    #[test]
    fn a_file_offer_rejects_a_path_shaped_name() {
        let mut offer = file_offer();
        offer.name = "../lease.pdf".into();
        assert!(matches!(
            offer.validate(),
            Err(crate::error::Error::InvalidFileOffer)
        ));
    }
}
