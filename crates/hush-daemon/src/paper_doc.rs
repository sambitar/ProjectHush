//! Conclave Paper — the daemon's CRDT plumbing.
//!
//! The document the trustees edit is a Yjs doc owned by the **webview's
//! editor** (ProseMirror binding); the daemon keeps the authoritative merged
//! state as opaque `yrs` updates. Yjs updates are type-agnostic at the doc
//! level, so this module never parses document content — it applies update
//! bytes, folds logs into snapshots, and encodes full state for peers that
//! need catch-up. Anything content-shaped (toolbar, cursors, marks) lives in
//! the webview.
//!
//! Merging is CRDT: updates are idempotent and order-free, so the same bytes
//! applied twice are a no-op and two trustees can apply each other's updates
//! in different orders and converge — the property the wire fan-out (Phase 2)
//! and the DataChannel fast path (Phase 3) both rest on.

use yrs::{Doc, ReadTxn, StateVector, Transact, Update};
use yrs::updates::decoder::Decode;

/// A freshly built doc holds nothing; its "full state" is the empty v1 update.
fn empty_state() -> Vec<u8> {
    Update::EMPTY_V1.to_vec()
}

/// Apply one opaque v1 update onto a base state and return the new full
/// state. Errors on bytes that are not a decodable v1 update — corruption is
/// refused, never guessed through.
pub fn apply(base: &[u8], update: &[u8]) -> Result<Vec<u8>, String> {
    let doc = Doc::new();
    if !base.is_empty() {
        let seed = Update::decode_v1(base)
            .map_err(|e| format!("paper state unreadable: {e}"))?;
        doc.transact_mut()
            .apply_update(seed)
            .map_err(|e| format!("paper state unreadable: {e}"))?;
    }
    let incoming = Update::decode_v1(update)
        .map_err(|e| format!("paper update undecodable: {e}"))?;
    doc.transact_mut()
        .apply_update(incoming)
        .map_err(|e| format!("paper update rejected: {e}"))?;
    let full = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    Ok(if full.is_empty() { empty_state() } else { full })
}

/// Fold a compacted snapshot plus its pending log (in revision order) into one
/// full v1 state — the doc as it stands. Same as applying each update in
/// sequence; order among the log entries is honored, but convergence does not
/// depend on it.
///
/// A paper with no content folds to zero bytes, but a zero-byte Yjs update is
/// **not decodable** — `Y.applyUpdate` throws `Unexpected end of array` on it.
/// The state this returns is therefore always a decodable v1 update: the
/// canonical empty one when there is nothing yet.
pub fn fold(snapshot: &[u8], log_updates: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut state = snapshot.to_vec();
    for update in log_updates {
        state = apply(&state, update)?;
    }
    Ok(if state.is_empty() { empty_state() } else { state })
}

#[cfg(test)]
mod paper_doc_tests {
    use super::*;
    use yrs::Text;

    /// Fixture: produce a real v1 update by making a change the way any Yjs
    /// peer (the webview editor included) would — a transaction on a fresh doc.
    fn fixture_update(text: &str) -> Vec<u8> {
        let doc = Doc::new();
        let content = doc.get_or_insert_text("content");
        let mut txn = doc.transact_mut();
        content.insert(&mut txn, 0, text);
        txn.encode_state_as_update_v1(&StateVector::default())
    }

    #[test]
    fn an_undecodable_update_is_an_error_not_a_crash() {
        assert!(apply(&[], b"not a yjs update").is_err());
    }

    #[test]
    fn applying_an_update_produces_nonempty_state() {
        let u = fixture_update("hello");
        let state = apply(&[], &u).unwrap();
        assert!(!state.is_empty());
    }

    #[test]
    fn two_docs_applied_in_different_orders_converge_to_the_same_bytes() {
        let u1 = fixture_update("one ");
        let u2 = fixture_update("two");
        // Two trustees apply each other's updates in opposite orders.
        let a = apply(apply(&[], &u1).unwrap().as_slice(), &u2).unwrap();
        let b = apply(apply(&[], &u2).unwrap().as_slice(), &u1).unwrap();
        assert_eq!(a, b, "CRDT convergence must not depend on apply order");
    }

    #[test]
    fn the_same_update_applied_twice_is_a_no_op() {
        let u = fixture_update("hello");
        let once = apply(&[], &u).unwrap();
        let twice = apply(&once, &u).unwrap();
        assert_eq!(once, twice, "updates are idempotent by clock");
    }

    #[test]
    fn fold_matches_incremental_applies() {
        let snapshot = apply(&[], &fixture_update("base")).unwrap();
        let u1 = fixture_update("one");
        let u2 = fixture_update("two");
        let incremental = apply(apply(&snapshot, &u1).unwrap().as_slice(), &u2).unwrap();
        let folded = fold(&snapshot, &[u1, u2]).unwrap();
        assert_eq!(incremental, folded);
    }

    #[test]
    fn fold_of_an_empty_log_is_the_snapshot() {
        let snapshot = apply(&[], &fixture_update("base")).unwrap();
        assert_eq!(fold(&snapshot, &[]).unwrap(), snapshot);
    }

    /// The webview applies this state with `Y.applyUpdate`, which throws on
    /// zero bytes. A paper that has never been edited must still hand back a
    /// decodable update, not an empty buffer.
    #[test]
    fn a_paper_with_no_content_folds_to_a_decodable_empty_update() {
        let state = fold(&[], &[]).unwrap();
        assert!(!state.is_empty(), "zero bytes is not a decodable Yjs update");
        assert!(
            Update::decode_v1(&state).is_ok(),
            "the empty state must decode as a v1 update"
        );
    }

    #[test]
    fn fold_refuses_garbage_in_the_log() {
        let snapshot = apply(&[], &fixture_update("base")).unwrap();
        assert!(fold(&snapshot, &[b"garbage".to_vec()]).is_err());
    }
}
