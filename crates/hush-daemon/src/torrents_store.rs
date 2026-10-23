//! The Torrents pane's state: the TorBox API key, the active queue, and the
//! library of everything already pulled into the vault.
//!
//! Encrypted at rest in a vault sidecar (`torrents-state.enc`, see
//! [`Vault::write_encrypted_file`](hush_core::Vault)) — the API key and every
//! torrent/file name are user secrets, so nothing here is plaintext on disk or
//! in the webview. The JSON schema in this module is also what the UI receives
//! from `/api/torrents/state`, minus the API key, which never leaves the daemon.
//!
//! The pipeline in [`crate::torrents`] drives the state machine; this module is
//! just the durable shape and its transitions.

use serde::{Deserialize, Serialize};

/// The vault sidecar file name.
pub const STATE_FILE: &str = "torrents-state.enc";

/// Default number of files downloaded from TorBox concurrently. Configurable
/// from Settings; a low default keeps a slow CDN link from starving everything.
pub const DEFAULT_MAX_CONCURRENT: u32 = 2;
const MAX_CONCURRENT_BOUND: u32 = 8;

/// How many incoming offers the sidecar keeps. Pending offers are kept longest;
/// a resolved one is the first to go when the cap is reached.
pub const MAX_TORRENT_OFFERS: usize = 64;

/// Where an item's data comes from. The bytes/magnet/URL are held only until
/// TorBox accepts the torrent, then dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Magnet,
    /// An HTTP(S) URL to a `.torrent` file; TorBox fetches it for us.
    Link,
    File,
}

/// The pipeline state of one queued torrent.
///
/// `Queued → Sending → Caching → Ready → Downloading → Completed`, with
/// `Paused` and `Error` as the two exits that stay in the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Queued,
    Sending,
    Caching,
    Ready,
    Downloading,
    Completed,
    Paused,
    Error,
}

impl ItemStatus {
    /// Whether this item still needs the pipeline's attention.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ItemStatus::Queued
                | ItemStatus::Sending
                | ItemStatus::Caching
                | ItemStatus::Ready
                | ItemStatus::Downloading
        )
    }
}

/// One file inside a torrent. `path` is the file's path within the torrent and
/// may contain `/`; the Library nests on it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemFile {
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub mimetype: Option<String>,
    /// The vault blob id once this file has been stored.
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub done: bool,
}

/// One queued or in-progress torrent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorrentItem {
    /// Local id (uuid); stable across restarts and independent of TorBox.
    pub id: String,
    pub name: String,
    pub source: SourceKind,
    /// The magnet string or base64 `.torrent` bytes, until TorBox accepts it.
    /// Never logged. Dropped as soon as `torrent_id` is known.
    #[serde(default)]
    pub source_data: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub torrent_id: Option<i64>,
    pub status: ItemStatus,
    /// TorBox caching progress, 0..1.
    #[serde(default)]
    pub progress: f64,
    /// TorBox's own download speed, bytes/s.
    #[serde(default)]
    pub speed: f64,
    #[serde(default)]
    pub seeds: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub cached: bool,
    /// TorBox's own `download_state` for this torrent, verbatim ("queued",
    /// "downloading", …). Shown while Caching so a stalled item says why —
    /// a slot queue on TorBox's side looks identical to a hang otherwise.
    #[serde(default)]
    pub remote_state: Option<String>,
    /// A human sentence when `status == Error`.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub files: Vec<ItemFile>,
    /// Aggregate vault-download progress (sum over files).
    #[serde(default)]
    pub dl_done: u64,
    #[serde(default)]
    pub dl_total: u64,
    #[serde(default)]
    pub dl_speed: f64,
    pub added_at: i64,
    /// Consecutive failures, for backoff and the UI's retry hint.
    #[serde(default)]
    pub retry: u32,
    /// Unix seconds before which the pipeline should not retry this item.
    #[serde(default)]
    pub next_attempt_at: i64,
}

impl TorrentItem {
    pub fn new(id: String, name: String, source: SourceKind) -> Self {
        Self {
            id,
            name,
            source,
            source_data: None,
            hash: None,
            torrent_id: None,
            status: ItemStatus::Queued,
            progress: 0.0,
            speed: 0.0,
            seeds: 0,
            size: 0,
            cached: false,
            remote_state: None,
            message: None,
            files: Vec::new(),
            dl_done: 0,
            dl_total: 0,
            dl_speed: 0.0,
            added_at: now_secs(),
            retry: 0,
            next_attempt_at: 0,
        }
    }

    /// Whether the pipeline may act on this item right now (backoff elapsed).
    pub fn is_due(&self, now: i64) -> bool {
        self.status.is_active() && now >= self.next_attempt_at
    }

    /// A copy safe to hand to the UI: the magnet/URL/`.torrent` bytes in
    /// `source_data` are a user secret and never leave the daemon, so they are
    /// dropped here. Everything the UI renders is preserved.
    pub fn public_clone(&self) -> Self {
        let mut copy = self.clone();
        copy.source_data = None;
        copy
    }

    /// Files still to fetch from TorBox.
    pub fn pending_files(&self) -> impl Iterator<Item = &ItemFile> {
        self.files.iter().filter(|f| !f.done)
    }

    pub fn all_files_done(&self) -> bool {
        !self.files.is_empty() && self.files.iter().all(|f| f.done)
    }

    /// Recompute the aggregate download counters from the per-file sizes and
    /// the bytes already on disk for the files that are not done yet (their
    /// temp files). Counting from disk means a resumed transfer reports its
    /// real position instead of restarting the bar at 0%.
    pub fn refresh_download_totals(&mut self, on_disk: u64) {
        let done_bytes: u64 = self.files.iter().filter(|f| f.done).map(|f| f.size).sum();
        self.dl_total = self.files.iter().map(|f| f.size).sum();
        self.dl_done = (done_bytes + on_disk).min(self.dl_total);
    }
}

/// Where an incoming torrent offer stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Pending,
    Accepted,
    Declined,
}

impl Default for OfferStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// One torrent a trustee has offered. The content is not here: the hash is the
/// pointer, and accepting adds a normal magnet item to the pipeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorrentOffer {
    pub offer_id: String,
    pub from_nick: String,
    /// The sender's account id, so a response survives a local nickname change.
    pub from_account: String,
    pub name: String,
    pub hash: String,
    #[serde(default)]
    pub size: u64,
    /// Unix seconds, like the rest of the torrents sidecar.
    pub received_at: i64,
    #[serde(default)]
    pub status: OfferStatus,
    /// The queue item created when the offer was accepted.
    #[serde(default)]
    pub item_id: Option<String>,
}

/// One completed torrent in the Library. Its files are real vault blobs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub hash: Option<String>,
    pub size: u64,
    pub added_at: i64,
    pub files: Vec<ItemFile>,
}

/// The whole persisted state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TorrentsState {
    /// The TorBox API key. Never serialized to the UI.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Concurrent vault downloads. `0` in a hand-edited file is repaired to the
    /// default on decode.
    #[serde(default)]
    pub max_concurrent: u32,
    #[serde(default)]
    pub items: Vec<TorrentItem>,
    #[serde(default)]
    pub library: Vec<LibraryEntry>,
    /// Incoming torrent offers, newest first. Bodies are held here because the
    /// signal audit log deliberately stores metadata only.
    #[serde(default)]
    pub offers: Vec<TorrentOffer>,
}

impl TorrentsState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("torrents state unwritable: {e}"))
    }

    pub fn concurrency(&self) -> u32 {
        if self.max_concurrent == 0 {
            DEFAULT_MAX_CONCURRENT
        } else {
            self.max_concurrent.min(MAX_CONCURRENT_BOUND)
        }
    }

    pub fn set_concurrency(&mut self, n: u32) {
        self.max_concurrent = n.clamp(1, MAX_CONCURRENT_BOUND);
    }

    pub fn item(&self, id: &str) -> Option<&TorrentItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Whether a queued or library torrent still points at this vault blob.
    ///
    /// The torrents sidecar is the only other place a blob id is kept outside
    /// the chat log, so a caller deleting a blob asks here too.
    pub fn references_blob(&self, blob_id: &str) -> bool {
        let same = |id: Option<&str>| id.is_some_and(|id| id.eq_ignore_ascii_case(blob_id));
        self.items
            .iter()
            .flat_map(|i| i.files.iter())
            .chain(self.library.iter().flat_map(|e| e.files.iter()))
            .any(|f| same(f.blob_id.as_deref()))
    }

    pub fn item_mut(&mut self, id: &str) -> Option<&mut TorrentItem> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn offer(&self, id: &str) -> Option<&TorrentOffer> {
        self.offers.iter().find(|o| o.offer_id == id)
    }

    pub fn offer_mut(&mut self, id: &str) -> Option<&mut TorrentOffer> {
        self.offers.iter_mut().find(|o| o.offer_id == id)
    }

    /// The queue item or library entry that already holds this torrent.
    ///
    /// A torrent is content-addressed: the same info hash is the same bytes.
    /// Accepting it twice must not download it twice, so this is what makes a
    /// duplicate offer, or a second answer to the same offer, a no-op.
    pub fn id_for_hash(&self, hash: &str) -> Option<&str> {
        let same = |h: Option<&str>| h.is_some_and(|h| h.eq_ignore_ascii_case(hash));
        self.items
            .iter()
            .find(|i| same(i.hash.as_deref()))
            .map(|i| i.id.as_str())
            .or_else(|| {
                self.library
                    .iter()
                    .find(|e| same(e.hash.as_deref()))
                    .map(|e| e.id.as_str())
            })
    }

    /// Store an offer once. A repeated id is a no-op, like the signal layer's
    /// duplicate suppression. So is a second *pending* offer from the same
    /// sender for the same torrent: sharing the same torrent twice (a second
    /// click on Share, a retried signal) must not present the recipient with
    /// two popups for one set of bytes, both of which would download it.
    pub fn add_offer(&mut self, offer: TorrentOffer) -> bool {
        if self.offer(&offer.offer_id).is_some() {
            return false;
        }
        let already_pending = self.offers.iter().any(|o| {
            o.status == OfferStatus::Pending
                && o.from_account == offer.from_account
                && o.hash.eq_ignore_ascii_case(&offer.hash)
        });
        if already_pending {
            return false;
        }
        self.offers.insert(0, offer);
        while self.offers.len() > MAX_TORRENT_OFFERS {
            let resolved = self
                .offers
                .iter()
                .rposition(|o| o.status != OfferStatus::Pending);
            if let Some(i) = resolved {
                self.offers.remove(i);
            } else {
                self.offers.pop();
            }
        }
        true
    }

    /// Whether any item still needs the pipeline.
    pub fn has_active(&self) -> bool {
        self.items.iter().any(|i| i.status.is_active())
    }

    /// Move a completed item into the Library, newest first. No-op if the item
    /// is missing or already filed.
    pub fn move_to_library(&mut self, id: &str) {
        let Some(item) = self.item(id) else { return };
        if self.library.iter().any(|e| e.id == id) {
            self.items.retain(|i| i.id != id);
            return;
        }
        let entry = LibraryEntry {
            id: item.id.clone(),
            name: item.name.clone(),
            hash: item.hash.clone(),
            size: item.size,
            added_at: now_secs(),
            files: item
                .files
                .iter()
                .filter(|f| f.blob_id.is_some())
                .cloned()
                .collect(),
        };
        self.items.retain(|i| i.id != id);
        self.library.insert(0, entry);
    }

    /// Remove an active item. Does not touch TorBox or the vault (the caller
    /// decides whether to delete the TorBox torrent).
    pub fn remove_item(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() != before
    }

    /// Remove a Library entry and return the blob ids it owned, so the caller
    /// can delete them from the vault.
    pub fn remove_library_entry(&mut self, id: &str) -> Vec<String> {
        let Some(pos) = self.library.iter().position(|e| e.id == id) else {
            return Vec::new();
        };
        let entry = self.library.remove(pos);
        entry.files.into_iter().filter_map(|f| f.blob_id).collect()
    }
}

/// Decode sidecar bytes; `None` (never saved) is the empty default. A corrupt
/// sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<TorrentsState, String> {
    match bytes {
        None | Some([]) => Ok(TorrentsState::default()),
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("torrents state unreadable: {e}")),
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: ItemStatus) -> TorrentItem {
        let mut i = TorrentItem::new(id.to_string(), format!("Torrent {id}"), SourceKind::Magnet);
        i.status = status;
        i
    }

    #[test]
    fn state_roundtrips_through_bytes_with_the_key() {
        let mut state = TorrentsState::default();
        state.api_key = Some("secret-key".into());
        state.set_concurrency(4);
        state.items.push(item("a", ItemStatus::Caching));
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.api_key.as_deref(), Some("secret-key"));
        assert_eq!(back.concurrency(), 4);
        assert_eq!(back.items.len(), 1);
        assert_eq!(back.items[0].status, ItemStatus::Caching);
    }

    #[test]
    fn missing_and_empty_decode_to_default_and_corrupt_is_an_error() {
        assert!(decode(None).unwrap().items.is_empty());
        assert!(decode(Some(b"")).unwrap().library.is_empty());
        assert!(decode(Some(b"not json")).is_err());
    }

    #[test]
    fn concurrency_defaults_and_clamps() {
        let mut s = TorrentsState::default();
        assert_eq!(s.concurrency(), DEFAULT_MAX_CONCURRENT);
        s.set_concurrency(0);
        assert_eq!(s.max_concurrent, 1);
        s.set_concurrency(99);
        assert_eq!(s.concurrency(), MAX_CONCURRENT_BOUND);
    }

    #[test]
    fn active_states_are_exactly_the_in_progress_ones() {
        for s in [
            ItemStatus::Queued,
            ItemStatus::Sending,
            ItemStatus::Caching,
            ItemStatus::Ready,
            ItemStatus::Downloading,
        ] {
            assert!(s.is_active(), "{s:?} should be active");
        }
        for s in [ItemStatus::Completed, ItemStatus::Paused, ItemStatus::Error] {
            assert!(!s.is_active(), "{s:?} should be inactive");
        }
    }

    #[test]
    fn move_to_library_files_only_stored_files_and_is_idempotent() {
        let mut state = TorrentsState::default();
        let mut i = item("a", ItemStatus::Downloading);
        i.size = 100;
        i.files = vec![
            ItemFile {
                file_id: 1,
                name: "one.mkv".into(),
                path: "one.mkv".into(),
                size: 60,
                mimetype: None,
                blob_id: Some("blob1".into()),
                done: true,
            },
            ItemFile {
                file_id: 2,
                name: "two.srt".into(),
                path: "subs/two.srt".into(),
                size: 40,
                mimetype: None,
                blob_id: None,
                done: false,
            },
        ];
        state.items.push(i);
        state.move_to_library("a");
        assert!(state.items.is_empty());
        assert_eq!(state.library.len(), 1);
        assert_eq!(
            state.library[0].files.len(),
            1,
            "unstored file is not filed"
        );
        assert_eq!(state.library[0].files[0].path, "one.mkv");

        // Calling again is a harmless no-op.
        state.move_to_library("a");
        assert_eq!(state.library.len(), 1);
    }

    /// A distinct info hash per offer id, so fixtures are not collapsed by the
    /// same-hash pending rule unless a test wants them to be.
    fn hash_for(id: &str) -> String {
        let mut hash: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        hash.truncate(40);
        format!("{hash:0>40}")
    }

    fn offer(id: &str, status: OfferStatus) -> TorrentOffer {
        TorrentOffer {
            offer_id: id.into(),
            from_nick: "alice".into(),
            from_account: "a".into(),
            name: format!("Torrent {id}"),
            hash: hash_for(id),
            size: 100,
            received_at: 1,
            status,
            item_id: None,
        }
    }

    #[test]
    fn offers_roundtrip_and_are_deduped() {
        let mut state = TorrentsState::default();
        assert!(state.add_offer(offer("o1", OfferStatus::Pending)));
        assert!(!state.add_offer(offer("o1", OfferStatus::Declined)));
        assert_eq!(state.offers.len(), 1);
        assert_eq!(state.offers[0].status, OfferStatus::Pending);
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.offers.len(), 1);
        assert_eq!(back.offers[0].hash, hash_for("o1"));
    }

    #[test]
    fn a_second_pending_offer_for_the_same_hash_is_ignored() {
        let mut state = TorrentsState::default();
        assert!(state.add_offer(offer("o1", OfferStatus::Pending)));
        // Same sender, same bytes, a new offer id: the second Share click.
        let mut second = offer("o2", OfferStatus::Pending);
        second.hash = hash_for("o1");
        assert!(!state.add_offer(second));
        assert_eq!(state.offers.len(), 1);
        assert!(state.offer("o2").is_none());

        // A different sender is a different offer, even for the same hash.
        let mut other = offer("o3", OfferStatus::Pending);
        other.from_account = "b".into();
        other.from_nick = "bob".into();
        other.hash = hash_for("o1");
        assert!(state.add_offer(other));
        assert_eq!(state.offers.len(), 2);
    }

    #[test]
    fn a_resolved_offer_does_not_block_a_fresh_one() {
        let mut state = TorrentsState::default();
        assert!(state.add_offer(offer("o1", OfferStatus::Declined)));
        // Answering an offer and then sharing again is a new intent.
        let mut again = offer("o2", OfferStatus::Pending);
        again.hash = hash_for("o1");
        assert!(state.add_offer(again));
        assert_eq!(state.offers.len(), 2);
    }

    #[test]
    fn id_for_hash_finds_a_queued_or_filed_torrent() {
        let hash = "ab".repeat(20);
        let mut state = TorrentsState::default();
        assert_eq!(state.id_for_hash(&hash), None);

        let mut item = TorrentItem::new("queued".into(), "One".into(), SourceKind::Magnet);
        item.hash = Some(hash.clone());
        state.items.push(item);
        assert_eq!(
            state.id_for_hash(&hash.to_ascii_uppercase()),
            Some("queued")
        );

        state.library.push(LibraryEntry {
            id: "filed".into(),
            name: "Two".into(),
            hash: Some("cd".repeat(20)),
            size: 1,
            added_at: 0,
            files: Vec::new(),
        });
        assert_eq!(state.id_for_hash(&"cd".repeat(20)), Some("filed"));
        assert_eq!(state.id_for_hash(&"ef".repeat(20)), None);
    }

    #[test]
    fn the_offer_cap_drops_resolved_before_pending() {
        let mut state = TorrentsState::default();
        for i in 0..MAX_TORRENT_OFFERS {
            state.add_offer(offer(&format!("o{i}"), OfferStatus::Pending));
        }
        // The oldest offer is resolved, so it is the one the cap should evict.
        let oldest = state.offers.last_mut().unwrap();
        oldest.status = OfferStatus::Declined;
        let resolved_id = oldest.offer_id.clone();
        state.add_offer(offer("newest", OfferStatus::Pending));
        assert_eq!(state.offers.len(), MAX_TORRENT_OFFERS);
        assert!(state.offer("newest").is_some());
        assert!(state.offer(&resolved_id).is_none());
        assert_eq!(
            state
                .offers
                .iter()
                .filter(|o| o.status == OfferStatus::Pending)
                .count(),
            MAX_TORRENT_OFFERS
        );
    }

    #[test]
    fn remove_library_entry_returns_blob_ids() {
        let mut state = TorrentsState::default();
        state.library.push(LibraryEntry {
            id: "x".into(),
            name: "X".into(),
            hash: None,
            size: 10,
            added_at: 0,
            files: vec![
                ItemFile {
                    file_id: 1,
                    name: "a".into(),
                    path: "a".into(),
                    size: 10,
                    mimetype: None,
                    blob_id: Some("blob-a".into()),
                    done: true,
                },
                ItemFile {
                    file_id: 2,
                    name: "b".into(),
                    path: "b".into(),
                    size: 0,
                    mimetype: None,
                    blob_id: None,
                    done: true,
                },
            ],
        });
        let blobs = state.remove_library_entry("x");
        assert_eq!(blobs, vec!["blob-a".to_string()]);
        assert!(state.library.is_empty());
        assert!(state.remove_library_entry("x").is_empty());
    }

    #[test]
    fn public_clone_drops_the_source_secret_but_keeps_the_rest() {
        let mut i = item("a", ItemStatus::Queued);
        i.source_data = Some("magnet:?xt=urn:btih:secret".into());
        i.size = 1234;
        let public = i.public_clone();
        assert!(
            public.source_data.is_none(),
            "the magnet must not reach the UI"
        );
        assert_eq!(public.id, "a");
        assert_eq!(public.size, 1234);
        // The original still holds it, so a restart can resume the add.
        assert!(i.source_data.is_some());
    }

    #[test]
    fn download_totals_sum_done_files_plus_bytes_on_disk() {
        let mut i = item("a", ItemStatus::Downloading);
        i.files = vec![
            ItemFile {
                file_id: 1,
                name: "a".into(),
                path: "a".into(),
                size: 100,
                mimetype: None,
                blob_id: Some("b".into()),
                done: true,
            },
            ItemFile {
                file_id: 2,
                name: "b".into(),
                path: "b".into(),
                size: 50,
                mimetype: None,
                blob_id: None,
                done: false,
            },
        ];
        i.refresh_download_totals(20);
        assert_eq!(i.dl_total, 150);
        assert_eq!(i.dl_done, 120);
        assert!(!i.all_files_done());
        i.files[1].done = true;
        assert!(i.all_files_done());
    }
}
