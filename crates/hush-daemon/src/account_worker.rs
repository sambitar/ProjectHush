use std::path::PathBuf;
use std::thread;

use hush_core::{Account, ChatRecord, ConclaveRecord, InboxResult, PendingInvite};
use tokio::sync::{mpsc, oneshot};

pub struct AccountWorker {
    tx: mpsc::Sender<Work>,
}

impl Clone for AccountWorker {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

/// Parse a hex blob id into the vault's `BlobId`.
fn blob_id_from_hex(s: &str) -> Option<hush_core::BlobId> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(hush_core::BlobId(arr))
}

enum Work {
    Unlock {
        dir: PathBuf,
        passphrase: String,
        reply: oneshot::Sender<WorkResult>,
    },
    Lock {
        reply: oneshot::Sender<WorkResult>,
    },
    Invite {
        reply: oneshot::Sender<WorkResult>,
    },
    Contacts {
        reply: oneshot::Sender<WorkResult>,
    },
    AddContact {
        nickname: String,
        invite: String,
        reply: oneshot::Sender<WorkResult>,
    },
    RemoveContact {
        nickname: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendText {
        to: String,
        text: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendFile {
        to: String,
        path: PathBuf,
        reply: oneshot::Sender<WorkResult>,
    },
    SendFileBytes {
        to: String,
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    Recv {
        reply: oneshot::Sender<WorkResult>,
    },
    Meta {
        reply: oneshot::Sender<WorkResult>,
    },
    Messages {
        reply: oneshot::Sender<WorkResult>,
    },
    Blob {
        id_hex: String,
        reply: oneshot::Sender<WorkResult>,
    },
    BackupExport {
        reply: oneshot::Sender<WorkResult>,
    },
    BackupImport {
        dir: PathBuf,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    RecoveryAck {
        reply: oneshot::Sender<WorkResult>,
    },
    RecoveryPending {
        reply: oneshot::Sender<WorkResult>,
    },
    HeartbeatPresence {
        reply: oneshot::Sender<WorkResult>,
    },
    ChangePassphrase {
        current: String,
        new: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SetMailHandle {
        handle: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendMail {
        to: String,
        thread_id: Option<String>,
        subject: Option<String>,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        reply: oneshot::Sender<WorkResult>,
    },
    MailRecords {
        reply: oneshot::Sender<WorkResult>,
    },
    DropRecords {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Reads pane: load the encrypted sidecar state (None when never saved).
    ReadsStateGet {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Reads pane: replace the encrypted sidecar state wholesale.
    ReadsStatePut {
        bytes: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Torrents pane: load the encrypted sidecar state (None when never saved).
    TorrentsStateGet {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Torrents pane: replace the encrypted sidecar state wholesale.
    TorrentsStatePut {
        bytes: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Store a file from disk into the vault as a chunked blob (large-file
    /// path for torrent downloads).
    BlobStoreFile {
        path: PathBuf,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Delete a stored blob (single or chunked).
    BlobDelete {
        id_hex: String,
        reply: oneshot::Sender<WorkResult>,
    },
    DraftRecords {
        reply: oneshot::Sender<WorkResult>,
    },
    SaveDraft {
        id: Option<String>,
        thread_id: Option<String>,
        to: String,
        subject: Option<String>,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        reply: oneshot::Sender<WorkResult>,
    },
    DeleteDraft {
        id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    DeleteMail {
        id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    RetryOutbox {
        reply: oneshot::Sender<WorkResult>,
    },
    Conclaves {
        reply: oneshot::Sender<WorkResult>,
    },
    CreateConclave {
        name: String,
        trustees: Vec<String>,
        reply: oneshot::Sender<WorkResult>,
    },
    SendConclaveMessage {
        conclave_id: String,
        body: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendConclaveFile {
        conclave_id: String,
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    InviteConclaveTrustee {
        conclave_id: String,
        nickname: String,
        reply: oneshot::Sender<WorkResult>,
    },
    KickConclaveTrustee {
        conclave_id: String,
        nickname: String,
        reply: oneshot::Sender<WorkResult>,
    },
    AcceptConclaveInvite {
        conclave_id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    DeclineConclaveInvite {
        conclave_id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    LeaveConclave {
        conclave_id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    PendingInvites {
        reply: oneshot::Sender<WorkResult>,
    },
    EndConclave {
        conclave_id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Delete an ended conclave and its message log from this vault. Local
    /// only: nothing is broadcast, and shared-file blobs stay in the vault.
    DeleteConclave {
        conclave_id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Flip the Tor relay switch on the live vault. A no-op while locked:
    /// the prefs file carries the intent and unlock applies it.
    RelayApply {
        use_tor: bool,
        onion: Option<String>,
        socks: Option<String>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// The vault's applied switch state. Errors while locked; callers fall
    /// back to the prefs values.
    RelayInfo {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Probe whether the relay answers right now, along the selected path.
    RelayProbe {
        reply: oneshot::Sender<WorkResult>,
    },
    /// The account's drop key, with the drop-page URL built on the same relay
    /// base the vault already prefers (onion when staged, clearnet otherwise).
    DropKey {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Create the drop keypair. An error when one exists; rotate instead.
    DropKeyCreate {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Rotate the drop keypair; the retired one keeps draining.
    DropKeyRotate {
        reply: oneshot::Sender<WorkResult>,
    },
    /// One drain pass over the drop slots.
    DrainDrops {
        reply: oneshot::Sender<WorkResult>,
    },
}

/// The drop page's URL on the relay base this vault already prefers: the
/// staged onion address when there is one, the clearnet base otherwise. This
/// is the string a member publishes next to their drop key.
fn drop_page_url(account: &Account) -> String {
    match account.onion_relay() {
        Some(base) => format!("{base}/drop"),
        None => format!("{}/drop", account.relay()),
    }
}

#[derive(Clone, serde::Serialize)]
pub struct ContactDto {
    pub nickname: String,
    /// The contact's account id.
    ///
    /// Still called `protocol_name` on the wire: it is the same value the UI has
    /// always keyed avatars and photos off, and renaming it would churn the UI
    /// for no behavioural gain. Multi-device made it the *account* identifier
    /// rather than a single device's, which is what it effectively already was.
    pub protocol_name: String,
    pub inbound_ready: bool,
    pub online: bool,
    /// This contact's mail handle (local part only), if they published one in
    /// their invite. The UI renders `handle@<vault>`.
    pub mail_handle: Option<String>,
    /// The contact's vault name, from their invite — the suffix of their mail
    /// address. `None` when their invite predates vault naming.
    pub mail_vault: Option<String>,
}

pub enum WorkResult {
    Ok,
    Invite(String),
    Contacts(Vec<ContactDto>),
    Inbox(InboxResult),
    Meta {
        protocol_name: String,
        vault_bytes: u64,
        contacts: usize,
        recovery_acknowledged: bool,
        last_backup_unix: u64,
        relay: String,
        mail_handle: Option<String>,
        vault_name: String,
        /// Whether the relay path is switched to Tor.
        relay_tor: bool,
    },
    /// The vault's applied Tor relay switch state.
    RelayInfo {
        relay_tor: bool,
        onion_url: Option<String>,
    },
    /// Whether the relay answered a health probe. `false` is a real answer
    /// (unreachable), distinct from `Err("locked")`.
    Reachable(bool),
    Backup(Vec<u8>),
    RecoveryCode(String),
    Messages(Vec<ChatRecord>),
    /// The thread the mail was filed under, and whether the relay took it.
    MailSent {
        thread_id: String,
        queued: bool,
    },
    Mail(Vec<ChatRecord>),
    Drafts(Vec<ChatRecord>),
    /// Every filed outsider drop.
    Drops(Vec<ChatRecord>),
    /// The Reads pane's encrypted sidecar state; `None` until first saved.
    ReadsState(Option<Vec<u8>>),
    /// The record id of the saved draft.
    DraftSaved(String),
    Blob(Vec<u8>),
    BlobId(String),
    /// Every conclave this vault holds, with its message log.
    Conclaves(Vec<ConclaveRecord>),
    /// The conclave that was just created.
    ConclaveCreated(ConclaveRecord),
    /// The conclave that was just joined by accepting an invite.
    ConclaveAccepted(ConclaveRecord),
    /// Conclave invitations received but not yet accepted or declined.
    PendingInvites(Vec<PendingInvite>),
    /// The supplied current passphrase did not match the vault.
    WrongPassphrase,
    /// The account's drop key and the URL outsiders post to.
    DropKey {
        exists: bool,
        public_key: Option<String>,
        page_url: Option<String>,
    },
    /// What one drop-drain pass did.
    Drain(hush_core::DropInboxReport),
    Err(String),
}

impl AccountWorker {
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::channel::<Work>(64);
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("account worker runtime");
            rt.block_on(async move {
                let mut account: Option<Account> = None;
                while let Some(work) = rx.recv().await {
                    let (reply, result) = match work {
                        Work::Unlock {
                            dir,
                            passphrase,
                            reply,
                        } => {
                            let result = match Account::open(&dir, &passphrase).await {
                                Ok(a) => {
                                    // Nothing to publish on unlock: peers learn
                                    // this account's inbox from its invite, and
                                    // there is no device list to refresh. A
                                    // rotated inbox cleared the cached invite in
                                    // `finish_open`, so the next invite mints a
                                    // fresh one.
                                    account = Some(a);
                                    WorkResult::Ok
                                }
                                Err(e) => WorkResult::Err(e.to_string()),
                            };
                            (reply, result)
                        }
                        Work::Lock { reply } => {
                            account = None;
                            (reply, WorkResult::Ok)
                        }
                        Work::Invite { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.invite().await {
                                    Ok(inv) => WorkResult::Invite(inv),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Contacts { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    // Presence reads the cached per-contact queue,
                                    // which only changes when a grant arrives, so
                                    // there is nothing to refresh here.
                                    let mut list = Vec::new();
                                    for c in a.contacts() {
                                        let online =
                                            a.contact_is_online(&c.nickname).await.unwrap_or(false);
                                        list.push(ContactDto {
                                            nickname: c.nickname.clone(),
                                            protocol_name: c.account_id.clone(),
                                            inbound_ready: c.inbound_ready,
                                            online,
                                            mail_handle: c.mail_handle.clone(),
                                            mail_vault: c.mail_vault.clone(),
                                        });
                                    }
                                    WorkResult::Contacts(list)
                                }
                            };
                            (reply, result)
                        }
                        Work::HeartbeatPresence { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.heartbeat_presence().await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::AddContact {
                            nickname,
                            invite,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.add_contact(&nickname, &invite).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RemoveContact { nickname, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.revoke_contact(&nickname).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendText { to, text, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_text(&to, &text).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendFile { to, path, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_file(&to, &path).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendFileBytes {
                            to,
                            name,
                            data,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_file_bytes(&to, &name, &data).await {
                                    Ok(id) => WorkResult::BlobId(hex::encode(id.0)),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Recv { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.recv().await {
                                    Ok(inbox) => {
                                        // Drops ride the normal poll: one drain
                                        // pass right after the mailbox pull, so
                                        // arrival needs no extra timer. A failed
                                        // pass is logged, not surfaced — the next
                                        // poll retries it, and mail receipt must
                                        // not depend on drop health.
                                        if let Err(e) = a.drain_drops().await {
                                            eprintln!("hush-daemon: drop drain failed: {e}");
                                        }
                                        WorkResult::Inbox(inbox)
                                    }
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Meta { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Meta {
                                    protocol_name: a.protocol_name().to_string(),
                                    vault_bytes: a.vault_bytes_on_disk(),
                                    contacts: a.contacts().len(),
                                    recovery_acknowledged: a.recovery_acknowledged(),
                                    last_backup_unix: a.last_backup_unix(),
                                    relay: a.relay().to_string(),
                                    mail_handle: a.mail_handle().map(|h| h.to_string()),
                                    vault_name: a.vault_name().to_string(),
                                    relay_tor: a.relay_tor_mode(),
                                },
                            };
                            (reply, result)
                        }
                        Work::RelayApply {
                            use_tor,
                            onion,
                            socks,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Ok,
                                Some(a) => {
                                    a.set_relay_socks(socks);
                                    match a.set_relay_tor(use_tor, onion.as_deref()).await {
                                        Ok(()) => WorkResult::Ok,
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::RelayInfo { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::RelayInfo {
                                    relay_tor: a.relay_tor_mode(),
                                    onion_url: a.onion_relay().map(|s| s.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RelayProbe { reply } => {
                            // Short timeout: this feeds a status light the UI
                            // polls, so a dead network must resolve quickly to
                            // "unreachable" rather than tie up the worker.
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Reachable(
                                    a.relay_ping(std::time::Duration::from_secs(4)).await,
                                ),
                            };
                            (reply, result)
                        }
                        Work::Messages { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Messages(a.chat_history()),
                            };
                            (reply, result)
                        }
                        Work::BackupExport { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.export_backup().await {
                                    Ok(data) => WorkResult::Backup(data),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::BackupImport { dir, data, reply } => {
                            if account.is_some() {
                                (reply, WorkResult::Err("unlock vault before import".into()))
                            } else {
                                let result = match Account::import_backup(&dir, &data) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                };
                                (reply, result)
                            }
                        }
                        Work::RecoveryAck { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.acknowledge_recovery().await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RecoveryPending { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.take_pending_recovery_code() {
                                    Some(code) => WorkResult::RecoveryCode(code),
                                    None => WorkResult::Err("no pending recovery code".into()),
                                },
                            };
                            (reply, result)
                        }
                        Work::ChangePassphrase {
                            current,
                            new,
                            reply,
                        } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.change_passphrase(&current, &new) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(hush_core::Error::WrongPassphrase) => {
                                        WorkResult::WrongPassphrase
                                    }
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SetMailHandle { handle, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.set_mail_handle(&handle).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendMail {
                            to,
                            thread_id,
                            subject,
                            body,
                            attachments,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_mail(
                                        &to,
                                        thread_id.as_deref(),
                                        subject.as_deref(),
                                        &body,
                                        &attachments,
                                    )
                                    .await
                                {
                                    Ok(outcome) => WorkResult::MailSent {
                                        thread_id: outcome.thread_id,
                                        queued: outcome.queued,
                                    },
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::MailRecords { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Mail(a.mail_records()),
                            };
                            (reply, result)
                        }
                        Work::DraftRecords { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Drafts(a.draft_records()),
                            };
                            (reply, result)
                        }
                        Work::DropRecords { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Drops(a.drop_records()),
                            };
                            (reply, result)
                        }
                        Work::ReadsStateGet { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a.read_vault_sidecar(crate::reads_store::STATE_FILE) {
                                        Ok(state) => WorkResult::ReadsState(state),
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::ReadsStatePut { bytes, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .write_vault_sidecar(crate::reads_store::STATE_FILE, &bytes)
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::TorrentsStateGet { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a.read_vault_sidecar(crate::torrents_store::STATE_FILE) {
                                        Ok(state) => WorkResult::ReadsState(state),
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::TorrentsStatePut { bytes, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.write_vault_sidecar(
                                    crate::torrents_store::STATE_FILE,
                                    &bytes,
                                ) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::BlobStoreFile { path, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.store_blob_file(&path) {
                                    Ok(id) => WorkResult::BlobId(hex::encode(id.0)),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::BlobDelete { id_hex, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match blob_id_from_hex(&id_hex) {
                                    None => WorkResult::Err("bad blob id".into()),
                                    Some(id) => match a.delete_blob(&id) {
                                        Ok(_) => WorkResult::Ok,
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    },
                                },
                            };
                            (reply, result)
                        }
                        Work::SaveDraft {
                            id,
                            thread_id,
                            to,
                            subject,
                            body,
                            attachments,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .save_draft(
                                        id.as_deref(),
                                        thread_id.as_deref(),
                                        &to,
                                        subject.as_deref(),
                                        &body,
                                        &attachments,
                                    )
                                    .await
                                {
                                    Ok(saved) => WorkResult::DraftSaved(saved),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeleteDraft { id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.delete_draft(&id).await {
                                    Ok(_) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeleteMail { id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.delete_mail(&id).await {
                                    Ok(_) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RetryOutbox { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.retry_outbox().await {
                                    // A failed flush is not worth surfacing: the
                                    // messages are still queued and the next tick
                                    // tries again.
                                    Ok(_) | Err(_) => WorkResult::Ok,
                                },
                            };
                            (reply, result)
                        }
                        Work::Conclaves { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Conclaves(a.conclaves()),
                            };
                            (reply, result)
                        }
                        Work::CreateConclave {
                            name,
                            trustees,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.create_conclave(&name, &trustees).await {
                                    Ok(conclave) => WorkResult::ConclaveCreated(conclave),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendConclaveMessage {
                            conclave_id,
                            body,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_conclave_message(&conclave_id, &body).await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendConclaveFile {
                            conclave_id,
                            name,
                            data,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a
                                        .send_conclave_file_bytes(&conclave_id, &name, &data)
                                        .await
                                    {
                                        Ok(id) => WorkResult::BlobId(hex::encode(id.0)),
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::InviteConclaveTrustee {
                            conclave_id,
                            nickname,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a.invite_conclave_trustee(&conclave_id, &nickname).await {
                                        Ok(()) => WorkResult::Ok,
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::KickConclaveTrustee {
                            conclave_id,
                            nickname,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a.kick_conclave_trustee(&conclave_id, &nickname).await {
                                        Ok(()) => WorkResult::Ok,
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::AcceptConclaveInvite { conclave_id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.accept_conclave_invite(&conclave_id).await {
                                    Ok(conclave) => WorkResult::ConclaveAccepted(conclave),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeclineConclaveInvite { conclave_id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.decline_conclave_invite(&conclave_id).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::LeaveConclave { conclave_id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.leave_conclave(&conclave_id).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::PendingInvites { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::PendingInvites(a.pending_invites()),
                            };
                            (reply, result)
                        }
                        Work::EndConclave { conclave_id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.end_conclave(&conclave_id).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeleteConclave { conclave_id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.delete_conclave(&conclave_id).await {
                                    Ok(_) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Blob { id_hex, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    let bytes = hex::decode(&id_hex)
                                        .map_err(|_| WorkResult::Err("invalid blob id".into()));
                                    match bytes {
                                        Err(e) => e,
                                        Ok(b) if b.len() != 32 => {
                                            WorkResult::Err("invalid blob id".into())
                                        }
                                        Ok(b) => {
                                            let mut id = [0u8; 32];
                                            id.copy_from_slice(&b);
                                            match a.read_blob(&hush_core::BlobId(id)) {
                                                Ok(data) => WorkResult::Blob(data),
                                                Err(e) => WorkResult::Err(e.to_string()),
                                            }
                                        }
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::DropKey { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    let pk = a.drop_public_key();
                                    WorkResult::DropKey {
                                        exists: pk.is_some(),
                                        page_url: Some(drop_page_url(a)),
                                        public_key: pk,
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::DropKeyCreate { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.create_drop_keys() {
                                    Ok(pk) => WorkResult::DropKey {
                                        exists: true,
                                        public_key: Some(pk.clone()),
                                        page_url: Some(drop_page_url(a)),
                                    },
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DropKeyRotate { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.rotate_drop_keys() {
                                    Ok(pk) => WorkResult::DropKey {
                                        exists: true,
                                        public_key: Some(pk.clone()),
                                        page_url: Some(drop_page_url(a)),
                                    },
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DrainDrops { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.drain_drops().await {
                                    Ok(report) => WorkResult::Drain(report),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                    };
                    reply.send(result).ok();
                }
            });
        });
        Self { tx }
    }

    async fn call(&self, build: impl FnOnce(oneshot::Sender<WorkResult>) -> Work) -> WorkResult {
        let (reply, work) = {
            let (tx, rx) = oneshot::channel();
            (rx, build(tx))
        };
        if self.tx.send(work).await.is_err() {
            return WorkResult::Err("account worker stopped".into());
        }
        reply
            .await
            .unwrap_or_else(|_| WorkResult::Err("worker dropped".into()))
    }

    pub async fn unlock(&self, dir: &PathBuf, passphrase: &str) -> WorkResult {
        let dir = dir.clone();
        let passphrase = passphrase.to_string();
        self.call(|reply| Work::Unlock {
            dir,
            passphrase,
            reply,
        })
        .await
    }

    pub async fn lock(&self) -> WorkResult {
        self.call(|reply| Work::Lock { reply }).await
    }

    pub async fn invite(&self) -> WorkResult {
        self.call(|reply| Work::Invite { reply }).await
    }

    pub async fn contacts(&self) -> WorkResult {
        self.call(|reply| Work::Contacts { reply }).await
    }

    pub async fn add_contact(&self, nickname: &str, invite: &str) -> WorkResult {
        self.call(|reply| Work::AddContact {
            nickname: nickname.to_string(),
            invite: invite.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_contact(&self, nickname: &str) -> WorkResult {
        self.call(|reply| Work::RemoveContact {
            nickname: nickname.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_text(&self, to: &str, text: &str) -> WorkResult {
        self.call(|reply| Work::SendText {
            to: to.to_string(),
            text: text.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_file(&self, to: &str, path: &PathBuf) -> WorkResult {
        self.call(|reply| Work::SendFile {
            to: to.to_string(),
            path: path.clone(),
            reply,
        })
        .await
    }

    pub async fn send_file_bytes(&self, to: &str, name: &str, data: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::SendFileBytes {
            to: to.to_string(),
            name: name.to_string(),
            data,
            reply,
        })
        .await
    }

    pub async fn recv(&self) -> WorkResult {
        self.call(|reply| Work::Recv { reply }).await
    }

    pub async fn meta(&self) -> WorkResult {
        self.call(|reply| Work::Meta { reply }).await
    }

    /// Flip the Tor relay switch on the live vault. See [`Work::RelayApply`].
    pub async fn relay_apply(
        &self,
        use_tor: bool,
        onion: Option<String>,
        socks: Option<String>,
    ) -> WorkResult {
        self.call(|reply| Work::RelayApply {
            use_tor,
            onion,
            socks,
            reply,
        })
        .await
    }

    /// The vault's applied switch state. See [`Work::RelayInfo`].
    pub async fn relay_info(&self) -> WorkResult {
        self.call(|reply| Work::RelayInfo { reply }).await
    }

    /// Whether the relay answers right now. See [`Work::RelayProbe`].
    pub async fn relay_probe(&self) -> WorkResult {
        self.call(|reply| Work::RelayProbe { reply }).await
    }

    pub async fn messages(&self) -> WorkResult {
        self.call(|reply| Work::Messages { reply }).await
    }

    pub async fn blob(&self, id_hex: &str) -> WorkResult {
        self.call(|reply| Work::Blob {
            id_hex: id_hex.to_string(),
            reply,
        })
        .await
    }

    pub async fn backup_export(&self) -> WorkResult {
        self.call(|reply| Work::BackupExport { reply }).await
    }

    pub async fn backup_import(&self, dir: &PathBuf, data: Vec<u8>) -> WorkResult {
        let dir = dir.clone();
        self.call(|reply| Work::BackupImport { dir, data, reply })
            .await
    }

    pub async fn recovery_ack(&self) -> WorkResult {
        self.call(|reply| Work::RecoveryAck { reply }).await
    }

    pub async fn recovery_pending(&self) -> WorkResult {
        self.call(|reply| Work::RecoveryPending { reply }).await
    }

    pub async fn heartbeat_presence(&self) -> WorkResult {
        self.call(|reply| Work::HeartbeatPresence { reply }).await
    }

    pub async fn change_passphrase(&self, current: &str, new: &str) -> WorkResult {
        self.call(|reply| Work::ChangePassphrase {
            current: current.to_string(),
            new: new.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_mail_handle(&self, handle: &str) -> WorkResult {
        self.call(|reply| Work::SetMailHandle {
            handle: handle.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_mail(
        &self,
        to: &str,
        thread_id: Option<String>,
        subject: Option<String>,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
    ) -> WorkResult {
        self.call(|reply| Work::SendMail {
            to: to.to_string(),
            thread_id,
            subject,
            body: body.to_string(),
            attachments,
            reply,
        })
        .await
    }

    pub async fn mail_records(&self) -> WorkResult {
        self.call(|reply| Work::MailRecords { reply }).await
    }

    pub async fn draft_records(&self) -> WorkResult {
        self.call(|reply| Work::DraftRecords { reply }).await
    }

    /// Every filed outsider drop. See [`Work::DropRecords`].
    pub async fn drop_records(&self) -> WorkResult {
        self.call(|reply| Work::DropRecords { reply }).await
    }

    /// The Reads pane's encrypted sidecar state, raw bytes (None until first
    /// saved). The JSON schema lives in [`crate::reads_store`].
    pub async fn reads_state_get(&self) -> WorkResult {
        self.call(|reply| Work::ReadsStateGet { reply }).await
    }

    /// Replace the Reads pane's encrypted sidecar state wholesale.
    pub async fn reads_state_put(&self, bytes: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::ReadsStatePut { bytes, reply })
            .await
    }

    /// The Torrents pane's encrypted sidecar state, raw bytes (None until first
    /// saved). The JSON schema lives in [`crate::torrents_store`].
    pub async fn torrents_state_get(&self) -> WorkResult {
        self.call(|reply| Work::TorrentsStateGet { reply }).await
    }

    /// Replace the Torrents pane's encrypted sidecar state wholesale.
    pub async fn torrents_state_put(&self, bytes: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::TorrentsStatePut { bytes, reply })
            .await
    }

    /// Store a file from disk into the vault as a chunked blob; returns its
    /// hex id. See [`hush_core::Vault::put_blob_stream`].
    pub async fn blob_store_file(&self, path: &PathBuf) -> WorkResult {
        let path = path.clone();
        self.call(|reply| Work::BlobStoreFile { path, reply }).await
    }

    /// Delete a stored blob by hex id.
    pub async fn blob_delete(&self, id_hex: &str) -> WorkResult {
        self.call(|reply| Work::BlobDelete {
            id_hex: id_hex.to_string(),
            reply,
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_draft(
        &self,
        id: Option<String>,
        thread_id: Option<String>,
        to: &str,
        subject: Option<String>,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
    ) -> WorkResult {
        self.call(|reply| Work::SaveDraft {
            id,
            thread_id,
            to: to.to_string(),
            subject,
            body: body.to_string(),
            attachments,
            reply,
        })
        .await
    }

    pub async fn delete_draft(&self, id: &str) -> WorkResult {
        self.call(|reply| Work::DeleteDraft {
            id: id.to_string(),
            reply,
        })
        .await
    }

    pub async fn delete_mail(&self, id: &str) -> WorkResult {
        self.call(|reply| Work::DeleteMail {
            id: id.to_string(),
            reply,
        })
        .await
    }

    /// Flush the Outbox. Called on a timer, not by the UI.
    pub async fn retry_outbox(&self) -> WorkResult {
        self.call(|reply| Work::RetryOutbox { reply }).await
    }

    pub async fn conclaves(&self) -> WorkResult {
        self.call(|reply| Work::Conclaves { reply }).await
    }

    pub async fn create_conclave(&self, name: &str, trustees: Vec<String>) -> WorkResult {
        self.call(|reply| Work::CreateConclave {
            name: name.to_string(),
            trustees,
            reply,
        })
        .await
    }

    pub async fn send_conclave_message(&self, conclave_id: &str, body: &str) -> WorkResult {
        self.call(|reply| Work::SendConclaveMessage {
            conclave_id: conclave_id.to_string(),
            body: body.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_conclave_file_bytes(
        &self,
        conclave_id: &str,
        name: &str,
        data: Vec<u8>,
    ) -> WorkResult {
        self.call(|reply| Work::SendConclaveFile {
            conclave_id: conclave_id.to_string(),
            name: name.to_string(),
            data,
            reply,
        })
        .await
    }

    pub async fn invite_conclave_trustee(&self, conclave_id: &str, nickname: &str) -> WorkResult {
        self.call(|reply| Work::InviteConclaveTrustee {
            conclave_id: conclave_id.to_string(),
            nickname: nickname.to_string(),
            reply,
        })
        .await
    }

    pub async fn kick_conclave_trustee(&self, conclave_id: &str, nickname: &str) -> WorkResult {
        self.call(|reply| Work::KickConclaveTrustee {
            conclave_id: conclave_id.to_string(),
            nickname: nickname.to_string(),
            reply,
        })
        .await
    }

    pub async fn accept_conclave_invite(&self, conclave_id: &str) -> WorkResult {
        self.call(|reply| Work::AcceptConclaveInvite {
            conclave_id: conclave_id.to_string(),
            reply,
        })
        .await
    }

    pub async fn decline_conclave_invite(&self, conclave_id: &str) -> WorkResult {
        self.call(|reply| Work::DeclineConclaveInvite {
            conclave_id: conclave_id.to_string(),
            reply,
        })
        .await
    }

    pub async fn leave_conclave(&self, conclave_id: &str) -> WorkResult {
        self.call(|reply| Work::LeaveConclave {
            conclave_id: conclave_id.to_string(),
            reply,
        })
        .await
    }

    pub async fn pending_invites(&self) -> WorkResult {
        self.call(|reply| Work::PendingInvites { reply }).await
    }

    pub async fn end_conclave(&self, conclave_id: &str) -> WorkResult {
        self.call(|reply| Work::EndConclave {
            conclave_id: conclave_id.to_string(),
            reply,
        })
        .await
    }

    pub async fn delete_conclave(&self, conclave_id: &str) -> WorkResult {
        self.call(|reply| Work::DeleteConclave {
            conclave_id: conclave_id.to_string(),
            reply,
        })
        .await
    }

    /// The account's drop key and the page outsiders post to.
    pub async fn drop_key(&self) -> WorkResult {
        self.call(|reply| Work::DropKey { reply }).await
    }

    /// Create the drop keypair. See [`Work::DropKeyCreate`].
    pub async fn drop_key_create(&self) -> WorkResult {
        self.call(|reply| Work::DropKeyCreate { reply }).await
    }

    /// Rotate the drop keypair. See [`Work::DropKeyRotate`].
    pub async fn drop_key_rotate(&self) -> WorkResult {
        self.call(|reply| Work::DropKeyRotate { reply }).await
    }

    /// One drain pass over the drop slots. See [`Work::DrainDrops`].
    pub async fn drain_drops(&self) -> WorkResult {
        self.call(|reply| Work::DrainDrops { reply }).await
    }
}
