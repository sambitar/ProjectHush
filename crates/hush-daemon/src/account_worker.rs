use std::path::{Path, PathBuf};
use std::thread;

use hush_core::{
    Account, ChatRecord, ConclaveRecord, InboxResult, MessagePayload, PaperDescriptor,
    PendingInvite,
};
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

/// Where decrypted blobs are materialized for the UI to open or save.
///
/// The system temp dir (disk-backed, honoring `TMPDIR`), never the vault and
/// never the runtime dir: on Linux `$XDG_RUNTIME_DIR` is a RAM-backed tmpfs
/// sized to a fraction of RAM, so a multi-GB file there exhausts it (ENOSPC)
/// and, worse, its pages count as memory. A plaintext copy must also not look
/// like, or be counted as, vault content. The dir is 0700, files are 0600, and
/// both are swept (see [`sweep_open_dir`]).
fn open_tmp_dir() -> PathBuf {
    std::env::temp_dir().join("hush-open")
}

/// A file name safe to join onto [`open_tmp_dir`]: base name only, no path
/// separators, and a non-empty fallback. The extension is preserved because it
/// drives the OS open handler (`.mkv` → the video player).
fn sanitize_open_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() || safe == "." || safe == ".." {
        "file".into()
    } else {
        safe
    }
}

/// Materialize `id` into a uniquely named file and return its path.
///
/// This is the streaming egress: `write_blob_to_file` decrypts chunk by chunk,
/// so peak memory is one chunk no matter how large the blob is. A failure
/// mid-write removes the partial file, so a later open never sees a truncated
/// blob.
fn materialize_blob(a: &Account, id: &hush_core::BlobId, name: &str) -> Result<PathBuf, String> {
    let dir = open_tmp_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create the open folder: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    sweep_open_dir(&dir);
    // A random token keeps two files of the same name from colliding, so
    // opening two `video.mkv` torrents cannot overwrite one with the other.
    let token = uuid::Uuid::new_v4().simple().to_string();
    let path = dir.join(format!("{token}-{}", sanitize_open_name(name)));
    if let Err(e) = a.write_blob_to_file(id, &path) {
        let _ = std::fs::remove_file(&path);
        return Err(e.to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Best-effort cleanup of materialized files older than an hour. Opening hands
/// the path to an external player, so the file must outlive this call; the
/// runtime dir is wiped on logout regardless.
fn sweep_open_dir(dir: &Path) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if now
            .duration_since(modified)
            .map(|d| d > MAX_AGE)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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
    GoOffline {
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
    /// This vault's own account id (the hex identity key), for features that
    /// stamp local authorship — Conclave Paper's creator/hub fields.
    AccountId {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Tell a conclave's trustees that a paper exists, or that its hub moved.
    SendPaperState {
        conclave_id: String,
        paper: PaperDescriptor,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Fan one opaque `yrs` update out to a conclave's trustees.
    SendPaperDelta {
        conclave_id: String,
        paper_id: String,
        update: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Pass one opaque `yrs` update on to the conclave's other trustees.
    ///
    /// The hub's half of the relay fan-out: a mesh can only reach the trustees
    /// the sender's roster knew about, and a roster that has not caught up with
    /// a join strands that edit forever.
    RelayPaperDelta {
        conclave_id: String,
        paper_id: String,
        from_account: String,
        update: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Hand a trustee that joined late the whole document.
    SendPaperSnapshot {
        conclave_id: String,
        paper: PaperDescriptor,
        state: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Announce a paper's new hub to the conclave. Convener-only.
    SendPaperHub {
        conclave_id: String,
        paper: PaperDescriptor,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Deliver one DataChannel signalling payload to a single trustee.
    SendPaperSignal {
        conclave_id: String,
        peer_account: String,
        payload: MessagePayload,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Take the hub of a paper whose hub left, when this vault is the conclave's
    /// convener.
    InheritPaperHub {
        conclave_id: String,
        gone_hub: String,
        paper: PaperDescriptor,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Reads pane: replace the encrypted sidecar state wholesale.
    ReadsStatePut {
        bytes: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Read a named encrypted vault sidecar for a feature that owns both the
    /// name and the schema inside it. Unlike the pane stores above, the name
    /// comes from the caller — the vault validates it (no separators, no
    /// traversal) before anything touches disk.
    SecretRead {
        name: String,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Write a named encrypted vault sidecar.
    SecretWrite {
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Delete a named encrypted vault sidecar.
    SecretRemove {
        name: String,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Zines: the pen name this vault publishes under, if it has one yet.
    PenNameGet {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Zines: mint a pen name (create when absent, rename when present).
    ///
    /// One command for both because the caller — the zines pane — wants the same
    /// answer either way ("the pen name I now publish under"), and a separate
    /// create/rename pair would make the UI decide which one applies from a
    /// snapshot that may already be stale.
    PenNameSet {
        name: String,
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
    /// Wallet: load the sealed wallet sidecar (None when never saved).
    WalletStateGet {
        reply: oneshot::Sender<WorkResult>,
    },
    /// Wallet: replace the sealed wallet sidecar wholesale.
    WalletStatePut {
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
    /// Materialize a stored blob to a temp file so the UI can open or save it
    /// without the bytes crossing the webview. Returns the file path.
    BlobMaterialize {
        id_hex: String,
        /// The original file name; its extension drives the OS open handler.
        name: String,
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

/// The zines pen name, as the UI sees it: a byline and two public keys. No
/// secret material crosses this boundary, and the private half never leaves the
/// vault — a pen name is only useful if the daemon signs with it.
#[derive(Clone, serde::Serialize)]
pub struct PenNameDto {
    /// `zine_id`: the seal public key, hex. The address a comment is sealed to.
    pub id: String,
    /// The byline shown to readers.
    pub name: String,
    /// The signing public key, hex. Provenance, published with a blob.
    pub sign_pk: String,
}

impl From<hush_core::PenName> for PenNameDto {
    fn from(p: hush_core::PenName) -> Self {
        Self {
            id: p.id,
            name: p.name,
            sign_pk: p.sign_pk,
        }
    }
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
    /// This vault's own account id (the hex identity key).
    AccountId(String),
    /// A named vault sidecar's bytes; `None` when nothing is stored under that
    /// name yet.
    Secret(Option<Vec<u8>>),
    /// A named vault sidecar was deleted. Deleting is idempotent, so this
    /// carries no "did it exist" flag: either way the name is now absent.
    SecretRemoved,
    /// The zines pen name this vault publishes under; `None` before one exists.
    PenName(Option<PenNameDto>),
    /// The record id of the saved draft.
    DraftSaved(String),
    Blob(Vec<u8>),
    BlobId(String),
    /// The path a stored blob was materialized to for open/save.
    Path(String),
    /// Every conclave this vault holds, with its message log.
    Conclaves(Vec<ConclaveRecord>),
    /// The conclave that was just created.
    ConclaveCreated(ConclaveRecord),
    /// The conclave that was just joined by accepting an invite.
    ConclaveAccepted(ConclaveRecord),
    /// Conclave invitations received but not yet accepted or declined.
    PendingInvites(Vec<PendingInvite>),
    /// The descriptor a hub succession sent, when this vault was the convener
    /// that inherited it. `None` is the ordinary answer for every other trustee
    /// that saw the same departure.
    Rehomed(Option<PaperDescriptor>),
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
                        Work::GoOffline { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.go_offline().await {
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
                        Work::AccountId { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::AccountId(a.protocol_name().to_string()),
                            };
                            (reply, result)
                        }
                        Work::SendPaperState {
                            conclave_id,
                            paper,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_conclave_paper_state(&conclave_id, paper)
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendPaperDelta {
                            conclave_id,
                            paper_id,
                            update,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_conclave_paper_delta(&conclave_id, &paper_id, &update)
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RelayPaperDelta {
                            conclave_id,
                            paper_id,
                            from_account,
                            update,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .relay_conclave_paper_delta(
                                        &conclave_id,
                                        &paper_id,
                                        &from_account,
                                        &update,
                                    )
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendPaperSnapshot {
                            conclave_id,
                            paper,
                            state,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_conclave_paper_snapshot(&conclave_id, paper, &state)
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendPaperSignal {
                            conclave_id,
                            peer_account,
                            payload,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_paper_signal(&conclave_id, &peer_account, payload)
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::InheritPaperHub {
                            conclave_id,
                            gone_hub,
                            paper,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .inherit_conclave_paper_hub(&conclave_id, &gone_hub, paper)
                                    .await
                                {
                                    Ok(moved) => WorkResult::Rehomed(moved),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendPaperHub {
                            conclave_id,
                            paper,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_conclave_paper_hub(&conclave_id, paper)
                                    .await
                                {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
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
                        Work::SecretRead { name, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.read_vault_sidecar(&name) {
                                    Ok(data) => WorkResult::Secret(data),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SecretWrite { name, data, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.write_vault_sidecar(&name, &data) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SecretRemove { name, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.remove_vault_sidecar(&name) {
                                    Ok(_) => WorkResult::SecretRemoved,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::PenNameGet { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    WorkResult::PenName(a.pen_name().map(PenNameDto::from))
                                }
                            };
                            (reply, result)
                        }
                        Work::PenNameSet { name, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    // Create when there is none, rename when
                                    // there is: the pane's question is "what am
                                    // I publishing under", not "does one exist".
                                    let outcome = match a.pen_name() {
                                        Some(_) => a.rename_pen_name(&name),
                                        None => a.create_pen_name(&name),
                                    };
                                    match outcome {
                                        Ok(pen) => WorkResult::PenName(Some(pen.into())),
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
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
                        Work::WalletStateGet { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    match a.read_vault_sidecar(crate::wallet::STATE_FILE) {
                                        Ok(state) => WorkResult::ReadsState(state),
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::WalletStatePut { bytes, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .write_vault_sidecar(crate::wallet::STATE_FILE, &bytes)
                                {
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
                        Work::BlobMaterialize { id_hex, name, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match blob_id_from_hex(&id_hex) {
                                    None => WorkResult::Err("invalid blob id".into()),
                                    Some(id) => {
                                        if !a.has_blob(&id) {
                                            WorkResult::Err("no such file in the vault".into())
                                        } else {
                                            match materialize_blob(a, &id, &name) {
                                                Ok(path) => {
                                                    WorkResult::Path(path.display().to_string())
                                                }
                                                Err(e) => WorkResult::Err(e),
                                            }
                                        }
                                    }
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

    pub async fn go_offline(&self) -> WorkResult {
        self.call(|reply| Work::GoOffline { reply }).await
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

    /// This vault's own account id (the hex identity key); `Err("locked")`
    /// while the vault is locked, like every other secret-touching call.
    pub async fn account_id(&self) -> WorkResult {
        self.call(|reply| Work::AccountId { reply }).await
    }

    /// Fan a paper descriptor out to a conclave's joined trustees.
    pub async fn send_paper_state(
        &self,
        conclave_id: &str,
        paper: PaperDescriptor,
    ) -> WorkResult {
        self.call(|reply| Work::SendPaperState {
            conclave_id: conclave_id.to_string(),
            paper,
            reply,
        })
        .await
    }

    /// Fan one opaque CRDT update out to a conclave's joined trustees.
    pub async fn send_paper_delta(
        &self,
        conclave_id: &str,
        paper_id: &str,
        update: &[u8],
    ) -> WorkResult {
        self.call(|reply| Work::SendPaperDelta {
            conclave_id: conclave_id.to_string(),
            paper_id: paper_id.to_string(),
            update: update.to_vec(),
            reply,
        })
        .await
    }

    /// Pass one opaque CRDT update on to the conclave's other trustees.
    ///
    /// Called by the paper's hub when a delta arrives from a peer, so an edit
    /// cannot be stranded by a sender whose roster had not caught up with a
    /// join.
    pub async fn relay_paper_delta(
        &self,
        conclave_id: &str,
        paper_id: &str,
        from_account: &str,
        update: &[u8],
    ) -> WorkResult {
        self.call(|reply| Work::RelayPaperDelta {
            conclave_id: conclave_id.to_string(),
            paper_id: paper_id.to_string(),
            from_account: from_account.to_string(),
            update: update.to_vec(),
            reply,
        })
        .await
    }

    /// Hand a trustee that joined late the whole document.
    pub async fn send_paper_snapshot(
        &self,
        conclave_id: &str,
        paper: PaperDescriptor,
        state: &[u8],
    ) -> WorkResult {
        self.call(|reply| Work::SendPaperSnapshot {
            conclave_id: conclave_id.to_string(),
            paper,
            state: state.to_vec(),
            reply,
        })
        .await
    }

    /// Deliver one DataChannel signalling payload to a single trustee.
    ///
    /// Addressed rather than fanned out: the channel is 1:1 between a trustee
    /// and the paper's hub.
    pub async fn send_paper_signal(
        &self,
        conclave_id: &str,
        peer_account: &str,
        payload: MessagePayload,
    ) -> WorkResult {
        self.call(|reply| Work::SendPaperSignal {
            conclave_id: conclave_id.to_string(),
            peer_account: peer_account.to_string(),
            payload,
            reply,
        })
        .await
    }

    /// Take the hub of a paper whose hub left, when this vault is the convener.
    ///
    /// The answer is the descriptor that was sent, or `None` when this vault
    /// has no authority to inherit — which is the ordinary answer for every
    /// trustee that is not the convener, not an error.
    pub async fn inherit_paper_hub(
        &self,
        conclave_id: &str,
        gone_hub: &str,
        paper: PaperDescriptor,
    ) -> WorkResult {
        self.call(|reply| Work::InheritPaperHub {
            conclave_id: conclave_id.to_string(),
            gone_hub: gone_hub.to_string(),
            paper,
            reply,
        })
        .await
    }

    /// Announce a paper's new hub. Refused unless this vault is the convener.
    pub async fn send_paper_hub(&self, conclave_id: &str, paper: PaperDescriptor) -> WorkResult {
        self.call(|reply| Work::SendPaperHub {
            conclave_id: conclave_id.to_string(),
            paper,
            reply,
        })
        .await
    }

    /// Replace the Reads pane's encrypted sidecar state wholesale.
    pub async fn reads_state_put(&self, bytes: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::ReadsStatePut { bytes, reply })
            .await
    }

    /// The zines pen name this vault publishes under, if it has one yet.
    pub async fn pen_name(&self) -> WorkResult {
        self.call(|reply| Work::PenNameGet { reply }).await
    }

    /// Mint or rename the zines pen name, returning the one now in force.
    pub async fn set_pen_name(&self, name: &str) -> WorkResult {
        self.call(|reply| Work::PenNameSet {
            name: name.to_string(),
            reply,
        })
        .await
    }

    /// Read a named encrypted vault sidecar. The caller owns the name and the
    /// schema inside; `WorkResult::Secret(None)` means the name is unused.
    pub async fn secret_read(&self, name: &str) -> WorkResult {
        self.call(|reply| Work::SecretRead {
            name: name.to_string(),
            reply,
        })
        .await
    }

    /// Write a named encrypted vault sidecar.
    pub async fn secret_write(&self, name: &str, data: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::SecretWrite {
            name: name.to_string(),
            data,
            reply,
        })
        .await
    }

    /// Delete a named encrypted vault sidecar.
    pub async fn secret_remove(&self, name: &str) -> WorkResult {
        self.call(|reply| Work::SecretRemove {
            name: name.to_string(),
            reply,
        })
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

    /// The Wallet's sealed sidecar state, raw bytes (None until first saved).
    /// The schema lives in [`crate::wallet`]; the bytes are ciphertext to us.
    pub async fn wallet_state_get(&self) -> WorkResult {
        self.call(|reply| Work::WalletStateGet { reply }).await
    }

    /// Replace the Wallet's sealed sidecar state wholesale.
    pub async fn wallet_state_put(&self, bytes: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::WalletStatePut { bytes, reply }).await
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

    /// Materialize a stored blob to a temp file so the UI can open or save it
    /// without the bytes crossing the webview. Returns the file path. See
    /// [`materialize_blob`].
    pub async fn blob_materialize(&self, id_hex: &str, name: &str) -> WorkResult {
        self.call(|reply| Work::BlobMaterialize {
            id_hex: id_hex.to_string(),
            name: name.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_open_name_keeps_the_base_and_extension_only() {
        // The extension must survive: it is what makes the OS pick the player.
        assert_eq!(sanitize_open_name("Movie.mkv"), "Movie.mkv");
        assert_eq!(sanitize_open_name("S01/E01.mkv"), "E01.mkv");
        assert_eq!(sanitize_open_name("/abs/path/video.mp4"), "video.mp4");
        // Path traversal and separators are stripped, not trusted. On Linux a
        // backslash is not a separator, so it becomes an underscore instead of
        // splitting the name — either way it cannot escape the open dir.
        assert_eq!(sanitize_open_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_open_name("a\\b.mkv"), "a_b.mkv");
        // Spaces and other unsafe characters become underscores.
        assert_eq!(sanitize_open_name("my movie (2024).mkv"), "my_movie__2024_.mkv");
        // Empty or dot-only names fall back to a usable default.
        assert_eq!(sanitize_open_name(""), "file");
        assert_eq!(sanitize_open_name(".."), "file");
        assert_eq!(sanitize_open_name("."), "file");
    }
}
