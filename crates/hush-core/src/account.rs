use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use libsignal_protocol::{IdentityKeyStore, SessionStore};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::backup::{export_vault, import_vault};
use crate::chat_log::{ChatLog, ChatRecordPayload, ConclaveRecord, MailAttachmentRecord, MailState};
use crate::payload::{ConclaveDescriptor, ConclaveTrustee, TrusteeState};
use crate::device::{DeviceKeys, DEVICE_ID, LEGACY_DEVICE_FILE};
use crate::prekey::PreKeyRecord;
use crate::recovery::{ensure_recovery_files, generate_recovery_code, recovery_files_exist, unwrap_master_with_recovery};
use crate::crypto::{decrypt, encrypt};
use crate::error::Error;
use crate::invite::InvitePayload;
use crate::message::{InboxResult, ReceivedAttachment, ReceivedMessage, ReceivedPaper};
use crate::payload::{MailAttachment, MessagePayload, PaperDescriptor, MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS};
use crate::relay_client::{normalize_relay_base, validate_onion_relay, RelayClient};
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
    /// True after we successfully received at least one message from them (they added our invite).
    pub inbound_ready: bool,
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
                #[serde(default)]
                inbound_ready: bool,
            },
            Devices {
                nickname: String,
                account_id: String,
                devices: Vec<LegacyDevice>,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default)]
                mail_vault: Option<String>,
                #[serde(default)]
                inbound_ready: bool,
            },
            Legacy {
                nickname: String,
                protocol_name: String,
                queue_id: String,
                queue_token: String,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default)]
                inbound_ready: bool,
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
                inbound_ready,
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
                inbound_ready,
            },
            Repr::Devices {
                nickname,
                account_id,
                devices,
                mail_handle,
                mail_vault,
                inbound_ready,
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
                    inbound_ready,
                }
            }
            Repr::Legacy {
                nickname,
                protocol_name,
                queue_id,
                queue_token,
                mail_handle,
                inbound_ready,
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
                    inbound_ready,
                }
            }
        })
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
        };
        let recovery_code = generate_recovery_code();
        ensure_recovery_files(dir, &master, &recovery_code)?;
        let chat_log = ChatLog::load(dir, &master)?;
        let account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code: Some(recovery_code),
            relay_socks: None,
        };
        account.save().await?;
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
    fn relay_client(&self) -> Result<RelayClient, Error> {
        if self.state.use_tor_relay {
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
            RelayClient::try_new_with_proxy(base, Some(&socks))
        } else {
            RelayClient::try_new_with_proxy(&self.state.relay, None)
        }
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
            return Ok(cached.clone());
        }
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
        self.state.invite_cache = Some(encoded.clone());
        self.save().await?;
        Ok(encoded)
    }

    pub async fn add_contact(&mut self, nickname: &str, invite_str: &str) -> Result<(), Error> {
        let payload = InvitePayload::decode(invite_str)?;
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
        let bundle = payload.to_prekey_bundle()?;
        let local_name = self.state.protocol_name.clone();
        process_contact_bundle(
            self.device.store_mut(),
            &local_name,
            &payload.protocol_name,
            &bundle,
        )
        .await?;
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
            inbound_ready: false,
        });
        // Adding a contact clears any revocation left from a previous delete, so
        // a re-added contact is granted a queue again instead of being treated
        // as still blocked.
        self.state.revoked.retain(|a| a != &payload.protocol_name);
        self.save().await?;
        // Hand this account's per-contact queue to the contact, which is what
        // makes deleting them revocable. Best effort: the next drain re-grants
        // if the relay was down.
        if let Err(e) = self.grant_queues_to(nickname).await {
            eprintln!("hush: could not grant a queue to {nickname}: {e}");
        }
        Ok(())
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
        let targets: Vec<String> = conclave
            .joined()
            .filter(|m| m.account_id != me)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(&nick, payload.clone(), false).await?;
        }
        let master = *self.vault.master_key();
        self.chat_log
            .append_conclave_message(&master, conclave_id, true, None, body)?;
        Ok(())
    }

    /// Fan one paper payload out to the conclave's joined trustees.
    ///
    /// Papers ride the same 1:1 sessions as conclave messages: there is no
    /// relay-side room and no group object, so this is one encrypted copy per
    /// joined trustee. Unlike a message it is **not** written to the local chat
    /// log — the document lives in the daemon's paper sidecar, and the sender's
    /// own copy of an update is recorded there, not here.
    ///
    /// A paper follows the conclave's lifecycle: once the conclave has ended
    /// the document is read-only on every peer, so sending is refused exactly
    /// as `send_conclave_message` refuses.
    async fn fan_out_paper(
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
        let targets: Vec<String> = conclave
            .joined()
            .filter(|m| m.account_id != me)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(&nick, payload.clone(), false).await?;
        }
        Ok(())
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
        self.fan_out_paper(conclave_id, MessagePayload::PaperState { paper })
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
        self.fan_out_paper(conclave_id, MessagePayload::PaperHub { paper })
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
        let targets: Vec<String> = conclave
            .joined()
            .filter(|m| m.account_id != me && m.account_id != from_account)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(
                &nick,
                MessagePayload::PaperDelta {
                    paper_id: paper_id.to_string(),
                    update: update.to_vec(),
                },
                false,
            )
            .await?;
        }
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
        self.fan_out_paper(
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
        self.fan_out_paper(
            conclave_id,
            MessagePayload::PaperSnapshot {
                paper,
                state: state.to_vec(),
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
        let targets: Vec<String> = conclave
            .joined()
            .filter(|m| m.account_id != me)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(&nick, payload.clone(), false).await?;
        }
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

    /// Leave a conclave. Any joined trustee may leave.
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
        let descriptor = descriptor_of(&record);
        let targets: Vec<String> = record
            .joined()
            .filter(|m| m.account_id != me)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(
                &nick,
                MessagePayload::ConclaveEnd {
                    descriptor: descriptor.clone(),
                },
                false,
            )
            .await?;
        }
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
        let me = self.state.protocol_name.clone();
        let descriptor = descriptor_of(record);
        let targets: Vec<String> = record
            .joined()
            .filter(|m| m.account_id != me)
            .filter_map(|m| self.contact_nick_for_protocol(&m.account_id))
            .collect();
        for nick in targets {
            self.send_payload_inner(
                &nick,
                MessagePayload::ConclaveUpdate {
                    descriptor: descriptor.clone(),
                },
                false,
            )
            .await?;
        }
        Ok(())
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

    async fn send_payload(&mut self, nickname: &str, payload: MessagePayload) -> Result<(), Error> {
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
    async fn grant_queues_to(&mut self, nickname: &str) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        // A revoked account is not granted a queue again even if a stale contact
        // record still names it. This is a shortcut, not the security boundary:
        // the grant is verified by its signature on the far side anyway (see
        // `apply_queue_grant`). Not sending it saves a pointless delivery.
        if self.state.revoked.contains(&contact.account_id) {
            return Ok(());
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
        if let Err(e) = self
            .deliver_to(&contact, &contact.queue_id, &contact.queue_token, &plain)
            .await
        {
            eprintln!(
                "hush: grant to {} failed: {e}",
                short_name(&contact.account_id)
            );
        }
        // Encrypting the grant advances the session ratchet. Persist it here:
        // otherwise a caller that exits right after adding a contact (the CLI)
        // leaves the ratchet on disk at its pre-grant position, and the next
        // message is encrypted with a reused chain key that the peer cannot
        // decrypt. The daemon masks this by saving again on its next send.
        self.save().await?;
        Ok(())
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
        for (expected_contact, queue_id, read_token) in queues {
            let ids = relay.list_optional(&queue_id, &read_token).await?;
            for blob_id in ids {
                let body = relay.get(&queue_id, &read_token, &blob_id).await?;
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
                    Err(e) => return Err(e),
                };

                // A deleted contact must not be able to keep delivering into the
                // shared inbox and be surfaced. Its session may still be in
                // memory on the device that did the delete, so the blob can be
                // decrypted and attributed; drop it instead of filing it.
                if self.state.revoked.contains(&from_account) {
                    relay.delete(&queue_id, &read_token, &blob_id).await?;
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
                    relay.delete(&queue_id, &read_token, &blob_id).await?;
                    continue;
                }

                let nick = self
                    .contact_nick_for_protocol(&from_account)
                    .unwrap_or_else(|| short_name(&from_account));

                // Conclave control payloads. Membership is consensual, so an
                // invite, accept, decline, leave, update or end changes local
                // state and is never surfaced as a chat message. Only an
                // explicit accept ever materialises a conclave.
                match &payload {
                    MessagePayload::ConclaveInvite { descriptor } => {
                        if self.receive_conclave_invite(descriptor, &from_account, &nick)? {
                            dirty = true;
                        }
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                    MessagePayload::ConclaveAccept { descriptor } => {
                        if self.receive_conclave_accept(descriptor).await? {
                            dirty = true;
                        }
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                    MessagePayload::ConclaveDecline { conclave_id, member } => {
                        if self.receive_conclave_decline(conclave_id, member)? {
                            dirty = true;
                        }
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                    MessagePayload::ConclaveLeave { descriptor } => {
                        if self.receive_conclave_leave(descriptor).await? {
                            dirty = true;
                        }
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                    MessagePayload::ConclaveUpdate { descriptor }
                    | MessagePayload::ConclaveEnd { descriptor } => {
                        if self.receive_conclave_update(descriptor, &from_account)? {
                            dirty = true;
                        }
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                    _ => {}
                }

                let (file_blob_id, mail_attachments) = match &payload {
                    MessagePayload::File { data, name } => {
                        let id = self.vault.put_blob(data)?;
                        let master = *self.vault.master_key();
                        self.chat_log
                            .append_file(&master, &nick, false, name, &id, data.len() as u64)?;
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
                        (None, Vec::new())
                    }
                    // A conclave message. The conclave must already exist and
                    // the sender must be a joined trustee: a message never
                    // materialises a conclave, so a stray fan-out cannot invent
                    // one. The invite handled above must be accepted first.
                    MessagePayload::Conclave {
                        conclave_id,
                        body,
                        file,
                        ..
                    } => {
                        let known = self
                            .chat_log
                            .conclave(conclave_id)
                            .is_some_and(|c| c.is_joined(&from_account));
                        if !known {
                            relay.delete(&queue_id, &read_token, &blob_id).await?;
                            dirty = true;
                            continue;
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
                    | MessagePayload::ConclaveEnd { .. } => {
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
                                relay.delete(&queue_id, &read_token, &blob_id).await?;
                                dirty = true;
                                continue;
                            }
                        }
                        papers.push(ReceivedPaper {
                            from_account: from_account.clone(),
                            from_nick: nick.clone(),
                            payload: payload.clone(),
                        });
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        dirty = true;
                        continue;
                    }
                    // A tag this build does not know, from one that does.
                    // Consumed and dropped: it can never be interpreted here,
                    // and leaving it would re-deliver it on every poll forever.
                    MessagePayload::Unknown => {
                        relay.delete(&queue_id, &read_token, &blob_id).await?;
                        continue;
                    }
                };
                if let Some(c) = self
                    .state
                    .contacts
                    .iter_mut()
                    .find(|c| c.account_id == from_account)
                {
                    c.inbound_ready = true;
                }
                out.push(ReceivedMessage {
                    from_nick: nick,
                    payload,
                    file_blob_id,
                    mail_attachments,
                });
                relay.delete(&queue_id, &read_token, &blob_id).await?;
                dirty = true;
            }
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
            joined,
            departed,
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
    use crate::vault::Vault;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hush-account-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
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
        assert!(contact.inbound_ready);

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
        assert!(contact.inbound_ready);
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

}
