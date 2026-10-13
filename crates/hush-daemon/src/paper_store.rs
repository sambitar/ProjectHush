//! Conclave Paper — the encrypted sidecar that holds the conclave's shared
//! collaborative document.
//!
//! One paper per conclave, creatable by any joined trustee (Phase 1 rule; see
//! [`docs/tasks/conclave-paper.md`](../../docs/tasks/conclave-paper.md)). The
//! document bytes are **opaque `yrs` CRDT state**: this module persists and
//! orders them, it never interprets them — the merge lives in
//! [`crate::paper_doc`]. Everything here is a user secret, so the sidecar
//! (`papers.enc`, via
//! [`Vault::write_encrypted_file`](hush_core::Vault)) is ciphertext on disk,
//! and a locked vault answers 401 before these types are ever touched.
//!
//! Phase 1 is offline: `origin` on an update is always the local account. The
//! wire fan-out (Phase 2) reuses exactly this log — each trustee's sidecar
//! converges because the CRDT merges, not because anyone is authoritative.

use serde::{Deserialize, Serialize};

/// The vault sidecar file name. Fixed here so the routes and any future
/// migration agree on one name.
pub const STATE_FILE: &str = "papers.enc";

/// Sidecar bound: one paper per conclave, and a vault does not hold unboundedly
/// many conclaves' papers. A create over the cap is refused with a reason,
/// never silently dropped.
const PAPERS_CAP: usize = 64;

/// Soft cap on one paper's document payload (snapshot + pending log updates +
/// the update being applied). Refusing at the boundary keeps a runaway editor
/// from bloating the sidecar; the doc itself is the thing users see and can
/// start a fresh section in a new conclave instead.
const DOC_SOFT_CAP: usize = 256 * 1024;

/// Pending-update entries before compaction is mandatory. The daemon compacts
/// ([`crate::paper_doc`]) well before this; the cap is the backstop.
const LOG_CAP: usize = 2048;

/// Who made a paper and where its live channel rendezvous is. `hub` exists
/// from Phase 1 so the schema never migrates: it is always the creator here,
/// and the convener-authoritative succession story lands in Phase 3+.
///
/// Defined once in `hush_core::payload` because the very same descriptor rides
/// the wire (`PaperState`, `PaperHub`) as well as this sidecar; re-exported so
/// the daemon's paper surface names one type instead of a drifting copy.
pub use hush_core::{PaperDescriptor, DEFAULT_PAPER_TITLE};

/// One CRDT update in the paper's local log. `origin` is the authoring
/// account id (the local account until fan-out lands); `update` is opaque
/// `yrs` state the store never parses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PaperUpdate {
    /// Local sequence number, gapless from 1. `next_rev` survives compaction,
    /// so revisions never restart or collide.
    pub rev: u64,
    pub origin: String,
    pub update: Vec<u8>,
    pub at_ms: u64,
}

/// One conclave's paper: its descriptor, the last compacted CRDT snapshot, and
/// the updates applied since that snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PaperFile {
    pub descriptor: PaperDescriptor,
    #[serde(default)]
    pub snapshot: Vec<u8>,
    #[serde(default)]
    pub log: Vec<PaperUpdate>,
    /// The next revision number `apply_update` will hand out.
    #[serde(default)]
    pub next_rev: u64,
    #[serde(default)]
    pub updated_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PapersState {
    #[serde(default)]
    pub papers: Vec<PaperFile>,
}

/// Decode sidecar bytes; `None` (never saved) decodes to the empty default.
/// A corrupt sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<PapersState, String> {
    match bytes {
        None | Some([]) => Ok(PapersState::default()),
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("papers state unreadable: {e}")),
    }
}

impl PaperFile {
    /// Total document payload this paper carries (compacted snapshot plus
    /// everything not yet compacted). The soft cap is measured against this.
    fn doc_bytes(&self, incoming: &[u8]) -> usize {
        self.snapshot.len() + self.log.iter().map(|u| u.update.len()).sum::<usize>() + incoming.len()
    }
}

/// Which of two descriptors for one conclave wins a create collision.
///
/// The rule has to be total and symmetric: both sides see the same two
/// descriptors and must pick the same winner without another round trip. The
/// earlier `created_ms` wins, which is the "first create wins" the UI promises;
/// equal timestamps — a clock coarse enough to tie, or a replayed descriptor —
/// fall back to the smaller id so the choice is still deterministic.
fn adopt_wins(incoming: &PaperDescriptor, held: &PaperDescriptor) -> bool {
    (incoming.created_ms, &incoming.paper_id) < (held.created_ms, &held.paper_id)
}

impl PapersState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("papers state unwritable: {e}"))
    }

    /// The paper a conclave holds, if it has one.
    pub fn get(&self, conclave_id: &str) -> Option<&PaperFile> {
        self.papers
            .iter()
            .find(|p| p.descriptor.conclave_id == conclave_id)
    }

    /// The conclave a paper id belongs to.
    ///
    /// Inbound deltas and snapshots carry only the paper id, because the id is
    /// what the document is named by everywhere; this is the one lookup back to
    /// the conclave the sidecar is keyed on.
    pub fn conclave_of(&self, paper_id: &str) -> Option<String> {
        self.papers
            .iter()
            .find(|p| p.descriptor.paper_id == paper_id)
            .map(|p| p.descriptor.conclave_id.clone())
    }

    /// Materialise a paper a peer created, from the descriptor it sent.
    ///
    /// The caller has already checked that the sender is the creator the
    /// descriptor names — this is the storage half of that decision. Returns
    /// whether the paper was new to this vault; a descriptor for a paper it
    /// already holds only refreshes the title and hub, so a re-send or a hub
    /// move is idempotent and never touches local content.
    ///
    /// **A vault holds one paper per conclave**, so the lookup is on the
    /// conclave and not on the paper id. Two trustees can each click "start the
    /// paper" before either has heard from the other, and `create` cannot refuse
    /// a second paper it has never seen; keying on the id here would then leave
    /// two entries for one conclave, each naming a different hub, so neither
    /// side would dial and the fast path would be dead for the life of the
    /// paper — while the deltas still crossed over, because the fan-out is by
    /// conclave, so nothing would look wrong. The collision is settled the way
    /// the UI promises it ("the first create wins and everyone else adopts
    /// it"): the earlier `created_ms` wins, ties go to the smaller id, and the
    /// loser takes the winner's identity while keeping the document it already
    /// has. The rule is total and needs no extra round trip, so both sides pick
    /// the same winner from the same two descriptors.
    pub fn adopt(&mut self, descriptor: PaperDescriptor, now_ms: u64) -> Result<bool, String> {
        if descriptor.conclave_id.is_empty() || descriptor.paper_id.is_empty() {
            return Err("a paper needs a conclave and an id".to_string());
        }
        if let Some(existing) = self
            .papers
            .iter_mut()
            .find(|p| p.descriptor.conclave_id == descriptor.conclave_id)
        {
            if existing.descriptor.paper_id != descriptor.paper_id {
                if !adopt_wins(&descriptor, &existing.descriptor) {
                    // This vault's paper is the earlier one: the claim loses,
                    // and the other side is the one that will move.
                    return Ok(false);
                }
                // Identity moves, content stays. The document is a CRDT keyed
                // by conclave, so nothing about it is tied to the old id, and
                // `true` is what tells the caller to re-announce it under the
                // winning id — the deltas this vault already sent under the old
                // id were dropped by the winner, and the snapshot path is what
                // puts those edits back.
                existing.descriptor.paper_id = descriptor.paper_id;
                existing.descriptor.creator = descriptor.creator;
                existing.descriptor.created_ms = descriptor.created_ms;
                existing.descriptor.title = descriptor.title;
                existing.descriptor.hub = descriptor.hub;
                existing.updated_ms = now_ms;
                return Ok(true);
            }
            existing.descriptor.title = descriptor.title;
            existing.descriptor.hub = descriptor.hub;
            existing.updated_ms = now_ms;
            return Ok(false);
        }
        if self.papers.len() >= PAPERS_CAP {
            return Err(format!("at most {PAPERS_CAP} papers are kept"));
        }
        self.papers.push(PaperFile {
            descriptor,
            snapshot: Vec::new(),
            log: Vec::new(),
            next_rev: 1,
            updated_ms: now_ms,
        });
        Ok(true)
    }

    /// Move a paper's DataChannel rendezvous.
    ///
    /// Only the `hub` field moves. A claim about *where the channel is* is not
    /// a claim about the title, and letting one carry a new title would make
    /// `PaperHub` a way to rename somebody else's paper. The caller has already
    /// checked that the sender is the conclave's convener.
    ///
    /// Returns whether anything moved. A claim about a paper this vault does not
    /// hold changes nothing: a hub is a rendezvous, and there is nothing here to
    /// rendezvous for — papers are created by `PaperState` from their creator,
    /// never by a hub claim.
    pub fn rehome(&mut self, paper_id: &str, hub: &str, now_ms: u64) -> Result<bool, String> {
        let Some(file) = self
            .papers
            .iter_mut()
            .find(|p| p.descriptor.paper_id == paper_id)
        else {
            return Ok(false);
        };
        if file.descriptor.hub == hub {
            return Ok(false);
        }
        file.descriptor.hub = hub.to_string();
        file.updated_ms = now_ms;
        Ok(true)
    }

    fn get_mut(&mut self, conclave_id: &str) -> Option<&mut PaperFile> {
        self.papers
            .iter_mut()
            .find(|p| p.descriptor.conclave_id == conclave_id)
    }

    /// Create the one paper a conclave may hold. Refuses a second create with
    /// a reason (the routes turn that into a 409), and refuses past the
    /// sidecar cap. A blank title becomes the default; the hub is the creator.
    pub fn create(
        &mut self,
        conclave_id: &str,
        creator: &str,
        title: &str,
        now_ms: u64,
    ) -> Result<String, String> {
        if conclave_id.is_empty() || creator.is_empty() {
            return Err("a paper needs a conclave and a creator".to_string());
        }
        if self.get(conclave_id).is_some() {
            return Err("this conclave already has a paper".to_string());
        }
        if self.papers.len() >= PAPERS_CAP {
            return Err(format!("at most {PAPERS_CAP} papers are kept"));
        }
        let paper_id = uuid::Uuid::new_v4().to_string();
        let title = title.trim();
        self.papers.push(PaperFile {
            descriptor: PaperDescriptor {
                paper_id: paper_id.clone(),
                conclave_id: conclave_id.to_string(),
                creator: creator.to_string(),
                hub: creator.to_string(),
                title: if title.is_empty() {
                    DEFAULT_PAPER_TITLE.to_string()
                } else {
                    title.to_string()
                },
                created_ms: now_ms,
            },
            snapshot: Vec::new(),
            log: Vec::new(),
            next_rev: 1,
            updated_ms: now_ms,
        });
        Ok(paper_id)
    }

    /// Append an opaque CRDT update to the paper's log and return its
    /// revision. Refuses an unknown paper, an empty update, and anything that
    /// would push the document payload over the soft cap.
    pub fn apply_update(
        &mut self,
        conclave_id: &str,
        origin: &str,
        update: &[u8],
        now_ms: u64,
    ) -> Result<u64, String> {
        if update.is_empty() {
            return Err("an empty update is not applied".to_string());
        }
        let paper = self
            .get_mut(conclave_id)
            .ok_or_else(|| "no paper in this conclave".to_string())?;
        if paper.doc_bytes(update) > DOC_SOFT_CAP {
            return Err(format!(
                "the document is over the {} KiB cap; start a new paper section or prune",
                DOC_SOFT_CAP / 1024
            ));
        }
        if paper.log.len() >= LOG_CAP {
            return Err(format!(
                "{LOG_CAP} pending updates; compact the document before editing further"
            ));
        }
        let rev = paper.next_rev;
        paper.next_rev += 1;
        paper.log.push(PaperUpdate {
            rev,
            origin: origin.to_string(),
            update: update.to_vec(),
            at_ms: now_ms,
        });
        paper.updated_ms = now_ms;
        Ok(rev)
    }

    /// Replace the snapshot and clear the log (compaction, or a trusted
    /// catch-up merge). Keeps `next_rev`, so later updates still sort after
    /// every revision already handed out.
    pub fn snapshot_replace(
        &mut self,
        conclave_id: &str,
        snapshot: &[u8],
        now_ms: u64,
    ) -> Result<(), String> {
        let paper = self
            .get_mut(conclave_id)
            .ok_or_else(|| "no paper in this conclave".to_string())?;
        let next = PaperFile {
            descriptor: paper.descriptor.clone(),
            snapshot: snapshot.to_vec(),
            log: Vec::new(),
            next_rev: paper.next_rev,
            updated_ms: now_ms,
        };
        if next.doc_bytes(&[]) > DOC_SOFT_CAP {
            return Err(format!(
                "the document is over the {} KiB cap; the snapshot was not replaced",
                DOC_SOFT_CAP / 1024
            ));
        }
        *paper = next;
        Ok(())
    }
}

#[cfg(test)]
mod paper_store_tests {
    use super::*;

    const NOW: u64 = 1_760_000_000_000;
    const CONCLAVE: &str = "c-1111";

    fn state_with_paper() -> (PapersState, String) {
        let mut s = PapersState::default();
        let id = s.create(CONCLAVE, "alice", "", NOW).unwrap();
        (s, id)
    }

    /// The descriptor type moved to `hush_core::payload` so the wire and this
    /// sidecar share one definition. The bytes on disk must not move with it: a
    /// `papers.enc` written by the previous build has to load unchanged, or
    /// every existing paper is lost on upgrade. The literal below is the shape
    /// the previous build wrote.
    #[test]
    fn a_sidecar_written_before_the_descriptor_moved_still_loads() {
        let json = br#"{"papers":[{"descriptor":{"paper_id":"p-1","conclave_id":"c-1",
            "creator":"aa","hub":"aa","title":"Minutes","created_ms":7},
            "snapshot":[0,0],"log":[{"rev":1,"origin":"aa","update":[1,2],"at_ms":9}],
            "next_rev":2,"updated_ms":9}]}"#;
        let state = decode(Some(json)).unwrap();
        let f = &state.papers[0];
        assert_eq!(f.descriptor.title, "Minutes");
        assert_eq!(f.descriptor.created_ms, 7);
        assert_eq!(f.snapshot, vec![0, 0]);
        assert_eq!(f.log[0].rev, 1);
        assert_eq!(f.log[0].update, vec![1, 2]);
        assert_eq!(f.next_rev, 2);
    }

    #[test]
    fn empty_and_missing_sidecars_decode_to_default() {
        let d = decode(None).unwrap();
        assert!(d.papers.is_empty());
        assert!(decode(Some(b"")).unwrap().papers.is_empty());
        assert!(
            decode(Some(b"garbage")).is_err(),
            "corrupt state is an error, not a reset"
        );
    }

    #[test]
    fn create_defaults_title_and_makes_creator_the_hub() {
        let (s, id) = state_with_paper();
        assert!(!id.is_empty());
        let p = s.get(CONCLAVE).unwrap();
        assert_eq!(p.descriptor.title, "Paper");
        assert_eq!(p.descriptor.creator, "alice");
        assert_eq!(p.descriptor.hub, "alice", "Phase 1: hub is the creator");
        assert_eq!(p.descriptor.created_ms, NOW);
        assert_eq!(p.updated_ms, NOW);
        assert!(p.snapshot.is_empty());
        assert!(p.log.is_empty());
    }

    #[test]
    fn a_second_paper_in_a_conclave_is_refused() {
        let mut s = PapersState::default();
        s.create(CONCLAVE, "alice", "", NOW).unwrap();
        let err = s.create(CONCLAVE, "bob", "", NOW).unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert_eq!(s.get(CONCLAVE).unwrap().descriptor.creator, "alice");
    }

    #[test]
    fn create_over_the_cap_is_refused() {
        let mut s = PapersState::default();
        for i in 0..PAPERS_CAP {
            s.create(&format!("c{i}"), "alice", "", NOW).unwrap();
        }
        let err = s.create("c-extra", "alice", "", NOW).unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn updates_get_gapless_revisions_and_stamp_time() {
        let (mut s, _) = state_with_paper();
        let r1 = s.apply_update(CONCLAVE, "alice", b"u1", NOW).unwrap();
        let r2 = s.apply_update(CONCLAVE, "alice", b"u2", NOW + 1).unwrap();
        assert_eq!((r1, r2), (1, 2));
        let p = s.get(CONCLAVE).unwrap();
        assert_eq!(p.log.len(), 2);
        assert_eq!(p.log[0].update, b"u1");
        assert_eq!(p.log[1].at_ms, NOW + 1);
        assert_eq!(p.next_rev, 3);
    }

    #[test]
    fn update_to_a_conclave_without_a_paper_is_refused() {
        let mut s = PapersState::default();
        let err = s.apply_update("ghost", "alice", b"u", NOW).unwrap_err();
        assert!(err.contains("no paper"), "{err}");
    }

    #[test]
    fn an_empty_update_is_refused() {
        let (mut s, _) = state_with_paper();
        let err = s.apply_update(CONCLAVE, "alice", b"", NOW).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn doc_soft_cap_refuses_with_a_reason() {
        let (mut s, _) = state_with_paper();
        // Exactly at the cap is allowed (boundary); one byte over refuses.
        let at_cap = vec![0u8; DOC_SOFT_CAP];
        s.apply_update(CONCLAVE, "alice", &at_cap, NOW)
            .expect("a doc exactly at the cap still fits");
        let err = s
            .apply_update(CONCLAVE, "alice", &[0u8], NOW)
            .unwrap_err();
        assert!(err.contains("cap") || err.contains("large"), "{err}");
    }

    #[test]
    fn log_cap_backstop_refuses_before_compaction_never_runs() {
        let (mut s, _) = state_with_paper();
        for i in 0..LOG_CAP {
            s.apply_update(CONCLAVE, "alice", b"u", NOW + i as u64)
                .unwrap_or_else(|e| panic!("update {i} refused: {e}"));
        }
        let err = s.apply_update(CONCLAVE, "alice", b"u", NOW).unwrap_err();
        assert!(err.contains("pending"), "{err}");
    }

    #[test]
    fn compaction_keeps_revisions_monotonic() {
        let (mut s, _) = state_with_paper();
        for _ in 0..3 {
            s.apply_update(CONCLAVE, "alice", b"u", NOW).unwrap();
        }
        s.snapshot_replace(CONCLAVE, b"compact-snapshot", NOW)
            .unwrap();
        let p = s.get(CONCLAVE).unwrap();
        assert_eq!(p.snapshot, b"compact-snapshot");
        assert!(p.log.is_empty());
        let r = s.apply_update(CONCLAVE, "alice", b"u", NOW).unwrap();
        assert_eq!(r, 4, "revisions never restart after compaction");
    }

    #[test]
    fn state_roundtrips_through_bytes() {
        let (mut s, _) = state_with_paper();
        s.apply_update(CONCLAVE, "alice", b"u1", NOW).unwrap();
        let bytes = s.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        let p = back.get(CONCLAVE).unwrap();
        assert_eq!(p.log.len(), 1);
        assert_eq!(p.log[0].update, b"u1");
        assert_eq!(p.descriptor.hub, "alice");
    }
}
