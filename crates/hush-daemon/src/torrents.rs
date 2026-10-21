//! The Torrents pipeline: the state machine that turns an added magnet or
//! `.torrent` file into files in the vault, via TorBox.
//!
//! ```text
//!   add ─▶ Queued ─▶ Sending ─▶ Caching ─▶ Ready ─▶ Downloading ─▶ Completed
//!                    (createtorrent)  (poll mylist)   (requestdl + Range)
//! ```
//!
//! One background task owns the loop. It wakes on a timer (4s while anything is
//! active, 30s when idle) and immediately when a route adds work, so the UI
//! never waits on a poll interval to see an add take effect.
//!
//! Design notes that matter:
//!
//! - **The daemon does the talking.** TorBox calls and CDN downloads happen
//!   here, never in the webview, so the API key (which `requestdl` puts in a
//!   query string) is never exposed.
//! - **Downloads stream and resume.** A file is fetched with an HTTP Range from
//!   where a previous attempt stopped, into a temp file under the vault dir,
//!   then stored as a chunked encrypted blob. A crash or a dropped connection
//!   resumes rather than restarting.
//! - **Bounded concurrency.** At most `max_concurrent` files download at once
//!   (default 2, configurable in Settings).
//! - **Errors are classified.** A bad key or a plan limit parks the item in
//!   `Error` for the user; a network blip or a 429 backs off and retries on its
//!   own.
//!
//! The state itself lives in [`crate::torrents_store`]; this module only drives
//! it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use tokio::sync::{Mutex, Notify};

use crate::account_worker::{AccountWorker, WorkResult};
use crate::torbox::{self, Control, TorboxClient, TorboxError, UserInfo};
use crate::torrents_store::{
    decode, now_secs, ItemFile, ItemStatus, LibraryEntry, OfferStatus, SourceKind, TorrentItem,
    TorrentOffer, TorrentsState,
};

/// Why one file's download failed, and whether another attempt could help.
///
/// The pipeline treats the whole item as one unit: the first failure decides
/// the item's fate, so a permanent cause (a CDN that will not resume, a file
/// that will never match its expected size) must not be retried forever the
/// way a dropped connection should be.
#[derive(Debug, Clone)]
struct FileError {
    message: String,
    transient: bool,
}

impl FileError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }

    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    /// Carry a TorBox error's own transient/permanent classification.
    fn from_torbox(e: TorboxError) -> Self {
        Self {
            transient: e.is_transient(),
            message: e.to_string(),
        }
    }
}

/// Poll cadence while at least one item is active.
const ACTIVE_POLL: Duration = Duration::from_secs(4);
/// Poll cadence when nothing is in flight (only wakes to notice a new key).
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Cap on automatic retry backoff.
const MAX_BACKOFF_SECS: i64 = 900;
/// How often the download reporter refreshes an item's progress.
const PROGRESS_TICK: Duration = Duration::from_secs(1);

/// The Torrents pane's view of the world. The API key is deliberately absent.
#[derive(Serialize)]
pub struct PublicState {
    pub has_key: bool,
    pub plan: Option<i64>,
    pub plan_label: Option<String>,
    pub email: Option<String>,
    pub max_concurrent: u32,
    pub items: Vec<TorrentItem>,
    pub library: Vec<LibraryEntry>,
    /// Incoming torrent offers. Bodies live here, not in the signal audit log.
    pub offers: Vec<TorrentOffer>,
}

pub struct TorrentsManager {
    worker: AccountWorker,
    client: TorboxClient,
    state: Mutex<TorrentsState>,
    /// Last known account info; memory only (re-validated on demand).
    account: Mutex<Option<UserInfo>>,
    /// Where resumable partial downloads live (under the vault dir).
    tmp_dir: PathBuf,
    loaded: AtomicBool,
    dirty: AtomicBool,
    notify: Notify,
}

impl TorrentsManager {
    pub fn new(worker: AccountWorker, vault_dir: &Path) -> Self {
        Self {
            worker,
            client: TorboxClient::new(),
            state: Mutex::new(TorrentsState::default()),
            account: Mutex::new(None),
            tmp_dir: vault_dir.join("torrents-tmp"),
            loaded: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// Wake the pipeline immediately (a route added or changed work).
    fn wake(&self) {
        self.notify.notify_one();
    }

    /// Load the encrypted sidecar once. `Err("locked")` while the vault is
    /// locked, which routes translate to a 401 and the pipeline treats as "wait".
    async fn ensure_loaded(&self) -> Result<(), String> {
        if self.loaded.load(Ordering::SeqCst) {
            return Ok(());
        }
        match self.worker.torrents_state_get().await {
            WorkResult::ReadsState(raw) => {
                let parsed = decode(raw.as_deref())?;
                *self.state.lock().await = parsed;
                self.loaded.store(true, Ordering::SeqCst);
                Ok(())
            }
            WorkResult::Err(m) if m == "locked" => Err("locked".into()),
            WorkResult::Err(m) => Err(m),
            _ => Err("unexpected worker reply".into()),
        }
    }

    /// Persist the state if it changed. A locked vault clears `loaded` so the
    /// next unlock reloads from disk.
    async fn persist(&self) -> Result<(), String> {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let bytes = self.state.lock().await.encode()?;
        match self.worker.torrents_state_put(bytes).await {
            WorkResult::Ok => Ok(()),
            WorkResult::Err(m) if m == "locked" => {
                self.loaded.store(false, Ordering::SeqCst);
                Err("locked".into())
            }
            WorkResult::Err(m) => Err(m),
            _ => Err("unexpected worker reply".into()),
        }
    }

    async fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// The UI's view. Does not include the API key.
    pub async fn public_state(&self) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let state = self.state.lock().await;
        let account = self.account.lock().await;
        Ok(PublicState {
            has_key: state.api_key.is_some(),
            plan: account.as_ref().map(|a| a.plan),
            plan_label: account.as_ref().map(|a| a.plan_label.clone()),
            email: account.as_ref().and_then(|a| a.email.clone()),
            max_concurrent: state.concurrency(),
            items: state.items.iter().map(|i| i.public_clone()).collect(),
            library: state.library.clone(),
            offers: state.offers.clone(),
        })
    }

    /// Validate a key without saving it (the Settings "Test connection").
    pub async fn test_key(&self, key: &str) -> Result<UserInfo, String> {
        if key.trim().is_empty() {
            return Err("Enter your TorBox API key first.".into());
        }
        self.client
            .user_me(key.trim())
            .await
            .map_err(|e| e.to_string())
    }

    /// Save a key after validating it. Empty clears the key (and stops the
    /// pipeline, which then has nothing to talk to).
    pub async fn set_key(&self, key: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let key = key.trim();
        if key.is_empty() {
            {
                let mut s = self.state.lock().await;
                s.api_key = None;
            }
            *self.account.lock().await = None;
            self.mark_dirty().await;
            self.persist().await?;
            self.wake();
            return self.public_state().await;
        }
        let info = self
            .client
            .user_me(key)
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut s = self.state.lock().await;
            s.api_key = Some(key.to_string());
        }
        *self.account.lock().await = Some(info);
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Re-read the account (plan/email) with the stored key.
    pub async fn refresh_account(&self) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let key = self.state.lock().await.api_key.clone();
        if let Some(key) = key {
            match self.client.user_me(&key).await {
                Ok(info) => *self.account.lock().await = Some(info),
                Err(TorboxError::Auth(m)) => {
                    return Err(m);
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        self.public_state().await
    }

    /// Add a magnet link or a `.torrent` URL. Returns the new state so the UI
    /// repaints at once.
    pub async fn add_source(&self, input: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let input = input.trim();
        if input.is_empty() {
            return Err("Paste a magnet link or a .torrent URL first.".into());
        }
        let (source, name, hash) = if torbox::looks_like_magnet(input) {
            let name = input
                .split('&')
                .find_map(|p| p.strip_prefix("dn="))
                .map(percent_decode)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Magnet".to_string());
            (SourceKind::Magnet, name, torbox::magnet_hash(input))
        } else if torbox::looks_like_torrent_url(input) {
            let name = input
                .rsplit('/')
                .next()
                .unwrap_or("Torrent")
                .split('?')
                .next()
                .unwrap_or("Torrent")
                .trim_end_matches(".torrent")
                .to_string();
            let name = if name.is_empty() { "Torrent".into() } else { name };
            (SourceKind::Link, name, None)
        } else {
            return Err("That doesn't look like a magnet link or a .torrent URL.".into());
        };
        let mut item = TorrentItem::new(uuid::Uuid::new_v4().to_string(), name, source);
        item.hash = hash;
        item.source_data = Some(input.to_string());
        {
            let mut s = self.state.lock().await;
            s.items.insert(0, item);
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// One stored offer, for the route that answers it before mutating state.
    pub async fn offer(&self, offer_id: &str) -> Result<Option<TorrentOffer>, String> {
        self.ensure_loaded().await?;
        let s = self.state.lock().await;
        Ok(s.offer(offer_id).cloned())
    }

    /// Store one incoming offer. A repeated id is a no-op.
    pub async fn add_offer(&self, offer: TorrentOffer) -> Result<bool, String> {
        self.ensure_loaded().await?;
        let added = {
            let mut s = self.state.lock().await;
            s.add_offer(offer)
        };
        if added {
            self.mark_dirty().await;
            self.persist().await?;
        }
        Ok(added)
    }

    /// The name, info hash and size of an entry this vault can offer. Looked up
    /// here rather than supplied by the UI, so a share cannot name content the
    /// vault does not hold.
    pub async fn shareable(&self, id: &str) -> Result<(String, String, u64), String> {
        self.ensure_loaded().await?;
        let s = self.state.lock().await;
        let (name, hash, size) = if let Some(item) = s.item(id) {
            (item.name.clone(), item.hash.clone(), item.size)
        } else if let Some(entry) = s.library.iter().find(|e| e.id == id) {
            (entry.name.clone(), entry.hash.clone(), entry.size)
        } else {
            return Err("no such torrent".into());
        };
        let hash = hash.ok_or("this torrent has no info hash to share")?;
        if !valid_offer_hash(&hash) {
            return Err("this torrent's info hash cannot be shared".into());
        }
        Ok((name, hash.to_ascii_lowercase(), size))
    }

    /// Accept an offer: queue the torrent as a normal magnet item. The caller
    /// sends the response signal after this returns.
    pub async fn accept_offer(
        &self,
        offer_id: &str,
    ) -> Result<(TorrentOffer, PublicState), String> {
        self.ensure_loaded().await?;
        if self.state.lock().await.api_key.is_none() {
            return Err("Add your TorBox API key in Settings first.".into());
        }
        let offer = {
            let s = self.state.lock().await;
            s.offer(offer_id)
                .cloned()
                .ok_or("no such torrent offer")?
        };
        if offer.status != OfferStatus::Pending {
            return Err("that offer has already been answered".into());
        }
        if !valid_offer_hash(&offer.hash) {
            return Err("that offer has an invalid info hash".into());
        }
        // Answering is one transition, made under one lock: a second request
        // (a double click, a retried signal) sees the offer resolved and is
        // refused instead of queueing the same bytes again. The hash check
        // also covers two *different* offers for one torrent, and a torrent
        // that is already in the Library.
        let updated = {
            let mut s = self.state.lock().await;
            match s.offer(offer_id) {
                Some(entry) if entry.status == OfferStatus::Pending => {}
                Some(_) => return Err("that offer has already been answered".into()),
                None => return Err("no such torrent offer".into()),
            }
            let item_id = match s.id_for_hash(&offer.hash) {
                Some(id) => id.to_string(),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    let mut item =
                        TorrentItem::new(id.clone(), offer.name.clone(), SourceKind::Magnet);
                    item.hash = Some(offer.hash.clone());
                    item.size = offer.size;
                    item.source_data = Some(magnet_for(&offer.name, &offer.hash));
                    s.items.insert(0, item);
                    id
                }
            };
            let Some(entry) = s.offer_mut(offer_id) else {
                return Err("no such torrent offer".into());
            };
            entry.status = OfferStatus::Accepted;
            entry.item_id = Some(item_id);
            entry.clone()
        };
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        let public = self.public_state().await?;
        Ok((updated, public))
    }

    /// Decline an offer. Nothing is queued.
    pub async fn decline_offer(
        &self,
        offer_id: &str,
    ) -> Result<(TorrentOffer, PublicState), String> {
        self.ensure_loaded().await?;
        let updated = {
            let mut s = self.state.lock().await;
            let Some(entry) = s.offer_mut(offer_id) else {
                return Err("no such torrent offer".into());
            };
            if entry.status != OfferStatus::Pending {
                return Err("that offer has already been answered".into());
            }
            entry.status = OfferStatus::Declined;
            entry.clone()
        };
        self.mark_dirty().await;
        self.persist().await?;
        let public = self.public_state().await?;
        Ok((updated, public))
    }

    /// Add a `.torrent` file's bytes.
    pub async fn add_torrent_file(&self, name: &str, bytes: Vec<u8>) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        if bytes.is_empty() {
            return Err("That torrent file is empty.".into());
        }
        let filename = if name.trim().is_empty() {
            "upload.torrent".to_string()
        } else {
            name.trim().to_string()
        };
        let display = filename
            .strip_suffix(".torrent")
            .unwrap_or(&filename)
            .to_string();
        let mut item = TorrentItem::new(uuid::Uuid::new_v4().to_string(), display, SourceKind::File);
        item.source_data = Some(base64_encode(&bytes));
        {
            let mut s = self.state.lock().await;
            s.items.insert(0, item);
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Pause: TorBox pauses the cloud torrent; the item stops progressing.
    pub async fn pause(&self, id: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let (key, torrent_id) = {
            let s = self.state.lock().await;
            let item = s.item(id).ok_or("no such torrent")?;
            (s.api_key.clone(), item.torrent_id)
        };
        if let (Some(key), Some(tid)) = (key, torrent_id) {
            // Best effort: pausing locally still stops our downloads.
            let _ = self.client.control(&key, tid, Control::Pause).await;
        }
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                item.status = ItemStatus::Paused;
            }
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Resume: TorBox resumes; the pipeline re-derives the next state.
    pub async fn resume(&self, id: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let (key, torrent_id) = {
            let s = self.state.lock().await;
            let item = s.item(id).ok_or("no such torrent")?;
            (s.api_key.clone(), item.torrent_id)
        };
        if let (Some(key), Some(tid)) = (key, torrent_id) {
            let _ = self.client.control(&key, tid, Control::Resume).await;
        }
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                // Re-derive: Caching if TorBox is still fetching, else Ready.
                item.status = if item.files.is_empty() || !item.files.iter().all(|f| f.done) {
                    if item.files.is_empty() {
                        ItemStatus::Caching
                    } else {
                        ItemStatus::Ready
                    }
                } else {
                    ItemStatus::Completed
                };
                item.message = None;
                item.next_attempt_at = 0;
            }
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Retry an errored item: clear the error and let the pipeline re-derive.
    pub async fn retry(&self, id: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        {
            let mut s = self.state.lock().await;
            let item = s.item_mut(id).ok_or("no such torrent")?;
            item.message = None;
            item.retry = 0;
            item.next_attempt_at = 0;
            item.status = if item.torrent_id.is_none() {
                ItemStatus::Queued
            } else if item.files.is_empty() || !item.files.iter().all(|f| f.done) {
                if item.files.is_empty() {
                    ItemStatus::Caching
                } else {
                    ItemStatus::Ready
                }
            } else {
                ItemStatus::Completed
            };
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Remove an active item. Deletes the TorBox torrent (best effort) and any
    /// partial download, but leaves nothing in the vault (nothing was stored).
    pub async fn remove(&self, id: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let (key, torrent_id) = {
            let s = self.state.lock().await;
            let item = s.item(id).ok_or("no such torrent")?;
            (s.api_key.clone(), item.torrent_id)
        };
        if let (Some(key), Some(tid)) = (key, torrent_id) {
            let _ = self.client.control(&key, tid, Control::Delete).await;
        }
        {
            let mut s = self.state.lock().await;
            s.remove_item(id);
        }
        self.cleanup_partials(id).await;
        self.mark_dirty().await;
        self.persist().await?;
        self.public_state().await
    }

    /// Delete a Library entry and its vault blobs.
    pub async fn delete_library(&self, id: &str) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let blobs = {
            let mut s = self.state.lock().await;
            s.remove_library_entry(id)
        };
        for blob in blobs {
            let _ = self.worker.blob_delete(&blob).await;
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.public_state().await
    }

    /// Change the concurrent-download limit.
    pub async fn set_concurrency(&self, n: u32) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        {
            let mut s = self.state.lock().await;
            s.set_concurrency(n);
        }
        self.mark_dirty().await;
        self.persist().await?;
        self.wake();
        self.public_state().await
    }

    /// Remove any `<id>-*.part` files for an item.
    async fn cleanup_partials(&self, id: &str) {
        let prefix = format!("{id}-");
        let dir = self.tmp_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let name = e.file_name();
                    if name.to_string_lossy().starts_with(&prefix) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        })
        .await;
    }

    // ------------------------------------------------------------------
    // The pipeline
    // ------------------------------------------------------------------

    /// Run the pipeline forever. Called once from `main` with an `Arc`.
    pub async fn run(self: Arc<Self>) {
        loop {
            let active = {
                // Best-effort: if we cannot read state yet, assume idle.
                self.state.lock().await.has_active()
            };
            let wait = if active { ACTIVE_POLL } else { IDLE_POLL };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.notify.notified() => {}
            }
            let _ = self.tick().await;
        }
    }

    /// One pass: advance every due item one step.
    async fn tick(self: &Arc<Self>) -> Result<(), String> {
        if self.ensure_loaded().await.is_err() {
            return Ok(()); // locked or unreadable; wait for the next wake
        }
        let key = { self.state.lock().await.api_key.clone() };
        let Some(key) = key else { return Ok(()) };

        // Validate the account once per process so the Settings pane can show
        // the plan without a manual refresh.
        if self.account.lock().await.is_none() {
            if let Ok(info) = self.client.user_me(&key).await {
                *self.account.lock().await = Some(info);
            }
        }

        let now = now_secs();
        let due: Vec<String> = {
            let s = self.state.lock().await;
            s.items
                .iter()
                .filter(|i| i.is_due(now))
                .map(|i| i.id.clone())
                .collect()
        };
        for id in due {
            let status = { self.state.lock().await.item(&id).map(|i| i.status) };
            match status {
                Some(ItemStatus::Queued) | Some(ItemStatus::Sending) => {
                    self.add_to_torbox(&key, &id).await;
                }
                Some(ItemStatus::Caching) => self.poll_caching(&key, &id).await,
                Some(ItemStatus::Ready) | Some(ItemStatus::Downloading) => {
                    self.download_item(&key, &id).await;
                }
                _ => {}
            }
        }
        let _ = self.persist().await;
        Ok(())
    }

    /// Send a queued item to TorBox, then move it to `Caching`.
    async fn add_to_torbox(&self, key: &str, id: &str) {
        // Show "Sending to TorBox" for the duration of the create call.
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                item.status = ItemStatus::Sending;
            }
        }
        self.mark_dirty().await;
        let (source, source_data, name, hash) = {
            let s = self.state.lock().await;
            let Some(item) = s.item(id) else { return };
            (
                item.source,
                item.source_data.clone(),
                item.name.clone(),
                item.hash.clone(),
            )
        };
        let Some(source_data) = source_data else {
            self.fail(id, "This torrent has no source to send.".into(), false)
                .await;
            return;
        };

        // A cached hash lets the UI say "cached" the moment it lands, but we
        // still create the torrent to get an id for requestdl.
        let mut was_cached = false;
        if let Some(hash) = &hash {
            if let Ok(map) = self.client.check_cached(key, std::slice::from_ref(hash)).await {
                was_cached = !map.is_empty();
            }
        }

        let result = match source {
            SourceKind::Magnet => self.client.create_torrent_magnet(key, &source_data, None).await,
            SourceKind::Link => self.client.create_torrent_link(key, &source_data, None).await,
            SourceKind::File => match base64_decode(&source_data) {
                Ok(bytes) => {
                    self.client
                        .create_torrent_file(key, &name, bytes, None)
                        .await
                }
                Err(_) => {
                    self.fail(id, "This torrent file could not be read.".into(), false)
                        .await;
                    return;
                }
            },
        };

        match result {
            Ok(created) => {
                let mut s = self.state.lock().await;
                if let Some(item) = s.item_mut(id) {
                    item.torrent_id = Some(created.torrent_id);
                    if !created.hash.is_empty() {
                        item.hash = Some(created.hash);
                    }
                    // Drop the sensitive source now that TorBox has it.
                    item.source_data = None;
                    item.cached = was_cached;
                    item.status = ItemStatus::Caching;
                    item.message = None;
                    item.retry = 0;
                    item.next_attempt_at = 0;
                }
                drop(s);
                self.mark_dirty().await;
            }
            Err(e) => {
                let transient = e.is_transient();
                self.fail(id, e.to_string(), transient).await;
            }
        }
    }

    /// Poll TorBox for a caching item; promote to `Ready` when the data is there.
    async fn poll_caching(&self, key: &str, id: &str) {
        let torrent_id = {
            let s = self.state.lock().await;
            s.item(id).and_then(|i| i.torrent_id)
        };
        let Some(torrent_id) = torrent_id else {
            // Lost the id somehow; send it back to Queued to re-create.
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                item.status = ItemStatus::Queued;
            }
            drop(s);
            self.mark_dirty().await;
            return;
        };

        match self.client.list(key, Some(torrent_id), true).await {
            Ok(list) => {
                let Some(info) = list.into_iter().next() else {
                    self.fail(id, "TorBox no longer has this torrent.".into(), false)
                        .await;
                    return;
                };
                let mut s = self.state.lock().await;
                let state = info.download_state.to_ascii_lowercase();
                let mut terminal = false;
                if let Some(item) = s.item_mut(id) {
                    item.progress = info.progress_fraction();
                    item.speed = info.download_speed;
                    item.seeds = info.seeds;
                    item.size = info.size;
                    item.cached = info.cached;
                    if !info.name.is_empty() {
                        item.name = info.name.clone();
                    }
                    if state == "paused" {
                        item.status = ItemStatus::Paused;
                        item.message = None;
                    } else if info.is_ready() {
                        item.files = info.files.iter().map(item_file_from_torbox).collect();
                        item.progress = 1.0;
                        item.speed = 0.0;
                        item.refresh_download_totals(0);
                        item.status = ItemStatus::Ready;
                        item.remote_state = None;
                        item.message = None;
                    } else if matches!(state.as_str(), "failed" | "not downloadable") {
                        // TorBox gave up on this torrent; waiting longer cannot
                        // help. Keep its own word so the reason is not lost.
                        item.status = ItemStatus::Error;
                        item.message = Some(format!(
                            "TorBox gave up on this torrent ({}).",
                            info.download_state
                        ));
                        terminal = true;
                    } else {
                        // Still caching. Record TorBox's own state so a stalled
                        // item says why — a slot queue on their side looks like
                        // a hang here.
                        item.remote_state = if info.download_state.is_empty() {
                            None
                        } else {
                            Some(info.download_state.clone())
                        };
                        item.message = None;
                    }
                }
                drop(s);
                self.mark_dirty().await;
                if terminal {
                    let _ = self.persist().await;
                }
            }
            Err(e) => {
                let transient = e.is_transient();
                self.fail(id, e.to_string(), transient).await;
            }
        }
    }

    /// Download the pending files of a ready item into the vault, with bounded
    /// concurrency, then file the item into the Library.
    async fn download_item(self: &Arc<Self>, key: &str, id: &str) {
        let (torrent_id, pending) = {
            let s = self.state.lock().await;
            let Some(item) = s.item(id) else { return };
            let Some(tid) = item.torrent_id else { return };
            let pending: Vec<ItemFile> = item.pending_files().cloned().collect();
            (tid, pending)
        };
        if pending.is_empty() {
            self.finish_item(id).await;
            return;
        }
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                item.status = ItemStatus::Downloading;
            }
        }
        self.mark_dirty().await;

        let concurrency = {
            let s = self.state.lock().await;
            s.concurrency() as usize
        }
        .max(1);

        let stop = Arc::new(AtomicBool::new(false));
        let reporter = self.spawn_progress_reporter(id.to_string(), stop.clone());

        let results: Vec<(i64, Result<String, FileError>)> = stream::iter(pending)
            .map(|file| async move {
                let res = self.download_one(key, torrent_id, id, &file).await;
                (file.file_id, res)
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        stop.store(true, Ordering::SeqCst);
        let _ = reporter.await;

        // Bytes already on disk for the files that are not done yet. The temp
        // files keep whatever a previous attempt (or a crash) downloaded, so
        // this is the true progress and does not snap back to 0% on resume.
        let on_disk = self.partial_bytes(id).await;

        // Apply results: stored blob ids for successes, an error otherwise.
        let mut first_error: Option<FileError> = None;
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                for (file_id, res) in &results {
                    match res {
                        Ok(blob_id) => {
                            if let Some(f) = item.files.iter_mut().find(|f| f.file_id == *file_id) {
                                f.blob_id = Some(blob_id.clone());
                                f.done = true;
                            }
                        }
                        Err(err) => {
                            if first_error.is_none() {
                                first_error = Some(err.clone());
                            }
                        }
                    }
                }
                item.refresh_download_totals(on_disk);
                item.dl_speed = 0.0;
            }
        }
        self.mark_dirty().await;

        if let Some(err) = first_error {
            self.fail(id, err.message, err.transient).await;
            return;
        }

        let complete = {
            let s = self.state.lock().await;
            s.item(id).map(|i| i.all_files_done()).unwrap_or(false)
        };
        if complete {
            self.finish_item(id).await;
        }
    }

    /// Download one file: Range-resume into a temp file, then store it as a
    /// chunked vault blob. Returns the blob id.
    async fn download_one(
        &self,
        key: &str,
        torrent_id: i64,
        item_id: &str,
        file: &ItemFile,
    ) -> Result<String, FileError> {
        let url = self
            .client
            .request_dl(key, torrent_id, file.file_id)
            .await
            .map_err(FileError::from_torbox)?;
        tokio::fs::create_dir_all(&self.tmp_dir)
            .await
            .map_err(|e| {
                FileError::transient(format!("could not create the download folder: {e}"))
            })?;
        let part = self.part_path(item_id, file.file_id);

        let mut offset = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        // A previous attempt may have finished the bytes but died before the
        // blob was stored. A Range request past the end would answer 416, so
        // trust the complete temp file instead of re-fetching it.
        if file.size > 0 && offset == file.size {
            return self.store_part(&part).await;
        }
        // A temp file longer than the file itself is corrupt; start over.
        if file.size > 0 && offset > file.size {
            let _ = tokio::fs::remove_file(&part).await;
            offset = 0;
        }

        let mut restarts = 0u32;
        loop {
            let file_handle = tokio::fs::OpenOptions::new()
                .create(true)
                .append(offset > 0)
                .write(true)
                .truncate(offset == 0)
                .open(&part)
                .await
                .map_err(|e| {
                    FileError::transient(format!("could not open the download file: {e}"))
                })?;
            let std_file = file_handle.into_std().await;
            match self.client.fetch_to(&url, offset, std_file).await {
                Ok(written) => {
                    offset += written;
                    break;
                }
                Err(TorboxError::Api { code, .. }) if code == "RANGE_UNSUPPORTED" => {
                    // The CDN ignored our Range and restarted from zero, so the
                    // partial must be dropped. Allow it twice, then stop: a
                    // server that never honours Range would otherwise loop
                    // forever, wiping and re-downloading the file each time.
                    restarts += 1;
                    if restarts > 2 {
                        return Err(FileError::permanent(format!(
                            "the download server refused to resume {}; it could not be fetched in one pass",
                            file.name
                        )));
                    }
                    let _ = tokio::fs::remove_file(&part).await;
                    offset = 0;
                    continue;
                }
                Err(e) => return Err(FileError::from_torbox(e)),
            }
        }

        if file.size > 0 && offset != file.size {
            return Err(FileError::transient(format!(
                "download of {} stopped early ({} of {} bytes)",
                file.name, offset, file.size
            )));
        }

        self.store_part(&part).await
    }

    /// Encrypt a completed temp file into the vault as a chunked blob, then
    /// drop the plaintext temp file. Returns the blob id.
    async fn store_part(&self, part: &Path) -> Result<String, FileError> {
        let blob = match self.worker.blob_store_file(&part.to_path_buf()).await {
            WorkResult::BlobId(id) => id,
            WorkResult::Err(m) => return Err(FileError::transient(m)),
            _ => {
                return Err(FileError::transient(
                    "could not store the file in the vault",
                ))
            }
        };
        let _ = tokio::fs::remove_file(part).await;
        Ok(blob)
    }

    /// Bytes already on disk for an item's not-yet-stored files.
    ///
    /// The honest progress figure: a temp file keeps whatever an earlier
    /// attempt (or a crash) downloaded, so reading it back means a resumed
    /// transfer reports where it actually is instead of restarting the bar at
    /// 0%. Cheap enough at one call per second.
    async fn partial_bytes(&self, item_id: &str) -> u64 {
        let file_ids: Vec<i64> = {
            let s = self.state.lock().await;
            match s.item(item_id) {
                Some(i) => i
                    .files
                    .iter()
                    .filter(|f| !f.done)
                    .map(|f| f.file_id)
                    .collect(),
                None => return 0,
            }
        };
        let mut total = 0u64;
        for fid in file_ids {
            if let Ok(m) = tokio::fs::metadata(self.part_path(item_id, fid)).await {
                total += m.len();
            }
        }
        total
    }

    /// Spawn the 1s progress reporter that keeps `dl_done`/`dl_speed` live while
    /// files download. Returns the join handle; the caller flips `stop` and
    /// awaits it.
    fn spawn_progress_reporter(
        self: &Arc<Self>,
        item_id: String,
        stop: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut last_bytes = this.partial_bytes(&item_id).await;
            loop {
                tokio::time::sleep(PROGRESS_TICK).await;
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let on_disk = this.partial_bytes(&item_id).await;
                let speed =
                    (on_disk.saturating_sub(last_bytes)) as f64 / PROGRESS_TICK.as_secs_f64();
                last_bytes = on_disk;
                {
                    let mut s = this.state.lock().await;
                    if let Some(item) = s.item_mut(&item_id) {
                        item.refresh_download_totals(on_disk);
                        item.dl_speed = speed;
                    }
                }
                this.mark_dirty().await;
                let _ = this.persist().await;
            }
        })
    }

    /// Move a finished item into the Library.
    async fn finish_item(&self, id: &str) {
        {
            let mut s = self.state.lock().await;
            if let Some(item) = s.item_mut(id) {
                item.status = ItemStatus::Completed;
                item.dl_done = item.dl_total;
                item.dl_speed = 0.0;
            }
            s.move_to_library(id);
        }
        self.mark_dirty().await;
        let _ = self.persist().await;
    }

    /// Park an item in `Error`, or back off and retry when the cause is
    /// transient (network blip, rate limit).
    async fn fail(&self, id: &str, message: String, transient: bool) {
        let mut s = self.state.lock().await;
        if let Some(item) = s.item_mut(id) {
            item.retry = item.retry.saturating_add(1);
            item.message = Some(message);
            if transient {
                let backoff = (5i64 << item.retry.min(6)).min(MAX_BACKOFF_SECS);
                item.next_attempt_at = now_secs() + backoff;
                // Stay in a runnable state so the pipeline retries it.
                if item.status == ItemStatus::Error {
                    item.status = ItemStatus::Queued;
                }
            } else {
                item.status = ItemStatus::Error;
            }
        }
        drop(s);
        self.mark_dirty().await;
        let _ = self.persist().await;
    }

    fn part_path(&self, item_id: &str, file_id: i64) -> PathBuf {
        self.tmp_dir.join(format!("{item_id}-{file_id}.part"))
    }
}

/// Map a TorBox file entry to our item file, choosing the in-torrent path.
fn item_file_from_torbox(f: &torbox::TorrentFile) -> ItemFile {
    let path = relative_path(f);
    let name = if f.short_name.is_empty() {
        path.rsplit('/').next().unwrap_or(&path).to_string()
    } else {
        f.short_name.clone()
    };
    ItemFile {
        file_id: f.id,
        name,
        path,
        size: f.size,
        mimetype: f.mimetype.clone(),
        blob_id: None,
        done: false,
    }
}

/// The path of a file within its torrent, with any leading separators removed.
/// TorBox reports multi-file torrents as `Folder/file.ext` in `name`.
fn relative_path(f: &torbox::TorrentFile) -> String {
    let raw = if f.name.is_empty() {
        f.short_name.clone()
    } else {
        f.name.clone()
    };
    raw.trim_start_matches(['/', '\\']).to_string()
}

/// The info-hash shape a torrent offer may carry: hex or base32, never a
/// string that could inject another magnet parameter.
fn valid_offer_hash(hash: &str) -> bool {
    (32..=64).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Turn a stored info hash and display name into the magnet the pipeline adds.
/// Only the hash and `dn` are carried: no tracker list from a peer is trusted.
fn magnet_for(name: &str, hash: &str) -> String {
    let dn: String = url::form_urlencoded::byte_serialize(name.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("magnet:?xt=urn:btih:{hash}&dn={dn}")
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.decode(s).map_err(|_| ())
}

/// Decode the few percent-escapes that show up in a magnet's `dn` (spaces,
/// mostly) without pulling in a URL crate for one field.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_prefers_name_and_strips_separators() {
        let f = torbox::TorrentFile {
            id: 1,
            name: "Folder/file.mkv".into(),
            short_name: "file.mkv".into(),
            ..Default::default()
        };
        assert_eq!(relative_path(&f), "Folder/file.mkv");

        let lead = torbox::TorrentFile {
            id: 2,
            name: "/leading/slash".into(),
            short_name: "slash".into(),
            ..Default::default()
        };
        assert_eq!(relative_path(&lead), "leading/slash");

        let only_short = torbox::TorrentFile {
            id: 3,
            name: String::new(),
            short_name: "just.srt".into(),
            ..Default::default()
        };
        assert_eq!(relative_path(&only_short), "just.srt");
    }

    #[test]
    fn item_file_uses_short_name_and_keeps_the_path() {
        let f = torbox::TorrentFile {
            id: 9,
            name: "S01/E01.mkv".into(),
            short_name: "E01.mkv".into(),
            size: 123,
            mimetype: Some("video/x-matroska".into()),
            absolute_path: None,
        };
        let item = item_file_from_torbox(&f);
        assert_eq!(item.name, "E01.mkv");
        assert_eq!(item.path, "S01/E01.mkv");
        assert_eq!(item.size, 123);
        assert!(!item.done);
    }

    #[test]
    fn a_magnet_carries_only_the_hash_and_encoded_name() {
        assert_eq!(
            magnet_for("A film", &"ab".repeat(20)),
            format!("magnet:?xt=urn:btih:{}&dn=A%20film", "ab".repeat(20))
        );
        assert_eq!(
            magnet_for("a&b?c", &"cd".repeat(20)),
            format!("magnet:?xt=urn:btih:{}&dn=a%26b%3Fc", "cd".repeat(20))
        );
    }

    #[test]
    fn offer_hashes_reject_injection_and_wrong_lengths() {
        assert!(valid_offer_hash(&"ab".repeat(20)));
        assert!(valid_offer_hash(&"a".repeat(64)));
        assert!(!valid_offer_hash("short"));
        assert!(!valid_offer_hash(&"a&b".repeat(20)));
    }

    #[test]
    fn percent_decode_handles_spaces_and_plain_text() {
        assert_eq!(percent_decode("My%20Torrent"), "My Torrent");
        assert_eq!(percent_decode("Plain"), "Plain");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
    }
}
