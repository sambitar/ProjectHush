use std::fs;
use std::path::{Path, PathBuf};

use libsignal_protocol::{IdentityKeyStore, SessionStore};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::backup::{export_vault, import_vault};
use crate::chat_log::{ChatLog, ChatRecordPayload, MailAttachmentRecord, MailState};
use crate::device::{DeviceKeys, LEGACY_DEVICE_FILE};
use crate::device_list::{DeviceList, DeviceRecord};
use crate::prekey::PreKeyRecord;
use crate::recovery::{ensure_recovery_files, generate_recovery_code, recovery_files_exist, unwrap_master_with_recovery};
use crate::crypto::{decrypt, encrypt};
use crate::error::Error;
use crate::invite::InvitePayload;
use crate::message::{InboxResult, ReceivedAttachment, ReceivedMessage};
use crate::payload::{MailAttachment, MessagePayload, MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS};
use crate::relay_client::{normalize_relay_base, RelayClient};
use crate::signal::{
    decrypt_message, encrypt_message, invite_from_bundle, mint_prekey_bundle,
    process_contact_bundle, protocol_address, protocol_name_from_identity,
};
use crate::signal_persist::import_store;
use crate::vault::{BlobId, Vault};
use crate::wire::{decode_relay_blob, encode_relay_blob};

/// How long a contact's cached device set is trusted before a send re-fetches it
/// (Q5).
///
/// The window is what makes revocation eventual rather than instant: a peer may
/// still encrypt to a device that was revoked inside this window.
const DEVICE_LIST_TTL_SECS: u64 = 5 * 60;

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

/// One device belonging to a contact.
///
/// This is the routing projection of a device-list entry: enough to deliver to
/// that device, plus the identity key the account vouched for, so a session is
/// only ever built against a key that device 1 signed for. Registration ids and
/// prekey material are deliberately absent — they arrive in the prekey bundle
/// fetched at session setup, and keeping a second copy here would be state that
/// can go stale.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactDevice {
    pub device_id: u32,
    /// The device's identity key, from the device-1-signed list (or the invite,
    /// for the first device).
    pub identity_key: Vec<u8>,
    /// That device's relay inbox.
    pub queue_id: String,
    pub queue_token: String,
    /// When the account signed this device into its list.
    ///
    /// Carried through a republish rather than recomputed: the date a device
    /// joined is a fact about that device, and overwriting it on every list
    /// write would make a device that linked a year ago look like it just did.
    /// `0` for a device known only from an invite, which has no list entry yet.
    #[serde(default)]
    pub added_unix: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Contact {
    pub nickname: String,
    /// Device 1's identity key hash, which is what the peer pinned when the
    /// invite was exchanged. Every device of the account shares it.
    pub account_id: String,
    /// Every device we know this contact has.
    ///
    /// Before multi-device a contact *was* one device with the details stored
    /// inline; that shape is folded into a one-device set on read, so an
    /// existing vault upgrades silently.
    pub devices: Vec<ContactDevice>,
    /// This contact's chosen mail handle (local part only), learned from their
    /// invite. `None` for contacts added before mail, or who never set one.
    #[serde(default)]
    pub mail_handle: Option<String>,
    /// True after we successfully received at least one message from them (they added our invite).
    pub inbound_ready: bool,
    /// When `devices` was last confirmed against the relay's signed list.
    ///
    /// `0` means "never fetched", which is the state every contact starts in:
    /// an invite describes one device and the rest are learned from the list.
    /// Peers refresh on a TTL rather than per send (Q5), so this is a cache
    /// timestamp, not a message ordering value.
    #[serde(default)]
    pub devices_fetched_unix: u64,
}

impl Contact {
    /// The device we deliver to when we have no reason to prefer another.
    pub fn primary_device(&self) -> Option<&ContactDevice> {
        self.devices
            .iter()
            .find(|d| d.device_id == 1)
            .or_else(|| self.devices.first())
    }
}

impl<'de> Deserialize<'de> for Contact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// The stored shapes. `Current` needs `devices`, `Legacy` needs
        /// `queue_id`, so the two cannot be confused for one another.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Current {
                nickname: String,
                account_id: String,
                devices: Vec<ContactDevice>,
                #[serde(default)]
                mail_handle: Option<String>,
                #[serde(default)]
                inbound_ready: bool,
                #[serde(default)]
                devices_fetched_unix: u64,
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
            Repr::Current {
                nickname,
                account_id,
                devices,
                mail_handle,
                inbound_ready,
                devices_fetched_unix,
            } => Contact {
                nickname,
                account_id,
                devices,
                mail_handle,
                inbound_ready,
                devices_fetched_unix,
            },
            Repr::Legacy {
                nickname,
                protocol_name,
                queue_id,
                queue_token,
                mail_handle,
                inbound_ready,
            } => {
                // A pre-multi-device contact was exactly one device, and its
                // `protocol_name` was that device's identity key hash. So the
                // identity key is recovered by decoding it rather than being
                // lost, and no re-invite is needed.
                let identity_key = hex::decode(&protocol_name).unwrap_or_default();
                Contact {
                    nickname,
                    account_id: protocol_name,
                    devices: vec![ContactDevice {
                        device_id: 1,
                        identity_key,
                        queue_id,
                        queue_token,
                        added_unix: 0,
                    }],
                    mail_handle,
                    inbound_ready,
                    // The legacy shape had no list to fetch from, so this is
                    // "never confirmed" and the next send will refresh it.
                    devices_fetched_unix: 0,
                }
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
struct AccountState {
    relay: String,
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
    /// This account's *other* devices, as learned from its own published device
    /// list. Empty for a single-device account, which is why the own-device copy
    /// costs nothing when multi-device is not in use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    own_devices: Vec<ContactDevice>,
}

pub struct Account {
    vault: Vault,
    /// This device's own keys and relay inbox, kept out of the shared vault so
    /// it is never handed to another device.
    device: DeviceKeys,
    state: AccountState,
    dir: PathBuf,
    chat_log: ChatLog,
    pending_recovery_code: Option<String>,
}

impl Account {
    pub async fn create(dir: &Path, passphrase: &str, relay: &str) -> Result<Self, Error> {
        let relay = normalize_relay_base(relay)?;
        let vault = Vault::create(dir, passphrase)?;
        let master = *vault.master_key();
        let mut device = DeviceKeys::create(dir, &master, 1).await?;
        let device_id = device.device_id();
        let _bundle = mint_prekey_bundle(device.store_mut(), device_id).await?;
        let protocol_name = protocol_name_from_identity(
            &device
                .store_mut()
                .get_identity_key_pair()
                .await
                .map_err(|_| Error::Corrupt)?,
        );
        let state = AccountState {
            relay,
            protocol_name,
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: false,
            last_backup_unix: 0,
            receive_queue_id: None,
            receive_queue_token: None,
            mail_handle: None,
            own_devices: Vec::new(),
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
                DeviceKeys::create(dir, &master, 1).await?
            }
            Err(e) => return Err(e),
        };

        let chat_log = ChatLog::load(dir, &master)?;

        // A rotated queue invalidates any cached invite, which names the old
        // inbox. The device list also has to be republished for peers to find
        // the new one; that needs the relay, so it is left to the caller rather
        // than done here, where open must stay offline-capable.
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
        let account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code,
        };
        if needs_save {
            account.save().await?;
        }
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

    pub fn vault_dir(&self) -> &Path {
        &self.dir
    }

    pub fn relay(&self) -> &str {
        &self.state.relay
    }

    pub fn protocol_name(&self) -> &str {
        &self.state.protocol_name
    }

    pub fn mail_handle(&self) -> Option<&str> {
        self.state.mail_handle.as_deref()
    }

    pub fn mail_address(&self) -> Option<String> {
        self.state
            .mail_handle
            .as_deref()
            .map(crate::mail::format_address)
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
        let local_id = self.device.device_id();
        process_contact_bundle(
            self.device.store_mut(),
            &local_name,
            local_id,
            &payload.protocol_name,
            payload.device_id,
            &bundle,
        )
        .await?;
        self.state.contacts.push(Contact {
            nickname: nickname.to_string(),
            account_id: payload.protocol_name.clone(),
            // An invite is exchanged with exactly one device, so a new contact
            // starts as a one-device set. Any other devices arrive when we
            // refresh the list from the relay.
            devices: vec![ContactDevice {
                device_id: payload.device_id,
                identity_key: payload.identity_key.clone(),
                queue_id: payload.queue_id,
                queue_token: payload.queue_token,
                // The invite is not the signed list, so there is no signed
                // "added" date to carry. The list refresh fills it in.
                added_unix: 0,
            }],
            mail_handle: payload.handle.clone(),
            inbound_ready: false,
            devices_fetched_unix: 0,
        });
        self.save().await?;
        // The invite only describes the one device that minted it. If the
        // account has linked others, this is where we find out — and if it has
        // not, the list is absent and the invite was the whole story.
        let _ = self.refresh_contact_devices(nickname).await;
        Ok(())
    }

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
    /// relay accepts as `Sent`. Returns how many went out.
    ///
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

    pub fn draft_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.draft_records().cloned().collect()
    }

    pub async fn heartbeat_presence(&self) -> Result<(), Error> {
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .touch_presence(self.device.queue_id(), self.device.read_token())
            .await
    }

    pub async fn contact_is_online(&self, nickname: &str) -> Result<bool, Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?;
        let device = contact.primary_device().ok_or(Error::Corrupt)?;
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .contact_presence_online(&device.queue_id, &device.queue_token)
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
        // A linked device that joined since we last looked has to be in the
        // device set before we encrypt, or it silently misses the message.
        self.refresh_contact_devices_if_stale(nickname).await?;
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        if require_online && !self.contact_is_online(nickname).await? {
            return Err(Error::ContactOffline);
        }

        // One independent session per recipient device. libsignal has no group
        // object, so N devices means N ciphertexts and N queue puts.
        let plain = payload.encode()?;
        for device in contact.devices.clone() {
            self.deliver(&contact.account_id, &device, &plain).await?;
        }

        // A copy to our own other devices, so a linked device shows what this one
        // sent (Q6). Skipped entirely for a single-device account, which is every
        // account that has not linked one.
        let others: Vec<ContactDevice> = self
            .state
            .own_devices
            .iter()
            .filter(|d| d.device_id != self.device.device_id())
            .cloned()
            .collect();
        if !others.is_empty() {
            let copy = MessagePayload::OwnCopy {
                to_account_id: contact.account_id.clone(),
                inner: Box::new(payload),
            }
            .encode()?;
            let own_account = self.state.protocol_name.clone();
            for device in others {
                self.deliver(&own_account, &device, &copy).await?;
            }
        }

        self.save().await?;
        Ok(())
    }

    /// Encrypt to one device and put the ciphertext in its queue, opening a
    /// session first if there is not one.
    async fn deliver(
        &mut self,
        account_id: &str,
        device: &ContactDevice,
        plain: &[u8],
    ) -> Result<(), Error> {
        self.ensure_session(account_id, device).await?;
        let local_name = self.state.protocol_name.clone();
        let local_id = self.device.device_id();
        let ct = encrypt_message(
            self.device.store_mut(),
            &local_name,
            local_id,
            account_id,
            device.device_id,
            plain,
        )
        .await?;
        let body = encode_relay_blob(&ct);
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .put(&device.queue_id, &device.queue_token, &body)
            .await?;
        Ok(())
    }

    /// Make sure a session with one device exists, opening one from its published
    /// prekey record if not.
    ///
    /// The fetched record is checked against the identity key the account's
    /// signed device list vouched for. Without that check a relay could serve a
    /// bundle for a key it controls and read everything sent to that device.
    async fn ensure_session(
        &mut self,
        account_id: &str,
        device: &ContactDevice,
    ) -> Result<(), Error> {
        let remote = protocol_address(account_id, device.device_id)?;
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

        let relay = RelayClient::try_new(&self.state.relay)?;
        let bytes = relay
            .get_prekey_record(account_id, device.device_id)
            .await?;
        let record = PreKeyRecord::decode(&bytes)?;
        record.verify(&device.identity_key)?;
        let bundle = record.to_prekey_bundle()?;

        let local_name = self.state.protocol_name.clone();
        let local_id = self.device.device_id();
        process_contact_bundle(
            self.device.store_mut(),
            &local_name,
            local_id,
            account_id,
            device.device_id,
            &bundle,
        )
        .await
    }

    /// Publish this device's prekey record, so a peer that has never exchanged an
    /// invite with it can still open a session.
    pub async fn publish_prekeys(&mut self, one_time_count: usize) -> Result<(), Error> {
        let record = self.device.mint_prekeys(one_time_count).await?;
        let body = record.encode()?;
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .put_prekey_record(&self.state.protocol_name, self.device.device_id(), &body)
            .await?;
        Ok(())
    }

    /// Publish this account's device list, signed by device 1.
    ///
    /// Only device 1 can sign it (Q11), so this refuses from any other device
    /// rather than producing a signature no peer would accept. Other devices are
    /// carried over from what we already know, so republishing does not drop
    /// them.
    pub async fn publish_device_list(&mut self) -> Result<(), Error> {
        if self.device.device_id() != 1 {
            return Err(Error::NotSigningDevice);
        }
        let mut devices: Vec<DeviceRecord> = self
            .state
            .own_devices
            .iter()
            .filter(|d| d.device_id != self.device.device_id())
            .map(|d| DeviceRecord {
                device_id: d.device_id,
                identity_key: d.identity_key.clone(),
                registration_id: 0,
                queue_id: d.queue_id.clone(),
                queue_token: d.queue_token.clone(),
                // Kept from the last list rather than reset, or a republish
                // would redate every device to today.
                added_unix: d.added_unix,
            })
            .collect();
        // This device's own entry. The date is kept from the last list if we have
        // already published one, so republishing does not redate device 1 either.
        let self_added = self
            .state
            .own_devices
            .iter()
            .find(|d| d.device_id == self.device.device_id())
            .map(|d| d.added_unix)
            .filter(|at| *at > 0)
            .unwrap_or_else(now_unix);
        devices.push(DeviceRecord {
            device_id: self.device.device_id(),
            identity_key: self.device.identity_public().await?,
            registration_id: self.device.registration_id().await?,
            queue_id: self.device.queue_id().to_string(),
            queue_token: self.device.queue_token(),
            added_unix: self_added,
        });

        let key_pair = self.device.identity_key_pair().await?;
        let signed = DeviceList::new(self.state.protocol_name.clone(), devices).sign(&key_pair)?;
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .put_device_list(&self.state.protocol_name, &signed)
            .await
    }

    /// Make sure the published device list names this device's current inbox.
    ///
    /// A rotated inbox is reachable by peers only through the published list.
    /// [`DeviceKeys::queue_rotated`] covers the single unlock that performed the
    /// rotation, and the publish there is best effort: if the relay is down, the
    /// flag is already spent (the device file is rewritten on that same open), so
    /// nothing ever retries and peers keep delivering to an inbox this device no
    /// longer reads. Comparing against what is actually published instead of
    /// trusting a one-shot flag makes the repair durable, and also heals a vault
    /// whose publish was lost before this check existed.
    ///
    /// Only device 1 can sign the list (Q11); a linked device has no way to
    /// republish its own inbox, so it is left alone here.
    ///
    /// Returns whether a publish was needed. A relay error is the caller's to
    /// log: the next unlock retries.
    pub async fn ensure_device_list_published(&mut self) -> Result<bool, Error> {
        if self.device.device_id() != 1 {
            return Ok(false);
        }
        // Also seeds `own_devices` with the stored `added_unix`, so republishing
        // does not redate this device.
        self.refresh_own_devices().await?;
        if published_list_names_inbox(
            &self.state.own_devices,
            self.device.device_id(),
            self.device.queue_id(),
        ) {
            return Ok(false);
        }
        self.publish_device_list().await?;
        Ok(true)
    }

    /// Learn this account's own device set from the list published to the relay.
    ///
    /// The list is signed by device 1, so a relay cannot add a device to it and
    /// have us deliver a copy of our own messages to a stranger. An absent list
    /// is not an error: an account that has never linked a device has nothing to
    /// publish (Q10), and this device is then the whole set.
    pub async fn refresh_own_devices(&mut self) -> Result<usize, Error> {
        let relay = RelayClient::try_new(&self.state.relay)?;
        let Some(body) = relay.get_device_list(&self.state.protocol_name).await? else {
            self.state.own_devices.clear();
            self.save().await?;
            return Ok(1);
        };
        let list = DeviceList::verify(&body, &self.state.protocol_name)?;
        self.state.own_devices = list
            .devices
            .iter()
            .map(|d| ContactDevice {
                device_id: d.device_id,
                identity_key: d.identity_key.clone(),
                queue_id: d.queue_id.clone(),
                queue_token: d.queue_token.clone(),
                added_unix: d.added_unix,
            })
            .collect();
        self.save().await?;
        Ok(self.state.own_devices.len())
    }

    /// Pull a contact's device set from the relay's signed list.
    ///
    /// The list is signed by *their* device 1 and verified against the account
    /// id we pinned when we exchanged invites, so a relay cannot add a device to
    /// it and have us deliver to a key it controls. A `404` means they have
    /// never linked a device: the invite we already hold stays the whole set, and
    /// we record the check so we do not re-ask on every send.
    ///
    /// The queue and identity key of the device we already knew are kept when the
    /// list agrees with the invite; a device the list *drops* is removed, which is
    /// what makes a revocation take effect on our side.
    pub async fn refresh_contact_devices(&mut self, nickname: &str) -> Result<usize, Error> {
        let account_id = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .account_id
            .clone();

        let relay = RelayClient::try_new(&self.state.relay)?;
        let devices = match relay.get_device_list(&account_id).await? {
            Some(body) => {
                let list = DeviceList::verify(&body, &account_id)?;
                list.devices
                    .iter()
                    .map(|d| ContactDevice {
                        device_id: d.device_id,
                        identity_key: d.identity_key.clone(),
                        queue_id: d.queue_id.clone(),
                        queue_token: d.queue_token.clone(),
                        added_unix: d.added_unix,
                    })
                    .collect::<Vec<_>>()
            }
            None => {
                // Nothing published. Keep what the invite gave us, but mark the
                // check as done so this is not retried on every send.
                let count = {
                    let contact = self
                        .state
                        .contacts
                        .iter_mut()
                        .find(|c| c.nickname == nickname)
                        .ok_or(Error::ContactNotFound)?;
                    contact.devices_fetched_unix = now_unix();
                    contact.devices.len()
                };
                self.save().await?;
                return Ok(count);
            }
        };

        if devices.is_empty() {
            // A signed list with no devices is not a revocation of everyone —
            // it is device 1 telling us it revoked itself, which Q11 forbids.
            // Treat it as a broken list rather than silently dropping the
            // contact's only way to receive.
            return Err(Error::Corrupt);
        }

        let contact = self
            .state
            .contacts
            .iter_mut()
            .find(|c| c.nickname == nickname)
            .ok_or(Error::ContactNotFound)?;
        contact.devices = devices;
        contact.devices_fetched_unix = now_unix();
        let count = contact.devices.len();
        self.save().await?;
        Ok(count)
    }

    /// Refresh a contact's device set if the cached one has aged out (Q5).
    ///
    /// Called before a send, and by the daemon before it reports presence. Both
    /// read the cached inbox: a contact whose queue has moved (the credential
    /// split, or a newly linked device) would otherwise read as offline and stay
    /// that way, because the UI refuses to send to a contact shown offline — so
    /// the send that would have refreshed the cache never happens. A failure
    /// here is deliberately not fatal: a relay hiccup should not stop a message
    /// the devices we already know can receive.
    pub async fn refresh_contact_devices_if_stale(&mut self, nickname: &str) -> Result<(), Error> {
        let fetched = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .devices_fetched_unix;
        if now_unix().saturating_sub(fetched) < DEVICE_LIST_TTL_SECS {
            return Ok(());
        }
        self.refresh_contact_devices(nickname).await?;
        Ok(())
    }

    /// This account's other devices, if any are known.
    pub fn own_devices(&self) -> &[ContactDevice] {
        &self.state.own_devices
    }

    /// The private credential for this account's own inbox.
    ///
    /// It authorizes listing, reading, deleting and burning, so it is never
    /// published — the invite and the device list carry only the derived
    /// delivery token. Exposed for local callers that talk to the relay directly
    /// instead of going through [`Account::recv`].
    pub fn own_read_token(&self) -> &str {
        self.device.read_token()
    }

    /// Whether this device's relay inbox was replaced when the vault was opened.
    ///
    /// True once, for a vault written before the read/delivery credential split,
    /// and for a vault migrating off the pre-multi-device layout. The caller must
    /// republish the device list so peers stop delivering to the abandoned inbox;
    /// until then this device receives nothing from anyone who has not refreshed.
    pub fn queue_rotated(&self) -> bool {
        self.device.queue_rotated()
    }

    /// The next device id this account has never used.
    ///
    /// Only device 1 may hand this out, because only device 1 can sign the list
    /// that records it. The value is derived from the ids we already know about,
    /// so a device linking while another is offline still gets a fresh id —
    /// provided we refreshed first, which the caller does.
    pub fn next_free_device_id(&self) -> u32 {
        self.state
            .own_devices
            .iter()
            .map(|d| d.device_id)
            .chain(std::iter::once(self.device.device_id()))
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Take over a vault another device transferred, as the given device id.
    ///
    /// The transfer carries the shared vault — history, contacts, chat log — and
    /// deliberately not the sender's device key material (Q4). So this device
    /// mints its own identity, prekeys and relay inbox, and cannot be reached by
    /// anyone until device 1 signs it into the account's list.
    ///
    /// The id is an argument rather than something derived here, because only
    /// device 1 can sign the list entry that makes an id real, and so only device
    /// 1 can assign one.
    pub async fn adopt_linked_vault(
        dir: &Path,
        passphrase: &str,
        device_id: u32,
    ) -> Result<Self, Error> {
        if device_id < 2 {
            // Device 1 is the account. A second vault claiming to be it would
            // share its identity, which is the thing linking must never do.
            return Err(Error::InvalidLink);
        }
        let vault = Vault::open(dir, passphrase)?;
        let master = *vault.master_key();
        let packed = fs::read(dir.join("account.enc")).map_err(|_| Error::NotFound)?;
        let state_bytes = decrypt(&master, &packed)?;
        let mut state: AccountState =
            serde_json::from_slice(&state_bytes).map_err(|_| Error::Corrupt)?;

        // Whatever device file came with the transfer is the sending device's.
        // Replaced rather than adopted: two devices sharing one identity could
        // never be told apart, which is exactly what revocation has to do.
        let mut device = DeviceKeys::create(dir, &master, device_id).await?;
        mint_prekey_bundle(device.store_mut(), device_id).await?;

        // A cached invite names the *sending* device's inbox, and handing it out
        // from here would send a contact to a queue this device cannot read.
        state.invite_cache = None;
        // The account's device set is learned from the published list, once
        // device 1 has added us. Until then this device knows only about itself.
        state.own_devices.clear();

        let chat_log = ChatLog::load(dir, &master)?;
        let account = Self {
            vault,
            device,
            state,
            dir: dir.to_path_buf(),
            chat_log,
            pending_recovery_code: None,
        };
        account.save().await?;
        Ok(account)
    }

    /// This device's entry for the account's signed list, for device 1 to sign.
    ///
    /// This is what a linking device sends back over the link channel. It carries
    /// the *delivery* token, never the read token: the list is public, and a
    /// public credential must not authorize reading or burning the inbox it
    /// names.
    pub async fn link_request(&self) -> Result<ContactDevice, Error> {
        Ok(ContactDevice {
            device_id: self.device.device_id(),
            identity_key: self.device.identity_public().await?,
            queue_id: self.device.queue_id().to_string(),
            queue_token: self.device.queue_token(),
            added_unix: now_unix(),
        })
    }

    /// Add a linked device to this account's signed list, and republish it.
    ///
    /// The request is the new device's own statement of its identity key, inbox
    /// and delivery token. It cannot be checked against the list, because writing
    /// the list is the whole point — so device 1's signature on the result is the
    /// only authority, and this must never be called with a request that did not
    /// arrive over an authenticated link channel. The relay enforces the same
    /// statement from the other side: it refuses a prekey record whose identity
    /// key is not what this list says.
    ///
    /// The list is published before this returns, so a failure leaves the device
    /// recorded locally but not announced. Repeating the call is safe.
    pub async fn record_linked_device(&mut self, request: &ContactDevice) -> Result<(), Error> {
        if self.device.device_id() != 1 {
            return Err(Error::NotSigningDevice);
        }
        if request.device_id < 2 || request.device_id == self.device.device_id() {
            return Err(Error::InvalidLink);
        }
        if request.identity_key.is_empty() || request.queue_id.is_empty() {
            return Err(Error::InvalidLink);
        }
        self.state
            .own_devices
            .retain(|d| d.device_id != request.device_id);
        self.state.own_devices.push(request.clone());
        self.state.own_devices.sort_by_key(|d| d.device_id);
        self.publish_device_list().await
    }

    /// Publish this device's prekeys and learn the account's device set.
    pub async fn announce_linked_device(&mut self, one_time_count: usize) -> Result<(), Error> {
        self.publish_prekeys(one_time_count).await?;
        self.refresh_own_devices().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<InboxResult, Error> {
        let relay = RelayClient::try_new(&self.state.relay)?;
        let ids = relay
            .list(self.device.queue_id(), self.device.read_token())
            .await?;
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let mut dirty = false;
        for blob_id in ids {
            let body = relay
                .get(self.device.queue_id(), self.device.read_token(), &blob_id)
                .await?;
            let (ct, legacy_from) = decode_relay_blob(&body)?;
            let (plain, from_account) = match legacy_from {
                Some(from) => match self.decrypt_from(&from, 1, &ct).await {
                    Ok(p) => (p, from),
                    Err(Error::Session) => {
                        warnings.push(format!(
                            "Could not read a message from {} — they may not have added your invite yet (use Copy my invite on your side, they paste it under Add contact).",
                            short_name(&from)
                        ));
                        continue;
                    }
                    Err(e) => return Err(e),
                },
                None => match self.try_decrypt_from_any_device(&ct).await {
                    Ok(pair) => pair,
                    Err(Error::Session) => {
                        warnings.push(
                            "Could not decrypt a queued message — unknown sender or session not ready.".into(),
                        );
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            let payload = decode_legacy_payload(&plain)?;
            // A copy of a message we sent from one of our own other devices. It
            // belongs in the conversation it names, recorded as ours, and must
            // not be surfaced as an incoming message or the UI would show it
            // twice.
            if let MessagePayload::OwnCopy {
                to_account_id,
                inner,
            } = payload
            {
                self.file_own_copy(&to_account_id, *inner)?;
                relay
                    .delete(self.device.queue_id(), self.device.read_token(), &blob_id)
                    .await?;
                dirty = true;
                continue;
            }

            let nick = self
                .contact_nick_for_protocol(&from_account)
                .unwrap_or_else(|| short_name(&from_account));
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
                // Own copies are filed into the chat log above, before this match.
                MessagePayload::OwnCopy { .. } => unreachable!("own copy handled above"),
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
            relay
                .delete(self.device.queue_id(), self.device.read_token(), &blob_id)
                .await?;
            dirty = true;
        }
        if dirty {
            self.save().await?;
        }
        Ok(InboxResult {
            messages: out,
            warnings,
        })
    }

    fn find_contact(&self, nickname: &str) -> Option<&Contact> {
        self.state.contacts.iter().find(|c| c.nickname == nickname)
    }

    fn contact_nick_for_protocol(&self, account_id: &str) -> Option<String> {
        self.state
            .contacts
            .iter()
            .find(|c| c.account_id == account_id)
            .map(|c| c.nickname.clone())
    }

    /// Record a message this account sent from another of its own devices.
    fn file_own_copy(&mut self, to_account_id: &str, inner: MessagePayload) -> Result<(), Error> {
        let master = *self.vault.master_key();
        let nick = self
            .contact_nick_for_protocol(to_account_id)
            .unwrap_or_else(|| short_name(to_account_id));
        match inner {
            MessagePayload::Text { body } => {
                self.chat_log.append_text(&master, &nick, true, &body)?;
            }
            MessagePayload::File { data, name } => {
                let id = self.vault.put_blob(&data)?;
                self.chat_log
                    .append_file(&master, &nick, true, &name, &id, data.len() as u64)?;
            }
            MessagePayload::Mail {
                thread_id,
                subject,
                body,
                attachments,
            } => {
                let mut records = Vec::with_capacity(attachments.len());
                for a in attachments {
                    let id = self.vault.put_blob(&a.data)?;
                    records.push(MailAttachmentRecord {
                        name: a.name.clone(),
                        blob_id_hex: hex::encode(id.0),
                        size: a.data.len() as u64,
                    });
                }
                self.chat_log.append_mail(
                    &master,
                    &nick,
                    true,
                    &thread_id,
                    subject.as_deref(),
                    &body,
                    &records,
                    MailState::Sent,
                )?;
            }
            // A copy of a copy is meaningless and would loop if we ever fanned
            // out from a received copy, so it is dropped rather than filed.
            MessagePayload::OwnCopy { .. } => {}
        }
        Ok(())
    }

    async fn decrypt_from(
        &mut self,
        account_id: &str,
        device_id: u32,
        ct: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let local_name = self.state.protocol_name.clone();
        let local_id = self.device.device_id();
        decrypt_message(
            self.device.store_mut(),
            &local_name,
            local_id,
            account_id,
            device_id,
            ct,
        )
        .await
    }

    /// Try every device we know of, and our own other devices.
    ///
    /// A relay blob carries no sender id — that is the whole point of the queue
    /// model — so the sender can only be recovered by attempting decryption. The
    /// candidate set has to include our own other devices, because a copy from one
    /// of them is indistinguishable from a contact's message until it decrypts.
    async fn try_decrypt_from_any_device(
        &mut self,
        ct: &[u8],
    ) -> Result<(Vec<u8>, String), Error> {
        let mut candidates: Vec<(String, u32)> = Vec::new();
        for contact in &self.state.contacts {
            for device in &contact.devices {
                candidates.push((contact.account_id.clone(), device.device_id));
            }
        }
        let own_account = self.state.protocol_name.clone();
        let own_id = self.device.device_id();
        for device in &self.state.own_devices {
            if device.device_id != own_id {
                candidates.push((own_account.clone(), device.device_id));
            }
        }

        for (account_id, device_id) in candidates {
            if let Ok(plain) = self.decrypt_from(&account_id, device_id, ct).await {
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

        // Every address a session could live under: each device of each contact,
        // plus our own other devices, whose copies arrive on sessions too.
        let own_id = self.device.device_id();
        let mut session_addresses: Vec<(String, u32)> = Vec::new();
        for contact in &self.state.contacts {
            for device in &contact.devices {
                session_addresses.push((contact.account_id.clone(), device.device_id));
            }
        }
        let own_account = self.state.protocol_name.clone();
        for device in &self.state.own_devices {
            if device.device_id != own_id {
                session_addresses.push((own_account.clone(), device.device_id));
            }
        }
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
/// anyway. `from_store` mints a fresh queue, and the caller republishes so peers
/// find the new one.
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
    // Device 1 is the identity that already existed, so it keeps its key.
    Ok(DeviceKeys::from_store(1, store))
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

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a published device set already names `queue_id` for `device_id`.
///
/// Used to decide whether this device's inbox is reachable by peers through the
/// published list, so a republish happens only when the list has actually fallen
/// behind — including the case where no list was ever published.
fn published_list_names_inbox(
    devices: &[ContactDevice],
    device_id: u32,
    queue_id: &str,
) -> bool {
    devices
        .iter()
        .any(|d| d.device_id == device_id && d.queue_id == queue_id)
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
        let mut device = DeviceKeys::create(dir, &master, 1).await.unwrap();
        let _ = mint_prekey_bundle(device.store_mut(), 1).await.unwrap();
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
            protocol_name: protocol_name.clone(),
            contacts: Vec::new(),
            invite_cache: None,
            recovery_acknowledged: true,
            last_backup_unix: 0,
            receive_queue_id: Some(queue_id.clone()),
            receive_queue_token: Some(queue_token.clone()),
            mail_handle: None,
            own_devices: Vec::new(),
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
            "the caller has to know, so it can drop the stale invite and republish"
        );
        assert!(
            account.contacts().is_empty() || account.contacts()[0].devices[0].queue_id != old_queue_id,
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
        assert_eq!(contact.devices.len(), 1);
        assert_eq!(contact.devices[0].device_id, 1);
        assert_eq!(
            contact.devices[0].queue_id, "bob-queue",
            "delivery must still go to the inbox the old record named"
        );
        assert_eq!(contact.devices[0].queue_token, "bob-token");
        assert_eq!(
            contact.devices[0].identity_key, remote_identity,
            "the identity key is recoverable from the old protocol_name"
        );
        assert!(contact.inbound_ready);

        // The upgrade has to be written back, or the next open redoes it from a
        // shape that no longer exists.
        drop(account);
        let reopened = Account::open(&dir, "pw").await.unwrap();
        let contact = &reopened.contacts()[0];
        assert_eq!(contact.account_id, hex::encode(&remote_identity));
        assert_eq!(contact.devices.len(), 1);
        assert_eq!(contact.devices[0].queue_id, "bob-queue");
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

    /// The decision behind [`Account::ensure_device_list_published`]: a list that
    /// does not name this device's inbox has to be treated as behind, because
    /// that is exactly the state a lost publish leaves behind.
    #[test]
    fn a_published_list_is_behind_when_it_does_not_name_our_inbox() {
        let device = |device_id: u32, queue_id: &str| ContactDevice {
            device_id,
            identity_key: vec![0u8; 33],
            queue_id: queue_id.into(),
            queue_token: "token".into(),
            added_unix: 0,
        };

        assert!(published_list_names_inbox(&[device(1, "q1")], 1, "q1"));
        assert!(
            !published_list_names_inbox(&[device(1, "abandoned")], 1, "q1"),
            "a moved inbox means the list is behind, even for a device we know"
        );
        assert!(
            !published_list_names_inbox(&[device(2, "q1")], 1, "q1"),
            "another device's entry must not stand in for ours"
        );
        assert!(
            !published_list_names_inbox(&[], 1, "q1"),
            "no list at all is the state a publish that never landed leaves"
        );
    }
}
