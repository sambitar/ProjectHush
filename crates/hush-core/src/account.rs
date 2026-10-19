use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use libsignal_protocol::{IdentityKeyStore, SessionStore};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::backup::{export_vault, import_vault};
use crate::chat_log::{
    ChatLog, ChatRecordPayload, ChatRetention, ConclaveRecord, MailAttachmentRecord, MailState,
};
use crate::payload::{ConclaveDescriptor, ConclaveTrustee, TrusteeReach, TrusteeState};
use crate::device::{DeviceKeys, DEVICE_ID, LEGACY_DEVICE_FILE};
use crate::prekey::PreKeyRecord;
use crate::recovery::{ensure_recovery_files, generate_recovery_code, recovery_files_exist, unwrap_master_with_recovery};
use crate::crypto::{decrypt, encrypt};
use crate::error::Error;
use crate::invite::{InvitePayload, ShortInvite};
use crate::message::{
    InboxResult, ReceivedAttachment, ReceivedMessage, ReceivedPaper, ReceivedVoice,
};
use crate::payload::{
    MailAttachment, MessagePayload, PaperDescriptor, VoiceDescriptor, MAX_INLINE_FILE_BYTES,
    MAX_MAIL_ATTACHMENTS,
};
use crate::relay_client::{
    normalize_relay_base, validate_onion_relay, BulletinBlob, DrainedBlob, RelayClient,
};
use crate::signal::{
    decrypt_message, encrypt_message, invite_from_bundle, mint_prekey_bundle,
    process_contact_bundle, protocol_address, protocol_name_from_identity, sign_queue_grant,
    verify_queue_grant,
};
use crate::signal_persist::import_store;
use crate::vault::{BlobId, DropDrainKey, Vault};
use crate::wire::{decode_relay_blob, encode_relay_blob};

/// What happened to a mail send: which thread it belongs to, and whether the
/// relay actually took it.
#[derive(Debug, Clone)]
pub struct MailOutcome {
    pub thread_id: String,
    /// True when delivery failed and the message is sitting in the Outbox.
    pub queued: bool,
}

/// A queued message lifted out of the log, so the send path can take `&mut self`
/// while it runs.
struct QueuedMail {
    id: String,
    contact: String,
    thread_id: String,
    subject: Option<String>,
    body: String,
    attachments: Vec<MailAttachmentRecord>,
}

/// Whether a failed delivery is worth another attempt.
///
/// An unreachable relay or a local IO failure is transient — that is the case
/// the Outbox exists for. A session that cannot be built is not: retrying would
/// fail identically forever, so it surfaces to the user instead of quietly
/// piling up in the Outbox.
///
/// A misconfigured relay URL also reports as `Relay`, so a vault pointed at a
/// bad address queues rather than erroring. That is visible (the message waits
/// in the Outbox) and fixable in Settings, which beats discarding the mail.
fn is_worth_retrying(e: &Error) -> bool {
    matches!(e, Error::Relay(_) | Error::Io(_))
}

/// What one drain pass over the drop slots did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct DropInboxReport {
    /// Rendezvous slots a container was actually found on this pass.
    pub hit_slots: Vec<String>,
    /// Containers still parked after the pass — the ones that could not be
    /// opened or filed stay on the relay for the next pass to retry.
    pub remaining: usize,
}

/// Longest subject line kept from a drop; anything longer is cut.
const MAX_DROP_SUBJECT_CHARS: usize = 256;
/// Longest body kept from a drop, in characters. The container already caps
/// the whole package at 8 MiB; this keeps one text blob from dominating the
/// chat log when the 8 MiB is all body.
const MAX_DROP_BODY_CHARS: usize = 1 << 20;

/// Clamp free text from a drop: control characters out (newlines and tabs are
/// body content), length capped. Used on everything a hostile dropper
/// controls that is rendered back to the member.
fn clamp_drop_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max_chars)
        .collect()
}

/// Reduce an attachment name to a bare, safe display name.
///
/// Paths are stripped (`/` and `\` both), control characters dropped, length
/// capped — an attachment is only ever *saved* by the member to a path they
/// choose, so the name must survive being shown and being used as a filename
/// suggestion without smuggling a directory component.
fn sanitize_drop_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control()).take(255).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

fn blob_id_from_hex(s: &str) -> Option<BlobId> {
    let bytes = hex::decode(s).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(&bytes);
    Some(BlobId(id))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A contact: one account, one device.
///
/// The routing details the send path needs live here directly. Before
/// multi-device a contact was a *set* of devices learned from a signed device
/// list; that dimension is gone, so the device-1 entry of an old set is folded
/// into these fields on read.
#[derive(Clone, Debug, Serialize)]
pub struct Contact {
    pub nickname: String,
    /// The contact's account id: the hash of the identity key they pinned when
    /// the invite was exchanged. This is the whole routing identity now.
    pub account_id: String,
    /// The contact's identity public key, learned from their invite.
    ///
    /// This is the authority a [`MessagePayload::QueueGrant`] is verified
    /// against: the invite is what established the relationship, so its key is
    /// trusted, and the grant signature proves the sender holds the matching
    /// private key.
    pub identity_key: Vec<u8>,
    /// The contact's relay inbox: their **grant channel**, and where messages go
    /// until they grant us a per-contact queue.
    pub queue_id: String,
    pub queue_token: String,
    /// The per-contact queue this contact granted us, if it has.
    ///
    /// Set from a verified [`MessagePayload::QueueGrant`]. When present, a send
    /// delivers here instead of to the contact's inbox, which is what makes
    /// deleting the contact able to burn exactly this queue and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_queue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_queue_token: Option<String>,
    /// This contact's chosen mail handle (local part only), learned from their
    /// invite. `None` for contacts added before mail, or who never set one.
    #[serde(default)]
    pub mail_handle: Option<String>,
    /// The contact's vault name, learned from their invite — the suffix of
    /// their mail address. `None` for contacts whose invite predates vault
    /// naming; the UI falls back to [`DEFAULT_VAULT_NAME`].
    #[serde(default)]
    pub mail_vault: Option<String>,
    /// True once a filed message — chat, file or mail — has arrived from this
    /// contact.
    ///
    /// This is **not** readiness; that is [`Contact::inbound_ready`], derived
    /// from the grant we hold. This is only the fallback for the one case the
    /// grant cannot cover: `add_contact` sends a grant once, best effort, with
    /// no retry, so a relay outage at add time can leave us without their queue
    /// while they remain perfectly able to write to us.
    ///
    /// The stored key used to be `inbound_ready`, which a verified grant also
    /// set. The alias carries that value over: this is one OR term of the
    /// derivation, and the grant term now answers the same way wherever a grant
    /// had been stored, so an inherited `true` cannot contradict it.
    #[serde(default, alias = "inbound_ready")]
    pub heard_from: bool,
    /// How this vault chose to reach this trustee (Task 24).
    ///
    /// The vault's **own** choice, not the peer's: it says which surfaces this
    /// account will use with them. `Full` unless the user narrowed it, and
    /// narrowed only by the daemon's reach route.
    #[serde(default)]
    pub reach: TrusteeReach,
    /// The reach this trustee announced for us, learned from a `Reach`
    /// payload.
    ///
    /// `Full` until they announce otherwise, so a peer that predates this
    /// setting — or has simply not announced one yet — is never mistaken for
    /// mail-only. The effective reach is the narrower of this and
    /// [`Contact::reach`]: a surface is usable only when both sides allow it.
    #[serde(default)]
    pub peer_reach: TrusteeReach,
    /// Account id of the Place this contact was vouched under, when a peer
    /// introduce verified ("place of …"). `None` for ordinary contacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_of: Option<String>,
}

impl<'de> Deserialize<'de> for Contact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// One entry of a pre-single-device contact's `devices` array.
        #[derive(Deserialize)]
        struct LegacyDevice {
            device_id: u32,
            identity_key: Vec<u8>,
            queue_id: String,
            queue_token: String,
            #[serde(default)]
            delivery_queue_id: Option<String>,
            #[serde(default)]
            delivery_queue_token: Option<String>,
        }

        /// The stored shapes.
        ///
        /// `Flat` is what this build writes. `Devices` is the multi-device
        /// shape: the device-1 entry becomes the contact. `Legacy` is the
        /// pre-multi-device inline shape, where `protocol_name` *was* the
        /// identity key hash, so the key is recovered by decoding it rather than
        /// being lost.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Flat {
                nickname: String,
                account_id: String,
                identity_key: Vec<u8>,
                queue_id: String,
                queue_token: String,
                #[serde(default)]
                delivery_queue_id: Option<String>,
                #[serde(default)]
                delivery_queue_token: Option<String>,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default)]
                mail_vault: Option<String>,
                #[serde(default, alias = "inbound_ready")]
                heard_from: bool,
                #[serde(default)]
                reach: TrusteeReach,
                #[serde(default)]
                peer_reach: TrusteeReach,
                #[serde(default)]
                place_of: Option<String>,
            },
            Devices {
                nickname: String,
                account_id: String,
                devices: Vec<LegacyDevice>,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default)]
                mail_vault: Option<String>,
                #[serde(default, alias = "inbound_ready")]
                heard_from: bool,
            },
            Legacy {
                nickname: String,
                protocol_name: String,
                queue_id: String,
                queue_token: String,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default, alias = "inbound_ready")]
                heard_from: bool,
            },
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Flat {
                nickname,
                account_id,
                identity_key,
                queue_id,
                queue_token,
                delivery_queue_id,
                delivery_queue_token,
                mail_handle,
                mail_vault,
                heard_from,
                reach,
                peer_reach,
                place_of,
            } => Contact {
                nickname,
                account_id,
                identity_key,
                queue_id,
                queue_token,
                delivery_queue_id,
                delivery_queue_token,
                mail_handle,
                mail_vault,
                heard_from,
                reach,
                peer_reach,
                place_of,
            },
            Repr::Devices {
                nickname,
                account_id,
                devices,
                mail_handle,
                mail_vault,
                heard_from,
            } => {
                // Keep the entry that used to be device 1 (or the first), so the
                // queue the contact is actually reachable on is not dropped.
                let primary = devices
                    .into_iter()
                    .find(|d| d.device_id == DEVICE_ID)
                    .ok_or_else(|| serde::de::Error::custom("contact has no device 1"))?;
                Contact {
                    nickname,
                    account_id,
                    identity_key: primary.identity_key,
                    queue_id: primary.queue_id,
                    queue_token: primary.queue_token,
                    delivery_queue_id: primary.delivery_queue_id,
                    delivery_queue_token: primary.delivery_queue_token,
                    mail_handle,
                    mail_vault,
                    heard_from,
                    reach: TrusteeReach::Full,
                    peer_reach: TrusteeReach::Full,
                    place_of: None,
                }
            }
            Repr::Legacy {
                nickname,
                protocol_name,
                queue_id,
                queue_token,
                mail_handle,
                heard_from,
            } => {
                let identity_key = hex::decode(&protocol_name).unwrap_or_default();
                Contact {
                    nickname,
                    account_id: protocol_name,
                    identity_key,
                    queue_id,
                    queue_token,
                    delivery_queue_id: None,
                    delivery_queue_token: None,
                    mail_handle,
                    mail_vault: None,
                    heard_from,
                    reach: TrusteeReach::Full,
                    peer_reach: TrusteeReach::Full,
                    place_of: None,
                }
            }
        })
    }
}

impl Contact {
    /// Whether this contact has proven they added our invite.
    ///
    /// Derived, never stored. The grant we hold *is* the proof — only the holder
    /// of their identity key can produce one, and `add_contact` is the only
    /// thing that sends one — so keeping a second copy of that fact only let the
    /// two disagree, which is how a mutual, working trust came to read as
    /// pending while the grant sat in `delivery_queue_id` all along.
    ///
    /// It must not wait on them having written first: a mutual trust where
    /// neither side has sent anything is established, not pending. That is what
    /// [`Contact::heard_from`] is a fallback *for*, not a precondition of.
    pub fn inbound_ready(&self) -> bool {
        self.delivery_queue_id.is_some() || self.heard_from
    }
}

/// An invitation this vault has received but not yet accepted or declined.
///
/// It is not a conclave: nothing is materialised and no message is filed until
/// the invitee accepts. This is the record that replaces the old silent
/// materialisation of an unknown conclave.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingInvite {
    pub conclave_id: String,
    pub name: String,
    /// The convener's account id. `alias` reads the pre-rename field name.
    #[serde(alias = "creator")]
    pub convener: String,
    /// The account id of whoever sent the invite.
    pub from_account: String,
    /// The sender's nickname as this vault knows it.
    pub from_nick: String,
    /// The trustees the invite carried, so the invitee can render who else is
    /// in. `alias` reads the pre-rename field name.
    #[serde(alias = "roster")]
    pub trustees: Vec<ConclaveTrustee>,
    pub received_ms: u64,
}

/// A request for an introduction, waiting on this vault.
///
/// The same record means two different things depending on who reads it: when
/// `target` is this vault, the seeker wants *our* invite and consent is asked;
/// otherwise the request came from the seeker and this vault is the courier
/// being asked to relay it to a member it knows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingIntroRequest {
    pub conclave_id: String,
    pub conclave_name: String,
    /// The account that wants to meet the target.
    pub seeker: String,
    /// The seeker's nickname as the sender knows it; display hint only.
    pub seeker_nick: String,
    /// The account whose invite is wanted.
    pub target: String,
    /// The target's nickname as the sender knows it; display hint only.
    pub target_nick: String,
    /// The account id of the member the request travelled through, so a
    /// consented invite goes back over a channel that is known to exist.
    pub via: String,
    pub received_ms: u64,
}

/// An introduction received: another member's standing invite, carried by a
/// mutual contact, waiting for this vault to accept or dismiss it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingIntro {
    pub conclave_id: String,
    pub conclave_name: String,
    /// The account the invite belongs to.
    pub target: String,
    /// The target's nickname as the sender knows it; display hint only.
    pub target_nick: String,
    /// The account id of the member who carried the invite here.
    pub via: String,
    /// The target's standing invite. Never serialised to the UI: the UI
    /// accepts or dismisses, the vault consumes it.
    pub invite: String,
    pub received_ms: u64,
    /// True when this is the acceptance half of an introduction: the sender
    /// already consumed one of our invites and opened their session, so our
    /// accept must add them as a contact without consuming the attached
    /// bundle (the session comes from their first message instead).
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Serialize, Deserialize)]
struct AccountState {
    /// The relay this vault was created with: the clearnet canonical base.
    /// Never rewritten by the Tor switch — flipping modes selects between
    /// this and [`AccountState::onion_relay`], so a flip back is always
    /// possible even if the onion address is the only one that still works.
    relay: String,
    /// The same relay's onion address, when one is on file: recorded at vault
    /// creation (`hush-cli init --onion`) or attached later through the
    /// daemon's relay endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onion_relay: Option<String>,
    /// Whether the relay path currently goes through Tor. The daemon applies
    /// the user's switch here (directly when unlocked, from staged prefs at
    /// unlock), so the vault's traffic follows the switch across restarts.
    #[serde(default)]
    use_tor_relay: bool,
    protocol_name: String,
    contacts: Vec<Contact>,
    /// Stable invite string so "Copy my invite" does not rotate keys every click.
    #[serde(default)]
    invite_cache: Option<String>,
    #[serde(default)]
    recovery_acknowledged: bool,
    #[serde(default)]
    last_backup_unix: u64,
    /// Legacy: this device's relay inbox used to live in `account.enc`. Read
    /// once when migrating to `device.enc`, then never written again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receive_queue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receive_queue_token: Option<String>,
    /// This member's chosen mail handle (local part only). Shared vault state,
    /// not device material: it is a label, and a restored vault should keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mail_handle: Option<String>,
    /// The vault's own name, chosen when the vault is created. It is the
    /// right-hand side of this member's mail address (`handle@vaultname`) and
    /// travels in the invite so contacts can render the address they see.
    /// `None` for vaults created before naming existed; [`DEFAULT_VAULT_NAME`]
    /// stands in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_name: Option<String>,
    /// Account ids whose contact was deleted.
    ///
    /// Deleting a contact burns the one per-contact queue it delivered into, so
    /// this is not what makes a revocation happen. It stays because a deleted
    /// contact still holds the invite token for the shared inbox, and a legacy
    /// blob that names its sender could otherwise be decrypted and surfaced: an
    /// id here drops those stragglers. It also re-drives the burn if the relay
    /// was down when the delete ran. Removing the signature check on
    /// [`Account::apply_queue_grant`] would be the thing that re-opens a
    /// revocation; removing this list would not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    revoked: Vec<String>,
    /// Conclave invitations received but not yet accepted or declined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_invites: Vec<PendingInvite>,
    /// Introduction requests waiting on this vault — consent when this vault
    /// is the target, a relay decision when it is the courier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_intro_requests: Vec<PendingIntroRequest>,
    /// Introductions received and not yet accepted or dismissed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_intros: Vec<PendingIntro>,
    /// Reach announcements (Task 24) not yet delivered to the relay.
    ///
    /// `set_contact_reach` records the peer's copy of the choice here and
    /// returns at once; the daemon's outbox timer drains it like queued mail.
    /// One entry per contact, latest scope wins: a rapid toggle leaves only
    /// the last choice to deliver.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_reach: Vec<PendingReach>,
    /// Conclave fan-out copies the relay refused, still owed to their targets.
    ///
    /// A conclave has no relay-side room and no group object, so a fan-out is N
    /// independent 1:1 puts and a subset can fail while the rest land. There is
    /// nowhere to re-read the refused copy from, so the encoded payload and the
    /// nicks still owed it are kept here and the daemon's outbox timer drains
    /// them, exactly like queued mail and reach announcements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_conclave: Vec<PendingConclave>,
    /// Set when this device's queues rotated — a v1 device file, or a migration
    /// off the pre-multi-device layout. Every contact must then be re-granted
    /// the new per-contact queue, or it keeps delivering into the one this
    /// device no longer reads. Drained by the daemon's outbox timer.
    #[serde(default)]
    regrant_all: bool,
    /// Set when this vault's device identity was minted fresh, because the vault
    /// was restored from a backup without this device's keys.
    ///
    /// Every contact pinned the previous identity key in their invite, so they
    /// refuse this vault's grants and cannot read its messages: the relationship
    /// has to be re-established by re-adding the contact. Recorded rather than
    /// repaired — accepting a changed identity key automatically is exactly the
    /// substitution the pin exists to stop.
    #[serde(default)]
    identity_restored: bool,
    /// Places this vault minted or was minted by (Settings → My places).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    places: Vec<PlaceRecord>,
}

/// Whether this vault minted the Place or was adopted by it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaceRole {
    MintedByMe,
    MintedMe,
}

/// One entry in Settings → My places.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaceRecord {
    pub account_id: String,
    pub nickname: String,
    pub label: String,
    pub role: PlaceRole,
    pub chain: crate::place::VouchChain,
    pub added_unix: u64,
}

/// One queued reach announcement: who it is for and what scope it carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingReach {
    nickname: String,
    scope: TrusteeReach,
}

/// One conclave payload still owed to some of its targets.
///
/// The payload is kept as the bytes it was encoded to rather than as a decoded
/// struct: the retry re-encrypts a fresh copy per target anyway, and storing the
/// wire bytes means a retry that outlives a payload-shape change still decodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingConclave {
    conclave_id: String,
    payload: Vec<u8>,
    /// The nicks still owed a copy.
    targets: Vec<String>,
}

/// What one conclave fan-out did.
///
/// A fan-out is N independent puts, so the outcome is split rather than a single
/// `Result`: `refused` is worth retrying over the same 1:1 session, `unreachable`
/// is not (there is no session to retry over), and a fan-out that reached nobody
/// is worth telling the sender about.
#[derive(Debug, Default, Clone)]
struct ConclaveFanOut {
    /// Nicks the payload reached.
    delivered: Vec<String>,
    /// Nicks the relay refused; owed a retry.
    refused: Vec<String>,
    /// Joined trustees this vault has no contact record for, by display name.
    unreachable: Vec<String>,
}

impl ConclaveFanOut {
    /// Whether the payload reached no one at all — nobody delivered, nothing to
    /// retry. A message that lands here went nowhere and only a warning says so.
    fn reached_nobody(&self) -> bool {
        self.delivered.is_empty() && self.refused.is_empty()
    }
}

/// The relay queues one vault drains and delivers into, derived on demand.
///
/// Queue ids are HMACs over the device read token and are never written to the
/// vault, which is the right call for privacy and a hard one for operations:
/// when a queue on the relay is filling and nothing is draining it, there is no
/// file that says whose it is. This is the local, read-only answer — a queue
/// that is not in this map does not belong to this vault.
///
/// Ids only, never the read tokens: an id is what the relay already knows and is
/// not reversible to the token, while the token is what authorizes draining and
/// burning.
#[derive(Debug, Clone, Serialize)]
pub struct QueueMap {
    /// The queue this vault drains for itself.
    pub inbox: String,
    /// One row per contact, in contact order.
    pub contacts: Vec<ContactQueues>,
    /// Whether every contact still owes a re-grant because this device's queues
    /// rotated. While true, the grants peers hold name queues this vault no
    /// longer drains, so their mail sits on the relay.
    pub regrant_pending: bool,
    /// Whether this vault's device identity is newer than the one every contact
    /// pinned. While true, peers refuse its grants and cannot read its messages:
    /// the contact has to be re-added, which no automatic sweep can do.
    pub identity_restored: bool,
}

/// One contact's queues, in both directions.
#[derive(Debug, Clone, Serialize)]
pub struct ContactQueues {
    pub nickname: String,
    pub account_id: String,
    /// The queue this vault mints for the contact and drains: the contact
    /// delivers into it.
    pub read_queue: String,
    /// Where this vault delivers to the contact — the contact's granted
    /// per-contact queue when one has arrived, the contact's inbox otherwise.
    pub send_queue: String,
    /// Whether `send_queue` is the granted per-contact queue. `false` means the
    /// grant has not arrived, so delivery falls back to the shared inbox and a
    /// delete of this contact cannot revoke it.
    pub granted: bool,
}

pub struct Account {
    vault: Vault,
    /// This account's own keys and relay inbox, kept out of the shared vault so
    /// an exported backup never carries the identity private key.
    device: DeviceKeys,
    state: AccountState,
    dir: PathBuf,
    chat_log: ChatLog,
    pending_recovery_code: Option<String>,
    /// The SOCKS proxy the daemon was told the local Tor listens on, when Tor
    /// relay mode is on. Runtime-only: the daemon re-applies it after every
    /// unlock and whenever the mode changes, so it is never persisted.
    relay_socks: Option<String>,
    /// The chat auto-delete policy the daemon loaded from its prefs file.
    /// Runtime-only like the proxy: the daemon re-applies it on unlock and
    /// whenever the prefs change, and the log enforces it on every append.
    chat_retention: ChatRetention,
    /// The relay client built for the current endpoint, reused across calls.
    ///
    /// Runtime-only. Building a client per call discards the HTTP connection
    /// pool, and a discarded pool means every operation pays a fresh
    /// DNS+TCP+TLS handshake — measured at 0.5-0.65 s against the production
    /// relay, twice per chat send, where a reused connection costs one round
    /// trip. The cache key is the endpoint pair the client was built from, so
    /// flipping Tor relay mode or swapping the SOCKS port rebuilds rather
    /// than serves stale.
    relay_client_cache: std::sync::Mutex<Option<RelayClientCache>>,
    /// Whether the relay serves the batched drain, if the question has come
    /// up. `None` until a poll has probed; `Some(false)` latches the legacy
    /// per-blob walk so later polls skip the probe. A relay is not upgraded
    /// mid-session in practice, and a wrong latch costs one 404 next poll.
    drain_supported: Option<bool>,
}

/// The cache key and client for [`Account::relay_client`]: the endpoint pair
/// the client was built from, so a change to either rebuilds it.
struct RelayClientCache {
    key: (String, Option<String>),
    client: RelayClient,
}

/// How many blobs one batched drain page asks for. The relay clamps harder
/// when a page would exceed its byte ceiling; the client keeps paging until
/// a page comes back empty, which is the only reliable stop signal.
const DRAIN_PAGE_LIMIT: usize = 256;

/// One queue's fetch: the blobs that were queued on it when the poll ran.
struct FetchedQueue {
    expected_contact: Option<String>,
    queue_id: String,
    read_token: String,
    /// Fetched through the batched drain, so filed blobs are confirmed with
    /// one ack rather than one `delete` per blob.
    drained: bool,
    /// What a drain attempt discovered about the relay, if one was tried
    /// this poll. `None` when the caller already knew and skipped the probe.
    drain_discovered: Option<bool>,
    blobs: Vec<DrainedBlob>,
}

/// The per-blob fetch walk a pre-drain relay needs: list, then one `get` per
/// blob. Blobs are deleted after they file, on the caller side.
async fn legacy_walk(
    relay: &RelayClient,
    queue_id: &str,
    read_token: &str,
) -> Result<Vec<DrainedBlob>, Error> {
    let ids = relay.list_optional(queue_id, read_token).await?;
    let mut blobs = Vec::with_capacity(ids.len());
    for id in ids {
        let body = relay.get(queue_id, read_token, &id).await?;
        blobs.push(DrainedBlob { id, body });
    }
    Ok(blobs)
}

/// Fetch one queue's queued blobs. Touches no account state, so queues can
/// run concurrently; the client's connection pool spreads the requests.
async fn fetch_queue(
    relay: RelayClient,
    expected_contact: Option<String>,
    queue_id: String,
    read_token: String,
    drain_supported: Option<bool>,
) -> Result<FetchedQueue, Error> {
    // Latched legacy: skip the probe, walk per blob.
    if drain_supported == Some(false) {
        let blobs = legacy_walk(&relay, &queue_id, &read_token).await?;
        return Ok(FetchedQueue {
            expected_contact,
            queue_id,
            read_token,
            drained: false,
            drain_discovered: None,
            blobs,
        });
    }
    match relay.drain(&queue_id, &read_token, DRAIN_PAGE_LIMIT).await? {
        // A relay without the drain route: walk per blob this poll and latch
        // the discovery so later polls skip the probe.
        None => {
            let blobs = legacy_walk(&relay, &queue_id, &read_token).await?;
            Ok(FetchedQueue {
                expected_contact,
                queue_id,
                read_token,
                drained: false,
                drain_discovered: Some(false),
                blobs,
            })
        }
        // One page per fetch. A peek never consumes — the ack that follows
        // filing does — so paging here would re-read the same rows forever.
        // Deeper backlogs are fetched by the round loop in `recv`, which
        // re-peeks after the ack.
        Some(page) => Ok(FetchedQueue {
            expected_contact,
            queue_id,
            read_token,
            drained: true,
            drain_discovered: Some(true),
            blobs: page,
        }),
    }
}

impl Account {
    pub async fn create(dir: &Path, passphrase: &str, relay: &str) -> Result<Self, Error> {
        let relay = normalize_relay_base(relay)?;
        let vault = Vault::create(dir, passphrase)?;
        let master = *vault.master_key();
        let mut device = DeviceKeys::create(dir, &master).await?;
        let _bundle = mint_prekey_bundle(device.store_mut(), DEVICE_ID).await?;
        let protocol_name = protocol_name_from_identity(
            &device
                .store_mut()
                .get_identity_key_pair()
                .await
                .map_err(|_| Error::Corrupt)?,
        );
        let state = AccountState {
            relay,
            onion_relay: None,
            use_tor_relay: false,
            protocol_name,
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: false,
            last_backup_unix: 0,
            receive_queue_id: None,
            receive_queue_token: None,
            mail_handle: None,
            vault_name: None,
            revoked: Vec::new(),
            pending_invites: Vec::new(),
            pending_intro_requests: Vec::new(),
            pending_intros: Vec::new(),
            pending_reach: Vec::new(),
            pending_conclave: Vec::new(),
            regrant_all: false,
            identity_restored: false,
            places: Vec::new(),
        };
        let recovery_code = generate_recovery_code();
        ensure_recovery_files(dir, &master, &recovery_code)?;
        let chat_log = ChatLog::load(dir, &master)?;
        let mut account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code: Some(recovery_code),
            relay_socks: None,
            chat_retention: ChatRetention::default(),
            relay_client_cache: std::sync::Mutex::new(None),
            drain_supported: None,
        };
        account.save().await?;
        // First prekey publish, so the relay directory can serve the pointer
        // invite from the moment the vault exists. Best effort: a relay that
        // is down costs the directory entry, not the vault — the next unlock
        // publishes again.
        if let Err(e) = account.publish_prekeys(64).await {
            eprintln!("hush: initial prekey publish skipped: {e}");
        }
        Ok(account)
    }

    pub async fn open(dir: &Path, passphrase: &str) -> Result<Self, Error> {
        let vault = Vault::open(dir, passphrase)?;
        Self::finish_open(dir, vault).await
    }

    pub async fn open_with_recovery(dir: &Path, recovery_code: &str) -> Result<Self, Error> {
        let master = unwrap_master_with_recovery(dir, recovery_code)?;
        let vault = Vault::open_with_master(dir, master)?;
        Self::finish_open(dir, vault).await
    }

    /// Rotate the vault passphrase.
    ///
    /// The master key is re-wrapped, not replaced, so `account.enc`,
    /// `signal.enc`, the chat log, every blob and any recovery code keep
    /// working. The account must already be open: the caller proves ownership
    /// with `current`, and a locked vault has no master key to re-wrap.
    pub fn change_passphrase(&self, current: &str, new: &str) -> Result<(), Error> {
        self.vault.change_passphrase(current, new)
    }

    async fn finish_open(dir: &Path, vault: Vault) -> Result<Self, Error> {
        let master = *vault.master_key();
        let packed = fs::read(dir.join("account.enc")).map_err(|_| Error::NotFound)?;
        let state_bytes = decrypt(&master, &packed)?;
        let mut state: AccountState =
            serde_json::from_slice(&state_bytes).map_err(|_| Error::Corrupt)?;

        let mut migrated = false;
        let mut fresh_device = false;
        let device = match DeviceKeys::load(dir, &master).await {
            Ok(device) => device,
            Err(Error::NotFound) if dir.join(LEGACY_DEVICE_FILE).exists() => {
                migrated = true;
                migrate_legacy_device(dir, &master, &mut state).await?
            }
            Err(Error::NotFound) => {
                // A restored backup carries the shared vault but not this
                // device's keys, by design: a backup must never hand our
                // identity private key to another device. Mint a new device
                // identity so the restored vault is usable.
                fresh_device = true;
                DeviceKeys::create(dir, &master).await?
            }
            Err(e) => return Err(e),
        };

        let chat_log = ChatLog::load(dir, &master)?;

        // A rotated queue invalidates any cached invite, which names the old
        // inbox. There is no device list to republish, so the only way peers
        // learn the new inbox is a fresh invite: clearing the cache here makes
        // the next `invite()` mint one. That needs no relay, which keeps open
        // offline-capable.
        let queue_rotated = device.queue_rotated();
        if queue_rotated {
            state.invite_cache = None;
        }
        // A rotated queue also invalidates every grant this vault handed out:
        // the per-contact queues are derived from the new read token, so peers
        // holding the old grant deliver into a queue nothing drains. Both ends
        // see success, so the blob simply sits until its TTL. Re-granting is the
        // only repair, and it needs the relay, so it is queued for the daemon
        // rather than done here — open stays offline-capable.
        //
        // Only a rotation is queued, never a fresh device. A rotation keeps the
        // identity key and every session, so peers verify the re-grant against
        // the key they already pinned. A fresh device mints a **new** identity,
        // and `apply_queue_grant` verifies the grant against the key pinned in
        // the peer's invite — so every re-grant is silently refused, and the
        // sweep would clear its own flag on a delivery the peer discarded. That
        // case is a re-add, not a re-grant; it is reported instead.
        if queue_rotated {
            state.regrant_all = true;
        }
        if fresh_device {
            state.identity_restored = true;
            eprintln!(
                "hush: this vault was restored without its device keys, so it has a new \
                 identity. Every contact pinned the previous one and will refuse its \
                 grants and messages: re-add each contact to re-establish the relationship."
            );
        }

        let mut pending_recovery_code = None;
        if !recovery_files_exist(dir) {
            let code = generate_recovery_code();
            ensure_recovery_files(dir, &master, &code)?;
            state.recovery_acknowledged = false;
            pending_recovery_code = Some(code);
        }

        let needs_save =
            migrated || fresh_device || queue_rotated || pending_recovery_code.is_some();
        let mut account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code,
            relay_socks: None,
            chat_retention: ChatRetention::default(),
            relay_client_cache: std::sync::Mutex::new(None),
            drain_supported: None,
        };
        if needs_save {
            account.save().await?;
        }
        // Fill legacy records before anything can read them: a conclave written
        // before consent and governance has nicknames only and no convener, so it
        // gets trustees and a convener here. Saves the log itself when it changes.
        account.hydrate_conclaves()?;
        if migrated {
            // Safe only now that `device.enc` holds the same identity: the
            // legacy file carries the identity private key and would otherwise
            // ride along in the next backup.
            let _ = fs::remove_file(dir.join(LEGACY_DEVICE_FILE));
        }
        Ok(account)
    }

    pub fn recovery_acknowledged(&self) -> bool {
        self.state.recovery_acknowledged
    }

    pub fn take_pending_recovery_code(&mut self) -> Option<String> {
        self.pending_recovery_code.take()
    }

    pub async fn acknowledge_recovery(&mut self) -> Result<(), Error> {
        self.state.recovery_acknowledged = true;
        self.pending_recovery_code = None;
        self.save().await
    }

    pub async fn export_backup(&mut self) -> Result<Vec<u8>, Error> {
        let archive = export_vault(&self.dir)?;
        self.state.last_backup_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.save().await?;
        Ok(archive)
    }

    pub fn last_backup_unix(&self) -> u64 {
        self.state.last_backup_unix
    }

    pub fn import_backup(dir: &Path, archive: &[u8]) -> Result<(), Error> {
        import_vault(dir, archive)
    }

    /// The chat rail's history. Drafts share this encrypted log — that keeps one
    /// unlock path and one backup — but they are not chat.
    pub fn chat_history(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.chat_records().cloned().collect()
    }

    pub fn read_blob(&self, id: &BlobId) -> Result<Vec<u8>, Error> {
        self.vault.get_blob(id)
    }

    /// Read a named encrypted vault sidecar (see
    /// [`Vault::read_encrypted_file`]). For feature-owned state that must be
    /// encrypted at rest; the caller owns the name and the schema inside.
    pub fn read_vault_sidecar(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        self.vault.read_encrypted_file(name)
    }

    /// Write a named encrypted vault sidecar (see
    /// [`Vault::write_encrypted_file`]).
    pub fn write_vault_sidecar(&self, name: &str, plaintext: &[u8]) -> Result<(), Error> {
        self.vault.write_encrypted_file(name, plaintext)
    }

    /// Delete a named encrypted vault sidecar (see
    /// [`Vault::remove_encrypted_file`]). `false` means there was nothing to
    /// delete.
    pub fn remove_vault_sidecar(&self, name: &str) -> Result<bool, Error> {
        self.vault.remove_encrypted_file(name)
    }

    /// Store a file from disk into the vault as a chunked, content-addressed
    /// blob without buffering the whole file in memory. See
    /// [`Vault::put_blob_stream`].
    pub fn store_blob_file(&self, path: &Path) -> Result<BlobId, Error> {
        let mut file = fs::File::open(path)?;
        self.vault.put_blob_stream(&mut file)
    }

    /// Stream a stored blob out to a file on disk. See
    /// [`Vault::read_blob_stream`].
    pub fn write_blob_to_file(&self, id: &BlobId, path: &Path) -> Result<u64, Error> {
        let mut file = fs::File::create(path)?;
        self.vault.read_blob_stream(id, &mut file)
    }

    /// Remove a stored blob. Returns whether anything was there.
    pub fn delete_blob(&self, id: &BlobId) -> Result<bool, Error> {
        self.vault.delete_blob(id)
    }

    /// Whether a blob id is stored in this vault.
    pub fn has_blob(&self, id: &BlobId) -> bool {
        self.vault.has_blob(id)
    }

    pub fn vault_dir(&self) -> &Path {
        &self.dir
    }

    /// The relay base this vault is using right now: the onion address when
    /// Tor relay mode is on, the clearnet base otherwise.
    pub fn relay(&self) -> &str {
        if self.state.use_tor_relay {
            self.state
                .onion_relay
                .as_deref()
                .unwrap_or(&self.state.relay)
        } else {
            &self.state.relay
        }
    }

    /// Whether the relay path is currently switched to Tor.
    pub fn relay_tor_mode(&self) -> bool {
        self.state.use_tor_relay
    }

    /// Whether this account can reach its relay right now, along the path the
    /// switch currently selects.
    ///
    /// Fail-closed by construction: it reuses [`Account::relay_client`], so a
    /// Tor-mode account with no live SOCKS proxy reports unreachable rather
    /// than silently probing over clearnet. The UI's connection light is only
    /// as honest as this answer.
    pub async fn relay_ping(&self, timeout: std::time::Duration) -> bool {
        match self.relay_client() {
            Ok(client) => client.ping(timeout).await,
            Err(_) => false,
        }
    }

    /// Point this vault at a different clearnet relay.
    ///
    /// This is an escape hatch for moving a vault that was created against
    /// another relay (a loopback dogfood relay, say) to the real one. It is
    /// deliberately not part of the app: the mailbox lives on the relay, so
    /// mail from contacts still pointed at the old relay stops meeting this
    /// vault until they move too. The clearnet base only — the Tor switch and
    /// the onion address on file are left exactly as they were.
    pub async fn set_relay_clearnet(&mut self, url: &str) -> Result<(), Error> {
        self.state.relay = normalize_relay_base(url)?;
        // The relay rides inside the invite payload, so a moved mailbox must
        // drop the cached invite — otherwise "Copy my invite" keeps
        // advertising the old address until the process restarts (the same
        // rule `set_mail_handle` and `set_vault_name` follow).
        self.state.invite_cache = None;
        self.save().await
    }

    /// The onion address on file, if any.
    pub fn onion_relay(&self) -> Option<&str> {
        self.state.onion_relay.as_deref()
    }

    /// Apply the Tor relay switch: record the address when given, then pick
    /// which base the relay path uses. Persisted, so the mode survives a
    /// restart. Turning the mode on with no address on file is refused — the
    /// alternative would be silently choosing which network the user's
    /// traffic takes, and that choice is the user's.
    pub async fn set_relay_tor(&mut self, use_tor: bool, onion: Option<&str>) -> Result<(), Error> {
        if let Some(raw) = onion {
            self.state.onion_relay = Some(validate_onion_relay(raw)?);
        }
        if use_tor && self.state.onion_relay.is_none() {
            return Err(Error::Relay(
                "cannot turn on Tor relay mode before an onion address is on file".into(),
            ));
        }
        self.state.use_tor_relay = use_tor;
        self.save().await
    }

    /// The SOCKS proxy the daemon was told the local Tor listens on. Runtime
    /// state only: the daemon re-applies it after unlock and on every mode
    /// change, so it is never persisted and never stale.
    pub fn set_relay_socks(&mut self, socks: Option<String>) {
        self.relay_socks = socks;
    }

    /// Build the relay client this account must use right now.
    ///
    /// Tor mode is fail-closed by design: with the switch on, every request
    /// goes to the onion base through the SOCKS proxy, and a missing half —
    /// no onion address on file, no live Tor SOCKS port — is an error rather
    /// than a silent fall back to clearnet. Only the switch turning the mode
    /// off sends traffic directly.
    ///
    /// The client is cached against the endpoint it was built for and reused,
    /// because a fresh `Client` carries an empty connection pool and its first
    /// request pays a full DNS+TCP+TLS handshake — measured at half a second
    /// against the production relay, where a reused connection costs one
    /// round trip. The cache key is the endpoint pair, so a mode flip or a
    /// new SOCKS port rebuilds instead of serving stale.
    fn relay_client(&self) -> Result<RelayClient, Error> {
        let key = if self.state.use_tor_relay {
            let base = self.state.onion_relay.as_deref().ok_or_else(|| {
                Error::Relay(
                    "Tor relay mode is on, but no onion address is on file for this vault."
                        .into(),
                )
            })?;
            // The daemon carries the shell-reported port in memory; the CLI
            // (which never gets one posted) falls back to the documented env
            // seam. Neither present means fail closed, never direct.
            let socks = self
                .relay_socks
                .clone()
                .or_else(|| std::env::var(crate::relay_client::ENV_RELAY_SOCKS_PROXY).ok())
                .ok_or_else(|| {
                    Error::Relay(
                        "Tor relay mode is on, but Tor is not connected yet. Mail waits in the outbox and sends when it connects."
                            .into(),
                    )
                })?;
            (base.to_string(), Some(socks))
        } else {
            (self.state.relay.clone(), None)
        };
        let mut cached = self
            .relay_client_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match cached.as_ref() {
            Some(hit) if hit.key == key => return Ok(hit.client.clone()),
            _ => {}
        }
        let client = match key.1.as_deref() {
            Some(socks) => RelayClient::try_new_with_proxy(&key.0, Some(socks))?,
            None => RelayClient::try_new_with_proxy(&key.0, None)?,
        };
        *cached = Some(RelayClientCache {
            key: key.clone(),
            client: client.clone(),
        });
        Ok(client)
    }

    pub fn protocol_name(&self) -> &str {
        &self.state.protocol_name
    }

    pub fn mail_handle(&self) -> Option<&str> {
        self.state.mail_handle.as_deref()
    }

    /// The vault's own name as the mail suffix, falling back to
    /// [`DEFAULT_VAULT_NAME`] for vaults created before naming existed.
    pub fn vault_name(&self) -> &str {
        self.state
            .vault_name
            .as_deref()
            .unwrap_or(crate::mail::DEFAULT_VAULT_NAME)
    }

    pub fn mail_address(&self) -> Option<String> {
        self.state
            .mail_handle
            .as_deref()
            .map(|h| crate::mail::format_address(h, self.vault_name()))
    }

    /// Set or change this member's mail handle.
    ///
    /// The cached invite is dropped so the next `invite()` mints one carrying
    /// the new handle; otherwise "Copy my invite" would keep advertising the old
    /// address until the process restarted.
    pub async fn set_mail_handle(&mut self, handle: &str) -> Result<(), Error> {
        self.state.mail_handle = Some(crate::mail::normalize_handle(handle)?);
        self.state.invite_cache = None;
        self.save().await
    }

    /// Name this vault, or rename it.
    ///
    /// The name is the suffix of this member's mail address and travels in the
    /// invite, so the cached invite is dropped exactly as a handle change does.
    /// The vault name is chosen at creation; this exists so a name can be set
    /// right after `create` (the CLI does that) and so a vault created before
    /// naming existed can adopt one.
    pub async fn set_vault_name(&mut self, name: &str) -> Result<(), Error> {
        self.state.vault_name = Some(crate::mail::normalize_vault_name(name)?);
        self.state.invite_cache = None;
        self.save().await
    }

    pub fn contacts(&self) -> &[Contact] {
        &self.state.contacts
    }

    pub fn vault_bytes_on_disk(&self) -> u64 {
        dir_size(&self.dir)
    }

    pub async fn invite(&mut self) -> Result<String, Error> {
        if let Some(cached) = &self.state.invite_cache {
            // A cache entry predating pointer invites holds a full v2 payload.
            // Its fields (relay, inbox, tokens) have not rotated, so it is
            // re-minted in the short format rather than handed out as-is.
            if cached.starts_with("hush1") {
                return Ok(cached.clone());
            }
            self.state.invite_cache = None;
        }
        // The pointer names this account; the peer fetches the prekey record
        // from the relay directory at accept time, so no bundle is minted
        // (or burned) here. The invite still carries the *delivery* token: a
        // contact only ever needs to put messages in this inbox, never to
        // read or burn it.
        let pointer = ShortInvite {
            relay: if normalize_relay(&self.state.relay) == normalize_relay(crate::invite::default_relay())
            {
                None
            } else {
                Some(self.state.relay.clone())
            },
            protocol_name: self.state.protocol_name.clone(),
            queue_id: self.device.queue_id().to_string(),
            queue_token: self.device.queue_token(),
            handle: self.state.mail_handle.clone(),
            vault: Some(self.vault_name().to_string()),
        };
        let encoded = pointer.encode()?;
        self.state.invite_cache = Some(encoded.clone());
        self.save().await?;
        Ok(encoded)
    }

    /// A one-consumer invite, minted on demand.
    ///
    /// Unlike [`Self::invite`], this never touches the shared cache: the
    /// bundle inside burns a one-time prekey, so an invite can be safely
    /// consumed by exactly one peer. Intro cards ride this, because the
    /// card's recipient is by definition a second consumer — a cached
    /// invite would point their session at a prekey this vault may have
    /// already deleted, leaving every message they send undecryptable.
    pub async fn fresh_invite(&mut self) -> Result<String, Error> {
        let local_id = self.device.device_id();
        let bundle = mint_prekey_bundle(self.device.store_mut(), local_id).await?;
        // The invite carries the *delivery* token: a contact only ever needs to
        // put messages in this inbox, never to read or burn it.
        let payload = invite_from_bundle(
            &self.state.relay,
            &self.state.protocol_name,
            self.device.queue_id(),
            &self.device.queue_token(),
            self.state.mail_handle.as_deref(),
            Some(self.vault_name()),
            &bundle,
        )?;
        let encoded = payload.encode()?;
        self.save().await?;
        Ok(encoded)
    }

    /// Places recorded in this vault (minted by me and who minted me).
    pub fn places(&self) -> &[PlaceRecord] {
        &self.state.places
    }

    /// Adopt another vault as a Place of this one.
    ///
    /// Scans `invite_str` (D15), adds the vault as a contact if needed, signs a
    /// vouch hop, stores a MintedByMe record, and sends [`MessagePayload::PlaceAdopt`]
    /// carrying our invite + chain.
    ///
    /// The adopted vault can only decrypt that message once it has added **this**
    /// vault as a contact (inbox blobs carry no sender id). The UI must show our
    /// invite to the phone after adopt so it can add us; then its next `recv`
    /// applies PlaceAdopt and records MintedMe (D6). A later sealed-bootstrap
    /// can collapse that to one scan without changing the vouch model.
    pub async fn adopt_place(
        &mut self,
        nickname: &str,
        invite_str: &str,
        label: &str,
    ) -> Result<(), Error> {
        let nickname = nickname.trim();
        if nickname.is_empty() {
            return Err(Error::InvalidHandle);
        }
        let payload = self.decode_invite(invite_str).await?;
        let place_id = payload.protocol_name.clone();
        if place_id == self.state.protocol_name {
            return Err(Error::InvalidPlace);
        }
        if self.state.places.iter().any(|p| p.account_id == place_id) {
            return Err(Error::PlaceExists);
        }

        if !self
            .state
            .contacts
            .iter()
            .any(|c| c.account_id == place_id)
        {
            self.add_contact(nickname, invite_str).await?;
        }

        let key = self.device.identity_key_pair().await?;
        let mut chain = self
            .state
            .places
            .iter()
            .find(|p| matches!(p.role, PlaceRole::MintedMe))
            .map(|p| p.chain.clone())
            .unwrap_or_default();
        // Keep room for the new hop under the badge cap (D13).
        while chain.hops.len() >= crate::place::MAX_PLACE_CHAIN_HOPS {
            chain.hops.remove(0);
        }
        chain.sign_hop(
            &key,
            crate::place::PlaceHop {
                minter_account_id: self.state.protocol_name.clone(),
                place_account_id: place_id.clone(),
                label: label.to_string(),
                signed_unix: crate::place::place_now_unix(),
            },
        )?;

        let nick = self
            .contact_nick_for_protocol(&place_id)
            .unwrap_or_else(|| nickname.to_string());
        self.state.places.push(PlaceRecord {
            account_id: place_id,
            nickname: nick.clone(),
            label: label.to_string(),
            role: PlaceRole::MintedByMe,
            chain: chain.clone(),
            added_unix: crate::place::place_now_unix(),
        });

        let our_invite = self.invite().await?;
        self.send_payload_inner(
            &nick,
            MessagePayload::PlaceAdopt {
                invite: our_invite,
                chain,
                label: label.to_string(),
            },
            false,
        )
        .await?;
        self.save().await?;
        Ok(())
    }

    /// Drop a Place from My places. The contact row is left alone (D12).
    pub async fn detach_place(&mut self, account_id: &str) -> Result<(), Error> {
        let before = self.state.places.len();
        self.state.places.retain(|p| p.account_id != account_id);
        if self.state.places.len() == before {
            return Err(Error::PlaceNotFound);
        }
        self.save().await?;
        Ok(())
    }

    /// Introduce one of our MintedByMe Places to a contact (D8, D16).
    pub async fn introduce_place(
        &mut self,
        place_account_id: &str,
        contact_nickname: &str,
    ) -> Result<(), Error> {
        let place = self
            .state
            .places
            .iter()
            .find(|p| {
                p.account_id == place_account_id && matches!(p.role, PlaceRole::MintedByMe)
            })
            .cloned()
            .ok_or(Error::PlaceNotFound)?;
        if self
            .state
            .contacts
            .iter()
            .find(|c| c.nickname == contact_nickname)
            .is_none()
        {
            return Err(Error::ContactNotFound);
        }
        let invite = self.pointer_invite_for_account(&place.account_id)?;
        self.send_payload_inner(
            contact_nickname,
            MessagePayload::PlaceIntroduce {
                invite,
                chain: place.chain,
                label: place.label,
            },
            false,
        )
        .await?;
        Ok(())
    }

    /// Rebuild a pointer invite for a contact we already hold (for introduce).
    fn pointer_invite_for_account(&self, account_id: &str) -> Result<String, Error> {
        let contact = self
            .state
            .contacts
            .iter()
            .find(|c| c.account_id == account_id)
            .ok_or(Error::ContactNotFound)?;
        let default = crate::invite::default_relay();
        let relay = if normalize_relay(&self.state.relay) == normalize_relay(default) {
            None
        } else {
            Some(self.state.relay.clone())
        };
        ShortInvite {
            relay,
            protocol_name: contact.account_id.clone(),
            queue_id: contact.queue_id.clone(),
            queue_token: contact.queue_token.clone(),
            handle: contact.mail_handle.clone(),
            vault: contact.mail_vault.clone(),
        }
        .encode()
    }

    /// Apply a PlaceAdopt from the minting Place (we are the adopted vault).
    async fn apply_place_adopt(
        &mut self,
        from_account: &str,
        invite: &str,
        chain: crate::place::VouchChain,
        label: &str,
    ) -> Result<bool, Error> {
        chain.verify()?;
        let leaf = chain.leaf_account_id().ok_or(Error::InvalidPlace)?;
        if leaf != self.state.protocol_name {
            return Err(Error::InvalidPlace);
        }
        let last = chain.hops.last().ok_or(Error::InvalidPlace)?;
        if last.hop.minter_account_id != from_account {
            return Err(Error::InvalidPlace);
        }
        if self
            .state
            .places
            .iter()
            .any(|p| p.account_id == from_account)
        {
            return Ok(false);
        }

        let base_nick = if label.trim().is_empty() {
            "place".to_string()
        } else {
            label.trim().to_string()
        };
        let nick = self.unique_contact_nick(&base_nick);

        if !self
            .state
            .contacts
            .iter()
            .any(|c| c.account_id == from_account)
        {
            self.add_contact(&nick, invite).await?;
        }

        let stored_nick = self
            .contact_nick_for_protocol(from_account)
            .unwrap_or(nick);
        self.state.places.push(PlaceRecord {
            account_id: from_account.to_string(),
            nickname: stored_nick,
            label: label.to_string(),
            role: PlaceRole::MintedMe,
            chain,
            added_unix: crate::place::place_now_unix(),
        });
        self.save().await?;
        Ok(true)
    }

    /// Apply a PlaceIntroduce from a known contact.
    async fn apply_place_introduce(
        &mut self,
        from_account: &str,
        invite: &str,
        chain: crate::place::VouchChain,
        label: &str,
    ) -> Result<bool, Error> {
        if self.contact_nick_for_protocol(from_account).is_none() {
            return Err(Error::ContactNotFound);
        }
        chain.verify()?;
        let leaf = chain
            .leaf_account_id()
            .ok_or(Error::InvalidPlace)?
            .to_string();
        if leaf == self.state.protocol_name {
            return Err(Error::InvalidPlace);
        }

        let trusted: Vec<&str> = self
            .state
            .contacts
            .iter()
            .map(|c| c.account_id.as_str())
            .collect();
        let anchor = chain
            .trusted_anchor(&trusted)
            .map(|s| s.to_string());

        let mut changed = false;
        if !self.state.contacts.iter().any(|c| c.account_id == leaf) {
            let base = if label.trim().is_empty() {
                short_name(&leaf)
            } else {
                label.trim().to_string()
            };
            let nick = self.unique_contact_nick(&base);
            self.add_contact(&nick, invite).await?;
            changed = true;
        }
        if let Some(anchor_id) = anchor {
            if let Some(contact) = self
                .state
                .contacts
                .iter_mut()
                .find(|c| c.account_id == leaf)
            {
                if contact.place_of.as_deref() != Some(anchor_id.as_str()) {
                    contact.place_of = Some(anchor_id);
                    changed = true;
                }
            }
        }
        if changed {
            self.save().await?;
        }
        Ok(changed)
    }

    fn unique_contact_nick(&self, base: &str) -> String {
        if !self.state.contacts.iter().any(|c| c.nickname == base) {
            return base.to_string();
        }
        for i in 2..1000 {
            let candidate = format!("{base}-{i}");
            if !self.state.contacts.iter().any(|c| c.nickname == candidate) {
                return candidate;
            }
        }
        format!("{base}-{}", &Uuid::new_v4().to_string()[..8])
    }

    pub async fn add_contact(&mut self, nickname: &str, invite_str: &str) -> Result<(), Error> {
        let payload = self.decode_invite(invite_str).await?;
        self.validate_contact(nickname, &payload)?;
        let bundle = payload.to_prekey_bundle()?;
        let local_name = self.state.protocol_name.clone();
        process_contact_bundle(
            self.device.store_mut(),
            &local_name,
            &payload.protocol_name,
            &bundle,
        )
        .await?;
        self.finish_contact(nickname, payload).await?;
        // Hand this account's per-contact queue to the contact, which is what
        // makes deleting them revocable. Best effort and one-shot: nothing
        // re-sends this, so a relay that is down here leaves the peer
        // delivering to our inbox until it drains a message from us.
        match self.grant_queues_to(nickname).await {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "hush: the queue grant to {nickname} did not reach the relay; they \
                 keep delivering to our inbox until they next drain a message"
            ),
            Err(e) => eprintln!("hush: could not grant a queue to {nickname}: {e}"),
        }
        Ok(())
    }

    /// Add a contact from an intro card *without* consuming the invite's
    /// prekey bundle.
    ///
    /// The reverse half of an introduction: the sender already consumed our
    /// invite, so their messages will arrive as a prekey chain we have never
    /// seen. libsignal refuses an incoming prekey message when a session for
    /// that address already exists, so we must not create one here — the
    /// contact record pins identity and queue routing only, and the session
    /// builds from their first message. Nothing is granted back: our queue
    /// credentials travelled with the invite they consumed.
    pub async fn add_contact_from_intro(
        &mut self,
        nickname: &str,
        invite_str: &str,
    ) -> Result<(), Error> {
        // Intro cards carry a full v2 payload inline (the card rides an
        // already-encrypted session, so there is no size pressure), and the
        // reverse half must not build a session — so no pointer resolution.
        let payload = InvitePayload::decode(invite_str)?;
        self.validate_contact(nickname, &payload)?;
        self.finish_contact(nickname, payload).await
    }

    /// Decode any accepted invite format into a full payload.
    ///
    /// A pointer invite (`hush1…`) names the account only: the prekey record
    /// is fetched from the relay's directory at accept time, which makes this
    /// the one place adding a contact can fail for a network reason. Legacy
    /// v1/v2 payloads are decoded locally as before.
    async fn decode_invite(&self, invite_str: &str) -> Result<InvitePayload, Error> {
        let trimmed = invite_str.trim();
        if trimmed.len() > 5 && trimmed[..5].eq_ignore_ascii_case("hush1") {
            if let Ok(pointer) = ShortInvite::decode(trimmed) {
                let relay = pointer.effective_relay();
                if normalize_relay(&relay) != normalize_relay(&self.state.relay) {
                    return Err(Error::RelayMismatch(relay, self.state.relay.clone()));
                }
                let client = self.relay_client()?;
                return pointer.resolve(&client).await;
            }
            // Not a valid pointer: fall through to the legacy decoders, since
            // a v1 base64 payload could coincidentally begin with "hush1".
        }
        InvitePayload::decode(trimmed)
    }

    /// Validate a decoded invite for a new contact.
    ///
    /// Shared by [`Self::add_contact`] and [`Self::add_contact_from_intro`]:
    /// the relay must match, and the account must be new.
    fn validate_contact(&self, nickname: &str, payload: &InvitePayload) -> Result<(), Error> {
        if normalize_relay(&payload.relay) != normalize_relay(&self.state.relay) {
            return Err(Error::RelayMismatch(payload.relay.clone(), self.state.relay.clone()));
        }
        if self
            .state
            .contacts
            .iter()
            .any(|c| c.account_id == payload.protocol_name)
        {
            return Err(Error::ContactExists);
        }
        if nickname.trim().is_empty() {
            return Err(Error::InvalidHandle);
        }
        Ok(())
    }

    /// Store a validated contact record and persist it.
    async fn finish_contact(
        &mut self,
        nickname: &str,
        payload: InvitePayload,
    ) -> Result<(), Error> {
        self.state.contacts.push(Contact {
            nickname: nickname.to_string(),
            account_id: payload.protocol_name.clone(),
            identity_key: payload.identity_key.clone(),
            queue_id: payload.queue_id,
            queue_token: payload.queue_token,
            // No grant yet: the contact has not told us where it wants our
            // messages for it, so until it does we deliver to its inbox.
            delivery_queue_id: None,
            delivery_queue_token: None,
            mail_handle: payload.handle.clone(),
            mail_vault: payload.vault.clone(),
            heard_from: false,
            reach: TrusteeReach::Full,
            peer_reach: TrusteeReach::Full,
            place_of: None,
        });
        // Adding a contact clears any revocation left from a previous delete, so
        // a re-added contact is granted a queue again instead of being treated
        // as still blocked.
        self.state.revoked.retain(|a| a != &payload.protocol_name);
        self.save().await?;
        Ok(())
    }

    /// Set how this vault reaches a trustee, and tell them (Task 24).
    ///
    /// The choice is this vault's own: it narrows what this account will use
    /// with them. It is announced so the peer's UI can mirror it, but the
    /// effective reach for the pair is the narrower of the two sides' choices,
    /// so a `MailOnly` here can only ever remove surfaces, never grant them.
    ///
    /// The announcement is best effort. The local choice stands even if the
    /// relay is down, and there is nothing to roll back: a `Reach` the peer
    /// never received just leaves them assuming `Full`, which is the safe
    /// direction — they may try a chat, and this vault refuses it at the
    /// boundary.
    pub async fn set_contact_reach(
        &mut self,
        nickname: &str,
        reach: TrusteeReach,
    ) -> Result<(), Error> {
        let changed = {
            let contact = self
                .state
                .contacts
                .iter_mut()
                .find(|c| c.nickname == nickname)
                .ok_or(Error::ContactNotFound)?;
            if contact.reach == reach {
                false
            } else {
                contact.reach = reach;
                true
            }
        };
        if !changed {
            return Ok(());
        }
        // Queue the announcement instead of making the caller wait for it.
        // The choice itself is local and saved below; the peer's copy is
        // store-and-forward — "the peer may be away" — so delivering it is
        // the outbox's job, not this call's. A slow relay used to hold the
        // user's toggle (and, with it, the whole account worker) for up to
        // three 15-second delivery attempts.
        self.state.pending_reach.retain(|p| p.nickname != nickname);
        self.state.pending_reach.push(PendingReach {
            nickname: nickname.to_string(),
            scope: reach,
        });
        self.save().await
    }

    /// Forget a contact without revoking it.
    ///
    /// Used where a contact must simply stop being surfaced (and by the older
    /// callers). [`Account::revoke_contact`] is the one that also burns the
    /// contact's queues, and is what the Trusts delete flow calls.
    pub async fn remove_contact(&mut self, nickname: &str) -> Result<(), Error> {
        let pos = self
            .state
            .contacts
            .iter()
            .position(|c| c.nickname == nickname)
            .ok_or(Error::ContactNotFound)?;
        self.state.contacts.remove(pos);
        // A queued reach announcement for a contact that no longer exists
        // could never deliver; dropping it here keeps the drain pass quiet.
        self.state.pending_reach.retain(|p| p.nickname != nickname);
        self.save().await?;
        Ok(())
    }

    /// Delete a contact and revoke its ability to deliver to this account.
    ///
    /// This burns the per-contact queue the contact was handed, so there is no
    /// queue left for it to deliver into. The contact's account id is recorded
    /// so the shared inbox cannot be used as a bypass (see
    /// [`AccountState::revoked`]).
    ///
    /// It does **not** burn this account's inbox. The inbox is shared by every
    /// contact, so burning it would cut off everyone at once, and a contact that
    /// has not yet picked up its grant still delivers there. That is the
    /// migration limit: a contact is fully revoked once it is on its per-contact
    /// queue.
    pub async fn revoke_contact(&mut self, nickname: &str) -> Result<(), Error> {
        let pos = self
            .state
            .contacts
            .iter()
            .position(|c| c.nickname == nickname)
            .ok_or(Error::ContactNotFound)?;
        let account_id = self.state.contacts[pos].account_id.clone();
        self.state.contacts.remove(pos);
        // A queued reach announcement for a revoked contact would keep
        // failing against the burned queue forever; the revocation is the
        // stronger statement about the relationship.
        self.state.pending_reach.retain(|p| p.nickname != nickname);
        if !self.state.revoked.contains(&account_id) {
            self.state.revoked.push(account_id.clone());
        }
        self.save().await?;
        self.burn_contact_queue(&account_id).await?;
        Ok(())
    }

    /// Burn this device's per-contact queue for one account.
    ///
    /// Best effort on a queue that was never created: there is nothing to burn,
    /// and the relay reports that the same way it reports an already-burned
    /// queue. A relay failure is returned so the caller can decide; the local
    /// revocation is already recorded, so the next drain re-drives it.
    async fn burn_contact_queue(&self, account_id: &str) -> Result<(), Error> {
        let relay = self.relay_client()?;
        relay
            .burn_if_exists(
                &self.device.contact_queue_id(account_id),
                &self.device.contact_queue_read_token(account_id),
            )
            .await
    }

    /// Re-drive the burns for every revoked contact.
    ///
    /// A delete burns immediately; if the relay was down the record is already
    /// saved, so this runs before a drain and burns whatever is still live.
    /// Burning is idempotent, so a contact whose queue is already gone is a
    /// no-op. An account id stays in the revoked set afterwards: it is also the
    /// filter that drops stragglers sent to the shared inbox.
    async fn burn_revoked_queues(&mut self) -> Result<(), Error> {
        if self.state.revoked.is_empty() {
            return Ok(());
        }
        let revoked = self.state.revoked.clone();
        for account_id in revoked {
            if let Err(e) = self.burn_contact_queue(&account_id).await {
                eprintln!(
                    "hush: could not burn revoked queue for {}: {e}",
                    short_name(&account_id)
                );
            }
        }
        Ok(())
    }

    pub async fn send_text(&mut self, nickname: &str, text: &str) -> Result<(), Error> {
        self.send_payload(
            nickname,
            MessagePayload::Text {
                body: text.to_string(),
            },
        )
        .await?;
        let master = *self.vault.master_key();
        self.chat_log
            .append_text(&master, nickname, true, text)?;
        self.enforce_chat_retention();
        Ok(())
    }

    pub async fn send_file(&mut self, nickname: &str, path: &Path) -> Result<(), Error> {
        let data = fs::read(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        self.send_file_bytes(nickname, &name, &data).await?;
        Ok(())
    }

    pub async fn send_file_bytes(
        &mut self,
        nickname: &str,
        name: &str,
        data: &[u8],
    ) -> Result<BlobId, Error> {
        if data.len() > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let blob_id = self.vault.put_blob(data)?;
        self.send_payload(
            nickname,
            MessagePayload::File {
                name: name.to_string(),
                data: data.to_vec(),
            },
        )
        .await?;
        let master = *self.vault.master_key();
        self.chat_log.append_file(
            &master,
            nickname,
            true,
            name,
            &blob_id,
            data.len() as u64,
        )?;
        self.enforce_chat_retention();
        Ok(blob_id)
    }

    /// Send an internal mail message, returning the thread id.
    ///
    /// Mail is async by design: unlike `send_text`, this does not check whether
    /// the contact is online. Attachments are inlined on the wire (capped) and
    /// also stored as encrypted vault blobs on our side so the sent message is
    /// openable after a restart.
    /// Send an internal mail message.
    ///
    /// The account's published drop key, if one has been created.
    pub fn drop_public_key(&self) -> Option<String> {
        self.vault.drop_public_key()
    }

    /// Create the account's drop keypair when it does not have one yet.
    ///
    /// Returns the published form of the current key either way — the string a
    /// member pastes into a forum bio or a profile page. For a "generate"
    /// button that must fail loudly on a second press, use [`Account::
    /// create_drop_keys`], which keeps the vault's strict semantics.
    pub fn ensure_drop_keys(&mut self) -> Result<String, Error> {
        if let Some(pk) = self.vault.drop_public_key() {
            return Ok(pk);
        }
        self.vault.create_drop_keypair()?;
        self.vault.drop_public_key().ok_or(Error::DropKeyMissing)
    }

    /// Create the account's drop keypair, erroring when one already exists.
    ///
    /// This is the API/CLI "generate" behavior: a second call is a mistake the
    /// caller needs to know about, not something to quietly absorb — a key
    /// that silently stays the same means someone thinks they rotated when
    /// they did not.
    pub fn create_drop_keys(&mut self) -> Result<String, Error> {
        self.vault.create_drop_keypair()
    }

    /// Rotate the drop keypair: the old key is retired, not deleted.
    ///
    /// In-flight drops sealed to the published key before the rotation drain
    /// until the relay's TTL expires them, so publishing the new string never
    /// orphans a drop that was already sent.
    pub fn rotate_drop_keys(&mut self) -> Result<String, Error> {
        self.vault.rotate_drop_keypair()
    }

    /// The zines pen name this vault publishes under, if it has one yet.
    pub fn pen_name(&self) -> Option<crate::PenName> {
        self.vault.pen_name()
    }

    /// Mint a pen name (refused when one already exists).
    pub fn create_pen_name(&mut self, name: &str) -> Result<crate::PenName, Error> {
        self.vault.create_pen_name(name)
    }

    /// Rename the current pen name. The keys do not move, so nothing already
    /// published under this byline changes its address.
    pub fn rename_pen_name(&mut self, name: &str) -> Result<crate::PenName, Error> {
        self.vault.rename_pen_name(name)
    }

    /// The pen name's seal keypair, for opening what is sealed to it.
    ///
    /// A comment is sealed to the pen name's *public* seal key, so only this
    /// vault can open one addressed to it. `None` when there is no pen name.
    pub fn pen_name_seal_keypair(&self) -> Option<crate::DropKeyPair> {
        self.vault.pen_name_seal_keypair()
    }

    /// Sign bytes as the pen name, for provenance on a published blob.
    pub fn pen_name_sign(&self, msg: &[u8]) -> Option<Vec<u8>> {
        self.vault.pen_name_sign(msg)
    }

    /// Drain every drop slot this account still holds a key for.
    ///
    /// One pass: for each drain key — current first, then retired ones — fetch
    /// the container parked on its rendezvous slot, open it, file it into the
    /// chat log, then consume it. A container that will not open stays parked:
    /// deleting on failure would let a hostile dropper destroy evidence by
    /// sealing garbage, and the slot only ever holds one container, so a bad
    /// one cannot wedge later drops either. The daemon decides when the next
    /// pass runs.
    pub async fn drain_drops(&mut self) -> Result<DropInboxReport, Error> {
        let relay = self.relay_client()?;
        let keys = self.vault.drop_keys_for_drain();
        let mut report = DropInboxReport::default();
        for key in &keys {
            let slot = crate::rendezvous_id(&key.pk);
            let Some(body) = relay.get_drop(&slot).await? else {
                continue;
            };
            report.hit_slots.push(slot.clone());
            if self.open_drop(key, &slot, &body)? {
                relay.delete_drop(&slot).await?;
            } else {
                report.remaining += 1;
            }
        }
        Ok(report)
    }

    /// Open a drop container and file it as a quarantined `Drop` record.
    ///
    /// Returns `Ok(false)` when the container is not for this key or is
    /// malformed: nothing is filed, nothing is deleted, the container stays
    /// parked. Content problems *inside* an opened container are clamped
    /// rather than rejected — an oversized attachment is skipped with a note,
    /// never a drain failure — so one crafted drop cannot park itself in the
    /// member's drain forever by refusing to parse.
    fn open_drop(
        &mut self,
        key: &DropDrainKey,
        slot: &str,
        body: &[u8],
    ) -> Result<bool, Error> {
        let pair = key.keypair();
        let pkg = match crate::open_with_key(body, &pair) {
            Ok(pkg) => pkg,
            Err(
                Error::DropNotForThisKey | Error::BadDrop | Error::UnsupportedDrop,
            ) => return Ok(false),
            Err(e) => return Err(e),
        };

        let subject_raw = clamp_drop_text(pkg.subject.trim(), MAX_DROP_SUBJECT_CHARS);
        let subject = if subject_raw.is_empty() {
            None
        } else {
            Some(subject_raw)
        };
        let mut body_text = clamp_drop_text(&pkg.body, MAX_DROP_BODY_CHARS);

        let master = *self.vault.master_key();
        let mut records = Vec::new();
        let mut notes = Vec::new();
        for (i, attachment) in pkg.attachments.iter().enumerate() {
            if records.len() == MAX_MAIL_ATTACHMENTS {
                notes.push(format!(
                    "{} more file(s) skipped: a drop carries at most {MAX_MAIL_ATTACHMENTS}",
                    pkg.attachments.len() - i,
                ));
                break;
            }
            let data = match BASE64_STANDARD.decode(&attachment.data) {
                Ok(data) => data,
                Err(_) => {
                    notes.push(format!(
                        "file {} skipped: not decodable",
                        sanitize_drop_name(&attachment.name),
                    ));
                    continue;
                }
            };
            if data.len() > MAX_INLINE_FILE_BYTES {
                notes.push(format!(
                    "{} skipped: larger than the {} KiB inline cap",
                    sanitize_drop_name(&attachment.name),
                    MAX_INLINE_FILE_BYTES / 1024,
                ));
                continue;
            }
            let name = sanitize_drop_name(&attachment.name);
            let blob_id = self.vault.put_blob(&data)?;
            records.push(MailAttachmentRecord {
                blob_id_hex: hex::encode(blob_id.0),
                name,
                size: data.len() as u64,
            });
        }
        if !notes.is_empty() {
            if !body_text.is_empty() {
                body_text.push_str("\n\n");
            }
            body_text.push_str("[attachments]\n");
            body_text.push_str(&notes.join("\n"));
        }

        self.chat_log.append_drop(
            &master,
            slot,
            subject.as_deref(),
            &body_text,
            &records,
            pkg.created_unix,
        )?;
        Ok(true)
    }

    /// Mail is async by design: unlike `send_text`, this does not check whether
    /// the contact is online. When the relay refuses the ciphertext the message
    /// is recorded as `Queued` and reported as such rather than failing, so the
    /// Outbox can retry it later. Only failures no amount of retrying would fix
    /// are returned as errors.
    pub async fn send_mail(
        &mut self,
        nickname: &str,
        thread_id: Option<&str>,
        subject: Option<&str>,
        body: &str,
        attachments: &[(String, Vec<u8>)],
    ) -> Result<MailOutcome, Error> {
        if attachments.len() > MAX_MAIL_ATTACHMENTS {
            return Err(Error::TooManyAttachments(MAX_MAIL_ATTACHMENTS));
        }
        let total: usize = attachments.iter().map(|(_, d)| d.len()).sum();
        if total > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        // Fail fast on an unknown recipient: a retry cannot conjure a contact.
        if self.find_contact(nickname).is_none() {
            return Err(Error::ContactNotFound);
        }
        let thread_id = thread_id
            .map(|t| t.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        // Attachment bytes are stored before the record because the record holds
        // blob ids: a queued message has to be able to find its bytes again on
        // the retry. `put_blob` is content-addressed, so this is idempotent.
        let records = self.store_attachments(attachments)?;
        let wire: Vec<MailAttachment> = attachments
            .iter()
            .map(|(name, data)| MailAttachment {
                name: name.clone(),
                data: data.clone(),
            })
            .collect();

        let state = match self
            .send_payload_inner(
                nickname,
                MessagePayload::Mail {
                    thread_id: thread_id.clone(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: wire,
                },
                false,
            )
            .await
        {
            Ok(()) => MailState::Sent,
            Err(e) if is_worth_retrying(&e) => MailState::Queued,
            Err(e) => return Err(e),
        };

        let master = *self.vault.master_key();
        self.chat_log.append_mail(
            &master, nickname, true, &thread_id, subject, body, &records, state,
        )?;
        Ok(MailOutcome {
            thread_id,
            queued: state == MailState::Queued,
        })
    }

    /// Attempt delivery of every queued message again, marking the ones the
    /// relay accepts as `Sent`. Returns how many went out.    ///
    /// The daemon calls this on a timer while the vault is unlocked. A message
    /// that fails again simply stays queued, and one whose attachment blob has
    /// gone missing is left alone rather than silently emptied.
    pub async fn retry_outbox(&mut self) -> Result<usize, Error> {
        // Collected first: the send path needs `&mut self`, so the log cannot
        // stay borrowed while it runs.
        let queued: Vec<QueuedMail> = self
            .chat_log
            .mail_records()
            .filter_map(|r| match &r.payload {
                ChatRecordPayload::Mail {
                    thread_id,
                    subject,
                    body,
                    attachments,
                    state: MailState::Queued,
                } if r.outgoing => Some(QueuedMail {
                    id: r.id.clone(),
                    contact: r.contact_nick.clone(),
                    thread_id: thread_id.clone(),
                    subject: subject.clone(),
                    body: body.clone(),
                    attachments: attachments.clone(),
                }),
                _ => None,
            })
            .collect();

        let mut delivered = 0;
        for mail in queued {
            let mut wire = Vec::with_capacity(mail.attachments.len());
            for a in &mail.attachments {
                let Some(id) = blob_id_from_hex(&a.blob_id_hex) else {
                    wire.clear();
                    break;
                };
                match self.vault.get_blob(&id) {
                    Ok(data) => wire.push(MailAttachment {
                        name: a.name.clone(),
                        data,
                    }),
                    Err(_) => {
                        wire.clear();
                        break;
                    }
                }
            }
            if wire.len() != mail.attachments.len() {
                continue;
            }

            let payload = MessagePayload::Mail {
                thread_id: mail.thread_id.clone(),
                subject: mail.subject.clone(),
                body: mail.body.clone(),
                attachments: wire,
            };
            if self
                .send_payload_inner(&mail.contact, payload, false)
                .await
                .is_err()
            {
                continue;
            }
            let master = *self.vault.master_key();
            self.chat_log.replace_payload(
                &master,
                &mail.id,
                ChatRecordPayload::Mail {
                    thread_id: mail.thread_id,
                    subject: mail.subject,
                    body: mail.body,
                    attachments: mail.attachments,
                    state: MailState::Sent,
                },
                now_ms(),
            )?;
            delivered += 1;
        }
        Ok(delivered)
    }

    /// Attempt delivery of every queued reach announcement (Task 24).
    ///
    /// The daemon calls this from the outbox timer and from the outbox retry
    /// route — nobody waits on the result, the way queued mail is drained. An
    /// entry the relay refuses stays queued for the next pass. One whose
    /// contact is gone is dropped: there is nobody left to announce to, and a
    /// revoked queue would refuse delivery forever.
    ///
    /// Returns how many announcements went out.
    pub async fn retry_pending_reach(&mut self) -> Result<usize, Error> {
        if self.state.pending_reach.is_empty() {
            return Ok(0);
        }
        let queued = std::mem::take(&mut self.state.pending_reach);
        let mut delivered = 0;
        let mut kept = Vec::new();
        for p in queued {
            if self.find_contact(&p.nickname).is_none() {
                continue;
            }
            match self
                .send_payload_inner(
                    &p.nickname,
                    MessagePayload::Reach { scope: p.scope },
                    false,
                )
                .await
            {
                Ok(()) => delivered += 1,
                Err(e) => {
                    eprintln!(
                        "hush: reach announcement to {} not delivered yet: {e}",
                        p.nickname
                    );
                    kept.push(p);
                }
            }
        }
        self.state.pending_reach = kept;
        // Re-arm and save. The success path inside `send_payload_inner` saves
        // too — with the queue momentarily empty — so this final save is what
        // keeps the on-disk queue matching the entries still owed. A crash
        // inside that window can only drop an undelivered announcement, which
        // is the safe direction: the peer then assumes `Full`.
        if delivered > 0 {
            self.save().await?;
        }
        Ok(delivered)
    }

    /// How many messages are waiting for the relay. Drives the Outbox count.
    pub fn queued_mail_count(&self) -> usize {
        self.chat_log
            .mail_records()
            .filter(|r| {
                matches!(
                    r.payload,
                    ChatRecordPayload::Mail {
                        state: MailState::Queued,
                        ..
                    }
                )
            })
            .count()
    }

    /// How many conclave payloads are waiting for the relay.
    ///
    /// The sibling of [`Self::queued_mail_count`], and the other half of the
    /// answer to "did that send land": a conclave fan-out that the relay refused
    /// is queued rather than reported, so the caller's `Ok` says nothing about
    /// delivery. A count that stays above zero across outbox ticks is a fan-out
    /// that is not getting through.
    pub fn pending_conclave_count(&self) -> usize {
        self.state.pending_conclave.len()
    }

    /// Create a draft, or replace an existing one when `id` is set. Returns the
    /// draft's record id.
    pub async fn save_draft(
        &mut self,
        id: Option<&str>,
        thread_id: Option<&str>,
        to: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[(String, Vec<u8>)],
    ) -> Result<String, Error> {
        if attachments.len() > MAX_MAIL_ATTACHMENTS {
            return Err(Error::TooManyAttachments(MAX_MAIL_ATTACHMENTS));
        }
        let total: usize = attachments.iter().map(|(_, d)| d.len()).sum();
        if total > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let records = self.store_attachments(attachments)?;
        let master = *self.vault.master_key();
        let stamp = now_ms();
        let draft = |records: Vec<MailAttachmentRecord>| ChatRecordPayload::Draft {
            thread_id: thread_id.map(|t| t.to_string()),
            to: to.to_string(),
            subject: subject.map(|s| s.to_string()),
            body: body.to_string(),
            attachments: records,
            updated_ms: stamp,
        };

        if let Some(id) = id {
            if self
                .chat_log
                .replace_payload(&master, id, draft(records.clone()), stamp)?
            {
                return Ok(id.to_string());
            }
            // Unknown id: the draft was deleted elsewhere (another window, or a
            // device that linked later). Fall through and write a fresh record
            // rather than losing what was just typed.
        }

        self.chat_log
            .append_draft(&master, thread_id, to, subject, body, &records)
    }

    /// Discard a draft. Returns whether it existed.
    pub async fn delete_draft(&mut self, id: &str) -> Result<bool, Error> {
        let master = *self.vault.master_key();
        self.chat_log.remove_record(&master, id)
    }

    /// Remove a mail message from the log for good, which is what "delete
    /// forever" from Trash means. Attachment blobs it referenced are left in the
    /// vault: there is no collection pass, the same trade chat already makes.
    pub async fn delete_mail(&mut self, id: &str) -> Result<bool, Error> {
        let master = *self.vault.master_key();
        self.chat_log.remove_record(&master, id)
    }

    /// Delete 1:1 chat messages with a contact, optionally bounded to
    /// `[from_ms, to_ms]` — no bounds means the entire history with them.
    /// Returns how many records went. Mail with them is not chat history and
    /// is untouched.
    pub async fn remove_chat_history(
        &mut self,
        nickname: &str,
        from_ms: Option<u64>,
        to_ms: Option<u64>,
    ) -> Result<usize, Error> {
        let master = *self.vault.master_key();
        self.chat_log
            .remove_for_contact(&master, nickname, from_ms, to_ms)
    }

    /// Install the chat auto-delete policy the daemon's prefs file states,
    /// and run it once — unlock is when a "delete every day" rule catches up
    /// with the days the vault spent locked.
    pub async fn set_chat_retention(&mut self, retention: ChatRetention) -> Result<usize, Error> {
        self.chat_retention = retention;
        let master = *self.vault.master_key();
        self.chat_log.retain_for_auto_delete(&master, retention)
    }

    /// Keep the log inside the auto-delete policy after a new chat record
    /// landed. Best-effort: the message is already recorded (and sent), so a
    /// retention failure must not fail the send.
    fn enforce_chat_retention(&mut self) {
        let master = *self.vault.master_key();
        if let Err(e) = self.chat_log.retain_for_auto_delete(&master, self.chat_retention) {
            eprintln!("hush: chat auto-delete pass failed: {e}");
        }
    }

    /// Write attachment bytes to the vault and describe them for the log.
    ///
    /// `put_blob` is content-addressed, so re-saving a draft or re-sending the
    /// same file does not grow the vault.
    fn store_attachments(
        &self,
        attachments: &[(String, Vec<u8>)],
    ) -> Result<Vec<MailAttachmentRecord>, Error> {
        let mut records = Vec::with_capacity(attachments.len());
        for (name, data) in attachments {
            let id = self.vault.put_blob(data)?;
            records.push(MailAttachmentRecord {
                name: name.clone(),
                blob_id_hex: hex::encode(id.0),
                size: data.len() as u64,
            });
        }
        Ok(records)
    }

    pub fn mail_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.mail_records().cloned().collect()
    }

    /// Every filed outsider drop, oldest first.
    pub fn drop_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.drop_records().cloned().collect()
    }

    pub fn draft_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.draft_records().cloned().collect()
    }

    /// Every conclave this vault holds, in creation order.
    ///
    /// Ended conclaves are returned too: they are read-only rather than hidden,
    /// so a peer that received the creator's end broadcast still sees the record
    /// and its history.
    pub fn conclaves(&self) -> Vec<ConclaveRecord> {
        // Ended conclaves are returned too: they are read-only rather than
        // hidden, so a peer that received the creator's end broadcast still sees
        // the record and its history.
        self.chat_log.conclaves().to_vec()
    }

    /// Conclave invitations received but not yet accepted or declined.
    pub fn pending_invites(&self) -> Vec<PendingInvite> {
        self.state.pending_invites.clone()
    }

    /// Create a conclave and return its record.
    ///
    /// The convener is the local account. Every named trustee is added as
    /// `Invited` and sent an invite; nobody is joined until they accept. A
    /// conclave needs a name and at least one other trustee: an empty conclave has
    /// no one to fan out to.
    pub async fn create_conclave(
        &mut self,
        name: &str,
        trustees: &[String],
    ) -> Result<ConclaveRecord, Error> {
        let name = name.trim();
        if name.is_empty() || trustees.is_empty() {
            return Err(Error::InvalidConclave);
        }
        let me = self.state.protocol_name.clone();
        let mut roster = vec![ConclaveTrustee {
            account_id: me.clone(),
            nick: String::new(),
            state: TrusteeState::Joined,
        }];
        for nickname in trustees {
            let contact = self.find_contact(nickname).ok_or(Error::ContactNotFound)?;
            if contact.account_id == me {
                continue;
            }
            // A mail-only relationship has no conclaves (Task 24). The picker
            // already filters these out; this is the boundary that makes it a
            // rule rather than a hidden button.
            if !self.effective_reach(nickname).allows_live() {
                return Err(Error::ReachMailOnly);
            }
            if roster.iter().any(|m| m.account_id == contact.account_id) {
                continue;
            }
            roster.push(ConclaveTrustee {
                account_id: contact.account_id.clone(),
                nick: nickname.clone(),
                state: TrusteeState::Invited,
            });
        }
        if roster.len() < 2 {
            return Err(Error::InvalidConclave);
        }
        let master = *self.vault.master_key();
        let id = self.chat_log.create_conclave(&master, name, &me, &roster)?;
        let record = self
            .chat_log
            .conclave(&id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        let descriptor = descriptor_of(&record);
        // Invite each invitee. Store-and-forward, so an offline invitee still
        // receives it on their next drain.
        for trustee in record.trustees.iter().filter(|m| m.state == TrusteeState::Invited) {
            if self.find_contact(&trustee.nick).is_some() {
                self.send_payload_inner(
                    &trustee.nick,
                    MessagePayload::ConclaveInvite {
                        descriptor: descriptor.clone(),
                    },
                    false,
                )
                .await?;
            }
        }
        Ok(record)
    }

    /// Send a message to every joined trustee of a conclave over their own 1:1
    /// session.
    ///
    /// This is the whole of "group chat": N independent encryptions and N queue
    /// puts. Fan-out targets the joined set only; an invitee who has not
    /// accepted receives the invite and nothing else. An ended conclave is
    /// read-only, so sending in it is refused.
    pub async fn send_conclave_message(
        &mut self,
        conclave_id: &str,
        body: &str,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        let payload = MessagePayload::Conclave {
            conclave_id: conclave.id.clone(),
            name: conclave.name.clone(),
            trustees: conclave
                .joined()
                .map(|t| t.nick.clone())
                .filter(|n| !n.is_empty())
                .collect(),
            body: body.to_string(),
            file: None,
        };
        let out = self.fan_out_conclave(&conclave, &payload, None).await;
        self.warn_conclave_fan_out(conclave_id, &out);
        self.queue_conclave_retry(conclave_id, &payload, out.refused)
            .await?;
        let master = *self.vault.master_key();
        self.chat_log
            .append_conclave_message(&master, conclave_id, true, None, body)?;
        Ok(())
    }

    /// Fan one payload out to a conclave's joined trustees, over their own 1:1
    /// sessions.
    ///
    /// This is the whole of "group messaging": there is no relay-side room and
    /// no group object, so a payload reaches a conclave as N encryptions and N
    /// queue puts. Keeping that in one place is what makes two rules hold for
    /// every payload that rides it — a message, a file, a roster update, an end,
    /// a paper delta:
    ///
    /// - a target the relay refuses does not abandon the rest. Propagating the
    ///   first error with `?` used to lose a whole roster update to one dead
    ///   peer, and the sender was never told;
    /// - a joined trustee with no contact record is reported, not dropped. It
    ///   cannot be retried — there is no session to retry over — so the only fix
    ///   is to add them as a contact, and only a warning can say so.
    ///
    /// `except` skips one trustee by account id, for a payload the sender already
    /// knows that peer has (the paper-delta relay).
    async fn fan_out_conclave(
        &mut self,
        record: &ConclaveRecord,
        payload: &MessagePayload,
        except: Option<&str>,
    ) -> ConclaveFanOut {
        let me = self.state.protocol_name.clone();
        let mut out = ConclaveFanOut::default();
        let targets: Vec<(String, String)> = record
            .joined()
            .filter(|m| m.account_id != me && except != Some(m.account_id.as_str()))
            .map(|m| (m.account_id.clone(), m.nick.clone()))
            .collect();
        for (account_id, nick) in targets {
            let Some(contact_nick) = self.contact_nick_for_protocol(&account_id) else {
                // A joined trustee this vault has no 1:1 session with. The copy
                // has nowhere to go, and it cannot be queued either — a retry
                // would resolve the same missing contact. Report it.
                let shown = if nick.is_empty() {
                    short_name(&account_id)
                } else {
                    nick
                };
                eprintln!(
                    "hush: conclave fan-out to {shown} skipped — no contact record, so no 1:1 session to ride"
                );
                out.unreachable.push(shown);
                continue;
            };
            match self
                .send_payload_inner(&contact_nick, payload.clone(), false)
                .await
            {
                Ok(()) => out.delivered.push(contact_nick),
                Err(e) => {
                    eprintln!(
                        "hush: conclave fan-out to {contact_nick} was refused, will retry: {e}"
                    );
                    out.refused.push(contact_nick);
                }
            }
        }
        out
    }

    /// Say so when a fan-out reached nobody.
    ///
    /// This is the failure the sender cannot otherwise see: the record and the
    /// local log both say the message was sent, and the peer's vault has no way
    /// to report an absence. The daemon log is where it lands.
    fn warn_conclave_fan_out(&self, conclave_id: &str, out: &ConclaveFanOut) {
        if out.reached_nobody() {
            eprintln!(
                "hush: a payload for conclave {conclave_id} reached nobody — no reachable joined trustee"
            );
        }
    }

    /// Record a fan-out's refused targets for the outbox timer, then save.
    ///
    /// An empty list queues nothing: a payload that reached nobody and was
    /// refused by nobody has no target to retry, and only a warning applies.
    async fn queue_conclave_retry(
        &mut self,
        conclave_id: &str,
        payload: &MessagePayload,
        refused: Vec<String>,
    ) -> Result<(), Error> {
        if refused.is_empty() {
            return Ok(());
        }
        let bytes = match payload.encode() {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("hush: a conclave payload could not be queued for retry: {e}");
                return Ok(());
            }
        };
        self.state.pending_conclave.push(PendingConclave {
            conclave_id: conclave_id.to_string(),
            payload: bytes,
            targets: refused,
        });
        self.save().await
    }

    /// Re-send the conclave copies the relay refused, once per outbox tick.
    ///
    /// Returns how many copies went out. A target that is gone — the contact was
    /// removed, the trustee left, or the conclave was ended or deleted — is
    /// dropped rather than retried forever: there is nobody left to reach, and
    /// an ended conclave is read-only on every peer.
    pub async fn retry_pending_conclave(&mut self) -> Result<usize, Error> {
        if self.state.pending_conclave.is_empty() {
            return Ok(0);
        }
        let queued = std::mem::take(&mut self.state.pending_conclave);
        let mut delivered = 0;
        let mut kept = Vec::new();
        for entry in queued {
            let Ok(payload) = MessagePayload::decode(&entry.payload) else {
                eprintln!("hush: dropping an unreadable queued conclave payload");
                continue;
            };
            let Some(record) = self
                .chat_log
                .conclave(&entry.conclave_id)
                .filter(|c| !c.ended)
                .cloned()
            else {
                continue;
            };
            let live: Vec<String> = record
                .joined()
                .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
                .collect();
            let mut still = Vec::new();
            for nick in entry.targets {
                if !live.contains(&nick) {
                    continue;
                }
                match self.send_payload_inner(&nick, payload.clone(), false).await {
                    Ok(()) => delivered += 1,
                    Err(e) => {
                        eprintln!("hush: conclave retry to {nick} still refused: {e}");
                        still.push(nick);
                    }
                }
            }
            if !still.is_empty() {
                kept.push(PendingConclave {
                    conclave_id: entry.conclave_id,
                    payload: entry.payload,
                    targets: still,
                });
            }
        }
        self.state.pending_conclave = kept;
        self.save().await?;
        Ok(delivered)
    }

    /// Re-announce this vault's per-contact queues after the device's queues
    /// rotated, once per outbox tick.
    ///
    /// A rotation replaces this device's queue, so the per-contact queues
    /// derived from the new read token all change, and every grant already
    /// handed out names a queue this vault no longer drains. Neither side
    /// notices: the sender's copy reads `sent` and the blob sits until its TTL.
    /// Re-granting is the only repair, and it is idempotent on the receiving
    /// side, so this retries until every contact has been re-announced.
    ///
    /// Only a rotation reaches here. A rotation keeps the identity key, so the
    /// peer can verify the re-grant against the key it pinned; a restored vault
    /// mints a **new** identity, and its grants are refused on arrival. See
    /// `AccountState::identity_restored`.
    ///
    /// Returns how many contacts were re-granted. Contacts that fail — offline,
    /// relay down — keep the flag set and are retried on the next tick.
    pub async fn retry_regrants(&mut self) -> Result<usize, Error> {
        if !self.state.regrant_all {
            return Ok(0);
        }
        let nicknames: Vec<String> = self
            .state
            .contacts
            .iter()
            .map(|c| c.nickname.clone())
            .collect();
        let mut regranted = 0;
        let mut failed = false;
        for nickname in nicknames {
            match self.grant_queues_to(&nickname).await {
                Ok(true) => regranted += 1,
                // Handed to the relay but refused: it never got there, so the
                // contact still holds the old queue. Retry next tick.
                Ok(false) => {
                    eprintln!("hush: re-grant to {nickname} did not reach the relay");
                    failed = true;
                }
                Err(e) => {
                    eprintln!("hush: could not re-grant a queue to {nickname}: {e}");
                    failed = true;
                }
            }
        }
        if !failed {
            self.state.regrant_all = false;
            self.save().await?;
        }
        Ok(regranted)
    }

    /// Fan one conclave control payload (paper or voice) out to the conclave's
    /// joined trustees.
    ///
    /// These payloads ride the same 1:1 sessions as conclave messages: there
    /// is no relay-side room and no group object, so this is one encrypted
    /// copy per joined trustee. Unlike a message it is **not** written to the
    /// local chat log — the state lives in the daemon (paper sidecar, voice
    /// manager), and the sender's own copy is recorded there, not here.
    ///
    /// Everything riding this fan-out follows the conclave's lifecycle: once
    /// the conclave has ended the paper is read-only on every peer and a voice
    /// room is over, so sending is refused exactly as
    /// `send_conclave_message` refuses.
    async fn fan_out_conclave_payload(
        &mut self,
        conclave_id: &str,
        payload: MessagePayload,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        let out = self.fan_out_conclave(&conclave, &payload, None).await;
        self.warn_conclave_fan_out(conclave_id, &out);
        self.queue_conclave_retry(conclave_id, &payload, out.refused)
            .await
    }

    /// Tell the conclave's trustees that a paper exists, or that its hub moved.
    ///
    /// Creator-authoritative at creation: the receiver accepts a new paper only
    /// from the trustee the descriptor names as its creator.
    pub async fn send_conclave_paper_state(
        &mut self,
        conclave_id: &str,
        paper: PaperDescriptor,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(conclave_id, MessagePayload::PaperState { paper })
            .await
    }

    /// Re-home a paper: the convener's word settles where the DataChannel
    /// rendezvous is.
    ///
    /// Convener-only at both ends, exactly like a roster change — the sender
    /// must be the convener to make the claim, and `recv` drops one from anyone
    /// else before it reaches the inbox. The paper's *creator* is deliberately
    /// not consulted: the creator is the first hub, and the whole point of
    /// succession is that it may have left or been kicked, which is precisely
    /// when a new hub is needed.
    pub async fn send_conclave_paper_hub(
        &mut self,
        conclave_id: &str,
        paper: PaperDescriptor,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let convener = self
            .chat_log
            .conclave(conclave_id)
            .map(|c| c.convener.clone())
            .ok_or(Error::ConclaveNotFound)?;
        if convener != me {
            return Err(Error::NotConclaveConvener);
        }
        self.fan_out_conclave_payload(conclave_id, MessagePayload::PaperHub { paper })
            .await
    }

    /// Inherit a paper's hub when the one that held it is gone.
    ///
    /// The default succession rule: the creator — or whoever last held the
    /// rendezvous — left or was kicked, so the conclave's **convener** takes it.
    /// Returns the descriptor that was sent when this vault is the convener and
    /// the claim went out, and `None` otherwise.
    ///
    /// `None` is not a failure: a trustee that is not the convener has no
    /// authority to re-home anything, and every daemon that sees the departure
    /// tries this, so most of them are supposed to come back empty-handed.
    ///
    /// Three things must hold, and the roster is checked here rather than by the
    /// caller because a caller does not always know who left: this vault is the
    /// convener, the paper names `gone_hub` as its rendezvous, and that account
    /// is **no longer a joined trustee**. The last check is what lets a daemon
    /// call this after any roster change — a kick of somebody who is not the hub
    /// must not move the paper — and it is why the argument is the hub to
    /// replace rather than an account id the caller has to go and find.
    pub async fn inherit_conclave_paper_hub(
        &mut self,
        conclave_id: &str,
        gone_hub: &str,
        paper: PaperDescriptor,
    ) -> Result<Option<PaperDescriptor>, Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.convener != me || paper.hub != gone_hub {
            return Ok(None);
        }
        if conclave.joined().any(|t| t.account_id == gone_hub) {
            return Ok(None);
        }
        let mut moved = paper;
        moved.hub = me;
        self.send_conclave_paper_hub(conclave_id, moved.clone())
            .await?;
        Ok(Some(moved))
    }

    /// Pass one opaque `yrs` update on to the conclave's other trustees.
    ///
    /// The relay fan-out is a mesh, and a mesh is only as good as the roster
    /// the sender had when it fanned out. A trustee that has not yet drained a
    /// third trustee's join does not send to them, and deltas are never
    /// replayed — so that edit would never reach them at all. The hub is the
    /// one party placed to close that gap: it is the paper's rendezvous, so it
    /// is the party every trustee addresses, and it is the party the
    /// DataChannel star already makes responsible for re-broadcast.
    ///
    /// The caller decides who is the hub (`PaperDescriptor::hub`); this is pure
    /// transport and carries no authority, exactly like
    /// [`send_conclave_paper_delta`](Self::send_conclave_paper_delta). Sending
    /// to a trustee that already had the update costs one duplicate blob and no
    /// correctness — merging an update twice is a no-op — which is the right
    /// trade against an edit that never arrives.
    ///
    /// The sender is skipped: it is the one peer that certainly has these bytes.
    pub async fn relay_conclave_paper_delta(
        &mut self,
        conclave_id: &str,
        paper_id: &str,
        from_account: &str,
        update: &[u8],
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended || !conclave.is_joined(&me) {
            return Ok(());
        }
        let payload = MessagePayload::PaperDelta {
            paper_id: paper_id.to_string(),
            update: update.to_vec(),
        };
        let out = self
            .fan_out_conclave(&conclave, &payload, Some(from_account))
            .await;
        self.queue_conclave_retry(conclave_id, &payload, out.refused)
            .await?;
        Ok(())
    }

    /// Fan one opaque `yrs` update out to the conclave's trustees.
    ///
    /// The bytes are never parsed here: merging is the CRDT's job and belongs
    /// to whoever owns the document, so this is pure transport and carries no
    /// authority.
    pub async fn send_conclave_paper_delta(
        &mut self,
        conclave_id: &str,
        paper_id: &str,
        update: &[u8],
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::PaperDelta {
                paper_id: paper_id.to_string(),
                update: update.to_vec(),
            },
        )
        .await
    }

    /// Send the whole document to the conclave's trustees.
    ///
    /// This is the catch-up a trustee who joined after the paper existed needs:
    /// the deltas it missed are not replayed, so the hub hands it the current
    /// state instead. The descriptor rides along because that trustee never saw
    /// `PaperState` — without it the state would name a paper it cannot place.
    pub async fn send_conclave_paper_snapshot(
        &mut self,
        conclave_id: &str,
        paper: PaperDescriptor,
        state: &[u8],
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::PaperSnapshot {
                paper,
                state: state.to_vec(),
            },
        )
        .await
    }

    /// Open a voice room: announce the session to every joined trustee.
    ///
    /// The room rules (one per conclave, joined-trustee opener, cap) are the
    /// daemon's voice manager's to enforce on both sides; this is transport.
    /// A receiver still checks identity — the opener named must be the sender
    /// — in `recv`, where the roster is in hand.
    pub async fn send_conclave_voice_open(
        &mut self,
        conclave_id: &str,
        session: VoiceDescriptor,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(conclave_id, MessagePayload::VoiceOpen { session })
            .await
    }

    /// Send a WebRTC offer to every joined trustee: in a mesh there is no hub
    /// to offer to, so a joining peer offers to each peer already in the room
    /// and answers their replies.
    pub async fn send_conclave_voice_offer(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        sdp: &str,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::VoiceOffer {
                session_id: session_id.to_string(),
                sdp: sdp.to_string(),
            },
        )
        .await
    }

    /// Send a WebRTC answer. Mesh topology: only the peers that were offered
    /// apply one, so this still fans out to all and lets each receiver decide
    /// by session state whether it answers this peer.
    pub async fn send_conclave_voice_answer(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        sdp: &str,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::VoiceAnswer {
                session_id: session_id.to_string(),
                sdp: sdp.to_string(),
            },
        )
        .await
    }

    /// Send a mute flag. The sender stops capturing *before* calling this, so
    /// silence is the truth and the flag is the label.
    pub async fn send_conclave_voice_mute(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        who: &str,
        muted: bool,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::VoiceMute {
                session_id: session_id.to_string(),
                who: who.to_string(),
                muted,
            },
        )
        .await
    }

    /// Send a leave. The opener's leave is the end of the room on every peer;
    /// anyone else's removes only them.
    pub async fn send_conclave_voice_leave(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        who: &str,
    ) -> Result<(), Error> {
        self.fan_out_conclave_payload(
            conclave_id,
            MessagePayload::VoiceLeave {
                session_id: session_id.to_string(),
                who: who.to_string(),
            },
        )
        .await
    }

    /// Send one DataChannel signalling payload to a single trustee, by account id.
    ///
    /// Signalling is addressed, never fanned out: the DataChannel is a 1:1 affair
    /// between a trustee and the paper's hub, so an offer or an answer has
    /// exactly one recipient. It rides the same session and the same relay queue
    /// as every other payload, which is the only path that exists before the
    /// channel does — so a signal can be delivered late, or to a peer that is
    /// offline, without anything having to be retried by hand.
    ///
    /// Gated the same way the fan-out is: the conclave must exist and both ends
    /// must be joined trustees, so a signal cannot be aimed at a stranger or at
    /// a conclave this vault is not in.
    pub async fn send_paper_signal(
        &mut self,
        conclave_id: &str,
        peer_account: &str,
        payload: MessagePayload,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) || !conclave.is_joined(peer_account) {
            return Err(Error::NotConclaveTrustee);
        }
        let nick = self
            .contact_nick_for_protocol(peer_account)
            .ok_or(Error::ContactNotFound)?;
        self.send_payload_inner(&nick, payload, false).await
    }

    /// Share a file into a conclave, one encrypted copy per joined trustee.
    ///
    /// There is no shared blob to point everyone at: files are per-recipient
    /// encrypted blobs, so this is N copies for N joined trustees, exactly like
    /// group text. It targets the **joined** set only, matching text; a trustee
    /// who joins later does not receive it, because there is no shared history
    /// to backfill from. The bytes are also stored as a local blob so the sender
    /// can reopen the file after a restart.
    ///
    /// Returns the local blob id.
    pub async fn send_conclave_file_bytes(
        &mut self,
        conclave_id: &str,
        name: &str,
        data: &[u8],
    ) -> Result<BlobId, Error> {
        if data.len() > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        let blob_id = self.vault.put_blob(data)?;
        let payload = MessagePayload::Conclave {
            conclave_id: conclave.id.clone(),
            name: conclave.name.clone(),
            trustees: conclave
                .joined()
                .map(|t| t.nick.clone())
                .filter(|n| !n.is_empty())
                .collect(),
            body: String::new(),
            file: Some(crate::payload::ConclaveFile {
                name: name.to_string(),
                data: data.to_vec(),
            }),
        };
        let out = self.fan_out_conclave(&conclave, &payload, None).await;
        self.warn_conclave_fan_out(conclave_id, &out);
        self.queue_conclave_retry(conclave_id, &payload, out.refused)
            .await?;
        let master = *self.vault.master_key();
        self.chat_log
            .append_conclave_file(&master, conclave_id, true, None, name, &blob_id, data.len() as u64)?;
        Ok(blob_id)
    }

    /// Invite a contact to a conclave. Convener only.
    ///
    /// The invitee is added as `Invited` and receives an invite; the joined
    /// trustees receive the convener's roster update so the invited count
    /// converges. The daemon rejects this at the API boundary for a non-convener,
    /// so the rule is not merely a hidden button.
    pub async fn invite_conclave_trustee(
        &mut self,
        conclave_id: &str,
        nickname: &str,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.convener.is_empty() && conclave.convener != me {
            return Err(Error::NotConclaveConvener);
        }
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        if conclave.trustee(&contact.account_id).is_some() {
            return Err(Error::AlreadyConclaveTrustee);
        }
        // A mail-only relationship has no conclaves (Task 24). Checked here so
        // the rule holds for the CLI and the daemon, not just the picker.
        if !self.effective_reach(nickname).allows_live() {
            return Err(Error::ReachMailOnly);
        }
        let master = *self.vault.master_key();
        let invited = ConclaveTrustee {
            account_id: contact.account_id.clone(),
            nick: nickname.to_string(),
            state: TrusteeState::Invited,
        };
        self.chat_log
            .update_conclave(&master, conclave_id, |c| {
                c.upsert_trustee(&invited);
            })?;
        let record = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        let descriptor = descriptor_of(&record);
        self.send_payload_inner(
            nickname,
            MessagePayload::ConclaveInvite {
                descriptor: descriptor.clone(),
            },
            false,
        )
        .await?;
        self.broadcast_update(&record).await?;
        self.save().await?;
        Ok(())
    }

    /// Accept an invite: materialise the conclave and tell the convener.
    ///
    /// This is the only path that creates a conclave from a received message. It
    /// moves the trustee from invited to joined and sends an acknowledgement to
    /// the convener, who re-broadcasts the authoritative roster to everyone.
    pub async fn accept_conclave_invite(
        &mut self,
        conclave_id: &str,
    ) -> Result<ConclaveRecord, Error> {
        let me = self.state.protocol_name.clone();
        let invite = self
            .state
            .pending_invites
            .iter()
            .find(|i| i.conclave_id == conclave_id)
            .cloned()
            .ok_or(Error::InviteNotFound)?;
        let master = *self.vault.master_key();
        let mut roster = self.localise_roster(&invite.trustees);
        match roster.iter_mut().find(|m| m.account_id == me) {
            Some(self_entry) => {
                self_entry.state = TrusteeState::Joined;
                self_entry.nick = String::new();
            }
            None => roster.push(ConclaveTrustee {
                account_id: me.clone(),
                nick: String::new(),
                state: TrusteeState::Joined,
            }),
        }
        if self.chat_log.conclave(conclave_id).is_none() {
            self.chat_log.create_conclave_with_id(
                &master,
                conclave_id,
                &invite.name,
                &invite.convener,
                &roster,
            )?;
        } else {
            self.chat_log
                .update_conclave(&master, conclave_id, |c| {
                    if c.convener.is_empty() {
                        c.convener = invite.convener.clone();
                    }
                    for m in &roster {
                        c.upsert_trustee(m);
                    }
                })?;
        }
        self.state
            .pending_invites
            .retain(|i| i.conclave_id != conclave_id);
        let record = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        let descriptor = descriptor_of(&record);
        if let Some(nick) = self.contact_nick_for_protocol(&invite.from_account) {
            self.send_payload_inner(
                &nick,
                MessagePayload::ConclaveAccept { descriptor },
                false,
            )
            .await?;
        }
        self.save().await?;
        Ok(record)
    }

    /// Decline an invite. Silent to the inviter beyond the state change: only the
    /// convener is told, and no roster update is broadcast.
    pub async fn decline_conclave_invite(&mut self, conclave_id: &str) -> Result<(), Error> {
        let invite = self
            .state
            .pending_invites
            .iter()
            .find(|i| i.conclave_id == conclave_id)
            .cloned()
            .ok_or(Error::InviteNotFound)?;
        self.state
            .pending_invites
            .retain(|i| i.conclave_id != conclave_id);
        if let Some(nick) = self.contact_nick_for_protocol(&invite.from_account) {
            self.send_payload_inner(
                &nick,
                MessagePayload::ConclaveDecline {
                    conclave_id: conclave_id.to_string(),
                    member: self.state.protocol_name.clone(),
                },
                false,
            )
            .await?;
        }
        self.save().await?;
        Ok(())
    }

    /// Offer an introduction between two joined trustees this vault knows.
    ///
    /// The "Introduce to…" action: this vault has both the seeker and the
    /// target as contacts, so it asks the target to share its invite with the
    /// seeker. Consent stays with the target — this only delivers the question.
    pub async fn introduce_members(
        &mut self,
        conclave_id: &str,
        seeker_account: &str,
        target_account: &str,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        if seeker_account == target_account || seeker_account == me || target_account == me {
            return Err(Error::InvalidConclave);
        }
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        // Both legs must be real channels: the request goes to the target over
        // one, and the consented invite will come back for the seeker over the
        // other. Without either, the introduction could not be delivered.
        if self.contact_nick_for_protocol(seeker_account).is_none() {
            return Err(Error::ContactNotFound);
        }
        let target_nick = self
            .contact_nick_for_protocol(target_account)
            .ok_or(Error::ContactNotFound)?;
        if !conclave.is_joined(seeker_account) || !conclave.is_joined(target_account) {
            return Err(Error::NotConclaveTrustee);
        }
        let seeker_roster_nick = conclave
            .trustee(seeker_account)
            .map(|t| t.nick.clone())
            .unwrap_or_default();
        let target_roster_nick = conclave
            .trustee(target_account)
            .map(|t| t.nick.clone())
            .unwrap_or_default();
        self.send_payload_inner(
            &target_nick,
            MessagePayload::ConclaveIntroReq {
                conclave_id: conclave_id.to_string(),
                name: conclave.name.clone(),
                seeker: seeker_account.to_string(),
                seeker_nick: seeker_roster_nick,
                target: target_account.to_string(),
                target_nick: target_roster_nick,
            },
            false,
        )
        .await?;
        self.save().await?;
        Ok(())
    }

    /// Ask the conclave for an introduction to a member this vault cannot
    /// reach: the "Request introduction" action.
    ///
    /// The request goes to every joined trustee this vault has a channel to —
    /// each can either relay it to the target or is silently skipped. Whoever
    /// first returns a consented invite wins; extras are dropped on arrival.
    pub async fn request_intro(
        &mut self,
        conclave_id: &str,
        target_account: &str,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        if target_account == me {
            return Err(Error::InvalidConclave);
        }
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        if !conclave.is_joined(target_account) {
            return Err(Error::NotConclaveTrustee);
        }
        if self.contact_nick_for_protocol(target_account).is_some() {
            return Err(Error::ContactExists);
        }
        let my_nick = conclave
            .trustee(&me)
            .map(|t| t.nick.clone())
            .unwrap_or_default();
        let target_roster_nick = conclave
            .trustee(target_account)
            .map(|t| t.nick.clone())
            .unwrap_or_default();
        let couriers: Vec<String> = conclave
            .joined()
            .filter(|t| t.account_id != me && t.account_id != target_account)
            .filter_map(|t| self.contact_nick_for_protocol(&t.account_id))
            .collect();
        if couriers.is_empty() {
            return Err(Error::NoIntroCourier);
        }
        for nick in couriers {
            self.send_payload_inner(
                &nick,
                MessagePayload::ConclaveIntroReq {
                    conclave_id: conclave_id.to_string(),
                    name: conclave.name.clone(),
                    seeker: me.clone(),
                    seeker_nick: my_nick.clone(),
                    target: target_account.to_string(),
                    target_nick: target_roster_nick.clone(),
                },
                false,
            )
            .await?;
        }
        self.save().await?;
        Ok(())
    }

    /// Relay a stored introduction request on to the target: the courier's
    /// action on the relay prompt. The stored request is consumed either way.
    pub async fn relay_intro_request(
        &mut self,
        conclave_id: &str,
        seeker_account: &str,
        target_account: &str,
    ) -> Result<(), Error> {
        self.introduce_members(conclave_id, seeker_account, target_account)
            .await?;
        self.state
            .pending_intro_requests
            .retain(|r| !(r.conclave_id == conclave_id && r.seeker == seeker_account));
        self.save().await?;
        Ok(())
    }

    /// Answer a request addressed to this vault: share the standing invite
    /// with the seeker (over the channel the request arrived on) or decline.
    ///
    /// Declining is silent, the same treatment `decline_conclave_invite`
    /// gives a conclave invite: no notification, no state change on peers.
    pub async fn answer_intro_request(
        &mut self,
        conclave_id: &str,
        seeker_account: &str,
        accept: bool,
    ) -> Result<(), Error> {
        let request = self
            .state
            .pending_intro_requests
            .iter()
            .find(|r| r.conclave_id == conclave_id && r.seeker == seeker_account)
            .cloned();
        let Some(request) = request else {
            return Err(Error::InviteNotFound);
        };
        self.state
            .pending_intro_requests
            .retain(|r| !(r.conclave_id == conclave_id && r.seeker == seeker_account));
        if accept {
            let conclave = self
                .chat_log
                .conclave(conclave_id)
                .cloned()
                .ok_or(Error::ConclaveNotFound)?;
            if !conclave.ended && conclave.is_joined(&self.state.protocol_name) {
                let invite = self.fresh_invite().await?;
                let my_nick = conclave
                    .trustee(&self.state.protocol_name)
                    .map(|t| t.nick.clone())
                    .unwrap_or_default();
                // The invite goes back the way the request came: the courier's
                // channel is the one path known to exist.
                let via_nick = self
                    .contact_nick_for_protocol(&request.via)
                    .ok_or(Error::ContactNotFound)?;
                self.send_payload_inner(
                    &via_nick,
                    MessagePayload::ConclaveIntro {
                        conclave_id: conclave_id.to_string(),
                        name: conclave.name.clone(),
                        target: self.state.protocol_name.clone(),
                        target_nick: my_nick,
                        seeker: seeker_account.to_string(),
                        invite,
                        reverse: false,
                    },
                    false,
                )
                .await?;
            }
        }
        self.save().await?;
        Ok(())
    }

    /// Accept a received introduction and make the other member a contact.
    ///
    /// Two shapes, distinguished by `intro.reverse`:
    ///
    /// Forward card (we sought the introduction): the card carries the
    /// target's fresh invite. We consume it and become the session initiator
    /// — exactly like a direct contact add — and send our own fresh invite
    /// back as a reverse card.
    ///
    /// Reverse card (the other side accepted first): the sender already
    /// consumed our invite and will message us on that session. We must add
    /// them as a contact *without* consuming the attached bundle: consuming
    /// would create an outbound session that shadows their incoming prekey
    /// chain, and libsignal refuses a prekey message when a session for that
    /// address already exists. Our session comes from decrypting their first
    /// message instead, which needs no action from us; their queue
    /// credentials arrived with our own invite when they consumed it, so
    /// there is nothing to grant either.
    pub async fn accept_intro(
        &mut self,
        conclave_id: &str,
        target_account: &str,
        nickname: &str,
    ) -> Result<(), Error> {
        let intro = self
            .state
            .pending_intros
            .iter()
            .find(|i| i.conclave_id == conclave_id && i.target == target_account)
            .cloned()
            .ok_or(Error::InviteNotFound)?;
        if nickname.trim().is_empty() {
            return Err(Error::InvalidHandle);
        }
        // A nickname is how every send path finds a contact, so a collision
        // with a different account would misroute messages. The UI dedupes;
        // this is the backstop.
        if self
            .state
            .contacts
            .iter()
            .any(|c| c.nickname == nickname && c.account_id != target_account)
        {
            return Err(Error::ContactExists);
        }
        self.state
            .pending_intros
            .retain(|i| !(i.conclave_id == conclave_id && i.target == target_account));
        if intro.reverse {
            self.add_contact_from_intro(nickname, &intro.invite).await?;
        } else {
            self.add_contact(nickname, &intro.invite).await?;
        }
        // The reverse card goes out on the forward path only: it is what
        // tells the other side we accepted, and carries the fresh invite
        // they will use to open their session. A reverse-card accept needs
        // no reply — the sender already holds everything of ours there is.
        if intro.reverse {
            self.save().await?;
            return Ok(());
        }
        if let Some(conclave) = self.chat_log.conclave(conclave_id).cloned() {
            if !conclave.ended && conclave.is_joined(target_account) {
                if let (Some(via_nick), Ok(my_invite)) = (
                    self.contact_nick_for_protocol(&intro.via),
                    self.fresh_invite().await,
                ) {
                    let my_nick = conclave
                        .trustee(&self.state.protocol_name)
                        .map(|t| t.nick.clone())
                        .unwrap_or_default();
                    let reverse = MessagePayload::ConclaveIntro {
                        conclave_id: conclave_id.to_string(),
                        name: conclave.name.clone(),
                        target: self.state.protocol_name.clone(),
                        target_nick: my_nick,
                        seeker: target_account.to_string(),
                        invite: my_invite,
                        reverse: true,
                    };
                    // Best effort: the contact edge works one way if the
                    // courier is down, and the card can be re-earned by
                    // repeating the introduction.
                    if let Err(e) = self
                        .send_payload_inner(&via_nick, reverse, false)
                        .await
                    {
                        eprintln!(
                            "hush: intro return-card to {} failed: {e}",
                            short_name(target_account)
                        );
                    }
                }
            }
        }
        self.save().await?;
        Ok(())
    }

    /// Dismiss a received introduction without adding the contact.
    pub async fn dismiss_intro(&mut self, conclave_id: &str, target_account: &str) -> Result<(), Error> {
        self.state
            .pending_intros
            .retain(|i| !(i.conclave_id == conclave_id && i.target == target_account));
        self.save().await?;
        Ok(())
    }

    /// Drop a held introduction request without relaying or answering it.
    pub async fn dismiss_intro_request(
        &mut self,
        conclave_id: &str,
        seeker_account: &str,
    ) -> Result<(), Error> {
        self.state
            .pending_intro_requests
            .retain(|r| !(r.conclave_id == conclave_id && r.seeker == seeker_account));
        self.save().await?;
        Ok(())
    }

    pub fn pending_intro_requests(&self) -> &[PendingIntroRequest] {
        &self.state.pending_intro_requests
    }

    pub fn pending_intros(&self) -> &[PendingIntro] {
        &self.state.pending_intros
    }

    /// Apply a received introduction request.
    ///
    /// Only two shapes are meaningful: this vault is the target (consent is
    /// asked), or the sender is the seeker and this vault can reach the target
    /// (a relay decision is asked). Anything else — a relayed request in a
    /// stranger's name, a conclave this vault is not part of, a seeker or
    /// target outside the roster — is consumed and dropped, the treatment
    /// every other forged claim in the drain loop gets.
    async fn receive_intro_req(
        &mut self,
        conclave_id: &str,
        conclave_name: &str,
        seeker: &str,
        seeker_nick: &str,
        target: &str,
        target_nick: &str,
        from_account: &str,
    ) -> Result<bool, Error> {
        let me = self.state.protocol_name.clone();
        if seeker == target {
            return Ok(false);
        }
        let Some(conclave) = self.chat_log.conclave(conclave_id).cloned() else {
            return Ok(false);
        };
        if conclave.ended || !conclave.is_joined(&me) {
            return Ok(false);
        }
        let is_target = target == me;
        let is_courier = seeker == from_account && target != me;
        if !is_target && !is_courier {
            return Ok(false);
        }
        if is_target {
            // Consent is only meaningful for a roster member this vault does
            // not already know.
            if seeker == me
                || !conclave.is_joined(seeker)
                || self.contact_nick_for_protocol(seeker).is_some()
            {
                return Ok(false);
            }
        } else {
            // A courier can only help if the target is in the roster and a
            // real channel to them exists. Silently skipped otherwise: no
            // point telling the seeker something they cannot act on.
            if !conclave.is_joined(target) || self.contact_nick_for_protocol(target).is_none() {
                return Ok(false);
            }
        }
        let record = PendingIntroRequest {
            conclave_id: conclave_id.to_string(),
            conclave_name: conclave_name.to_string(),
            seeker: seeker.to_string(),
            seeker_nick: seeker_nick.to_string(),
            target: target.to_string(),
            target_nick: target_nick.to_string(),
            via: from_account.to_string(),
            received_ms: now_ms(),
        };
        match self
            .state
            .pending_intro_requests
            .iter_mut()
            .find(|r| r.conclave_id == conclave_id && r.seeker == seeker)
        {
            Some(existing) => *existing = record,
            None => self.state.pending_intro_requests.push(record),
        }
        Ok(true)
    }

    /// Apply a received introduction.
    ///
    /// The invite is only ever valid in its owner's name, so `target` must
    /// equal the sender. This vault is either the seeker (the introduction is
    /// stored for the accept prompt) or the courier (it is forwarded to the
    /// seeker over the seeker's channel). Everything else is dropped.
    async fn receive_intro(
        &mut self,
        conclave_id: &str,
        conclave_name: &str,
        target: &str,
        target_nick: &str,
        seeker: &str,
        invite: &str,
        from_account: &str,
        reverse: bool,
    ) -> Result<bool, Error> {
        let me = self.state.protocol_name.clone();
        if seeker == target || target == me {
            return Ok(false);
        }
        let Some(conclave) = self.chat_log.conclave(conclave_id).cloned() else {
            return Ok(false);
        };
        if conclave.ended || !conclave.is_joined(&me) {
            return Ok(false);
        }
        if !conclave.is_joined(target) {
            return Ok(false);
        }
        if seeker == me {
            // The introduction is for this vault. Known contacts need nothing;
            // malformed invites are not stored; a second copy replaces the
            // first, so a fresh invite always wins.
            if self.contact_nick_for_protocol(target).is_some() {
                return Ok(false);
            }
            // The invite is self-authenticating: it must name the roster
            // member it claims, so a courier cannot swap in their own
            // identity and become the channel.
            let payload = match InvitePayload::decode(invite) {
                Ok(p) => p,
                Err(_) => return Ok(false),
            };
            if payload.protocol_name != target {
                return Ok(false);
            }
            let record = PendingIntro {
                conclave_id: conclave_id.to_string(),
                conclave_name: conclave_name.to_string(),
                target: target.to_string(),
                target_nick: target_nick.to_string(),
                via: from_account.to_string(),
                invite: invite.to_string(),
                received_ms: now_ms(),
                reverse,
            };
            match self
                .state
                .pending_intros
                .iter_mut()
                .find(|i| i.conclave_id == conclave_id && i.target == target)
            {
                Some(existing) => *existing = record,
                None => self.state.pending_intros.push(record),
            }
            return Ok(true);
        }
        // Courier: forward to the seeker over their channel, consuming any
        // relay prompt this delivery answers.
        let Some(seeker_nick) = self.contact_nick_for_protocol(seeker) else {
            return Ok(false);
        };
        if !conclave.is_joined(seeker) {
            return Ok(false);
        }
        self.state
            .pending_intro_requests
            .retain(|r| !(r.conclave_id == conclave_id && r.seeker == seeker));
        self.send_payload_inner(
            &seeker_nick,
            MessagePayload::ConclaveIntro {
                conclave_id: conclave_id.to_string(),
                name: conclave_name.to_string(),
                target: target.to_string(),
                target_nick: target_nick.to_string(),
                seeker: seeker.to_string(),
                invite: invite.to_string(),
                reverse,
            },
            false,
        )
        .await?;
        Ok(true)
    }

    ///
    /// If the convener leaves, the conclave ends for everyone (decision a): the
    /// convener is the only trustee who can administer or close it, so a departed
    /// convener would leave a conclave nobody can govern. Otherwise the trustee is
    /// marked `Left` and the convener is told, so the roster converges from the
    /// convener's broadcast.
    pub async fn leave_conclave(&mut self, conclave_id: &str) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if !conclave.is_joined(&me) {
            return Err(Error::NotConclaveTrustee);
        }
        if conclave.convener == me {
            return self.end_conclave(conclave_id).await;
        }
        let master = *self.vault.master_key();
        let left = ConclaveTrustee {
            account_id: me.clone(),
            nick: String::new(),
            state: TrusteeState::Left,
        };
        self.chat_log
            .update_conclave(&master, conclave_id, |c| {
                c.upsert_trustee(&left);
            })?;
        let record = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        let descriptor = descriptor_of(&record);
        if let Some(nick) = self.contact_nick_for_protocol(&conclave.convener) {
            self.send_payload_inner(&nick, MessagePayload::ConclaveLeave { descriptor }, false)
                .await?;
        }
        self.save().await?;
        Ok(())
    }

    /// Remove a trustee. Convener only.
    ///
    /// The trustee is marked `Kicked` and the convener broadcasts the
    /// authoritative roster to every joined trustee, so the kick converges
    /// everywhere.
    pub async fn kick_conclave_trustee(
        &mut self,
        conclave_id: &str,
        nickname: &str,
    ) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !conclave.convener.is_empty() && conclave.convener != me {
            return Err(Error::NotConclaveConvener);
        }
        let target = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        let Some(existing) = conclave.trustee(&target.account_id) else {
            return Err(Error::NotConclaveTrustee);
        };
        if matches!(existing.state, TrusteeState::Left | TrusteeState::Kicked) {
            return Err(Error::NotConclaveTrustee);
        }
        let master = *self.vault.master_key();
        let kicked = ConclaveTrustee {
            account_id: target.account_id.clone(),
            nick: nickname.to_string(),
            state: TrusteeState::Kicked,
        };
        self.chat_log
            .update_conclave(&master, conclave_id, |c| {
                c.upsert_trustee(&kicked);
            })?;
        let record = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        self.broadcast_update(&record).await?;
        // Tell the trustee itself. `broadcast_update` targets the *joined* set,
        // and a kick removes them from it first — so without this the one vault
        // that most needs to know is the only one never told. It would keep a
        // joined roster: still fan out into a conclave that has removed it, and
        // still edit the conclave's paper, which nobody else would ever see.
        // Addressed by nickname because that is the handle the caller had, and
        // the roster entry for them is `Kicked` from here on.
        self.send_payload_inner(
            nickname,
            MessagePayload::ConclaveUpdate {
                descriptor: descriptor_of(&record),
            },
            false,
        )
        .await?;
        self.save().await?;
        Ok(())
    }

    /// End a conclave for everyone. Convener only.
    ///
    /// The record and its message log stay in every vault; the `ended` flag makes
    /// the conclave read-only on every peer, not only on the convener's. This is a
    /// broadcast, not the local hide it used to be.
    pub async fn end_conclave(&mut self, conclave_id: &str) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let conclave = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        if !conclave.convener.is_empty() && conclave.convener != me {
            return Err(Error::NotConclaveConvener);
        }
        let master = *self.vault.master_key();
        if !self.chat_log.end_conclave(&master, conclave_id)? {
            return Ok(());
        }
        let record = self
            .chat_log
            .conclave(conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        let payload = MessagePayload::ConclaveEnd {
            descriptor: descriptor_of(&record),
        };
        let out = self.fan_out_conclave(&record, &payload, None).await;
        self.queue_conclave_retry(conclave_id, &payload, out.refused)
            .await?;
        self.save().await?;
        Ok(())
    }

    /// Delete an ended conclave and its whole message log from this vault.
    ///
    /// Local only: nothing is sent to the other trustees, who keep their own
    /// copies — a conclave has no shared history to delete, only per-vault ones.
    /// Ended only: an active conclave still has people in it, and leaving or
    /// ending is how membership ends. The record and its messages leave the
    /// encrypted log, but the vault blobs its shared files referenced stay: the
    /// blob store has no collection pass, the same trade
    /// [`delete_mail`](Self::delete_mail) already makes, and a blob is
    /// content-addressed so the same bytes may be referenced from a 1:1 chat or
    /// a mail draft too.
    ///
    /// Returns whether the record existed and was removed. An unknown id is a
    /// quiet no-op rather than an error, matching
    /// [`delete_mail`](Self::delete_mail): the second of two racing deletes, or
    /// a retry after the first went through, must not fail.
    pub async fn delete_conclave(&mut self, conclave_id: &str) -> Result<bool, Error> {
        let ended = match self.chat_log.conclave(conclave_id) {
            None => return Ok(false),
            Some(conclave) => conclave.ended,
        };
        if !ended {
            return Err(Error::ConclaveNotEnded);
        }
        let master = *self.vault.master_key();
        let deleted = self.chat_log.remove_conclave(&master, conclave_id)?;
        if deleted {
            self.save().await?;
        }
        Ok(deleted)
    }

    /// Send the convener's authoritative roster to every joined trustee.
    async fn broadcast_update(&mut self, record: &ConclaveRecord) -> Result<(), Error> {
        let payload = MessagePayload::ConclaveUpdate {
            descriptor: descriptor_of(record),
        };
        let out = self.fan_out_conclave(record, &payload, None).await;
        self.queue_conclave_retry(&record.id, &payload, out.refused)
            .await
    }

    /// Map a wire roster onto this vault's nicknames.
    ///
    /// The account id is stable across peers; the nickname is local. A trustee
    /// this vault knows is renamed to the local nickname, and our own entry gets
    /// an empty nickname so the UI can render it as "you".
    fn localise_roster(&self, trustees: &[ConclaveTrustee]) -> Vec<ConclaveTrustee> {
        trustees
            .iter()
            .map(|m| {
                let nick = if m.account_id == self.state.protocol_name {
                    String::new()
                } else if let Some(contact) = self.contact_by_account(&m.account_id) {
                    contact.nickname.clone()
                } else {
                    m.nick.clone()
                };
                ConclaveTrustee {
                    account_id: m.account_id.clone(),
                    nick,
                    state: m.state,
                }
            })
            .collect()
    }

    /// Apply a convener-originated descriptor to a conclave this vault already
    /// holds. Returns `false` when the conclave is unknown, so a stray update is
    /// ignored rather than inventing a conclave.
    fn apply_descriptor(&mut self, descriptor: &ConclaveDescriptor) -> Result<bool, Error> {
        if self.chat_log.conclave(&descriptor.conclave_id).is_none() {
            return Ok(false);
        }
        let localised = self.localise_roster(&descriptor.trustees);
        let name = descriptor.name.clone();
        let convener = descriptor.convener.clone();
        let ended = descriptor.ended;
        let master = *self.vault.master_key();
        self.chat_log
            .update_conclave(&master, &descriptor.conclave_id, |c| {
                if !name.is_empty() {
                    c.name = name.clone();
                }
                if !convener.is_empty() {
                    c.convener = convener.clone();
                }
                if ended {
                    c.ended = true;
                }
                for m in &localised {
                    c.upsert_trustee(m);
                }
            })?;
        Ok(true)
    }

    /// Fill every legacy conclave's trustees from its nickname list, ensure the
    /// local account is a member, and give a record with no convener the local
    /// account as convener.
    ///
    /// A record written before consent and governance existed has nicknames only
    /// and no convener. Without this it would have no trustees and no one
    /// could administer it; assigning the local account is the only sensible
    /// default for a record that predates the concept. The local account is also
    /// resolved from its own nickname (it is never its own contact), so a
    /// pre-consent record is sendable rather than only listable.
    fn hydrate_conclaves(&mut self) -> Result<(), Error> {
        let me = self.state.protocol_name.clone();
        let ids: Vec<String> = self.chat_log.conclaves().iter().map(|c| c.id.clone()).collect();
        let master = *self.vault.master_key();
        for id in ids {
            let contacts: Vec<(String, String)> = self
                .state
                .contacts
                .iter()
                .map(|c| (c.nickname.clone(), c.account_id.clone()))
                .collect();
            self.chat_log
                .hydrate_trustees(&master, &id, &me, |nick| {
                    contacts
                        .iter()
                        .find(|(n, _)| n == nick)
                        .map(|(_, id)| id.clone())
                })?;
        }
        Ok(())
    }

    /// Store a received invite as a pending record. Returns whether anything
    /// changed.
    ///
    /// Nothing is materialised: the conclave does not exist locally until the
    /// invitee accepts. A repeat invite refreshes the pending record; an invite
    /// for a conclave already joined or ended is ignored.
    fn receive_conclave_invite(
        &mut self,
        descriptor: &ConclaveDescriptor,
        from_account: &str,
        from_nick: &str,
    ) -> Result<bool, Error> {
        if let Some(existing) = self.chat_log.conclave(&descriptor.conclave_id) {
            if existing.ended || existing.is_joined(&self.state.protocol_name) {
                return Ok(false);
            }
        }
        let invite = PendingInvite {
            conclave_id: descriptor.conclave_id.clone(),
            name: descriptor.name.clone(),
            convener: descriptor.convener.clone(),
            from_account: from_account.to_string(),
            from_nick: from_nick.to_string(),
            trustees: descriptor.trustees.clone(),
            received_ms: now_ms(),
        };
        match self
            .state
            .pending_invites
            .iter_mut()
            .find(|i| i.conclave_id == descriptor.conclave_id)
        {
            Some(existing) => {
                *existing = invite;
                Ok(true)
            }
            None => {
                self.state.pending_invites.push(invite);
                Ok(true)
            }
        }
    }

    /// Apply a received acceptance. Convener only.
    ///
    /// The trustee moves from invited to joined in the convener's record, which
    /// the convener then re-broadcasts so the joined count converges everywhere.
    async fn receive_conclave_accept(
        &mut self,
        descriptor: &ConclaveDescriptor,
    ) -> Result<bool, Error> {
        if descriptor.convener != self.state.protocol_name {
            return Ok(false);
        }
        if !self.apply_descriptor(descriptor)? {
            return Ok(false);
        }
        let record = self
            .chat_log
            .conclave(&descriptor.conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        self.broadcast_update(&record).await?;
        Ok(true)
    }

    /// Apply a received decline. Convener only.
    ///
    /// The declined trustee is dropped from the convener's record so the invited
    /// count is honest; nothing is broadcast, because no one else needs to know.
    fn receive_conclave_decline(
        &mut self,
        conclave_id: &str,
        trustee_account: &str,
    ) -> Result<bool, Error> {
        if self.chat_log.conclave(conclave_id).is_none() {
            return Ok(false);
        }
        let master = *self.vault.master_key();
        self.chat_log.update_conclave(&master, conclave_id, |c| {
            c.trustees.retain(|t| t.account_id != trustee_account);
        })?;
        Ok(true)
    }

    /// Apply a received leave. Convener only.
    ///
    /// The leaver is marked `Left` and the convener re-broadcasts, so every
    /// joined trustee converges from one authority rather than a peer-to-peer
    /// flood.
    async fn receive_conclave_leave(
        &mut self,
        descriptor: &ConclaveDescriptor,
    ) -> Result<bool, Error> {
        if descriptor.convener != self.state.protocol_name {
            return Ok(false);
        }
        if !self.apply_descriptor(descriptor)? {
            return Ok(false);
        }
        let record = self
            .chat_log
            .conclave(&descriptor.conclave_id)
            .cloned()
            .ok_or(Error::ConclaveNotFound)?;
        self.broadcast_update(&record).await?;
        Ok(true)
    }

    /// Apply a convener-originated roster snapshot, or an end.
    ///
    /// Accepted only when the sender is the convener named in the descriptor, so
    /// a trustee cannot forge membership changes.
    fn receive_conclave_update(
        &mut self,
        descriptor: &ConclaveDescriptor,
        from_account: &str,
    ) -> Result<bool, Error> {
        if descriptor.convener.is_empty() || descriptor.convener != from_account {
            return Ok(false);
        }
        self.apply_descriptor(descriptor)
    }

    pub async fn heartbeat_presence(&self) -> Result<(), Error> {
        let relay = self.relay_client()?;
        relay
            .touch_presence(self.device.queue_id(), self.device.read_token())
            .await
    }

    /// Mark this vault offline at the relay, at once.
    ///
    /// The "appear offline" switch uses this on the way in so peers stop
    /// seeing the vault online immediately, instead of waiting out the
    /// heartbeat's 90 s freshness window.
    pub async fn go_offline(&self) -> Result<(), Error> {
        let relay = self.relay_client()?;
        relay
            .clear_presence(self.device.queue_id(), self.device.read_token())
            .await
    }

    pub async fn contact_is_online(&self, nickname: &str) -> Result<bool, Error> {
        let contact = self.find_contact(nickname).ok_or(Error::ContactNotFound)?;
        let relay = self.relay_client()?;
        relay
            .contact_presence_online(&contact.queue_id, &contact.queue_token)
            .await
    }

    /// Send a live-only payload — a chat line or a file — to a contact.
    ///
    /// This is the chat surface's one gate (Task 24): a mail-only relationship
    /// has no chat, so the send is refused here rather than merely hidden in
    /// the UI. Mail and the other store-and-forward payloads ride
    /// [`Self::send_payload_inner`] directly and are unaffected.
    async fn send_payload(&mut self, nickname: &str, payload: MessagePayload) -> Result<(), Error> {
        if !self.effective_reach(nickname).allows_live() {
            return Err(Error::ReachMailOnly);
        }
        self.send_payload_inner(nickname, payload, true).await
    }

    /// `require_online` is false for mail: mail is delivered into the recipient's
    /// store-and-forward queue while they are away, which is exactly the
    /// difference from live-only chat (5h).
    async fn send_payload_inner(
        &mut self,
        nickname: &str,
        payload: MessagePayload,
        require_online: bool,
    ) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        if require_online && !self.contact_is_online(nickname).await? {
            return Err(Error::ContactOffline);
        }

        let plain = payload.encode()?;
        // One relay POST is not a delivery guarantee over a real network — the
        // relay may be restarting or a packet may die mid-TLS. Retry the
        // delivery a couple of times before reporting failure, so a transient
        // blip does not turn into "sometimes messages just don't arrive".
        // "Offline" never reaches here: it is rejected above, and a peer who
        // is genuinely away should hear about it, not wait out a retry.
        let mut last_err: Option<Error> = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt))).await;
            }
            match self.deliver(&contact, &plain).await {
                Ok(()) => {
                    self.save().await?;
                    return Ok(());
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or(Error::ContactNotFound))
    }

    /// Encrypt to a contact and put the ciphertext in the right queue.
    ///
    /// The queue is the contact's granted per-contact queue when there is one,
    /// and its inbox otherwise. The inbox fallback is what keeps a contact that
    /// has not been granted a queue (a pre-existing contact, or one whose grant
    /// has not arrived) working, at the cost that such a contact cannot be
    /// revoked without burning the shared inbox.
    async fn deliver(&mut self, contact: &Contact, plain: &[u8]) -> Result<(), Error> {
        let (queue_id, queue_token) = match (
            contact.delivery_queue_id.as_deref(),
            contact.delivery_queue_token.as_deref(),
        ) {
            (Some(id), Some(token)) => (id, token),
            _ => (contact.queue_id.as_str(), contact.queue_token.as_str()),
        };
        self.deliver_to(contact, queue_id, queue_token, plain).await
    }

    /// Encrypt to a contact and put the ciphertext in a named queue.
    ///
    /// Split from [`Account::deliver`] so a queue grant can be sent to the
    /// contact's *inbox* even when a per-contact delivery queue is already
    /// known: the grant is what establishes that queue, so it must not be sent
    /// to it.
    async fn deliver_to(
        &mut self,
        contact: &Contact,
        queue_id: &str,
        queue_token: &str,
        plain: &[u8],
    ) -> Result<(), Error> {
        self.ensure_session(&contact.account_id, &contact.identity_key)
            .await?;
        let local_name = self.state.protocol_name.clone();
        let ct = encrypt_message(
            self.device.store_mut(),
            &local_name,
            &contact.account_id,
            plain,
        )
        .await?;
        let body = encode_relay_blob(&ct);
        let relay = self.relay_client()?;
        relay.put(queue_id, queue_token, &body).await?;
        Ok(())
    }

    /// Send this device's per-contact queue grant to a contact.
    ///
    /// The grant tells the contact where to deliver this account's messages for
    /// it, which is what makes deleting the contact able to burn exactly that
    /// queue. Sent to the contact's **inbox**, which is the grant channel, never
    /// to a delivery queue.
    ///
    /// Returns whether the grant reached the relay. A revoked contact is owed
    /// nothing and reports `true`, so a re-grant sweep does not retry it forever.
    async fn grant_queues_to(&mut self, nickname: &str) -> Result<bool, Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        // A revoked account is not granted a queue again even if a stale contact
        // record still names it. This is a shortcut, not the security boundary:
        // the grant is verified by its signature on the far side anyway (see
        // `apply_queue_grant`). Not sending it saves a pointless delivery.
        if self.state.revoked.contains(&contact.account_id) {
            return Ok(true);
        }
        let queue_id = self.device.contact_queue_id(&contact.account_id);
        let delivery_token = self.device.contact_queue_delivery_token(&contact.account_id);
        let key = self.device.identity_key_pair().await?;
        let signature = sign_queue_grant(&key, &queue_id, &delivery_token)?;
        let plain = MessagePayload::QueueGrant {
            queue_id,
            delivery_token,
            signature,
        }
        .encode()?;

        // The grant channel is the contact's inbox; the grant must not be sent
        // to a per-contact queue, which the recipient may not have bound yet.
        //
        // The outcome is reported, not swallowed: a caller re-granting after a
        // queue rotation must know whether the grant actually reached the relay,
        // or it would clear its retry flag on a delivery that never happened.
        // `add_contact` ignores the value on purpose — a contact is added whether
        // or not the relay is reachable at that moment.
        let delivered = match self
            .deliver_to(&contact, &contact.queue_id, &contact.queue_token, &plain)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "hush: grant to {} failed: {e}",
                    short_name(&contact.account_id)
                );
                false
            }
        };
        // Encrypting the grant advances the session ratchet. Persist it here:
        // otherwise a caller that exits right after adding a contact (the CLI)
        // leaves the ratchet on disk at its pre-grant position, and the next
        // message is encrypted with a reused chain key that the peer cannot
        // decrypt. The daemon masks this by saving again on its next send.
        self.save().await?;
        Ok(delivered)
    }

    /// Make sure a session with a contact exists, opening one from its published
    /// prekey record if not.
    ///
    /// The fetched record is checked against the identity key the contact pinned
    /// in their invite. Without that check a relay could serve a bundle for a key
    /// it controls and read everything sent to that contact.
    async fn ensure_session(&mut self, account_id: &str, identity_key: &[u8]) -> Result<(), Error> {
        let remote = protocol_address(account_id)?;
        if self
            .device
            .store_mut()
            .session_store
            .load_session(&remote)
            .await
            .map_err(|_| Error::Corrupt)?
            .is_some()
        {
            return Ok(());
        }

        let relay = self.relay_client()?;
        let bytes = relay.get_prekey_record(account_id).await?;
        let record = PreKeyRecord::decode(&bytes)?;
        record.verify(identity_key)?;
        let bundle = record.to_prekey_bundle()?;

        let local_name = self.state.protocol_name.clone();
        process_contact_bundle(self.device.store_mut(), &local_name, account_id, &bundle).await
    }

    /// Publish this device's prekey record, so a peer that has never exchanged an
    /// invite with it can still open a session.
    ///
    /// The minted private halves are saved with the device, not just held in
    /// memory: the record on the relay is useless if the key that decrypts the
    /// first message is lost on the next unlock.
    pub async fn publish_prekeys(&mut self, one_time_count: usize) -> Result<(), Error> {
        let record = self.device.mint_prekeys(one_time_count).await?;
        self.save().await?;
        let body = record.encode()?;
        let relay = self.relay_client()?;
        relay
            .put_prekey_record(&self.state.protocol_name, &body)
            .await?;
        Ok(())
    }

    /// The private credential for this account's own inbox.
    ///
    /// It authorizes listing, reading, deleting and burning, so it is never
    /// published — the invite carries only the derived delivery token. Exposed
    /// for local callers that talk to the relay directly instead of going
    /// through [`Account::recv`].
    pub fn own_read_token(&self) -> &str {
        self.device.read_token()
    }

    /// The per-contact queue this device reads for `account_id`, and the read
    /// credential that authorizes listing, reading and burning it.
    ///
    /// Exposed for the same local callers as [`Account::own_read_token`]. Both
    /// values are per device and must never be published: the read token is what
    /// makes a per-contact delete revocable.
    pub fn own_contact_queue(&self, account_id: &str) -> (String, String) {
        (
            self.device.contact_queue_id(account_id),
            self.device.contact_queue_read_token(account_id),
        )
    }

    /// The relay queues this vault drains and delivers into.
    ///
    /// Derived on demand and never stored, which is why a queue filling on the
    /// relay cannot be attributed from the vault files: this is the local,
    /// read-only view that answers "is that queue mine?" in one call. A queue
    /// that is not in this map belongs to the peer, not to this vault.
    pub fn queue_map(&self) -> QueueMap {
        let contacts = self
            .state
            .contacts
            .iter()
            .map(|c| {
                let granted = c.delivery_queue_id.is_some() && c.delivery_queue_token.is_some();
                let send_queue = match c.delivery_queue_id.as_deref() {
                    Some(id) if granted => id.to_string(),
                    _ => c.queue_id.clone(),
                };
                ContactQueues {
                    nickname: c.nickname.clone(),
                    account_id: c.account_id.clone(),
                    read_queue: self.device.contact_queue_id(&c.account_id),
                    send_queue,
                    granted,
                }
            })
            .collect();
        QueueMap {
            inbox: self.device.queue_id().to_string(),
            contacts,
            regrant_pending: self.state.regrant_all,
            identity_restored: self.state.identity_restored,
        }
    }

    /// Publish one opaque blob to this member's bulletin, answering with its
    /// `seq`.
    ///
    /// The queue is derived from the **read** token rather than from the inbox
    /// id, so no contact — who holds the inbox id and its delivery token from
    /// the invite — can bind or poison it; see
    /// [`bulletin_queue_id`](crate::bulletin_queue_id). Re-publishing the same
    /// bytes is idempotent and refreshes the row's TTL, which is what makes a
    /// holder's re-post sweep affordable.
    pub async fn publish_bulletin(&self, body: &[u8]) -> Result<i64, Error> {
        let relay = self.relay_client()?;
        relay
            .put_bulletin(
                &crate::bulletin_queue_id(self.device.read_token()),
                self.device.read_token(),
                body,
            )
            .await
    }

    /// Drain this member's bulletin: everything after `since`, oldest first.
    ///
    /// A member that has never published has no bulletin queue yet, and an empty
    /// lane is not a failure — the relay answers an unknown queue as empty.
    pub async fn drain_bulletin(&self, since: i64) -> Result<Vec<BulletinBlob>, Error> {
        let relay = self.relay_client()?;
        relay
            .list_bulletin(
                &crate::bulletin_queue_id(self.device.read_token()),
                self.device.read_token(),
                since,
            )
            .await
    }

    /// Hand one contact a post's read key.
    ///
    /// Store-and-forward rather than live-only, like mail: a key is durable and
    /// the reader may simply be away, so refusing to send one because the
    /// recipient is offline would leave a zine unreadable for no reason.
    ///
    /// Authority-free by design: any holder of a key may re-share it, so there is
    /// nothing to verify and no signature. The session is already authenticated
    /// to a known contact, and the worst a forged grant can do is hand over a key
    /// the sender already held.
    pub async fn send_zine_grant(
        &mut self,
        nickname: &str,
        zine_id: &str,
        key: &[u8],
        latest_hash: &str,
    ) -> Result<(), Error> {
        self.send_payload_inner(
            nickname,
            MessagePayload::ZineGrant {
                zine_id: zine_id.to_string(),
                key: key.to_vec(),
                latest_hash: latest_hash.to_string(),
            },
            false,
        )
        .await
    }

    /// Whether this device's relay inbox was replaced when the vault was opened.
    ///
    /// True once, for a vault written before the read/delivery credential split,
    /// and for a vault migrating off the pre-multi-device layout. The caller must
    /// drop any cached invite and issue a fresh one, so peers stop delivering to
    /// the abandoned inbox; until then this device receives nothing from anyone
    /// who has not seen the new invite.
    pub fn queue_rotated(&self) -> bool {
        self.device.queue_rotated()
    }

    /// Drain every queue this device reads: its inbox, and one per-contact queue
    /// for each contact.
    ///
    /// The inbox carries queue grants and messages from contacts that have not
    /// been granted a per-contact queue yet. Each per-contact queue carries only
    /// that one contact's messages, which is what makes a queue the unit of
    /// revocation: deleting the contact burns exactly that queue.
    pub async fn recv(&mut self) -> Result<InboxResult, Error> {
        // A delete burns the contact's queue immediately; if the relay was down
        // then, this re-drives it. Burning is idempotent, so a queue that is
        // already gone is a no-op.
        self.burn_revoked_queues().await?;

        let relay = self.relay_client()?;
        // (expected sender account, queue id, read token). `None` marks the
        // inbox, whose sender can be any contact.
        let mut queues: Vec<(Option<String>, String, String)> = Vec::new();
        queues.push((
            None,
            self.device.queue_id().to_string(),
            self.device.read_token().to_string(),
        ));
        for contact in self.state.contacts.clone() {
            queues.push((
                Some(contact.account_id.clone()),
                self.device.contact_queue_id(&contact.account_id),
                self.device.contact_queue_read_token(&contact.account_id),
            ));
        }

        let mut out = Vec::new();
        let mut papers = Vec::new();
        let mut voices = Vec::new();
        let mut zine_grants = Vec::new();
        let mut warnings = Vec::new();
        let mut dirty = false;
        // Who was joined before this poll, so a trustee that joins during it can
        // be reported to the caller (the paper hub hands it a snapshot) and one
        // that leaves can be too (the convener inherits a paper whose hub just
        // left). The account ids rather than a count: a swap that keeps the size
        // the same still has to report both halves.
        let joined_before: Vec<(String, Vec<String>)> = self
            .chat_log
            .conclaves()
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.joined().map(|t| t.account_id.clone()).collect(),
                )
            })
            .collect();
        // The drain is round-based, not page-looped inside a queue: each
        // round peeks one page per queue (concurrently), files it, acks what
        // it filed, and only re-peeks queues that made progress. The ack is
        // what makes the next peek see different rows — a peek alone never
        // consumes, so a page-loop without the ack would re-read the same
        // page forever. A queue that files nothing in a round (everything in
        // its page is held — a conclave message waiting on its roster, say)
        // stops until the next poll, exactly like the per-blob walk it
        // replaces.
        let mut pending: Vec<(Option<String>, String, String)> = queues;
        while !pending.is_empty() {
        // Phase 1 — fetch one page per queue. Every queue is fetched
        // concurrently: the client's pool spreads the requests over separate
        // connections, so a contact count no longer multiplies the wall-clock
        // cost of a poll. The batches themselves are what turn a drain into
        // one round trip per page instead of one `get` and one `delete` per
        // message.
        let drain_supported = self.drain_supported;
        let mut handles = Vec::new();
        for (expected_contact, queue_id, read_token) in std::mem::take(&mut pending) {
            let relay = relay.clone();
            handles.push(tokio::spawn(async move {
                fetch_queue(
                    relay,
                    expected_contact,
                    queue_id,
                    read_token,
                    drain_supported,
                )
                .await
            }));
        }
        let mut fetched: Vec<FetchedQueue> = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(result) => fetched.push(result?),
                Err(join_err) => return Err(Error::Relay(format!("fetch task: {join_err}"))),
            }
        }
        // Latch what the probes found, so a legacy relay is walked directly
        // from the next poll on.
        if self.drain_supported.is_none() {
            self.drain_supported = fetched.iter().find_map(|f| f.drain_discovered);
        }

        // Phase 2 — file, serially: decryption and payload application mutate
        // session and account state. A blob fetched through the drain is
        // already out of the "delete per blob" business — it is confirmed
        // with one ack per queue once everything in the page that could be
        // filed has been filed.
        let mut acks: Vec<(String, String, Vec<String>)> = Vec::new();
        let mut repeek: Vec<(Option<String>, String, String)> = Vec::new();
        for mut fetched_queue in fetched {
            let drained = fetched_queue.drained;
            let expected_contact = fetched_queue.expected_contact.take();
            let queue_id = std::mem::take(&mut fetched_queue.queue_id);
            let read_token = std::mem::take(&mut fetched_queue.read_token);
            let mut acked: Vec<String> = Vec::new();
            // Consume a blob the way its transport demands: a drained page
            // records the id for the queue's single ack; the legacy walk
            // deletes the blob here, exactly as it always did.
            macro_rules! consume {
                ($id:ident) => {
                    if drained {
                        acked.push($id.clone());
                    } else {
                        relay.delete(&queue_id, &read_token, &$id).await?;
                    }
                };
            }
            for DrainedBlob { id: blob_id, body } in std::mem::take(&mut fetched_queue.blobs) {
                let (ct, legacy_from) = decode_relay_blob(&body)?;
                let decrypted = match &expected_contact {
                    // A per-contact queue can only carry that contact's messages.
                    Some(account_id) => self
                        .decrypt_from(account_id, &ct)
                        .await
                        .map(|plain| (plain, account_id.clone())),
                    None => match legacy_from {
                        Some(from) => self.decrypt_from(&from, &ct).await.map(|plain| (plain, from)),
                        None => self.try_decrypt_from_any_contact(&ct).await,
                    },
                };
                let (plain, from_account) = match decrypted {
                    Ok(v) => v,
                    Err(Error::Session) => {
                        warnings.push(
                            "Could not decrypt a queued message — unknown sender or session not ready.".into(),
                        );
                        continue;
                    }
                    Err(e) => {
                        // Surface the error, but not at the cost of the page's
                        // filing progress: confirm what already filed, so the
                        // next poll does not re-file — and duplicate — it.
                        if drained && !acked.is_empty() {
                            let _ = relay.ack_drain(&queue_id, &read_token, &acked).await;
                        }
                        return Err(e);
                    }
                };

                // A deleted contact must not be able to keep delivering into the
                // shared inbox and be surfaced. Its session may still be in
                // memory on the device that did the delete, so the blob can be
                // decrypted and attributed; drop it instead of filing it.
                if self.state.revoked.contains(&from_account) {
                consume!(blob_id);
                    dirty = true;
                    continue;
                }

                let payload = decode_legacy_payload(&plain)?;

                // A queue grant: the sender tells us where to deliver this
                // account's messages for it. The signature is verified against
                // the identity key the sender pinned in their invite, so an
                // unauthenticated write to the public grant channel cannot name
                // a queue the sender does not own.
                if let MessagePayload::QueueGrant {
                    queue_id: granted_id,
                    delivery_token,
                    signature,
                } = &payload
                {
                    if self.apply_queue_grant(&from_account, granted_id, delivery_token, signature)?
                    {
                        dirty = true;
                    }
                consume!(blob_id);
                    continue;
                }

                let nick = self
                    .contact_nick_for_protocol(&from_account)
                    .unwrap_or_else(|| short_name(&from_account));

                // A zine grant: a contact hands over the read key of one post.
                // It is not a chat message and is never surfaced as one; the
                // daemon applies it to its zines sidecar and then sweeps the
                // bulletin for blobs that have just become readable. Nothing is
                // verified, by design — any holder may re-share a key they hold,
                // and a grant can only confer read of a post the sender could
                // already read.
                if let MessagePayload::ZineGrant {
                    zine_id,
                    key,
                    latest_hash,
                } = &payload
                {
                    zine_grants.push(crate::message::ReceivedZineGrant {
                        from_account: from_account.clone(),
                        from_nick: nick.clone(),
                        zine_id: zine_id.clone(),
                        key: key.clone(),
                        latest_hash: latest_hash.clone(),
                    });
                consume!(blob_id);
                    continue;
                }

                // A reach announcement: the sender tells us which surfaces they
                // will use with us (Task 24). Applied only from a known contact
                // — the session is the authentication — and it can only ever
                // narrow, because the effective reach is the narrower of the
                // two sides' choices. A stranger's announcement is dropped.
                if let MessagePayload::Reach { scope } = &payload {
                    if let Some(contact) = self
                        .state
                        .contacts
                        .iter_mut()
                        .find(|c| c.account_id == from_account)
                    {
                        if contact.peer_reach != *scope {
                            contact.peer_reach = *scope;
                            dirty = true;
                        }
                    }
                consume!(blob_id);
                    continue;
                }

                if let MessagePayload::PlaceAdopt {
                    invite,
                    chain,
                    label,
                } = &payload
                {
                    match self
                        .apply_place_adopt(&from_account, invite, chain.clone(), label)
                        .await
                    {
                        Ok(true) => dirty = true,
                        Ok(false) => {}
                        Err(e) => warnings.push(format!(
                            "A place-adopt from {nick} was refused: {e}"
                        )),
                    }
                    consume!(blob_id);
                    continue;
                }

                if let MessagePayload::PlaceIntroduce {
                    invite,
                    chain,
                    label,
                } = &payload
                {
                    match self
                        .apply_place_introduce(&from_account, invite, chain.clone(), label)
                        .await
                    {
                        Ok(true) => dirty = true,
                        Ok(false) => {}
                        Err(e) => warnings.push(format!(
                            "A place-introduce from {nick} was refused: {e}"
                        )),
                    }
                    consume!(blob_id);
                    continue;
                }

                // Conclave control payloads. Membership is consensual, so an
                // invite, accept, decline, leave, update or end changes local
                // state and is never surfaced as a chat message. Only an
                // explicit accept ever materialises a conclave.
                match &payload {
                    MessagePayload::ConclaveInvite { descriptor } => {
                        // A mail-only relationship has no conclaves (Task 24).
                        // Refused before a pending invite is materialised, so a
                        // peer that ignored the reach cannot pull us in.
                        if !self.effective_reach_for_account(&from_account).allows_live() {
                            warnings.push(format!(
                                "An invite from {nick} was refused — this is a mail-only relationship."
                            ));
                    consume!(blob_id);
                            dirty = true;
                            continue;
                        }
                        if self.receive_conclave_invite(descriptor, &from_account, &nick)? {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveAccept { descriptor } => {
                        if self.receive_conclave_accept(descriptor).await? {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveDecline { conclave_id, member } => {
                        if self.receive_conclave_decline(conclave_id, member)? {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveLeave { descriptor } => {
                        if self.receive_conclave_leave(descriptor).await? {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveUpdate { descriptor }
                    | MessagePayload::ConclaveEnd { descriptor } => {
                        if self.receive_conclave_update(descriptor, &from_account)? {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveIntroReq {
                        conclave_id,
                        name,
                        seeker,
                        seeker_nick,
                        target,
                        target_nick,
                    } => {
                        if self
                            .receive_intro_req(
                                &conclave_id,
                                &name,
                                &seeker,
                                &seeker_nick,
                                &target,
                                &target_nick,
                                &from_account,
                            )
                            .await?
                        {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    MessagePayload::ConclaveIntro {
                        conclave_id,
                        name,
                        target,
                        target_nick,
                        seeker,
                        invite,
                        reverse,
                    } => {
                        if self
                            .receive_intro(
                                &conclave_id,
                                &name,
                                &target,
                                &target_nick,
                                &seeker,
                                &invite,
                                &from_account,
                                *reverse,
                            )
                            .await?
                        {
                            dirty = true;
                        }
                    consume!(blob_id);
                        continue;
                    }
                    _ => {}
                }

                // A live chat line or file from a mail-only relationship is
                // refused at the boundary (Task 24), not merely hidden in the
                // sender's UI: a peer that has not yet learned the reach, or
                // ignored it, must not get through. Mail is the floor and is
                // never gated.
                if matches!(
                    &payload,
                    MessagePayload::Text { .. } | MessagePayload::File { .. }
                ) && !self.effective_reach_for_account(&from_account).allows_live()
                {
                    warnings.push(format!(
                        "A chat from {nick} was refused — this is a mail-only relationship."
                    ));
                consume!(blob_id);
                    dirty = true;
                    continue;
                }

                let (file_blob_id, mail_attachments) = match &payload {
                    MessagePayload::File { data, name } => {
                        let id = self.vault.put_blob(data)?;
                        let master = *self.vault.master_key();
                        self.chat_log
                            .append_file(&master, &nick, false, name, &id, data.len() as u64)?;
                        self.enforce_chat_retention();
                        (Some(id), Vec::new())
                    }
                    MessagePayload::Mail {
                        thread_id,
                        subject,
                        body,
                        attachments,
                    } => {
                        let mut blobs = Vec::with_capacity(attachments.len());
                        let mut records = Vec::with_capacity(attachments.len());
                        for a in attachments {
                            let id = self.vault.put_blob(&a.data)?;
                            records.push(MailAttachmentRecord {
                                name: a.name.clone(),
                                blob_id_hex: hex::encode(id.0),
                                size: a.data.len() as u64,
                            });
                            blobs.push(ReceivedAttachment {
                                name: a.name.clone(),
                                blob_id: id,
                                size: a.data.len() as u64,
                            });
                        }
                        let master = *self.vault.master_key();
                        self.chat_log.append_mail(
                            &master,
                            &nick,
                            false,
                            thread_id,
                            subject.as_deref(),
                            body,
                            &records,
                            MailState::Sent,
                        )?;
                        (None, blobs)
                    }
                    MessagePayload::Text { body } => {
                        let master = *self.vault.master_key();
                        self.chat_log.append_text(&master, &nick, false, body)?;
                        self.enforce_chat_retention();
                        (None, Vec::new())
                    }
                    // A conclave message. A message never materialises a
                    // conclave, so a stray fan-out cannot invent one — the
                    // invite handled above must be accepted first. But the
                    // guard must not turn a roster race into silent loss:
                    //
                    // - a joined sender is delivered;
                    // - a terminal sender (left or kicked) is no longer a
                    //   member, so the blob is consumed and dropped;
                    // - anything else — the conclave is not known here yet, or
                    //   the sender is still only `invited` because the roster
                    //   has not converged — is HELD, not deleted, so accepting
                    //   the invite or a later roster update can deliver it.
                    //
                    // Deleting on `!known` was how a message that arrived
                    // before its invite, or during a convergence race, was lost
                    // with no trace: the sender saw "sent", the receiver never
                    // saw it, and the relay row was already gone.
                    MessagePayload::Conclave {
                        conclave_id,
                        body,
                        file,
                        ..
                    } => {
                        let sender_state = self
                            .chat_log
                            .conclave(conclave_id)
                            .and_then(|c| c.trustee(&from_account))
                            .map(|t| t.state);
                        match sender_state {
                            Some(TrusteeState::Joined) => {}
                            Some(TrusteeState::Left) | Some(TrusteeState::Kicked) => {
                        consume!(blob_id);
                                dirty = true;
                                continue;
                            }
                            Some(TrusteeState::Invited) | None => {
                                warnings.push(format!(
                                    "A message for a conclave from {nick} was held — this vault has not joined it yet."
                                ));
                                continue;
                            }
                        }
                        let master = *self.vault.master_key();
                        match file {
                            Some(f) => {
                                let id = self.vault.put_blob(&f.data)?;
                                self.chat_log.append_conclave_file(
                                    &master,
                                    conclave_id,
                                    false,
                                    Some(&nick),
                                    &f.name,
                                    &id,
                                    f.data.len() as u64,
                                )?;
                            }
                            None => {
                                self.chat_log.append_conclave_message(
                                    &master,
                                    conclave_id,
                                    false,
                                    Some(&nick),
                                    body,
                                )?;
                            }
                        }
                        (None, Vec::new())
                    }
                    // Handled above, before this match.
                    MessagePayload::QueueGrant { .. }
                    | MessagePayload::ConclaveInvite { .. }
                    | MessagePayload::ConclaveAccept { .. }
                    | MessagePayload::ConclaveDecline { .. }
                    | MessagePayload::ConclaveLeave { .. }
                    | MessagePayload::ConclaveUpdate { .. }
                    | MessagePayload::ConclaveEnd { .. }
                    | MessagePayload::ConclaveIntroReq { .. }
                    | MessagePayload::ConclaveIntro { .. }
                    // Handed over above, into `zine_grants`: a read key is not
                    // a chat message and must never be rendered as one.
                    | MessagePayload::ZineGrant { .. }
                    // Applied above, into `peer_reach`: a reach announcement is
                    // not a chat message either.
                    | MessagePayload::Reach { .. }
                    | MessagePayload::PlaceAdopt { .. }
                    | MessagePayload::PlaceIntroduce { .. } => {
                        unreachable!("control payload handled above")
                    }
                    // A paper payload is not a chat message, so it must not
                    // reach `out` as one: it leaves through `papers` for the
                    // caller's paper store. The blob is consumed here because
                    // it has been delivered — leaving it would re-deliver it on
                    // every poll forever.
                    MessagePayload::PaperState { .. }
                    | MessagePayload::PaperDelta { .. }
                    | MessagePayload::PaperSnapshot { .. }
                    | MessagePayload::PaperPresence { .. }
                    | MessagePayload::PaperHub { .. }
                    | MessagePayload::PaperOffer { .. }
                    | MessagePayload::PaperAnswer { .. } => {
                        // Hub succession is convener-authoritative, and the
                        // check belongs here rather than in the daemon: this is
                        // where the conclave record is in hand, so there is no
                        // second copy of the roster to drift. A claim from
                        // anyone else is consumed and dropped — the same
                        // treatment `Unknown` gets, for the same reason.
                        if let MessagePayload::PaperHub { paper } = &payload {
                            let convener = self
                                .chat_log
                                .conclave(&paper.conclave_id)
                                .map(|c| c.convener.as_str())
                                .unwrap_or_default();
                            if convener.is_empty() || convener != from_account {
                        consume!(blob_id);
                                dirty = true;
                                continue;
                            }
                        }
                        papers.push(ReceivedPaper {
                            from_account: from_account.clone(),
                            from_nick: nick.clone(),
                            payload: payload.clone(),
                        });
                    consume!(blob_id);
                        dirty = true;
                        continue;
                    }
                    // A voice payload is not a chat message (Task 23): it is
                    // routed into `voices` for the daemon's voice manager and
                    // consumed, exactly like a paper payload. Identity is
                    // checked here rather than only in the manager because
                    // this is where the conclave roster is in hand, and the
                    // same treatment `PaperHub` gets applies: a claim in
                    // someone else's name is consumed and dropped — it never
                    // becomes room state, not even briefly.
                    MessagePayload::VoiceOpen { session }
                        if session.opener == from_account
                            && self
                                .chat_log
                                .conclave(&session.conclave_id)
                                .is_some_and(|c| {
                                    c.joined().any(|t| t.account_id == from_account)
                                }) =>
                    {
                        voices.push(ReceivedVoice {
                            from_account: from_account.clone(),
                            from_nick: nick.clone(),
                            payload: payload.clone(),
                        });
                    consume!(blob_id);
                        dirty = true;
                        continue;
                    }
                    MessagePayload::VoiceMute { who, .. } | MessagePayload::VoiceLeave { who, .. }
                        if *who == from_account =>
                    {
                        voices.push(ReceivedVoice {
                            from_account: from_account.clone(),
                            from_nick: nick.clone(),
                            payload: payload.clone(),
                        });
                    consume!(blob_id);
                        dirty = true;
                        continue;
                    }
                    MessagePayload::VoiceOffer { .. } | MessagePayload::VoiceAnswer { .. } => {
                        // Session-level traffic: the receiver's manager decides
                        // whether the sender is a peer it is handshaking with.
                        voices.push(ReceivedVoice {
                            from_account: from_account.clone(),
                            from_nick: nick.clone(),
                            payload: payload.clone(),
                        });
                    consume!(blob_id);
                        dirty = true;
                        continue;
                    }
                    // A voice payload in someone else's name (a forged open, a
                    // mute or leave for another trustee) or for a conclave this
                    // vault has never joined is consumed and dropped — the
                    // same treatment `Unknown` gets, for the same reason.
                    MessagePayload::VoiceOpen { .. }
                    | MessagePayload::VoiceMute { .. }
                    | MessagePayload::VoiceLeave { .. } => {
                    consume!(blob_id);
                        dirty = true;
                        continue;
                    }
                    // A tag this build does not know, from one that does.
                    // Consumed and dropped: it can never be interpreted here,
                    // and leaving it would re-deliver it on every poll forever.
                    MessagePayload::Unknown => {
                    consume!(blob_id);
                        continue;
                    }
                };
                // A message we filed is proof they added us. Readiness itself is
                // read off the grant we hold, but `add_contact` sends a grant
                // once, best effort, so a grant lost at add time would otherwise
                // leave a working trust reading as pending forever.
                if let Some(c) = self
                    .state
                    .contacts
                    .iter_mut()
                    .find(|c| c.account_id == from_account)
                {
                    c.heard_from = true;
                }
                out.push(ReceivedMessage {
                    from_nick: nick,
                    payload,
                    file_blob_id,
                    mail_attachments,
                });
                consume!(blob_id);
                dirty = true;
            }
            // A drained queue that filed something this round gets its acks
            // sent and another peek next round; a drained queue that filed
            // nothing (everything held) waits for the next poll. The legacy
            // walk already deleted per blob, so it has nothing to ack.
            if drained && !acked.is_empty() {
                repeek.push((expected_contact, queue_id.clone(), read_token.clone()));
                acks.push((queue_id, read_token, acked));
            }
        }
        // Phase 3 — confirm the filed blobs, one request per queue. The ack
        // is what moves the next peek past this page: a peek alone never
        // consumes, so re-peeking without the ack would return the same rows.
        for (queue_id, read_token, ids) in acks {
            relay.ack_drain(&queue_id, &read_token, &ids).await?;
        }
        pending = repeek;
        }
        if dirty {
            self.save().await?;
        }
        // A conclave whose joined set grew this poll. A trustee that was
        // already joined counts as before, so a kick-and-rejoin or a roster
        // re-broadcast that changes nothing reports nothing.
        let joined: Vec<String> = self
            .chat_log
            .conclaves()
            .iter()
            .filter(|c| {
                let before = joined_before
                    .iter()
                    .find(|(id, _)| id == &c.id)
                    .map(|(_, ids)| ids.len())
                    .unwrap_or(0);
                c.joined().count() > before
            })
            .map(|c| c.id.clone())
            .collect();
        // ...and the mirror: every trustee that stopped being joined, named, so
        // the caller can tell whether the one that left was the paper's hub.
        let departed: Vec<(String, String)> = self
            .chat_log
            .conclaves()
            .iter()
            .flat_map(|c| {
                let Some((_, before)) = joined_before.iter().find(|(id, _)| id == &c.id) else {
                    return Vec::new();
                };
                let now: Vec<&str> = c.joined().map(|t| t.account_id.as_str()).collect();
                before
                    .iter()
                    .filter(|account| !now.contains(&account.as_str()))
                    .map(|account| (c.id.clone(), account.clone()))
                    .collect()
            })
            .collect();
        Ok(InboxResult {
            messages: out,
            papers,
            voices,
            joined,
            departed,
            zine_grants,
            warnings,
        })
    }

    /// Store a queue grant from the contact that sent it, if it verifies.
    ///
    /// Returns whether anything changed. The grant is trusted only when its
    /// signature checks out under the identity key the contact pinned in their
    /// invite — the same key that hashes to their account id. That is what keeps
    /// the public grant channel safe: anyone can write to it, but only the
    /// holder of the identity private key can name a queue the recipient will
    /// use.
    ///
    /// Storing the queue is also the whole of the contact's readiness: a
    /// verified grant is exactly the proof that they added our invite, and
    /// [`Contact::inbound_ready`] reads it straight off this field, so there is
    /// no separate flag to write back. Waiting for their first message instead
    /// would leave a mutual, working trust reading as pending, because both
    /// sides grant on add and a grant is not a message.
    fn apply_queue_grant(
        &mut self,
        from_account: &str,
        queue_id: &str,
        delivery_token: &str,
        signature: &[u8],
    ) -> Result<bool, Error> {
        let Some(contact) = self
            .state
            .contacts
            .iter_mut()
            .find(|c| c.account_id == from_account)
        else {
            // Not a contact: a stranger. Nothing to store.
            return Ok(false);
        };
        if !verify_queue_grant(&contact.identity_key, queue_id, delivery_token, signature) {
            return Ok(false);
        }
        if contact.delivery_queue_id.as_deref() == Some(queue_id)
            && contact.delivery_queue_token.as_deref() == Some(delivery_token)
        {
            // The same grant again: the queue is already recorded, and there is
            // no readiness flag left to write back, so nothing changed.
            return Ok(false);
        }
        contact.delivery_queue_id = Some(queue_id.to_string());
        contact.delivery_queue_token = Some(delivery_token.to_string());
        Ok(true)
    }

    fn find_contact(&self, nickname: &str) -> Option<&Contact> {
        self.state.contacts.iter().find(|c| c.nickname == nickname)
    }

    fn contact_by_account(&self, account_id: &str) -> Option<&Contact> {
        self.state.contacts.iter().find(|c| c.account_id == account_id)
    }

    /// The reach this vault uses with a contact: the narrower of this vault's
    /// choice and the peer's announced one (Task 24). `Full` when there is no
    /// such contact, so an unknown name is never silently restricted.
    fn effective_reach(&self, nickname: &str) -> TrusteeReach {
        self.find_contact(nickname)
            .map(|c| c.reach.narrow(c.peer_reach))
            .unwrap_or(TrusteeReach::Full)
    }

    /// [`Self::effective_reach`] keyed on the stable account id, for the
    /// receive path where only the sender's account is in hand.
    fn effective_reach_for_account(&self, account_id: &str) -> TrusteeReach {
        self.contact_by_account(account_id)
            .map(|c| c.reach.narrow(c.peer_reach))
            .unwrap_or(TrusteeReach::Full)
    }

    fn contact_nick_for_protocol(&self, account_id: &str) -> Option<String> {
        self.state
            .contacts
            .iter()
            .find(|c| c.account_id == account_id)
            .map(|c| c.nickname.clone())
    }

    async fn decrypt_from(&mut self, account_id: &str, ct: &[u8]) -> Result<Vec<u8>, Error> {
        let local_name = self.state.protocol_name.clone();
        decrypt_message(self.device.store_mut(), &local_name, account_id, ct).await
    }

    /// Try every contact, to recover the sender of a blob on the shared inbox.
    ///
    /// A relay blob carries no sender id — that is the whole point of the queue
    /// model — so the sender can only be recovered by attempting decryption.
    async fn try_decrypt_from_any_contact(
        &mut self,
        ct: &[u8],
    ) -> Result<(Vec<u8>, String), Error> {
        let candidates: Vec<String> = self
            .state
            .contacts
            .iter()
            .map(|c| c.account_id.clone())
            .collect();
        for account_id in candidates {
            if let Ok(plain) = self.decrypt_from(&account_id, ct).await {
                return Ok((plain, account_id));
            }
        }
        Err(Error::Session)
    }

    async fn save(&self) -> Result<(), Error> {
        let master = self.vault.master_key();
        let state_bytes = serde_json::to_vec(&self.state).map_err(|_| Error::Corrupt)?;
        let state_enc = encrypt(master, &state_bytes)?;
        fs::write(self.dir.join("account.enc"), state_enc)?;

        // Every address a session could live under: one per contact.
        let session_addresses: Vec<(String, u32)> = self
            .state
            .contacts
            .iter()
            .map(|c| (c.account_id.clone(), DEVICE_ID))
            .collect();
        self.device.save(&self.dir, master, &session_addresses).await?;
        Ok(())
    }
}

/// Move a pre-multi-device vault onto `device.enc`.
///
/// The identity key is preserved exactly, so the protocol name, every contact
/// and every session are unaffected. The legacy file is deleted by the caller
/// once the new one is written.
///
/// The legacy **inbox is not** carried over. Before the read/delivery split
/// there was one token for both jobs and it was handed out in every invite, so
/// it is already public and the relay would refuse it as a read credential
/// anyway. `from_store` mints a fresh queue, and the caller drops the cached
/// invite so peers learn the new inbox.
async fn migrate_legacy_device(
    dir: &Path,
    master: &[u8; crate::crypto::MASTER_KEY_LEN],
    state: &mut AccountState,
) -> Result<DeviceKeys, Error> {
    let signal_bytes = fs::read(dir.join(LEGACY_DEVICE_FILE)).map_err(|_| Error::NotFound)?;
    let signal_plain = decrypt(master, &signal_bytes)?;
    let store = import_store(&signal_plain).await?;
    // Dropped rather than reused: keeping them would keep a published credential
    // authorizing reads of this device's inbox.
    state.receive_queue_id.take();
    state.receive_queue_token.take();
    Ok(DeviceKeys::from_store(store))
}

fn decode_legacy_payload(plain: &[u8]) -> Result<MessagePayload, Error> {
    if let Ok(p) = MessagePayload::decode(plain) {
        return Ok(p);
    }
    let text = String::from_utf8(plain.to_vec()).map_err(|_| Error::Corrupt)?;
    Ok(MessagePayload::Text { body: text })
}

fn normalize_relay(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn short_name(protocol_name: &str) -> String {
    if protocol_name.len() > 12 {
        format!("{}…", &protocol_name[..12])
    } else {
        protocol_name.to_string()
    }
}

/// The wire snapshot of a conclave.
///
/// An invite, accept, leave, update or end all carry this, so every peer
/// converges on the convener's view of the trustee list without a server object.
fn descriptor_of(record: &ConclaveRecord) -> ConclaveDescriptor {
    ConclaveDescriptor {
        conclave_id: record.id.clone(),
        name: record.name.clone(),
        convener: record.convener.clone(),
        trustees: record.trustees.clone(),
        ended: record.ended,
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DEVICE_FILE;
    use crate::signal_persist::export_store;
    use crate::testutil::ScratchDir;
    use crate::vault::Vault;

    /// A vault directory that deletes itself when the test ends.
    fn scratch(name: &str) -> ScratchDir {
        ScratchDir::new(&format!("account-{name}"))
    }

    /// Write the on-disk layout a vault had before the multi-device model: the
    /// device identity in `signal.enc`, and this device's relay inbox in
    /// `account.enc`.
    async fn write_legacy_vault(dir: &Path, passphrase: &str) -> (String, String, Vec<u8>) {
        let vault = Vault::create(dir, passphrase).unwrap();
        let master = *vault.master_key();
        let mut device = DeviceKeys::create(dir, &master).await.unwrap();
        let _ = mint_prekey_bundle(device.store_mut(), DEVICE_ID).await.unwrap();
        let protocol_name = protocol_name_from_identity(
            &device
                .store_mut()
                .get_identity_key_pair()
                .await
                .unwrap(),
        );
        let identity = device.identity_public().await.unwrap();
        let queue_id = device.queue_id().to_string();
        // The pre-split layout had one token doing both jobs, and it was
        // published in invites.
        let queue_token = device.queue_token();

        let store_bytes = export_store(device.store_mut(), &[]).await.unwrap();
        fs::write(
            dir.join(LEGACY_DEVICE_FILE),
            encrypt(&master, &store_bytes).unwrap(),
        )
        .unwrap();

        let state = AccountState {
            relay: "http://127.0.0.1:1".into(),
            onion_relay: None,
            use_tor_relay: false,
            protocol_name: protocol_name.clone(),
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: true,
            last_backup_unix: 0,
            receive_queue_id: Some(queue_id.clone()),
            receive_queue_token: Some(queue_token.clone()),
            mail_handle: None,
            vault_name: None,
            revoked: Vec::new(),
            pending_invites: Vec::new(),
            pending_intro_requests: Vec::new(),
            pending_intros: Vec::new(),
            pending_reach: Vec::new(),
            pending_conclave: Vec::new(),
            regrant_all: false,
            identity_restored: false,
            places: Vec::new(),
        };
        let state_bytes = serde_json::to_vec(&state).unwrap();
        fs::write(dir.join("account.enc"), encrypt(&master, &state_bytes).unwrap()).unwrap();

        // The whole point: this vault predates device.enc.
        fs::remove_file(dir.join(DEVICE_FILE)).unwrap();
        (protocol_name, queue_id, identity)
    }

    #[tokio::test]
    async fn legacy_vault_migrates_keeping_identity_and_name_but_not_its_inbox() {
        let dir = scratch("legacy");
        let (protocol_name, old_queue_id, identity) = write_legacy_vault(&dir, "pw").await;

        let account = Account::open(&dir, "pw").await.unwrap();

        assert_eq!(
            account.protocol_name(),
            protocol_name,
            "migration must not rename the account, or every contact breaks"
        );
        assert_ne!(
            account.device.queue_id(),
            old_queue_id,
            "the legacy inbox cannot be carried over: its single token was published in \
             every invite, so it cannot serve as the private read credential"
        );
        assert!(
            account.device.queue_rotated(),
            "the caller has to know, so it can drop the stale invite"
        );
        assert!(
            account.contacts().is_empty() || account.contacts()[0].queue_id != old_queue_id,
            "nothing may keep pointing at the abandoned inbox"
        );
        assert!(dir.join(DEVICE_FILE).exists(), "migration must write device.enc");
        assert!(
            !dir.join(LEGACY_DEVICE_FILE).exists(),
            "the legacy key file holds the identity private key and must not linger"
        );

        let master = *account.vault.master_key();
        let reloaded = DeviceKeys::load(&dir, &master).await.unwrap();
        assert_eq!(
            reloaded.identity_public().await.unwrap(),
            identity,
            "the identity key must be preserved exactly, or sessions with existing contacts break"
        );
        assert_eq!(
            reloaded.queue_id(),
            account.device.queue_id(),
            "the new inbox must survive a reload rather than being regenerated again"
        );
        assert!(
            !reloaded.queue_rotated(),
            "a v2 device file is already on the split, so it must not rotate on every open"
        );
    }

    /// A device file written before the read/delivery split has no read token,
    /// and its published `queue_token` cannot stand in for one. Loading it must
    /// replace the queue and say so, while leaving the identity and every session
    /// alone — otherwise an upgrade would silently make the vault unreadable to
    /// itself.
    #[tokio::test]
    async fn a_v1_device_file_rotates_its_queue_once_and_keeps_its_identity() {
        let dir = scratch("v1-device");
        let (_, _, identity) = write_legacy_vault(&dir, "pw").await;

        // Open once so the vault is on `device.enc`, then rewrite that file in
        // the v1 shape.
        let account = Account::open(&dir, "pw").await.unwrap();
        let master = *account.vault.master_key();
        let current_queue = account.device.queue_id().to_string();
        drop(account);

        let raw = fs::read(dir.join(DEVICE_FILE)).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&decrypt(&master, &raw).unwrap()).unwrap();
        let signal = value["signal"].clone();
        value = serde_json::json!({
            "v": 1,
            "device_id": 1,
            "queue_id": current_queue,
            "queue_token": "a-token-that-was-published-in-invites",
            "signal": signal,
        });
        fs::write(
            dir.join(DEVICE_FILE),
            encrypt(&master, &serde_json::to_vec(&value).unwrap()).unwrap(),
        )
        .unwrap();

        let upgraded = Account::open(&dir, "pw").await.unwrap();
        assert_ne!(
            upgraded.device.queue_id(),
            current_queue,
            "a v1 queue was bound to a public token, so it must be replaced"
        );
        assert!(upgraded.device.queue_rotated());
        assert!(
            upgraded.queue_map().regrant_pending,
            "the contacts still hold grants for the replaced queue, so the open \
             must queue a re-grant or they deliver into a queue nothing drains"
        );
        assert!(
            !upgraded.queue_map().identity_restored,
            "a rotation keeps the identity, so this is a re-grant, not a re-add"
        );

        let reloaded = DeviceKeys::load(&dir, &master).await.unwrap();
        assert_eq!(
            reloaded.identity_public().await.unwrap(),
            identity,
            "the identity key must survive the queue rotation"
        );
        assert!(
            !reloaded.queue_rotated(),
            "the rewrite must land as v2, so this happens exactly once"
        );
    }

    /// A contact stored in the pre-multi-device shape must survive the upgrade
    /// intact: same account id, same inbox, and an identity key recovered from
    /// the old `protocol_name` rather than lost. If any of that were dropped,
    /// the contact would have to be re-invited, which the upgrade must never
    /// require.
    #[tokio::test]
    async fn a_legacy_contact_is_migrated_and_still_routes() {
        let dir = scratch("legacy-contact");
        let (_protocol_name, _queue_id, _identity) = write_legacy_vault(&dir, "pw").await;

        // Rewrite `account.enc` with a contact in the old inline shape.
        let vault = Vault::open(&dir, "pw").unwrap();
        let master = *vault.master_key();
        let raw = fs::read(dir.join("account.enc")).unwrap();
        let mut state: serde_json::Value =
            serde_json::from_slice(&decrypt(&master, &raw).unwrap()).unwrap();

        let remote_identity = vec![9u8; 33];
        state["contacts"] = serde_json::json!([{
            "nickname": "bob",
            "protocol_name": hex::encode(&remote_identity),
            "queue_id": "bob-queue",
            "queue_token": "bob-token",
            "inbound_ready": true,
        }]);
        let bytes = serde_json::to_vec(&state).unwrap();
        fs::write(
            dir.join("account.enc"),
            encrypt(&master, &bytes).unwrap(),
        )
        .unwrap();

        let account = Account::open(&dir, "pw").await.unwrap();
        let contact = &account.contacts()[0];
        assert_eq!(contact.nickname, "bob");
        assert_eq!(contact.account_id, hex::encode(&remote_identity));
        assert_eq!(
            contact.queue_id, "bob-queue",
            "delivery must still go to the inbox the old record named"
        );
        assert_eq!(contact.queue_token, "bob-token");
        assert_eq!(
            contact.identity_key, remote_identity,
            "the identity key is recoverable from the old protocol_name"
        );
        assert!(
            contact.heard_from,
            "the legacy `inbound_ready` key must carry over as the message-seen \
             fallback — this record holds no grant, so it is the only thing that \
             can keep it reading as ready"
        );
        assert!(contact.inbound_ready());

        // The upgrade has to be written back, or the next open redoes it from a
        // shape that no longer exists.
        drop(account);
        let reopened = Account::open(&dir, "pw").await.unwrap();
        let contact = &reopened.contacts()[0];
        assert_eq!(contact.account_id, hex::encode(&remote_identity));
        assert_eq!(contact.queue_id, "bob-queue");
    }

    /// A contact stored in the multi-device shape must survive the downgrade:
    /// the device-1 entry (and the per-contact queue it granted us) becomes the
    /// contact, so an existing relationship does not need re-inviting.
    #[tokio::test]
    async fn a_multi_device_contact_folds_to_its_device_one() {
        let dir = scratch("multi-device-contact");
        let (_protocol_name, _queue_id, _identity) = write_legacy_vault(&dir, "pw").await;

        let vault = Vault::open(&dir, "pw").unwrap();
        let master = *vault.master_key();
        let raw = fs::read(dir.join("account.enc")).unwrap();
        let mut state: serde_json::Value =
            serde_json::from_slice(&decrypt(&master, &raw).unwrap()).unwrap();

        let remote_identity = vec![7u8; 33];
        state["contacts"] = serde_json::json!([{
            "nickname": "carol",
            "account_id": hex::encode(&remote_identity),
            "devices": [
                {
                    "device_id": 1,
                    "identity_key": remote_identity,
                    "queue_id": "carol-inbox",
                    "queue_token": "carol-token",
                    "delivery_queue_id": "carol-granted",
                    "delivery_queue_token": "carol-granted-token",
                },
                {
                    "device_id": 2,
                    "identity_key": vec![8u8; 33],
                    "queue_id": "carol-second",
                    "queue_token": "carol-second-token",
                }
            ],
            "inbound_ready": true,
            "devices_fetched_unix": 1234,
        }]);
        let bytes = serde_json::to_vec(&state).unwrap();
        fs::write(dir.join("account.enc"), encrypt(&master, &bytes).unwrap()).unwrap();

        let account = Account::open(&dir, "pw").await.unwrap();
        let contact = &account.contacts()[0];
        assert_eq!(contact.nickname, "carol");
        assert_eq!(contact.queue_id, "carol-inbox");
        assert_eq!(contact.queue_token, "carol-token");
        assert_eq!(
            contact.delivery_queue_id.as_deref(),
            Some("carol-granted"),
            "the per-contact queue device 1 granted us must be carried over"
        );
        assert_eq!(
            contact.delivery_queue_token.as_deref(),
            Some("carol-granted-token")
        );
        assert!(
            contact.inbound_ready(),
            "the grant device 1 carried over is itself the proof of readiness"
        );
    }

    #[tokio::test]
    async fn moving_the_relay_invalidates_the_cached_invite() {
        let dir = scratch("relay-invite");
        let mut account = Account::create(&dir, "pw", "http://127.0.0.1:1")
            .await
            .unwrap();
        let first = account.invite().await.unwrap();

        account
            .set_relay_clearnet("http://127.0.0.1:2")
            .await
            .unwrap();
        let second = account.invite().await.unwrap();

        assert_ne!(
            first, second,
            "a cached invite must not survive a relay move: it advertises the old relay"
        );
        // A non-default relay travels explicitly inside the pointer.
        let decoded = crate::invite::ShortInvite::decode(&second).unwrap();
        assert_eq!(decoded.effective_relay(), "http://127.0.0.1:2");
    }

    #[tokio::test]
    async fn a_restored_backup_gets_a_fresh_device_identity() {
        let dir = scratch("restore-source");
        let account = Account::create(&dir, "pw", "http://127.0.0.1:1")
            .await
            .unwrap();
        let original = account
            .device
            .identity_public()
            .await
            .unwrap();
        let archive = export_vault(&dir).unwrap();
        drop(account);

        let restored = scratch("restore-target");
        import_vault(&restored, &archive).unwrap();
        let account = Account::open(&restored, "pw").await.unwrap();

        assert!(
            restored.join(DEVICE_FILE).exists(),
            "a restored vault must mint a device identity, or it cannot be used"
        );
        assert_ne!(
            account.device.identity_public().await.unwrap(),
            original,
            "the restore must not resurrect the backed-up identity key"
        );
    }

    #[tokio::test]
    async fn a_legacy_vault_does_not_leak_its_identity_through_backup() {
        let dir = scratch("legacy-backup");
        let (_, _, _) = write_legacy_vault(&dir, "pw").await;
        let archive = export_vault(&dir).unwrap();

        let mut tar = tar::Archive::new(&archive[..]);
        let names: Vec<String> = tar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.ends_with(LEGACY_DEVICE_FILE)),
            "an unmigrated vault must not export its identity key: {names:?}"
        );
    }

    // ---- introductions -------------------------------------------------

    const SAM: &str = "aa";
    const FRIEND: &str = "bb";
    const STRANGER: &str = "cc";

    /// An account in a vault of its own, with the vault directory.
    ///
    /// The directory is returned because it deletes itself when it goes out of
    /// scope, and the account writes to it for the whole test: the caller has to
    /// hold it. Dropping it here instead would pull the vault out from under the
    /// account mid-test.
    async fn intro_vault(name: &str) -> (ScratchDir, Account) {
        let dir = scratch(name);
        let account = Account::create(&dir, "pw", "http://127.0.0.1:1")
            .await
            .unwrap();
        (dir, account)
    }

    /// Seed the account's conclave record: three joined members, one conclave.
    /// `me` is the role this vault plays — the state machine compares senders
    /// and targets against the account's own protocol name. Returns the real
    /// conclave id, which `create_conclave` mints.
    fn seed_mesh_conclave(account: &mut Account, me: &str) -> String {
        let master = *account.vault.master_key();
        account.state.protocol_name = me.to_string();
        let members = [
            ("sam", SAM),
            ("friend", FRIEND),
            ("stranger", STRANGER),
        ];
        let trustees: Vec<ConclaveTrustee> = members
            .iter()
            .map(|(nick, id)| ConclaveTrustee {
                nick: nick.to_string(),
                account_id: id.to_string(),
                state: TrusteeState::Joined,
            })
            .collect();
        account
            .chat_log
            .create_conclave(&master, "mesh", SAM, &trustees)
            .unwrap()
    }

    /// Seed a contact edge without touching the relay: identity material is
    /// never used by the introduction state machine, only looked up.
    fn seed_contact(account: &mut Account, nick: &str, id: &str) {
        account.state.contacts.push(Contact {
            nickname: nick.to_string(),
            account_id: id.to_string(),
            identity_key: vec![7u8; 33],
            queue_id: format!("{nick}-queue"),
            queue_token: format!("{nick}-token"),
            delivery_queue_id: None,
            delivery_queue_token: None,
            mail_handle: None,
            mail_vault: None,
            heard_from: false,
            reach: TrusteeReach::Full,
            peer_reach: TrusteeReach::Full,
            place_of: None,
        });
    }

    /// A queued conclave copy is pruned, not retried forever, once its conclave
    /// is gone or ended.
    ///
    /// The outbox keeps the copy because a conclave has no relay-side room to
    /// re-read it from, and that only holds while the conclave is live: an ended
    /// conclave is read-only on every peer and a deleted one has no targets
    /// left, so both must prune rather than spin. The relay here is dead, so a
    /// copy for a live conclave is refused and has to stay.
    #[tokio::test]
    async fn a_queued_conclave_copy_is_pruned_once_its_conclave_is_gone_or_ended() {
        let (_dir, mut account) = intro_vault("conclave-outbox-prune").await;
        let cid = seed_mesh_conclave(&mut account, SAM);
        seed_contact(&mut account, "friend", FRIEND);

        let payload = MessagePayload::Conclave {
            conclave_id: cid.clone(),
            name: "mesh".into(),
            trustees: vec!["friend".into()],
            body: "hello".into(),
            file: None,
        };
        account
            .queue_conclave_retry(&cid, &payload, vec!["friend".to_string()])
            .await
            .unwrap();
        account
            .queue_conclave_retry("c-gone", &payload, vec!["friend".to_string()])
            .await
            .unwrap();
        assert_eq!(account.state.pending_conclave.len(), 2);

        let _ = account.retry_pending_conclave().await;
        assert_eq!(
            account.state.pending_conclave.len(),
            1,
            "the unknown conclave's copy is pruned; the live one's is kept"
        );
        assert_eq!(account.state.pending_conclave[0].conclave_id, cid);
    }

    /// A grant naming a *different* queue must replace the one on record.
    ///
    /// This is the whole of the re-grant repair: after a queue rotation the peer
    /// signs a new queue id with the same identity key, and the contact record
    /// has to follow it. Treating the second grant as "already granted" would
    /// leave the peer delivering into a queue nothing drains, with no error on
    /// either side.
    #[tokio::test]
    async fn a_regrant_from_the_same_identity_replaces_the_stored_queue() {
        let (_dir, mut account) = intro_vault("regrant-replace").await;
        seed_contact(&mut account, "friend", FRIEND);

        let identity = account.device.identity_public().await.unwrap();
        let key = account.device.identity_key_pair().await.unwrap();
        account.state.contacts[0].identity_key = identity;

        let first = sign_queue_grant(&key, "queue-one", "token-one").unwrap();
        assert!(account
            .apply_queue_grant(FRIEND, "queue-one", "token-one", &first)
            .unwrap());
        assert_eq!(
            account.state.contacts[0].delivery_queue_id.as_deref(),
            Some("queue-one")
        );

        let second = sign_queue_grant(&key, "queue-two", "token-two").unwrap();
        assert!(
            account
                .apply_queue_grant(FRIEND, "queue-two", "token-two", &second)
                .unwrap(),
            "the rotated queue must be adopted, not ignored as a duplicate"
        );
        assert_eq!(
            account.state.contacts[0].delivery_queue_id.as_deref(),
            Some("queue-two")
        );

        // The identical grant again is a no-op, so a sweep can re-send freely.
        assert!(!account
            .apply_queue_grant(FRIEND, "queue-two", "token-two", &second)
            .unwrap());
    }

    /// A grant signed by a key the contact did not pin is refused.
    ///
    /// This is why a restored vault cannot re-grant its way out of a stale
    /// queue: a fresh device signs with a new identity key, and the peer
    /// verifies against the key it took from the invite. Accepting it anyway
    /// would be the identity substitution the pin exists to stop.
    #[tokio::test]
    async fn a_grant_signed_by_an_unpinned_identity_is_refused() {
        let (_dir, mut account) = intro_vault("regrant-unpinned").await;
        seed_contact(&mut account, "friend", FRIEND);

        // The contact pinned some other key; this vault's own key is the signer.
        let key = account.device.identity_key_pair().await.unwrap();
        let grant = sign_queue_grant(&key, "queue-new", "token-new").unwrap();

        assert!(!account
            .apply_queue_grant(FRIEND, "queue-new", "token-new", &grant)
            .unwrap());
        assert!(
            account.state.contacts[0].delivery_queue_id.is_none(),
            "a refused grant must not be recorded"
        );
    }

    /// A re-grant that never reaches the relay stays queued.
    ///
    /// The relay here is unreachable, so the sweep must report nothing regranted
    /// and keep its flag. Clearing it would mark the rotation repaired while
    /// every contact still delivers into the abandoned queue.
    #[tokio::test]
    async fn a_regrant_sweep_keeps_its_flag_while_the_relay_is_unreachable() {
        let (_dir, mut account) = intro_vault("regrant-offline").await;
        seed_contact(&mut account, "friend", FRIEND);
        account.state.regrant_all = true;

        assert_eq!(account.retry_regrants().await.unwrap(), 0);
        assert!(
            account.state.regrant_all,
            "an undelivered re-grant must be retried, not marked done"
        );
    }

    #[tokio::test]
    async fn an_intro_request_addressed_to_this_vault_waits_for_consent() {
        let (_dir, mut stranger) = intro_vault("intro-target").await;
        let cid = seed_mesh_conclave(&mut stranger, STRANGER);

        // The friend relays Sam's request; the stranger is the target.
        let dirty = stranger
            .receive_intro_req(&cid, "mesh", SAM, "sam", STRANGER, "stranger", FRIEND)
            .await
            .unwrap();
        assert!(dirty, "a request addressed to this vault must be stored");
        assert_eq!(stranger.pending_intro_requests().len(), 1);
        assert_eq!(stranger.pending_intro_requests()[0].via, FRIEND);

        // A second copy replaces the first instead of stacking prompts.
        stranger
            .receive_intro_req(&cid, "mesh", SAM, "sam", STRANGER, "stranger", FRIEND)
            .await
            .unwrap();
        assert_eq!(stranger.pending_intro_requests().len(), 1);

        // Declining is silent and clears the record without adding a contact.
        stranger
            .answer_intro_request(&cid, SAM, false)
            .await
            .unwrap();
        assert!(stranger.pending_intro_requests().is_empty());
        assert!(
            stranger.contact_nick_for_protocol(SAM).is_none(),
            "a declined introduction must not create a contact"
        );
    }

    #[tokio::test]
    async fn a_relayed_intro_request_for_an_unreachable_target_is_dropped() {
        let (_dir, mut friend) = intro_vault("intro-courier").await;
        let cid = seed_mesh_conclave(&mut friend, FRIEND);

        // The courier does not know the target: it cannot help, so the
        // request is consumed without leaving a relay prompt.
        let dirty = friend
            .receive_intro_req(&cid, "mesh", SAM, "sam", STRANGER, "stranger", SAM)
            .await
            .unwrap();
        assert!(!dirty);
        assert!(friend.pending_intro_requests().is_empty());

        // Knowing the target makes the same request a relay prompt.
        seed_contact(&mut friend, "stranger", STRANGER);
        let dirty = friend
            .receive_intro_req(&cid, "mesh", SAM, "sam", STRANGER, "stranger", SAM)
            .await
            .unwrap();
        assert!(dirty);
        assert_eq!(friend.pending_intro_requests().len(), 1);

        // Dismissing it clears the record without sending anything.
        friend
            .dismiss_intro_request(&cid, SAM)
            .await
            .unwrap();
        assert!(friend.pending_intro_requests().is_empty());
    }

    #[tokio::test]
    async fn a_delivered_intro_is_stored_and_accepting_adds_the_contact() {
        let (_dir, mut sam) = intro_vault("intro-seeker").await;
        let cid = seed_mesh_conclave(&mut sam, SAM);

        // A forged origin — an invite "for" someone else's account — is
        // dropped: the invite is only valid in its owner's name.
        let dirty = sam
            .receive_intro(
                &cid, "mesh", STRANGER, "stranger", SAM, "not-a-real-invite", FRIEND, false,
            )
            .await
            .unwrap();
        assert!(!dirty, "the invite must come from the account it names");

        // A courier cannot swap in their own invite: the payload must name
        // the roster member it claims.
        let swapped = {
            let (_dir, mut friend_vault) = intro_vault("intro-swap").await;
            friend_vault.state.protocol_name = FRIEND.to_string();
            friend_vault.invite().await.unwrap()
        };
        let dirty = sam
            .receive_intro(&cid, "mesh", STRANGER, "stranger", SAM, &swapped, FRIEND, false)
            .await
            .unwrap();
        assert!(!dirty, "an invite naming someone else must be dropped");

        // The real shape: the stranger's own invite, carried by the friend.
        let invite = {
            let (_dir, mut stranger) = intro_vault("intro-owner").await;
            stranger.state.protocol_name = STRANGER.to_string();
            stranger.fresh_invite().await.unwrap()
        };
        let dirty = sam
            .receive_intro(&cid, "mesh", STRANGER, "stranger", SAM, &invite, FRIEND, false)
            .await
            .unwrap();
        assert!(dirty);
        assert_eq!(sam.pending_intros().len(), 1);

        // Accepting consumes the invite and creates the mutual contact.
        sam.accept_intro(&cid, STRANGER, "stranger").await.unwrap();
        assert!(sam.pending_intros().is_empty());
        let contact = sam
            .contacts()
            .iter()
            .find(|c| c.account_id == STRANGER)
            .expect("the introduced member must become a contact");
        assert_eq!(contact.nickname, "stranger");

        // A late duplicate has nothing to add.
        sam.receive_intro(&cid, "mesh", STRANGER, "stranger", SAM, &invite, FRIEND, false)
            .await
            .unwrap();
        assert!(sam.pending_intros().is_empty());
        assert_eq!(sam.contacts().iter().filter(|c| c.account_id == STRANGER).count(), 1);
    }

    #[tokio::test]
    async fn an_intro_cannot_collide_with_another_contacts_nickname() {
        let (_dir, mut sam) = intro_vault("intro-nick").await;
        let cid = seed_mesh_conclave(&mut sam, SAM);
        seed_contact(&mut sam, "taken", "deadbeef");
        let invite = {
            let (_dir, mut stranger) = intro_vault("intro-owner-2").await;
            stranger.state.protocol_name = STRANGER.to_string();
            stranger.fresh_invite().await.unwrap()
        };
        sam.receive_intro(&cid, "mesh", STRANGER, "stranger", SAM, &invite, FRIEND, false)
            .await
            .unwrap();

        assert!(sam.accept_intro(&cid, STRANGER, "taken").await.is_err());
        // The pending record survives a rejected nickname: the caller picks
        // another one rather than losing the introduction.
        assert_eq!(sam.pending_intros().len(), 1);
    }

    #[tokio::test]
    async fn a_reverse_intro_adds_the_contact_without_consuming_the_bundle() {
        let (_dir, mut sam) = intro_vault("intro-reverse").await;
        let cid = seed_mesh_conclave(&mut sam, SAM);

        // The stranger accepted our invite first, so their card comes back
        // flagged reverse: the bundle inside must stay unconsumed.
        let invite = {
            let (_dir, mut stranger) = intro_vault("intro-reverse-owner").await;
            stranger.state.protocol_name = STRANGER.to_string();
            stranger.fresh_invite().await.unwrap()
        };
        let dirty = sam
            .receive_intro(&cid, "mesh", STRANGER, "stranger", SAM, &invite, FRIEND, true)
            .await
            .unwrap();
        assert!(dirty);

        sam.accept_intro(&cid, STRANGER, "stranger").await.unwrap();
        assert!(sam.pending_intros().is_empty());
        let contact = sam
            .contacts()
            .iter()
            .find(|c| c.account_id == STRANGER)
            .expect("the reverse card still makes the sender a contact");
        assert_eq!(contact.nickname, "stranger");
        assert!(
            contact.delivery_queue_id.is_none(),
            "a reverse accept must not grant a queue: our credentials already travelled with the invite the sender consumed"
        );
    }

    #[test]
    fn intro_payloads_round_trip_through_their_tags() {
        let req = MessagePayload::ConclaveIntroReq {
            conclave_id: "c1".into(),
            name: "mesh".into(),
            seeker: SAM.into(),
            seeker_nick: "sam".into(),
            target: STRANGER.into(),
            target_nick: "stranger".into(),
        };
        let intro = MessagePayload::ConclaveIntro {
            conclave_id: "c1".into(),
            name: "mesh".into(),
            target: STRANGER.into(),
            target_nick: "stranger".into(),
            seeker: SAM.into(),
            invite: "hush-invite-1".into(),
            reverse: false,
        };
        for payload in [req, intro] {
            let decoded = MessagePayload::decode(&payload.encode().unwrap()).unwrap();
            match (payload, decoded) {
                (MessagePayload::ConclaveIntroReq { seeker, .. }, MessagePayload::ConclaveIntroReq { seeker: s, .. })
                | (MessagePayload::ConclaveIntro { seeker, .. }, MessagePayload::ConclaveIntro { seeker: s, .. }) => {
                    assert_eq!(seeker, s);
                }
                _ => panic!("intro payload did not round trip"),
            }
        }
    }

}
