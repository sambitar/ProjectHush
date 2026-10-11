//! The Notes pane's personal state: sticky notes (and, later, the regions that
//! gather them) on a private board.
//!
//! Encrypted at rest in a vault sidecar (`notes-state.enc`, see
//! [`Vault::write_encrypted_file`](hush_core::Vault)) — note titles and bodies
//! are user secrets, so nothing here is plaintext on disk or in the webview.
//! The JSON schema in this module is also exactly what the UI receives from
//! `GET /api/notes/state`.
//!
//! There is no network path: notes never leave the device.

use serde::{Deserialize, Serialize};

/// The vault sidecar file name. Fixed here so the routes and any future
/// migration agree on one name.
pub const STATE_FILE: &str = "notes-state.enc";

/// The board this version ships. It is written onto every note and region so a
/// future multi-board version is additive rather than a migration.
pub const DEFAULT_BOARD_ID: &str = "default";

/// A bound that keeps the sidecar (and the UI's render) sane. A create over the
/// cap is refused with a reason, never silently dropped. Region and checklist
/// caps arrive with those steps.
const NOTES_CAP: usize = 2000;

/// The paper colours the UI offers. A note's `color` is one of these; anything
/// else is coerced to the first on write.
pub const PAPER_COLORS: [&str; 5] = ["yellow", "blue", "green", "pink", "white"];

fn default_board() -> String {
    DEFAULT_BOARD_ID.to_string()
}

fn default_color() -> String {
    PAPER_COLORS[0].to_string()
}

/// One tickable line on a note. Structured (not `[ ]` text parsed from the
/// body) so a box can be toggled without opening the editor and the id stays
/// stable for a future export. The checklist endpoints land in a later step;
/// the shape is fixed now so nothing needs migrating.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

/// A sticky note: a title, an optional body, an optional checklist, and its
/// place on the board. `x`/`y`/`z` are only meaningful once the canvas lands
/// (a later step); they are stored from the start so nothing needs migrating.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    #[serde(default = "default_board")]
    pub board_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub z: i64,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: i64,
    /// Unix seconds.
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
}

/// A labelled rectangle that gathers notes by position. Stored, but its
/// membership is derived from a note's centre — see the task doc.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    #[serde(default = "default_board")]
    pub board_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotesState {
    #[serde(default = "default_board")]
    pub board_id: String,
    /// Notes, in no particular order — the board lays them out by `x`/`y`.
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub regions: Vec<Region>,
}

impl Default for NotesState {
    fn default() -> Self {
        NotesState {
            board_id: default_board(),
            notes: Vec::new(),
            regions: Vec::new(),
        }
    }
}

/// Decode sidecar bytes; `None` (never saved) decodes to the empty default.
/// A corrupt sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<NotesState, String> {
    match bytes {
        None | Some([]) => Ok(NotesState::default()),
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("notes state unreadable: {e}")),
    }
}

/// The request body for creating or updating a note. `id` absent means create.
#[derive(Clone, Debug, Deserialize)]
pub struct NoteDraft {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub color: Option<String>,
}

impl NotesState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("notes state unwritable: {e}"))
    }

    /// Create (no `id`) or update (matching `id`) a note and return its id.
    ///
    /// A create over [`NOTES_CAP`] is refused; an update naming an unknown id is
    /// refused rather than silently creating, so a stale UI cannot resurrect a
    /// deleted note. A blank title and body is allowed — an empty sticky is a
    /// legitimate starting point.
    pub fn upsert_note(&mut self, draft: NoteDraft) -> Result<String, String> {
        let color = draft
            .color
            .as_deref()
            .filter(|c| PAPER_COLORS.contains(c))
            .map(str::to_string);

        if let Some(id) = draft.id.as_deref() {
            let note = self
                .notes
                .iter_mut()
                .find(|n| n.id == id)
                .ok_or_else(|| "that note no longer exists".to_string())?;
            note.title = draft.title;
            note.body = draft.body;
            if let Some(c) = color {
                note.color = c;
            }
            note.updated_at = now_secs();
            return Ok(note.id.clone());
        }

        if self.notes.len() >= NOTES_CAP {
            return Err(format!("at most {NOTES_CAP} notes are kept"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_secs();
        self.notes.push(Note {
            id: id.clone(),
            board_id: self.board_id.clone(),
            title: draft.title,
            body: draft.body,
            color: color.unwrap_or_else(default_color),
            x: 0.0,
            y: 0.0,
            z: 0,
            created_at: now,
            updated_at: now,
            checklist: Vec::new(),
        });
        Ok(id)
    }

    /// Remove a note; `true` when one was there. Checklist items go with it.
    pub fn delete_note(&mut self, id: &str) -> bool {
        let before = self.notes.len();
        self.notes.retain(|n| n.id != id);
        self.notes.len() != before
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod notes_store_tests {
    use super::*;

    fn draft(title: &str, body: &str) -> NoteDraft {
        NoteDraft {
            id: None,
            title: title.to_string(),
            body: body.to_string(),
            color: None,
        }
    }

    fn find<'a>(state: &'a NotesState, id: &str) -> &'a Note {
        state.notes.iter().find(|n| n.id == id).expect("note present")
    }

    #[test]
    fn empty_and_missing_sidecars_decode_to_default() {
        let d = decode(None).unwrap();
        assert!(d.notes.is_empty());
        assert_eq!(d.board_id, DEFAULT_BOARD_ID);
        assert!(decode(Some(b"")).unwrap().regions.is_empty());
        assert!(
            decode(Some(b"garbage")).is_err(),
            "corrupt state is an error, not a reset"
        );
    }

    #[test]
    fn create_assigns_id_and_stamps_times() {
        let mut state = NotesState::default();
        let id = state.upsert_note(draft("Groceries", "milk")).unwrap();
        assert!(!id.is_empty());
        let n = find(&state, &id);
        assert_eq!(n.title, "Groceries");
        assert_eq!(n.body, "milk");
        assert_eq!(n.board_id, DEFAULT_BOARD_ID);
        assert_eq!(n.color, "yellow");
        assert!(n.created_at > 0);
        assert_eq!(n.created_at, n.updated_at);
    }

    #[test]
    fn update_edits_in_place_and_keeps_created_at() {
        let mut state = NotesState::default();
        let id = state.upsert_note(draft("a", "b")).unwrap();
        let created = find(&state, &id).created_at;

        let again = state
            .upsert_note(NoteDraft {
                id: Some(id.clone()),
                title: "a2".into(),
                body: "b2".into(),
                color: Some("blue".into()),
            })
            .unwrap();
        assert_eq!(again, id);
        assert_eq!(state.notes.len(), 1, "update does not duplicate");
        let n = find(&state, &id);
        assert_eq!(n.title, "a2");
        assert_eq!(n.body, "b2");
        assert_eq!(n.color, "blue");
        assert_eq!(n.created_at, created);
        assert!(n.updated_at >= created);
    }

    #[test]
    fn updating_an_unknown_id_is_refused_not_created() {
        let mut state = NotesState::default();
        let err = state
            .upsert_note(NoteDraft {
                id: Some("ghost".into()),
                title: "x".into(),
                body: String::new(),
                color: None,
            })
            .unwrap_err();
        assert!(err.contains("no longer exists"));
        assert!(state.notes.is_empty());
    }

    #[test]
    fn unknown_colour_is_coerced_to_the_default() {
        let mut state = NotesState::default();
        let id = state
            .upsert_note(NoteDraft {
                id: None,
                title: String::new(),
                body: String::new(),
                color: Some("chartreuse".into()),
            })
            .unwrap();
        assert_eq!(find(&state, &id).color, "yellow");
    }

    #[test]
    fn delete_removes_and_is_idempotent() {
        let mut state = NotesState::default();
        let id = state.upsert_note(draft("t", "b")).unwrap();
        assert!(state.delete_note(&id));
        assert!(state.notes.iter().all(|n| n.id != id));
        assert!(!state.delete_note(&id), "deleting twice is a no-op");
    }

    #[test]
    fn create_over_the_cap_is_refused() {
        let mut state = NotesState::default();
        for i in 0..NOTES_CAP {
            state.upsert_note(draft(&format!("n{i}"), "")).unwrap();
        }
        assert_eq!(state.notes.len(), NOTES_CAP);
        assert!(state.upsert_note(draft("one more", "")).is_err());
    }

    #[test]
    fn state_roundtrips_through_bytes() {
        let mut state = NotesState::default();
        let id = state.upsert_note(draft("Groceries", "milk")).unwrap();
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.notes.len(), 1);
        assert_eq!(find(&back, &id).title, "Groceries");
        assert_eq!(back.board_id, DEFAULT_BOARD_ID);
    }
}
