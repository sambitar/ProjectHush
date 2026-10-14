use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, MASTER_KEY_LEN};
use crate::error::Error;
use crate::payload::{ConclaveTrustee, TrusteeState};
use crate::vault::BlobId;

/// The chat auto-delete policy, as the daemon's prefs file states it. The log
/// itself enforces it on every append; mail, drafts and drops are out of scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRetention {
    /// Delete every chat message older than the current UTC day.
    #[serde(default)]
    pub daily: bool,
    /// Keep at most this many chat messages per contact, oldest deleted
    /// first. `None` keeps whatever the daily rule does not cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_lines: Option<u32>,
}

/// Whether a record is a 1:1 chat message (text or file), as opposed to the
/// mail, draft and drop records that share the same log.
fn is_chat_payload(r: &ChatRecord) -> bool {
    matches!(
        r.payload,
        ChatRecordPayload::Text { .. } | ChatRecordPayload::File { .. }
    )
}

/// Whether `ts_ms` sits inside an optional `[from, to]` millisecond window.
fn in_range(ts_ms: u64, from_ms: Option<u64>, to_ms: Option<u64>) -> bool {
    from_ms.map_or(true, |f| ts_ms >= f) && to_ms.map_or(true, |t| ts_ms <= t)
}

/// The millisecond at which the current UTC day began. The daily rule is a
/// whole-day purge, and the boundary is UTC: the log has no timezone of its
/// own, and a local boundary would make the purge depend on where the vault
/// happens to run.
fn utc_day_start_ms() -> u64 {
    let now = now_ms();
    now - (now % 86_400_000)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: String,
    pub contact_nick: String,
    pub outgoing: bool,
    pub ts_ms: u64,
    pub payload: ChatRecordPayload,
}

/// One attachment as stored in the log: the bytes live in a vault blob, and the
/// record keeps only the reference, name and size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailAttachmentRecord {
    pub name: String,
    pub blob_id_hex: String,
    pub size: u64,
}

/// Where an outgoing mail message is in its journey.
///
/// Hush has no delivery receipt, so this is deliberately only two states: the
/// relay accepted the ciphertext (`Sent`), or it did not and we still owe the
/// recipient a copy (`Queued`). Anything finer would be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MailState {
    /// Handed to the relay. This is what the Sent folder shows.
    #[default]
    Sent,
    /// Composed, recorded, but the relay did not accept it. The daemon retries
    /// and flips this to `Sent`; the Outbox folder shows exactly these.
    Queued,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatRecordPayload {    Text {
        body: String,
    },
    File {
        name: String,
        blob_id_hex: String,
        size: u64,
    },
    Mail {
        thread_id: String,
        subject: Option<String>,
        body: String,
        attachments: Vec<MailAttachmentRecord>,
        /// Defaulted so records written before the Outbox existed decode as
        /// `Sent`, which is what they are.
        #[serde(default)]
        state: MailState,
    },
    /// An outsider drop, opened and filed. Unlike mail there is no peer and no
    /// thread: provenance is the rendezvous slot the container was parked on,
    /// and the only place it appears is the member's own Drops folder.
    Drop {
        /// The slot it arrived on — which published drop key claimed it.
        rendezvous_id: String,
        #[serde(default)]
        subject: Option<String>,
        body: String,
        attachments: Vec<MailAttachmentRecord>,
        /// When the dropper sealed it, which is not when it arrived.
        #[serde(default)]
        created_unix: u64,
    },
    /// A composed message that has not been sent. Unlike mail this is mutable:
    /// it is edited and deleted in place, which is why the log grew
    /// `replace_payload`/`remove_record` alongside it.
    Draft {
        /// Set when the draft is a reply, so sending keeps the conversation.
        #[serde(default)]
        thread_id: Option<String>,
        /// Recipient nickname; empty for a draft that is not addressed yet.
        #[serde(default)]
        to: String,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        body: String,
        #[serde(default)]
        attachments: Vec<MailAttachmentRecord>,
        #[serde(default)]
        updated_ms: u64,
    },
}

/// One message inside a conclave's own log.
///
/// Conclave messages do not go through [`ChatRecord`]: a conclave is a local
/// construct with its own log, and keeping its messages here is what keeps them
/// out of the chat and mail projections without a filter that could be forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveMessage {
    pub id: String,
    pub outgoing: bool,
    /// The sender's nickname as this vault knows it; `None` for our own messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub body: String,
    /// The file shared with this message, if any. Only the vault blob reference
    /// is stored: the bytes live in the blob store, exactly like a 1:1 file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<ConclaveFileRecord>,
    pub ts_ms: u64,
}

/// A file shared into a conclave, as stored in the conclave's log.
///
/// This is the record shape; the wire shape is `payload::ConclaveFile`, which
/// carries the bytes. The bytes are stored as an encrypted vault blob and only
/// this reference travels in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveFileRecord {
    pub name: String,
    pub blob_id_hex: String,
    pub size: u64,
}

/// A conclave: a local record, not a relay-side room.
///
/// It holds the id a descriptor is keyed on, the name, the trustees (with their
/// per-member state), when it was created, its convener, and its own message
/// log. Group chat is sender-side fan-out over the trustees' 1:1 sessions; there
/// is no group key and no server object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConclaveRecord {
    pub id: String,
    pub name: String,
    /// The trustees, keyed on the stable account id.
    ///
    /// `alias` reads the pre-rename `members` key, whose entries were bare
    /// nickname strings (still accepted by [`ConclaveTrustee`]'s custom
    /// `Deserialize`), so an existing vault loads. New records write `trustees`.
    #[serde(default, alias = "members")]
    pub trustees: Vec<ConclaveTrustee>,
    pub created_ms: u64,
    #[serde(default)]
    pub messages: Vec<ConclaveMessage>,
    /// Set when the conclave ends. The record and its message log stay in the
    /// vault; an ended conclave is read-only on every peer. Defaulted so a
    /// record written before ending existed loads as still active.
    #[serde(default)]
    pub ended: bool,
    /// The convener's account id. The convener is the only trustee who may
    /// invite, kick, or end. Empty for a record written before governance
    /// existed; hydration fills it with the local account.
    #[serde(default, alias = "creator")]
    pub convener: String,
}

impl ConclaveRecord {
    /// Every joined trustee, in order.
    pub fn joined(&self) -> impl Iterator<Item = &ConclaveTrustee> {
        self.trustees
            .iter()
            .filter(|m| m.state == TrusteeState::Joined)
    }

    /// Every invited (not yet accepted) trustee.
    pub fn invited(&self) -> impl Iterator<Item = &ConclaveTrustee> {
        self.trustees
            .iter()
            .filter(|m| m.state == TrusteeState::Invited)
    }

    pub fn trustee(&self, account_id: &str) -> Option<&ConclaveTrustee> {
        self.trustees.iter().find(|m| m.account_id == account_id)
    }

    pub fn is_joined(&self, account_id: &str) -> bool {
        self.trustee(account_id)
            .is_some_and(|m| m.state == TrusteeState::Joined)
    }

    /// Insert or update one trustee's state.
    ///
    /// The merge is monotonic, so a stale snapshot cannot undo a newer fact:
    /// a terminal state (left or kicked) is never revived by a live one, and a
    /// joined trustee is never downgraded back to invited by a peer that had not
    /// yet heard they joined. Returns whether anything changed.
    pub fn upsert_trustee(&mut self, incoming: &ConclaveTrustee) -> bool {
        match self
            .trustees
            .iter_mut()
            .find(|m| m.account_id == incoming.account_id)
        {
            Some(existing) => {
                let terminal = matches!(existing.state, TrusteeState::Left | TrusteeState::Kicked);
                let incoming_live =
                    matches!(incoming.state, TrusteeState::Invited | TrusteeState::Joined);
                let downgrade =
                    existing.state == TrusteeState::Joined && incoming.state == TrusteeState::Invited;
                let mut changed = false;
                if !(terminal && incoming_live) && !downgrade && existing.state != incoming.state {
                    existing.state = incoming.state;
                    changed = true;
                }
                if existing.nick.is_empty() && !incoming.nick.is_empty() {
                    existing.nick = incoming.nick.clone();
                    changed = true;
                }
                changed
            }
            None => {
                self.trustees.push(incoming.clone());
                true
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ChatLogFile {
    messages: Vec<ChatRecord>,
    /// Conclaves this vault holds. Defaulted so a log written before conclaves
    /// existed still loads.
    ///
    /// The on-disk key stays `meetings`: every existing `chat.enc` writes it that
    /// way, and pinning it means an older build can still read a log this build
    /// writes (a rollback must not lose records). `alias` accepts the renamed key
    /// if one is ever written.
    #[serde(default, rename = "meetings", alias = "conclaves")]
    conclaves: Vec<ConclaveRecord>,
}

pub struct ChatLog {
    path: PathBuf,
    file: ChatLogFile,
}

impl ChatLog {
    pub fn load(dir: &Path, master: &[u8; MASTER_KEY_LEN]) -> Result<Self, Error> {
        let path = dir.join("chat.enc");
        let file = if path.exists() {
            let packed = fs::read(&path)?;
            let plain = decrypt(master, &packed)?;
            serde_json::from_slice(&plain).map_err(|_| Error::Corrupt)?
        } else {
            ChatLogFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn save(&self, master: &[u8; MASTER_KEY_LEN]) -> Result<(), Error> {
        let plain = serde_json::to_vec(&self.file).map_err(|_| Error::Corrupt)?;
        let packed = encrypt(master, &plain)?;
        fs::write(&self.path, packed)?;
        Ok(())
    }

    pub fn append_text(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        body: &str,
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Text {
                    body: body.to_string(),
                },
            },
        )
    }

    pub fn append_file(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        name: &str,
        blob_id: &BlobId,
        size: u64,
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::File {
                    name: name.to_string(),
                    blob_id_hex: hex::encode(blob_id.0),
                    size,
                },
            },
        )
    }

    /// Append an outgoing or incoming mail message, returning its record id.
    #[allow(clippy::too_many_arguments)]
    pub fn append_mail(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        thread_id: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
        state: MailState,
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Mail {
                    thread_id: thread_id.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    state,
                },
            },
        )?;
        Ok(id)
    }

    /// Append an opened outsider drop to the Drops folder, returning its id.
    ///
    /// `subject` is `None` when the dropper left it empty — the folder renders
    /// the difference between no subject and one that was left blank. There is
    /// no contact: the record's provenance is the rendezvous slot alone.
    pub fn append_drop(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        rendezvous_id: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
        created_unix: u64,
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                // No peer exists to name. Mail drafts already leave the contact
                // column empty until they are addressed; a drop never gets one.
                contact_nick: String::new(),
                outgoing: false,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Drop {
                    rendezvous_id: rendezvous_id.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    created_unix,
                },
            },
        )?;
        Ok(id)
    }

    /// Append a new draft, returning its record id.
    #[allow(clippy::too_many_arguments)]
    pub fn append_draft(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        thread_id: Option<&str>,
        to: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        self.push(
            master,
            ChatRecord {
                id: id.clone(),
                // A draft has no peer yet when it is unaddressed, so the
                // contact column is filled in when it is sent.
                contact_nick: to.to_string(),
                outgoing: true,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Draft {
                    thread_id: thread_id.map(|t| t.to_string()),
                    to: to.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                    updated_ms: now_ms(),
                },
            },
        )?;
        Ok(id)
    }

    /// Replace one record's payload in place, keeping its id and contact.
    ///
    /// Returns `false` when the id is unknown, so callers can tell "nothing to
    /// do" from "written".
    pub fn replace_payload(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        payload: ChatRecordPayload,
        ts_ms: u64,
    ) -> Result<bool, Error> {
        let Some(record) = self.file.messages.iter_mut().find(|r| r.id == id) else {
            return Ok(false);
        };
        record.payload = payload;
        record.ts_ms = ts_ms;
        self.save(master)?;
        Ok(true)
    }

    /// Drop a record. Returns `false` when the id is unknown.
    pub fn remove_record(&mut self, master: &[u8; MASTER_KEY_LEN], id: &str) -> Result<bool, Error> {
        let before = self.file.messages.len();
        self.file.messages.retain(|r| r.id != id);
        if self.file.messages.len() == before {
            return Ok(false);
        }
        self.save(master)?;
        Ok(true)
    }

    /// Drop chat messages for one contact, optionally bounded to `[from_ms,
    /// to_ms]`. Returns how many records went. Only the 1:1 chat payloads
    /// (text and file) are touched: mail, drafts and drops share this log and
    /// have their own deletion rules.
    pub fn remove_for_contact(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        from_ms: Option<u64>,
        to_ms: Option<u64>,
    ) -> Result<usize, Error> {
        let before = self.file.messages.len();
        self.file.messages.retain(|r| {
            !(is_chat_payload(r) && r.contact_nick == contact_nick && in_range(r.ts_ms, from_ms, to_ms))
        });
        let removed = before - self.file.messages.len();
        if removed > 0 {
            self.save(master)?;
        }
        Ok(removed)
    }

    /// Enforce the chat auto-delete policy across every contact: `daily`
    /// deletes chat messages older than the current UTC day, `keep_lines`
    /// caps how many chat messages one contact may hold and deletes the
    /// oldest past it. Mail, drafts and drops are never touched. Returns how
    /// many records went.
    pub fn retain_for_auto_delete(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        retention: ChatRetention,
    ) -> Result<usize, Error> {
        let ChatRetention {
            daily,
            keep_lines: cap,
        } = retention;
        if !daily && cap.is_none() {
            return Ok(0);
        }
        let before = self.file.messages.len();

        if daily {
            let cutoff = utc_day_start_ms();
            self.file
                .messages
                .retain(|r| !(is_chat_payload(r) && r.ts_ms < cutoff));
        }

        if let Some(cap) = cap {
            // Walk newest-first and keep the first `cap` chat records per
            // contact; the log arrives in append order, so the oldest sit at
            // the front and are the ones that go.
            let mut kept: HashMap<&str, u32> = HashMap::new();
            let mut keep = vec![true; self.file.messages.len()];
            for (i, r) in self.file.messages.iter().enumerate().rev() {
                if is_chat_payload(r) {
                    let count = kept.entry(r.contact_nick.as_str()).or_insert(0);
                    if *count >= cap {
                        keep[i] = false;
                    } else {
                        *count += 1;
                    }
                }
            }
            let mut i = 0;
            self.file
                .messages
                .retain(|_| {
                    let k = keep[i];
                    i += 1;
                    k
                });
        }

        let removed = before - self.file.messages.len();
        if removed > 0 {
            self.save(master)?;
        }
        Ok(removed)
    }

    /// The conclaves this vault holds, in creation order.
    pub fn conclaves(&self) -> &[ConclaveRecord] {
        &self.file.conclaves
    }

    pub fn conclave(&self, id: &str) -> Option<&ConclaveRecord> {
        self.file.conclaves.iter().find(|m| m.id == id)
    }

    /// Create a conclave with an empty message log, returning its id.
    ///
    /// `convener` is the convener's account id and `trustees` is the initial
    /// roster (the convener as joined, each invitee as invited).
    pub fn create_conclave(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        name: &str,
        convener: &str,
        trustees: &[ConclaveTrustee],
    ) -> Result<String, Error> {
        let id = Uuid::new_v4().to_string();
        let record = ConclaveRecord {
            id: id.clone(),
            name: name.to_string(),
            trustees: trustees.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
            ended: false,
            convener: convener.to_string(),
        };
        self.file.conclaves.push(record);
        self.save(master)?;
        Ok(id)
    }

    /// Create a conclave under an id chosen elsewhere (the id an invite carried),
    /// returning whether it was created. Used when accepting an invite, so both
    /// peers key the same conclave.
    pub fn create_conclave_with_id(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        name: &str,
        convener: &str,
        trustees: &[ConclaveTrustee],
    ) -> Result<bool, Error> {
        if self.file.conclaves.iter().any(|m| m.id == id) {
            return Ok(false);
        }
        let record = ConclaveRecord {
            id: id.to_string(),
            name: name.to_string(),
            trustees: trustees.to_vec(),
            created_ms: now_ms(),
            messages: Vec::new(),
            ended: false,
            convener: convener.to_string(),
        };
        self.file.conclaves.push(record);
        self.save(master)?;
        Ok(true)
    }

    /// Apply a change to one conclave in place, then persist it. Returns `false`
    /// when the id is unknown, so a caller can tell "no such conclave" from
    /// "written".
    pub fn update_conclave<F>(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        f: F,
    ) -> Result<bool, Error>
    where
        F: FnOnce(&mut ConclaveRecord),
    {
        let Some(record) = self.file.conclaves.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        f(record);
        self.save(master)?;
        Ok(true)
    }

    /// Fill a legacy record's trustees from its nickname list, resolving each
    /// nickname to an account id with `resolve`. Returns whether anything
    /// changed.
    ///
    /// A record written before consent existed stored bare nicknames; without
    /// this it would have no trustees and every membership operation would see
    /// an empty set. Two fix-ups happen here:
    ///
    /// - Every nickname that names a contact resolves to that contact's account
    ///   id.
    /// - The **local account** is made a member. A vault is never its own
    ///   contact, so the local account can never resolve through `resolve`; it
    ///   is always one of the entries that stays unresolved. When exactly one
    ///   entry is unresolved it is the local account's own nickname and is
    ///   mapped to `local_account`; when none is, the local account was absent
    ///   from the legacy list and is added. This is what keeps a pre-consent
    ///   record usable: without it the local user's own entry keeps a nickname
    ///   as its account id, so `is_joined` is false and the record cannot be
    ///   sent in.
    ///
    /// An entry whose account id equals its nickname is a fallback written by an
    /// earlier hydration, not a real id, so it is re-processed rather than
    /// trusted. That is what lets a vault hydrated by the old code self-heal.
    pub fn hydrate_trustees<F>(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        id: &str,
        local_account: &str,
        mut resolve: F,
    ) -> Result<bool, Error>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let Some(record) = self.file.conclaves.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        // An entry needs work when it has no account id, or has one that is only
        // the nickname fallback. The record also needs work when the local
        // account is missing from the roster.
        let needs = record
            .trustees
            .iter()
            .any(|t| t.account_id.is_empty() || t.account_id == t.nick);
        let missing_self = !record
            .trustees
            .iter()
            .any(|t| t.account_id == local_account);
        if !needs && !missing_self {
            return Ok(false);
        }

        // Resolve what we can; remember which entries stay unresolved.
        let mut resolved: Vec<ConclaveTrustee> = Vec::with_capacity(record.trustees.len());
        let mut unresolved: Vec<usize> = Vec::new();
        for t in &record.trustees {
            let account_id = if t.account_id.is_empty() || t.account_id == t.nick {
                resolve(&t.nick)
            } else {
                Some(t.account_id.clone())
            };
            match account_id {
                Some(account_id) => resolved.push(ConclaveTrustee {
                    account_id,
                    nick: t.nick.clone(),
                    state: t.state,
                }),
                None => {
                    unresolved.push(resolved.len());
                    resolved.push(ConclaveTrustee {
                        account_id: t.account_id.clone(),
                        nick: t.nick.clone(),
                        state: t.state,
                    });
                }
            }
        }

        // The local account is always a member of a conclave in its own vault,
        // and it never resolves through contacts. So one unresolved entry is the
        // local account's nickname; zero means it was absent from the legacy
        // list. More than one is ambiguous (the others are strangers with no
        // contact), so the local account is added and those entries are left as
        // they are.
        if !resolved.iter().any(|t| t.account_id == local_account) {
            if unresolved.len() == 1 {
                let i = unresolved[0];
                resolved[i].account_id = local_account.to_string();
                resolved[i].state = TrusteeState::Joined;
            } else {
                resolved.push(ConclaveTrustee {
                    account_id: local_account.to_string(),
                    nick: String::new(),
                    state: TrusteeState::Joined,
                });
            }
        }

        record.trustees = resolved;
        if record.convener.is_empty() {
            record.convener = local_account.to_string();
        }
        self.save(master)?;
        Ok(true)
    }

    /// Append one message to a conclave. Returns `false` when the conclave is
    /// unknown, so a caller can tell "no such conclave" from "written".
    pub fn append_conclave_message(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
        outgoing: bool,
        from: Option<&str>,
        body: &str,
    ) -> Result<bool, Error> {
        let Some(conclave) = self.file.conclaves.iter_mut().find(|m| m.id == conclave_id) else {
            return Ok(false);
        };
        conclave.messages.push(ConclaveMessage {
            id: Uuid::new_v4().to_string(),
            outgoing,
            from: from.map(|f| f.to_string()),
            body: body.to_string(),
            file: None,
            ts_ms: now_ms(),
        });
        self.save(master)?;
        Ok(true)
    }

    /// Append a file message to a conclave's log.
    ///
    /// The bytes are already stored as an encrypted vault blob; this records the
    /// reference so the message can be reopened after a restart. Mirrors
    /// [`append_file`](Self::append_file) for 1:1 chat.
    pub fn append_conclave_file(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
        outgoing: bool,
        from: Option<&str>,
        name: &str,
        blob_id: &BlobId,
        size: u64,
    ) -> Result<bool, Error> {
        let Some(conclave) = self.file.conclaves.iter_mut().find(|m| m.id == conclave_id) else {
            return Ok(false);
        };
        conclave.messages.push(ConclaveMessage {
            id: Uuid::new_v4().to_string(),
            outgoing,
            from: from.map(|f| f.to_string()),
            body: String::new(),
            file: Some(ConclaveFileRecord {
                name: name.to_string(),
                blob_id_hex: hex::encode(blob_id.0),
                size,
            }),
            ts_ms: now_ms(),
        });
        self.save(master)?;
        Ok(true)
    }

    /// Drop a conclave and its whole message log. Returns `false` when the id
    /// is unknown.
    ///
    /// Only the log entries go. The vault blobs the messages' files referenced
    /// stay exactly where they were: the blob store has no collection pass, the
    /// same trade [`remove_record`](Self::remove_record) already makes for mail
    /// attachments, and a blob is content-addressed so the same bytes may be
    /// referenced from a 1:1 chat or a mail draft too.
    pub fn remove_conclave(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
    ) -> Result<bool, Error> {
        let before = self.file.conclaves.len();
        self.file.conclaves.retain(|m| m.id != conclave_id);
        if self.file.conclaves.len() == before {
            return Ok(false);
        }
        self.save(master)?;
        Ok(true)
    }

    /// Mark a conclave as ended. Returns `false` when the id is unknown or it was
    /// already ended.
    ///
    /// The record and its message log stay in the encrypted log; the flag makes
    /// the conclave read-only on every peer it is broadcast to.
    pub fn end_conclave(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        conclave_id: &str,
    ) -> Result<bool, Error> {
        let Some(conclave) = self.file.conclaves.iter_mut().find(|m| m.id == conclave_id) else {
            return Ok(false);
        };
        if conclave.ended {
            return Ok(false);
        }
        conclave.ended = true;
        self.save(master)?;
        Ok(true)
    }

    /// Mail records only, in log order. The Mail rail filters the same encrypted
    /// log the chat rail reads rather than keeping a second store.
    pub fn mail_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Mail { .. }))
    }

    /// Filed outsider drops, oldest first.
    pub fn drop_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Drop { .. }))
    }

    /// Drafts only, newest first.
    pub fn draft_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Draft { .. }))
    }

    /// Everything the chat rail renders. Drafts live in this log for the
    /// encryption and backup, but they are not chat history.
    pub fn chat_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| !matches!(r.payload, ChatRecordPayload::Draft { .. }))
    }

    fn push(&mut self, master: &[u8; 32], record: ChatRecord) -> Result<(), Error> {
        self.file.messages.push(record);
        self.save(master)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_record_survives_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("hush-mail-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        log.append_mail(
            &master,
            "alice",
            true,
            "t-1",
            Some("hi"),
            "body",
            &[MailAttachmentRecord {
                name: "a.txt".into(),
                blob_id_hex: "ab".into(),
                size: 3,
            }],
            MailState::Sent,
        )
        .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        assert_eq!(reloaded.mail_records().count(), 1);
        assert_eq!(reloaded.chat_records().count(), 1);
    }

    #[test]
    fn a_queued_message_keeps_its_state_across_a_reload() {
        let dir = std::env::temp_dir().join(format!("hush-queued-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .append_mail(&master, "alice", true, "t-1", None, "waiting", &[], MailState::Queued)
            .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let record = reloaded.mail_records().next().unwrap();
        assert_eq!(record.id, id);
        assert!(matches!(
            record.payload,
            ChatRecordPayload::Mail {
                state: MailState::Queued,
                ..
            }
        ));
    }

    #[test]
    fn a_record_written_before_the_outbox_decodes_as_sent() {
        // The wire shape from before `state` existed. It must keep loading, and
        // it must mean Sent rather than Queued.
        let json = r#"{"messages":[{"id":"r1","contact_nick":"alice","outgoing":true,"ts_ms":1,
            "payload":{"t":"mail","thread_id":"t-1","subject":null,"body":"old","attachments":[]}}]}"#;
        let file: ChatLogFile = serde_json::from_str(json).unwrap();
        assert!(matches!(
            &file.messages[0].payload,
            ChatRecordPayload::Mail {
                state: MailState::Sent,
                ..
            }
        ));
    }

    #[test]
    fn a_draft_is_editable_and_deletable() {
        let dir = std::env::temp_dir().join(format!("hush-draft-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .append_draft(&master, None, "alice", Some("hi"), "first", &[])
            .unwrap();
        assert_eq!(log.draft_records().count(), 1);

        let replaced = log
            .replace_payload(
                &master,
                &id,
                ChatRecordPayload::Draft {
                    thread_id: None,
                    to: "alice".into(),
                    subject: Some("hi".into()),
                    body: "second".into(),
                    attachments: vec![],
                    updated_ms: 5,
                },
                5,
            )
            .unwrap();
        assert!(replaced);

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        match &reloaded.draft_records().next().unwrap().payload {
            ChatRecordPayload::Draft { body, .. } => assert_eq!(body, "second"),
            other => panic!("expected a draft, got {other:?}"),
        }

        let mut log = reloaded;
        assert!(log.remove_record(&master, &id).unwrap());
        assert_eq!(log.draft_records().count(), 0);
        // Removing an unknown id reports "nothing to do" rather than erroring.
        assert!(!log.remove_record(&master, &id).unwrap());
    }

    #[test]
    fn a_draft_is_not_chat_history() {
        let dir = std::env::temp_dir().join(format!("hush-draft-chat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        log.append_text(&master, "alice", true, "hello").unwrap();
        log.append_draft(&master, None, "alice", None, "unsent", &[])
            .unwrap();

        assert_eq!(log.chat_records().count(), 1);
        assert_eq!(log.draft_records().count(), 1);
        // Both live in the one log; only the chat projection hides the draft.
        assert_eq!(log.chat_records().count() + log.draft_records().count(), 2);
    }

    fn trustee(account_id: &str, nick: &str, state: TrusteeState) -> ConclaveTrustee {
        ConclaveTrustee {
            account_id: account_id.to_string(),
            nick: nick.to_string(),
            state,
        }
    }

    #[test]
    fn a_conclave_and_its_messages_survive_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Standup",
                "self",
                &[
                    trustee("self", "", TrusteeState::Joined),
                    trustee("bob-id", "bob", TrusteeState::Invited),
                    trustee("carol-id", "carol", TrusteeState::Invited),
                ],
            )
            .unwrap();
        log.append_conclave_message(&master, &id, true, None, "morning")
            .unwrap();

        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let conclave = reloaded.conclave(&id).expect("the conclave survives a reload");
        assert_eq!(conclave.name, "Standup");
        assert_eq!(conclave.convener, "self");
        assert_eq!(conclave.trustees.len(), 3);
        assert_eq!(conclave.invited().count(), 2);
        assert_eq!(conclave.messages.len(), 1);
        assert_eq!(conclave.messages[0].body, "morning");
        assert!(conclave.messages[0].outgoing);
        // A conclave is not chat history, so the chat rail cannot show it twice.
        assert_eq!(reloaded.chat_records().count(), 0);
    }

    #[test]
    fn a_conclave_created_under_an_invite_id_keeps_that_id() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-id-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        assert!(
            log.create_conclave_with_id(
                &master,
                "c-1",
                "Trip",
                "convener",
                &[trustee("convener", "alice", TrusteeState::Joined)]
            )
            .unwrap()
        );
        assert!(
            !log.create_conclave_with_id(&master, "c-1", "Again", "convener", &[]).unwrap(),
            "the same id is not created twice"
        );
        assert_eq!(log.conclave("c-1").unwrap().name, "Trip");
    }

    #[test]
    fn conclave_trustee_updates_converge_and_never_revive_a_terminal_state() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-mem-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Trip",
                "alice",
                &[
                    trustee("alice", "alice", TrusteeState::Joined),
                    trustee("bob", "bob", TrusteeState::Invited),
                ],
            )
            .unwrap();

        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Joined));
        })
        .unwrap();
        assert_eq!(log.conclave(&id).unwrap().joined().count(), 2);
        assert_eq!(log.conclave(&id).unwrap().invited().count(), 0);

        // A stale snapshot that still says invited must not downgrade a joined
        // trustee: a peer that had not yet heard bob joined is not authoritative.
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Invited));
        })
        .unwrap();
        assert_eq!(
            log.conclave(&id).unwrap().trustee("bob").unwrap().state,
            TrusteeState::Joined
        );

        // A kick is terminal: a stale snapshot that still says joined must not
        // revive the trustee.
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Kicked));
        })
        .unwrap();
        log.update_conclave(&master, &id, |c| {
            c.upsert_trustee(&trustee("bob", "bob", TrusteeState::Joined));
        })
        .unwrap();
        assert_eq!(
            log.conclave(&id).unwrap().trustee("bob").unwrap().state,
            TrusteeState::Kicked
        );
        assert!(!log.conclave(&id).unwrap().is_joined("bob"));
    }

    #[test]
    fn a_legacy_record_hydrates_its_trustees_from_its_nickname_list() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-hydrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        // A record written before consent existed: a bare nickname, which decodes
        // with an empty account id.
        assert!(
            log.create_conclave_with_id(&master, "m-1", "Standup", "", &[])
                .unwrap()
        );
        log.update_conclave(&master, "m-1", |c| {
            c.trustees = vec![trustee("", "bob", TrusteeState::Joined)];
        })
        .unwrap();

        let hydrated = log
            .hydrate_trustees(&master, "m-1", "self", |nick| {
                (nick == "bob").then(|| "bob-id".to_string())
            })
            .unwrap();
        assert!(hydrated);
        let c = log.conclave("m-1").unwrap();
        // The resolved contact, plus the local account, which the legacy list
        // never carried because it is not its own contact.
        assert_eq!(c.trustees.len(), 2);
        assert!(c.is_joined("self"));
        assert_eq!(c.trustee("bob-id").unwrap().nick, "bob");
        assert_eq!(c.trustee("bob-id").unwrap().state, TrusteeState::Joined);
        assert_eq!(c.convener, "self");
        // A second hydration is a no-op.
        assert!(!log.hydrate_trustees(&master, "m-1", "self", |_| None).unwrap());
    }

    /// The pre-existing "Daily sync" shape: the legacy nickname list names the
    /// local account under its own nickname alongside a contact. The local
    /// account's entry must resolve to the local account id, not keep the
    /// nickname, or `is_joined` is false and the conclave cannot be sent in.
    /// This is the regression that made a pre-consent vault unusable.
    #[test]
    fn a_legacy_record_resolves_the_local_accounts_own_nickname() {
        let dir =
            std::env::temp_dir().join(format!("hush-conclave-hydrate-self-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        assert!(
            log.create_conclave_with_id(&master, "m-1", "Daily sync", "", &[])
                .unwrap()
        );
        log.update_conclave(&master, "m-1", |c| {
            c.trustees = vec![
                trustee("", "alice", TrusteeState::Joined),
                trustee("", "bob", TrusteeState::Joined),
            ];
        })
        .unwrap();

        // "alice" is the local account; it is not a contact, so `resolve` cannot
        // place it.
        let hydrated = log
            .hydrate_trustees(&master, "m-1", "alice-id", |nick| {
                (nick == "bob").then(|| "bob-id".to_string())
            })
            .unwrap();
        assert!(hydrated);
        let c = log.conclave("m-1").unwrap();
        assert_eq!(c.trustees.len(), 2);
        assert!(c.is_joined("alice-id"), "the local account must be joined");
        assert!(c.is_joined("bob-id"));
        assert_eq!(c.convener, "alice-id");
        // The nickname is no longer masquerading as an account id.
        assert!(c.trustee("alice").is_none());
    }

    /// A vault that the *old* hydration already touched has the local account's
    /// nickname written into `account_id` (the old fallback). That entry must be
    /// re-processed so the vault self-heals rather than staying unusable.
    #[test]
    fn an_already_hydrated_fallback_account_id_self_heals() {
        let dir = std::env::temp_dir()
            .join(format!("hush-conclave-hydrate-heal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        assert!(
            log.create_conclave_with_id(&master, "m-1", "Daily sync", "", &[])
                .unwrap()
        );
        // The shape the old hydration left: the local account's nickname as its
        // own account id, next to a properly resolved contact.
        log.update_conclave(&master, "m-1", |c| {
            c.trustees = vec![
                trustee("alice", "alice", TrusteeState::Joined),
                trustee("bob-id", "bob", TrusteeState::Joined),
            ];
        })
        .unwrap();

        let hydrated = log
            .hydrate_trustees(&master, "m-1", "alice-id", |_| None)
            .unwrap();
        assert!(hydrated);
        let c = log.conclave("m-1").unwrap();
        assert_eq!(c.trustees.len(), 2);
        assert!(c.is_joined("alice-id"));
        assert!(c.is_joined("bob-id"));
        // Now idempotent: nothing left to heal.
        assert!(!log.hydrate_trustees(&master, "m-1", "alice-id", |_| None).unwrap());
    }

    /// The two legacy copies of one conclave disagreed: one vault's nickname list
    /// named the local account alongside the contact, the other named only the
    /// contact. Hydration has to bring both to the same trustee set, or the peers
    /// still disagree (2 trustees vs 1) after the fix.
    #[test]
    fn both_legacy_shapes_hydrate_to_the_same_trustee_set() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-hydrate-agree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let master = [7u8; MASTER_KEY_LEN];

        // Alice's copy: the legacy list named her own nickname too.
        let alice_dir = dir.join("alice");
        fs::create_dir_all(&alice_dir).unwrap();
        let mut alice_log = ChatLog::load(&alice_dir, &master).unwrap();
        assert!(
            alice_log
                .create_conclave_with_id(&master, "m-1", "Daily sync", "", &[])
                .unwrap()
        );
        alice_log
            .update_conclave(&master, "m-1", |c| {
                c.trustees = vec![
                    trustee("", "alice", TrusteeState::Joined),
                    trustee("", "bob", TrusteeState::Joined),
                ];
            })
            .unwrap();
        alice_log
            .hydrate_trustees(&master, "m-1", "alice-id", |nick| {
                (nick == "bob").then(|| "bob-id".to_string())
            })
            .unwrap();

        // Bob's copy: only alice was listed; bob's own entry was absent.
        let bob_dir = dir.join("bob");
        fs::create_dir_all(&bob_dir).unwrap();
        let mut bob_log = ChatLog::load(&bob_dir, &master).unwrap();
        assert!(
            bob_log
                .create_conclave_with_id(&master, "m-1", "Daily sync", "", &[])
                .unwrap()
        );
        bob_log
            .update_conclave(&master, "m-1", |c| {
                c.trustees = vec![trustee("", "alice", TrusteeState::Joined)];
            })
            .unwrap();
        bob_log
            .hydrate_trustees(&master, "m-1", "bob-id", |nick| {
                (nick == "alice").then(|| "alice-id".to_string())
            })
            .unwrap();

        let roster = |log: &ChatLog| {
            let mut v: Vec<(String, TrusteeState)> = log
                .conclave("m-1")
                .unwrap()
                .trustees
                .iter()
                .map(|t| (t.account_id.clone(), t.state))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        assert_eq!(
            roster(&alice_log),
            vec![
                ("alice-id".to_string(), TrusteeState::Joined),
                ("bob-id".to_string(), TrusteeState::Joined),
            ]
        );
        assert_eq!(roster(&alice_log), roster(&bob_log));
    }

    #[test]
    fn ending_a_conclave_keeps_its_record_and_log() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-end-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Trip",
                "alice",
                &[
                    trustee("alice", "alice", TrusteeState::Joined),
                    trustee("bob", "bob", TrusteeState::Joined),
                ],
            )
            .unwrap();
        log.append_conclave_message(&master, &id, true, None, "we leave at six")
            .unwrap();

        assert!(log.end_conclave(&master, &id).unwrap());
        assert!(
            !log.end_conclave(&master, &id).unwrap(),
            "ending the same conclave twice is a no-op"
        );

        // A flag, not a delete: the record and its log survive a reload.
        let reloaded = ChatLog::load(&dir, &master).unwrap();
        let record = reloaded.conclave(&id).expect("the record stays in the log");
        assert!(record.ended, "the record is marked ended");
        assert_eq!(record.messages.len(), 1, "the message log is kept");
        assert_eq!(record.messages[0].body, "we leave at six");
    }

    #[test]
    fn deleting_a_conclave_drops_its_log_but_keeps_the_list_shape() {
        let dir = std::env::temp_dir().join(format!("hush-conclave-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let master = [7u8; MASTER_KEY_LEN];

        let mut log = ChatLog::load(&dir, &master).unwrap();
        let id = log
            .create_conclave(
                &master,
                "Trip",
                "alice",
                &[trustee("alice", "alice", TrusteeState::Joined)],
            )
            .unwrap();
        log.append_conclave_message(&master, &id, true, None, "we leave at six")
            .unwrap();
        log.append_conclave_message(&master, &id, false, Some("bob"), "see you there")
            .unwrap();

        assert!(log.remove_conclave(&master, &id).unwrap());
        assert!(
            !log.remove_conclave(&master, &id).unwrap(),
            "deleting an unknown id reports \"nothing to do\" rather than erroring"
        );

        // The whole record — messages included — is gone, and it stays gone
        // after a reload.
        let reloaded = ChatLog::load(&dir, &master).unwrap();
        assert!(reloaded.conclave(&id).is_none(), "the record does not survive a reload");
        assert_eq!(reloaded.conclaves().len(), 0);
        assert_eq!(reloaded.chat_records().count(), 0);
    }

    /// A log written by the build that keyed the list as `meetings` must still
    /// load, so the rename cannot make an existing vault look empty or corrupt.
    #[test]
    fn a_log_written_before_the_rename_still_loads() {
        let json = r#"{"messages":[],"meetings":[{"id":"m-1","name":"Standup","members":["bob"],"created_ms":1,"messages":[]}]}"#;
        let file: ChatLogFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.conclaves.len(), 1, "the old `meetings` key still loads");
        assert_eq!(file.conclaves[0].id, "m-1");
        assert!(!file.conclaves[0].ended, "an old record loads as active");
        // The bare nickname decodes into a trustee with an unknown account id,
        // which hydration resolves on first open.
        assert_eq!(file.conclaves[0].trustees.len(), 1);
        assert_eq!(file.conclaves[0].trustees[0].nick, "bob");
        assert_eq!(file.conclaves[0].trustees[0].account_id, "");
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
