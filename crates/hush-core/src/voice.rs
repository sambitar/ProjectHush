//! Conclave voice state machine (Task 23).
//!
//! Pure rules, no I/O: the daemon owns a [`VoiceManager`] and feeds it the
//! payloads `recv` routes in; `account.rs`'s senders call it before fanning
//! out. There is deliberately no store — a voice session is live-only, so a
//! restarted daemon is a fresh manager, which *is* the leave.
//!
//! The rules (spec §2, resolved 2026-10-13):
//!
//! - A room is **opened** by any joined trustee and **joined** explicitly per
//!   trustee — joining is the consent, there is no ring and so no ring timeout.
//!   Nothing here runs on a clock: a room ends by rule, not by timer.
//! - One live room per conclave; a second open refuses.
//! - At most [`MAX_VOICE_SPEAKERS`] joined trustees; a join past the cap
//!   refuses rather than dropping silently.
//! - A `leave` from the **opener** ends the room for everyone; any other leave
//!   removes one member, and the room ends by itself when fewer than two
//!   remain. A room open with only its opener is fine — it is waiting.
//! - `ConclaveEnd` ends any live room.
//! - You can only mute yourself; the mute flag is advisory (capture stops
//!   before the flag is sent).
use crate::error::Error;
use crate::payload::{ConclaveDescriptor, TrusteeState, VoiceDescriptor};
use std::collections::HashMap;

/// The most trustees a mesh voice room admits in v1.
///
/// Not a magic number to defend: the dogfood measures where the mesh stops
/// being right (each member encodes once per listener, so cost grows as
/// N×(N−1)), and the cap moves with that measurement. Joining past it refuses
/// with a reason rather than dropping.
pub const MAX_VOICE_SPEAKERS: usize = 8;

/// One member of a live voice room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceMember {
    pub account: String,
    /// Advisory: capture stopped before the flag was set. See `VoiceMute`.
    pub muted: bool,
}

/// A live voice room. Exists only in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    pub session: VoiceDescriptor,
    /// Joined trustees in join order. The opener is first by construction.
    pub joined: Vec<VoiceMember>,
}

/// What applying a voice payload did, so the caller can fan out, update the
/// UI, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceEffect {
    Opened,
    Joined,
    Left,
    /// The room ended (opener left, fewer than two remain, or conclave ended).
    Ended,
    MuteSet,
}

/// All live voice rooms, keyed by conclave id.
#[derive(Default)]
pub struct VoiceManager {
    sessions: HashMap<String, VoiceSession>,
}

/// `sender` is a trustee who has accepted — the distinction the conclave
/// already tracks per member, and the one an invited trustee must not cross
/// (they have consented to membership, not to a room).
fn is_joined_trustee(conclave: &ConclaveDescriptor, account: &str) -> bool {
    conclave
        .trustees
        .iter()
        .any(|t| t.account_id == account && t.state == TrusteeState::Joined)
}

impl VoiceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The live room for a conclave, if any.
    pub fn session(&self, conclave_id: &str) -> Option<&VoiceSession> {
        self.sessions.get(conclave_id)
    }

    /// Open a voice room in a conclave.
    ///
    /// `sender` must be a joined trustee of `conclave`; the conclave must not
    /// have ended; there must be no live room already. `session_id` is the
    /// opener's random UUID and is stored as given.
    pub fn open(
        &mut self,
        conclave: &ConclaveDescriptor,
        sender: &str,
        session_id: String,
        now_ms: u64,
    ) -> Result<VoiceEffect, Error> {
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        if !is_joined_trustee(conclave, sender) {
            return Err(Error::NotConclaveTrustee);
        }
        if self.sessions.contains_key(&conclave.conclave_id) {
            return Err(Error::VoiceSessionExists);
        }
        self.sessions.insert(
            conclave.conclave_id.clone(),
            VoiceSession {
                session: VoiceDescriptor {
                    session_id,
                    conclave_id: conclave.conclave_id.clone(),
                    opener: sender.to_string(),
                    started_ms: now_ms,
                },
                joined: vec![VoiceMember {
                    account: sender.to_string(),
                    muted: false,
                }],
            },
        );
        Ok(VoiceEffect::Opened)
    }

    /// Join a live voice room. Joining is the consent act, so it is explicit.
    pub fn join(
        &mut self,
        conclave: &ConclaveDescriptor,
        sender: &str,
        session_id: &str,
    ) -> Result<VoiceEffect, Error> {
        if conclave.ended {
            return Err(Error::ConclaveEnded);
        }
        let s = self
            .sessions
            .get_mut(&conclave.conclave_id)
            .ok_or(Error::VoiceNoSession)?;
        if s.session.session_id != session_id {
            return Err(Error::VoiceNoSession);
        }
        if !is_joined_trustee(conclave, sender) {
            return Err(Error::NotConclaveTrustee);
        }
        if s.joined.iter().any(|m| m.account == sender) {
            return Err(Error::AlreadyConclaveTrustee);
        }
        if s.joined.len() >= MAX_VOICE_SPEAKERS {
            return Err(Error::VoiceCapReached);
        }
        s.joined.push(VoiceMember {
            account: sender.to_string(),
            muted: false,
        });
        Ok(VoiceEffect::Joined)
    }

    /// Leave a live voice room. The opener's leave ends it for everyone.
    pub fn leave(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        sender: &str,
    ) -> Result<VoiceEffect, Error> {
        let s = self.sessions.get_mut(conclave_id).ok_or(Error::VoiceNoSession)?;
        if s.session.session_id != session_id {
            return Err(Error::VoiceNoSession);
        }
        // The opener's leave *is* the end of the room: the API has no separate
        // end route (spec §7), so ending for all rides the opener's leave.
        if s.session.opener == sender {
            self.sessions.remove(conclave_id);
            return Ok(VoiceEffect::Ended);
        }
        if !s.joined.iter().any(|m| m.account == sender) {
            return Err(Error::VoiceNotJoined);
        }
        s.joined.retain(|m| m.account != sender);
        if s.joined.len() < 2 {
            self.sessions.remove(conclave_id);
            return Ok(VoiceEffect::Ended);
        }
        Ok(VoiceEffect::Left)
    }

    /// Set the sender's own mute flag. Nobody else's.
    pub fn mute(
        &mut self,
        conclave_id: &str,
        session_id: &str,
        sender: &str,
        muted: bool,
    ) -> Result<VoiceEffect, Error> {
        let s = self.sessions.get_mut(conclave_id).ok_or(Error::VoiceNoSession)?;
        if s.session.session_id != session_id {
            return Err(Error::VoiceNoSession);
        }
        let member = s
            .joined
            .iter_mut()
            .find(|m| m.account == sender)
            .ok_or(Error::VoiceNotJoined)?;
        member.muted = muted;
        Ok(VoiceEffect::MuteSet)
    }

    /// End any live room in a conclave — what `ConclaveEnd` calls. Returns
    /// whether there was one to end.
    pub fn end_conclave(&mut self, conclave_id: &str) -> bool {
        self.sessions.remove(conclave_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::ConclaveTrustee;

    const CONCLAVE_ID: &str = "c-1";

    struct Fixture {
        manager: VoiceManager,
        conclave: ConclaveDescriptor,
    }

    /// Alice's conclave: she convenes, Bob and Carol are joined trustees, Dan
    /// is invited (not joined), Eve is not a trustee at all.
    fn fixture(joined_trustees: &[&str]) -> Fixture {
        let mut conclave = ConclaveDescriptor {
            conclave_id: CONCLAVE_ID.into(),
            name: "Standup".into(),
            convener: "aa".into(),
            trustees: vec![
                ConclaveTrustee {
                    account_id: "aa".into(),
                    nick: "alice".into(),
                    state: TrusteeState::Joined,
                },
                ConclaveTrustee {
                    account_id: "dd".into(),
                    nick: "dan".into(),
                    state: TrusteeState::Invited,
                },
            ],
            ended: false,
        };
        for (i, id) in ["bb", "cc"].iter().enumerate() {
            conclave.trustees.insert(
                1 + i,
                ConclaveTrustee {
                    account_id: (*id).into(),
                    nick: (*id).into(),
                    state: TrusteeState::Joined,
                },
            );
        }
        for (i, id) in joined_trustees.iter().enumerate() {
            conclave.trustees.insert(
                3 + i,
                ConclaveTrustee {
                    account_id: (*id).into(),
                    nick: (*id).into(),
                    state: TrusteeState::Joined,
                },
            );
        }
        Fixture {
            manager: VoiceManager::new(),
            conclave,
        }
    }

    fn open(f: &mut Fixture, opener: &str, session_id: &str) -> Result<VoiceEffect, Error> {
        f.manager.open(
            &f.conclave,
            opener,
            session_id.to_string(),
            1_000,
        )
    }

    /// Only a *joined* trustee may open: not an invited one, not a stranger.
    #[test]
    fn only_a_joined_trustee_may_open_a_voice_room() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert!(
            matches!(open(&mut f, "dd", "v-2"), Err(Error::NotConclaveTrustee)),
            "an invited trustee has not consented to anything yet"
        );
        assert!(matches!(open(&mut f, "ee", "v-3"), Err(Error::NotConclaveTrustee)));
    }

    /// One live room per conclave: a second open refuses, and the refusal does
    /// not disturb the first room.
    #[test]
    fn a_second_open_for_a_conclave_refuses() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert!(matches!(open(&mut f, "bb", "v-2"), Err(Error::VoiceSessionExists)));
        assert_eq!(f.manager.session(CONCLAVE_ID).unwrap().session.session_id, "v-1");
    }

    /// The room starts with only its opener in it — that is waiting, not a
    /// problem. The auto-end rule counts members after a *leave*, never at
    /// open.
    #[test]
    fn an_open_room_waits_for_members() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        let s = f.manager.session(CONCLAVE_ID).unwrap();
        assert_eq!(s.joined.len(), 1);
        assert_eq!(s.joined[0].account, "aa");
        assert!(!s.joined[0].muted);
    }

    #[test]
    fn a_join_refuses_once_the_cap_is_reached() {
        // Nine joined trustees ("aa".."ii" minus the fixture's three) is built
        // below; the ninth join is the one past MAX_VOICE_SPEAKERS.
        let mut f = fixture(&["ee", "ff", "gg", "hh", "ii", "jj"]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        for id in ["bb", "cc", "dd", "ee", "ff", "gg", "hh"] {
            // "dd" is invited in the base fixture, so the cap test needs it
            // "dd" is the fixture's invited trustee and cannot join; the
            // joined list is the fixture's other nine.
            if id == "dd" {
                continue;
            }
            assert_eq!(
                f.manager.join(&f.conclave, id, "v-1").unwrap(),
                VoiceEffect::Joined,
                "{id} should join"
            );
        }
        // One seat left ("ii" is the ninth joined trustee, "jj" the tenth).
        // Join "ii" to fill the room, then "jj" must refuse.
        assert_eq!(f.manager.join(&f.conclave, "ii", "v-1").unwrap(), VoiceEffect::Joined);
        assert!(matches!(
            f.manager.join(&f.conclave, "jj", "v-1"),
            Err(Error::VoiceCapReached)
        ));
    }

    /// The opener's leave ends the room for everyone; it is gone, not empty.
    #[test]
    fn the_openers_leave_ends_the_room_for_all() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert_eq!(f.manager.leave(CONCLAVE_ID, "v-1", "aa").unwrap(), VoiceEffect::Ended);
        assert!(f.manager.session(CONCLAVE_ID).is_none());
    }

    /// Anyone else's leave just removes them — until fewer than two remain.
    #[test]
    fn a_room_ends_by_itself_when_fewer_than_two_remain() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert_eq!(f.manager.join(&f.conclave, "bb", "v-1").unwrap(), VoiceEffect::Joined);
        assert_eq!(f.manager.join(&f.conclave, "cc", "v-1").unwrap(), VoiceEffect::Joined);
        // Three in the room: a leave leaves two, the room stays.
        assert_eq!(f.manager.leave(CONCLAVE_ID, "v-1", "cc").unwrap(), VoiceEffect::Left);
        assert_eq!(f.manager.session(CONCLAVE_ID).unwrap().joined.len(), 2);
        // Two in the room: a leave leaves one, the room ends.
        assert_eq!(f.manager.leave(CONCLAVE_ID, "v-1", "bb").unwrap(), VoiceEffect::Ended);
        assert!(f.manager.session(CONCLAVE_ID).is_none());
    }

    #[test]
    fn a_trustee_cannot_leave_a_room_they_are_not_in() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert!(matches!(
            f.manager.leave(CONCLAVE_ID, "v-1", "bb"),
            Err(Error::VoiceNotJoined)
        ));
        // The room is untouched by the refused leave.
        assert_eq!(f.manager.session(CONCLAVE_ID).unwrap().joined.len(), 1);
    }

    /// Mute is self-service: bob can mute bob and nobody else's flag moves.
    #[test]
    fn mute_only_touches_the_sender_own_flag() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert_eq!(f.manager.join(&f.conclave, "bb", "v-1").unwrap(), VoiceEffect::Joined);
        assert_eq!(f.manager.mute(CONCLAVE_ID, "v-1", "bb", true).unwrap(), VoiceEffect::MuteSet);
        let s = f.manager.session(CONCLAVE_ID).unwrap();
        assert_eq!(s.joined.iter().find(|m| m.account == "bb").unwrap().muted, true);
        assert_eq!(s.joined.iter().find(|m| m.account == "aa").unwrap().muted, false);
    }

    /// Voice is live-only and conclave-gated: an ended conclave opens nothing.
    #[test]
    fn voice_refuses_once_the_conclave_has_ended() {
        let mut f = fixture(&[]);
        f.conclave.ended = true;
        assert!(matches!(open(&mut f, "aa", "v-1"), Err(Error::ConclaveEnded)));
    }

    /// `ConclaveEnd` kills a live room; it is a no-op without one.
    #[test]
    fn ending_the_conclave_ends_a_live_room() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert!(f.manager.end_conclave(CONCLAVE_ID));
        assert!(f.manager.session(CONCLAVE_ID).is_none());
        assert!(!f.manager.end_conclave(CONCLAVE_ID));
    }

    /// A room that ended is not a zombie: the next open is a new session.
    #[test]
    fn a_new_room_can_open_after_one_ends() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert_eq!(f.manager.leave(CONCLAVE_ID, "v-1", "aa").unwrap(), VoiceEffect::Ended);
        assert_eq!(open(&mut f, "aa", "v-2").unwrap(), VoiceEffect::Opened);
        assert_eq!(f.manager.session(CONCLAVE_ID).unwrap().session.session_id, "v-2");
    }

    /// A join or leave naming a session id other than the live one's refuses —
    /// a stale payload from a previous room must not touch the current one.
    #[test]
    fn a_stale_session_id_refuses() {
        let mut f = fixture(&[]);
        assert_eq!(open(&mut f, "aa", "v-1").unwrap(), VoiceEffect::Opened);
        assert!(matches!(
            f.manager.join(&f.conclave, "bb", "v-old"),
            Err(Error::VoiceNoSession)
        ));
    }
}
