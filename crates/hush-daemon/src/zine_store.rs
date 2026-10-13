//! Zines — the members-only long-form corpus: every member's own blog, held in
//! this vault as an encrypted sidecar.
//!
//! Phase 1 is **offline and local**: a zine is authored here, stored here, and
//! read here. Nothing in this module talks to a relay or a session. The schema
//! is nonetheless the wire schema, because Phase 2 publishes exactly these
//! bytes and a migration later would have to re-sign every revision:
//!
//! - A **zine** is one long-form article: a **post**. Its author is a **pen
//!   name** ([`ZineDescriptor::pen_id`]), and one pen name holds **many** posts
//!   ([`ZineDescriptor::zine_id`]), each with its own read key and its own
//!   revision chain. More than one pen name per vault is a later feature, which
//!   is why the pen name is a field rather than a constant.
//! - Its body is **sanitized HTML** ([`crate::reads::sanitize_rich_text`]),
//!   pinned here rather than left open (the task doc's open question 5). HTML
//!   is what the editor already produces and what a later Nostr export needs,
//!   and one allow-list for every rich-text surface beats two renderers that
//!   can disagree.
//! - Every write is a **new revision** carrying the hash of the one before it,
//!   so a holder can tell a superseded blob from the head without holding the
//!   read key. The chain is what makes "the newest revision a member holds
//!   wins" decidable.
//! - **Comments** are merged, not appended: a comment exists in this vault only
//!   once the author has accepted it. Moderation is entirely author-side, and an
//!   unaccepted comment simply is not here.
//!
//! Everything here is a user secret — a draft zine is as private as a draft
//! message — so the sidecar (`zines.enc`, via
//! [`Vault::write_encrypted_file`](hush_core::Vault)) is ciphertext on disk, and
//! a locked vault answers 401 before these types are touched.

use serde::{Deserialize, Serialize};

/// The vault sidecar file name. Fixed here so the routes and any future
/// migration agree on one name.
pub const STATE_FILE: &str = "zines.enc";

/// How many zines a vault holds. The newspaper model means a member accumulates
/// other people's work, so this is deliberately larger than the Paper cap; it is
/// still a cap, and going over it is refused with a reason rather than dropped.
const ZINES_CAP: usize = 512;

/// One zine's body budget: the whole text of the article. Matches the bulletin's
/// per-blob cap, so a zine that can be written can also be published.
const BODY_SOFT_CAP: usize = 256 * 1024;

/// Superseded revisions kept per zine. Enough to undo a mistake, bounded so a
/// long-lived zine cannot grow without limit. The oldest fall off.
const HISTORY_CAP: usize = 32;

/// Merged comments kept per zine.
///
/// Phase 3 is what reaches this from a route; until then only the unit tests
/// do, which is why the build reports it unused.
#[allow(dead_code)]
const COMMENTS_CAP: usize = 512;

/// Sections (tags) per zine. A section is free text inside the encrypted
/// descriptor; the cap keeps the facet index and the UI honest.
const SECTIONS_CAP: usize = 12;

/// Longest section label, in characters. A tag is a facet, not a sentence.
const SECTION_MAX_CHARS: usize = 32;

/// Timestamps are author-declared and therefore forgeable, so a future one is
/// clamped to now on ingest — otherwise a zine could pin itself to the top of
/// every reader's index forever. The same rule the old articles feed used.
const MAX_CLOCK_SKEW_MS: u64 = 24 * 60 * 60 * 1000;

/// A zine as it travels and as it is stored: the author's identity, the article,
/// and the comment thread the author has merged into it.
///
/// This is the plaintext inside the sealed blob. It is a `Serialize` type with
/// no `Deserialize` shortcut on purpose — every field is validated by
/// [`ZineDraft::into_descriptor`] on the way in, so a hostile descriptor cannot
/// arrive by constructing one directly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineDescriptor {
    /// This post's id: a fresh random id per post, never an identity. One pen
    /// name publishes many zines, and each one carries its own read key and its
    /// own revision chain, so the id addresses a post rather than an author.
    pub zine_id: String,
    /// The author's pen name: their seal public key, hex, stable across every
    /// post they write. This is the address a comment is sealed to, which is why
    /// it is the seal key rather than the signing key — see `PenName` in
    /// `hush_core::vault`. Empty only on a record written before the reshape,
    /// which [`ZinesState::migrate`] repairs on first read.
    #[serde(default)]
    pub pen_id: String,
    /// The byline shown to readers. Chosen by the author, so it can change
    /// without changing the id.
    #[serde(default)]
    pub pen_name: String,
    #[serde(default)]
    pub title: String,
    /// Sections (tags). They exist only inside this descriptor, so the relay
    /// never sees one and there is no registry to squat.
    #[serde(default)]
    pub sections: Vec<String>,
    /// Author-declared, clamped on ingest.
    #[serde(default)]
    pub created_ms: u64,
    /// Monotonic per zine, from 1.
    #[serde(default)]
    pub revision: u64,
    /// The content hash of the previous revision; empty at revision 1.
    #[serde(default)]
    pub prev_hash: String,
    /// Sanitized HTML.
    #[serde(default)]
    pub body: String,
    /// The merged thread. A comment the author has not accepted is not here.
    #[serde(default)]
    pub comments: Vec<ZineComment>,
}

/// One comment the author has merged into the thread.
///
/// `hash` is the content hash of the comment blob as it arrived, so the same
/// comment cannot be merged twice and a reader can be told which blob a line
/// came from. `author` is whatever the commenter signed with — this vault does
/// not decide who they are, it records what they claimed and leaves the judgement
/// to the person reading.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineComment {
    pub hash: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_ms: u64,
    /// When this vault merged it, which is a local fact and never published.
    #[serde(default)]
    pub merged_ms: u64,
}

/// A superseded revision, kept so a mistake can be undone and so a holder can
/// answer "is this blob older than what I have?".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineRevision {
    pub revision: u64,
    /// The content hash of this revision's canonical bytes.
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub replaced_ms: u64,
}

/// One zine in this vault: the head, its history, and the local facts about
/// holding it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineFile {
    pub descriptor: ZineDescriptor,
    /// Superseded revisions, newest first, capped at [`HISTORY_CAP`].
    #[serde(default)]
    pub history: Vec<ZineRevision>,
    /// Whether this vault is the author. A held zine is read-only here: only
    /// its pen name can publish a new revision, because only that pen name can
    /// sign one.
    #[serde(default)]
    pub authored: bool,
    /// The symmetric read key, base64, when this vault holds one. Phase 1 mints
    /// it for our own zine so the field is populated before anything publishes;
    /// a held zine without one is [`ZineFile::unreadable`].
    #[serde(default)]
    pub read_key: Option<String>,
    /// Held but not yet openable: a blob arrived before its key did. Kept, never
    /// discarded, so the retry sweep can classify it later.
    #[serde(default)]
    pub unreadable: bool,
    /// Local mute. No wire effect — the zine is still held and still counted.
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub updated_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ZinesState {
    #[serde(default)]
    pub zines: Vec<ZineFile>,
}

/// Decode sidecar bytes; `None` (never saved) decodes to the empty default.
/// A corrupt sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<ZinesState, String> {
    match bytes {
        None | Some([]) => Ok(ZinesState::default()),
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("zines state unreadable: {e}")),
    }
}

/// What the UI posts to create a post or revise one.
///
/// Absent `zine_id` means "a new post": the daemon mints the id and takes the
/// byline from the vault, which is the only thing Phase 1 can author. A present
/// id names the post to revise, and is refused rather than treated as a create
/// when this vault does not hold it — a stale UI must not resurrect a post that
/// was forgotten locally.
#[derive(Clone, Debug, Deserialize)]
pub struct ZineDraft {
    #[serde(default)]
    pub zine_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// One entry in the local index the UI renders. Deliberately not the whole
/// zine: a list of five hundred bodies would be megabytes of JSON for a screen
/// that shows titles.
#[derive(Clone, Debug, Serialize)]
pub struct ZineSummary {
    pub zine_id: String,
    pub pen_name: String,
    pub title: String,
    pub sections: Vec<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub revision: u64,
    pub comments: usize,
    /// A short plain-text taste of the body, for the list row.
    pub preview: String,
    pub authored: bool,
    pub muted: bool,
    pub unreadable: bool,
}

/// Normalise a section label: trimmed, collapsed whitespace, length-capped.
/// Two labels that differ only in case or spacing are the same section, which is
/// what keeps a facet index from fragmenting.
fn normalize_section(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let trimmed: String = collapsed.chars().take(SECTION_MAX_CHARS).collect();
    Some(trimmed.to_lowercase())
}

fn normalize_sections(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in raw {
        let Some(s) = normalize_section(s) else { continue };
        if out.len() >= SECTIONS_CAP {
            break;
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Clamp an author-declared timestamp: a future one becomes now.
fn clamp_ms(declared: u64, now_ms: u64) -> u64 {
    if declared > now_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
        now_ms
    } else {
        declared
    }
}

/// A plain-text taste of a body, for the index row. Strips tags rather than
/// parsing them: the body is already sanitized, so anything that looks like a tag
/// here is one of the allowed ones and contributes no text.
fn preview_of(body: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in body.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(160).collect()
}

/// Whether a comment body is worth merging: an empty one is not a comment.
///
/// Sanitised on the way in, the same as an article body, so a merged thread
/// cannot carry markup the reader's sanitizer would strip anyway. Phase 3 wires
/// the route that calls this; until then only the unit tests do.
#[allow(dead_code)]
fn normalize_comment_body(raw: &str) -> String {
    crate::reads::sanitize_rich_text(raw)
}

/// Mint the symmetric read key for a zine this vault authors.
///
/// Minted at create time rather than at publish time: Phase 2 seals exactly this
/// key to each reader's seal key, and a zine written before that lands would
/// otherwise have to be migrated — which for a signed revision chain means
/// re-signing. Base64 because the descriptor is JSON on the wire.
fn mint_read_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use rand::{RngCore, TryRngCore};

    let mut key = [0u8; 32];
    rand::rngs::OsRng.unwrap_err().fill_bytes(&mut key);
    STANDARD.encode(key)
}

impl ZinesState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("zines state unwritable: {e}"))
    }

    pub fn get(&self, zine_id: &str) -> Option<&ZineFile> {
        self.zines.iter().find(|z| z.descriptor.zine_id == zine_id)
    }

    fn get_mut(&mut self, zine_id: &str) -> Option<&mut ZineFile> {
        self.zines
            .iter_mut()
            .find(|z| z.descriptor.zine_id == zine_id)
    }

    /// Give every authored zine a read key if it does not have one, and report
    /// whether anything changed so the caller can persist the result.
    ///
    /// A zine written before the key was minted at create time would otherwise
    /// sit without one forever. Phase 2 publishes exactly this key, so a vault
    /// that cannot mint it is a vault that cannot publish the zine it already
    /// wrote — and re-signing a published revision chain is not a migration
    /// anyone should have to run.
    ///
    /// A held zine is left alone: only the author holds the plaintext, so only
    /// the author can mint the key. A held zine with none stays `unreadable`.
    pub fn backfill_read_keys(&mut self) -> bool {
        let mut changed = false;
        for z in self.zines.iter_mut() {
            if z.authored && z.read_key.is_none() {
                z.read_key = Some(mint_read_key());
                changed = true;
            }
        }
        changed
    }

    /// One-time upgrade of records written before a pen name could hold many
    /// posts. Reports whether anything changed, so the caller persists once.
    ///
    /// Before the reshape the record's `zine_id` **was** the pen name's seal
    /// public key, which is what limited a pen name to one post. An empty
    /// `pen_id` is the marker: the field did not exist, so it decodes as empty,
    /// and the id it carries is the identity. Both are repaired together — the
    /// identity moves to `pen_id` and the record gets a fresh post id — so the
    /// invariant "a `zine_id` is an address, never an identity" holds for every
    /// record afterwards.
    ///
    /// Nothing on the wire references a post id yet (Phase 2 has not landed), so
    /// re-addressing is free here and would not be later.
    pub fn migrate(&mut self) -> bool {
        let mut changed = self.backfill_read_keys();
        for z in self.zines.iter_mut() {
            if z.descriptor.pen_id.is_empty() {
                // A held zine predates the reshape too, and its blob did carry
                // the author's identity in the id — the same field, the same
                // repair. Only an authored record is re-addressed: a held one is
                // addressed by the blob that carries it, and a fresh id here
                // would make this vault's copy unrecognisable to its author.
                z.descriptor.pen_id = z.descriptor.zine_id.clone();
                if z.authored {
                    z.descriptor.zine_id = uuid::Uuid::new_v4().to_string();
                }
                changed = true;
            }
        }
        changed
    }

    /// The index the UI renders: newest first, optionally filtered by section
    /// and by pen name. Muted zines are included but flagged — muting is a
    /// display decision, and a caller that wants them gone filters on the flag.
    pub fn index(&self, section: Option<&str>, pen: Option<&str>) -> Vec<ZineSummary> {
        let section = section.and_then(normalize_section);
        let pen = pen.map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty());
        let mut out: Vec<ZineSummary> = self
            .zines
            .iter()
            .filter(|z| {
                section
                    .as_ref()
                    .is_none_or(|s| z.descriptor.sections.contains(s))
            })
            .filter(|z| {
                pen.as_ref().is_none_or(|p| {
                    z.descriptor.pen_name.to_lowercase().contains(p)
                        || z.descriptor.pen_id.to_lowercase().starts_with(p)
                })
            })
            .map(|z| ZineSummary {
                zine_id: z.descriptor.zine_id.clone(),
                pen_name: z.descriptor.pen_name.clone(),
                title: z.descriptor.title.clone(),
                sections: z.descriptor.sections.clone(),
                created_ms: z.descriptor.created_ms,
                updated_ms: z.updated_ms,
                revision: z.descriptor.revision,
                comments: z.descriptor.comments.len(),
                preview: preview_of(&z.descriptor.body),
                authored: z.authored,
                muted: z.muted,
                unreadable: z.unreadable,
            })
            .collect();
        // Newest first, by the author's own clock — clamped on write, so a
        // forged future timestamp has already been pulled back to now.
        out.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.zine_id.cmp(&b.zine_id))
        });
        out
    }

    /// Every section any held zine carries, with how many carry it, sorted by
    /// count then name. The local facet index: there is no registry anywhere
    /// else, so this is derived rather than stored.
    pub fn sections(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for z in &self.zines {
            for s in &z.descriptor.sections {
                match counts.iter_mut().find(|(name, _)| name == s) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((s.clone(), 1)),
                }
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts
    }

    /// Publish a new post, or write a new revision of one this vault wrote.
    ///
    /// `body` arrives from the UI as HTML and is sanitized here, at the
    /// boundary, rather than trusted: the editor is not the only thing that can
    /// post to the route. Returns the post's id and the revision it now holds.
    ///
    /// The draft's `zine_id` is `None` to publish a new post under `pen_id`, and
    /// `Some` to revise one — read from the draft rather than passed alongside
    /// it, so the two can never disagree about what is being written. The two
    /// are genuinely different operations now that a pen name holds many posts:
    /// a revise of an id this vault does not hold is refused rather than
    /// silently created, so a stale UI cannot resurrect something that was
    /// forgotten locally, and a post this vault only *holds* is refused too —
    /// only the pen name that signed it can publish a new revision, and a local
    /// edit would be unsigned and unshareable. A create is refused past
    /// [`ZINES_CAP`].
    pub fn write(
        &mut self,
        pen_id: &str,
        pen_name: &str,
        draft: ZineDraft,
        now_ms: u64,
    ) -> Result<(String, u64), String> {
        let ZineDraft {
            zine_id,
            title,
            sections,
            body,
        } = draft;
        if pen_id.is_empty() {
            return Err("a zine needs a pen name to publish under".to_string());
        }
        let body = crate::reads::sanitize_rich_text(&body);
        if body.len() > BODY_SOFT_CAP {
            return Err(format!(
                "the article is over the {} KiB cap",
                BODY_SOFT_CAP / 1024
            ));
        }
        let title = title.trim().to_string();
        let sections = normalize_sections(&sections);

        if let Some(id) = zine_id.as_deref() {
            if id.is_empty() {
                return Err("a revision needs the id of the post it revises".to_string());
            }
            let Some(file) = self.get_mut(id) else {
                return Err("no post with that id is held".to_string());
            };
            if !file.authored {
                return Err("only the pen name that wrote a zine can edit it".to_string());
            }
            // The old head becomes history before the new one replaces it, so
            // the chain is always readable and an undo has somewhere to go.
            let superseded = ZineRevision {
                revision: file.descriptor.revision,
                hash: content_hash(&file.descriptor),
                title: file.descriptor.title.clone(),
                body: file.descriptor.body.clone(),
                sections: file.descriptor.sections.clone(),
                replaced_ms: now_ms,
            };
            file.history.insert(0, superseded);
            file.history.truncate(HISTORY_CAP);
            file.descriptor.prev_hash = content_hash(&file.descriptor);
            file.descriptor.revision += 1;
            file.descriptor.title = title;
            file.descriptor.sections = sections;
            file.descriptor.body = body;
            file.descriptor.pen_id = pen_id.to_string();
            file.descriptor.pen_name = pen_name.trim().to_string();
            file.descriptor.created_ms = clamp_ms(file.descriptor.created_ms, now_ms);
            file.updated_ms = now_ms;
            return Ok((file.descriptor.zine_id.clone(), file.descriptor.revision));
        }

        if self.zines.len() >= ZINES_CAP {
            return Err(format!("at most {ZINES_CAP} zines are held"));
        }
        // A post id is minted here rather than derived: it is an address, not an
        // identity, and deriving it from the pen name is what limited a pen name
        // to one post.
        let id = uuid::Uuid::new_v4().to_string();
        self.zines.push(ZineFile {
            descriptor: ZineDescriptor {
                zine_id: id.clone(),
                pen_id: pen_id.to_string(),
                pen_name: pen_name.trim().to_string(),
                title,
                sections,
                created_ms: clamp_ms(now_ms, now_ms),
                revision: 1,
                prev_hash: String::new(),
                body,
                comments: Vec::new(),
            },
            history: Vec::new(),
            authored: true,
            read_key: Some(mint_read_key()),
            unreadable: false,
            muted: false,
            updated_ms: now_ms,
        });
        Ok((id, 1))
    }

    /// Rename the byline on every post this pen name wrote.
    ///
    /// A display name is not an identity: `pen_id` and every `zine_id` stay
    /// put, so nothing already published is invalidated and no reader's index
    /// has to be rebuilt. All of them, because the byline belongs to the author
    /// rather than to one post — one pen name holds many. Local only until the
    /// next revision publishes.
    pub fn rename_byline(&mut self, pen_id: &str, pen_name: &str, now_ms: u64) -> bool {
        let name = pen_name.trim().to_string();
        let mut changed = false;
        for z in self.zines.iter_mut() {
            if z.descriptor.pen_id == pen_id && z.authored && z.descriptor.pen_name != name {
                z.descriptor.pen_name = name.clone();
                z.updated_ms = now_ms;
                changed = true;
            }
        }
        changed
    }

    /// Merge one comment into the thread.
    ///
    /// Author-side by construction: this is the author accepting a comment, so
    /// an unaccepted one is simply never here. Idempotent on `hash`, because the
    /// same blob can arrive on every drain and a thread that grows a duplicate on
    /// each pass is a thread nobody can read.
    ///
    /// Phase 3 is what calls this from a route; until then only the unit tests
    /// do, which is why the build reports it unused.
    #[allow(dead_code)]
    pub fn merge_comment(
        &mut self,
        zine_id: &str,
        comment: ZineComment,
        now_ms: u64,
    ) -> Result<(), String> {
        if comment.hash.is_empty() {
            return Err("a comment needs a hash to be accepted".to_string());
        }
        let body = normalize_comment_body(&comment.body);
        if body.trim().is_empty() {
            return Err("an empty comment is not merged".to_string());
        }
        let file = self
            .get_mut(zine_id)
            .ok_or_else(|| "no post with that id is held".to_string())?;
        if !file.authored {
            return Err("only the pen name that wrote a zine can merge comments".to_string());
        }
        if file
            .descriptor
            .comments
            .iter()
            .any(|c| c.hash == comment.hash)
        {
            return Ok(());
        }
        if file.descriptor.comments.len() >= COMMENTS_CAP {
            return Err(format!("at most {COMMENTS_CAP} comments are kept"));
        }
        file.descriptor.comments.push(ZineComment {
            hash: comment.hash,
            author: comment.author,
            body,
            created_ms: clamp_ms(comment.created_ms, now_ms),
            merged_ms: now_ms,
        });
        // Oldest first: a thread reads in the order it was written, unlike the
        // index, which is newest first because that is how a newspaper is read.
        file.descriptor
            .comments
            .sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.hash.cmp(&b.hash)));
        file.updated_ms = now_ms;
        Ok(())
    }

    /// Set the local mute flag. `false` when there is no such zine.
    pub fn set_muted(&mut self, zine_id: &str, muted: bool, now_ms: u64) -> bool {
        let Some(file) = self.get_mut(zine_id) else {
            return false;
        };
        if file.muted == muted {
            return false;
        }
        file.muted = muted;
        file.updated_ms = now_ms;
        true
    }

    /// Forget a zine locally. `true` when one was there.
    ///
    /// This is not a retraction and never claims to be: in Phase 2 a published
    /// revision is on the bulletin and in every holder's vault, and this only
    /// stops *this* vault from keeping it. A reader who wants it gone again has
    /// to wait for the window to age out.
    pub fn forget(&mut self, zine_id: &str) -> bool {
        let before = self.zines.len();
        self.zines.retain(|z| z.descriptor.zine_id != zine_id);
        self.zines.len() != before
    }
}

/// The content hash of a descriptor's canonical bytes.
///
/// Canonical means the serialized form with the hash-bearing fields it does not
/// cover taken out, so a hash can be computed before `prev_hash` is set. Phase 2
/// signs over this same form, which is why it is one function here rather than
/// inlined at the two call sites that need it.
pub fn content_hash(descriptor: &ZineDescriptor) -> String {
    let canonical = ZineDescriptor {
        prev_hash: String::new(),
        ..descriptor.clone()
    };
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    hex::encode(hush_core::sha256(&bytes))
}

#[cfg(test)]
mod zine_store_tests {
    use super::*;
    use base64::Engine;

    const NOW: u64 = 1_760_000_000_000;
    /// The vault's pen name: the seal public key in production, a fixed string
    /// here. A post id is minted by the store, so no test chooses one.
    const PEN: &str = "pen-ada";

    fn draft(title: &str, body: &str) -> ZineDraft {
        ZineDraft {
            zine_id: None,
            title: title.to_string(),
            sections: Vec::new(),
            body: body.to_string(),
        }
    }

    /// Publish a new post and hand back the id the store minted.
    fn create(s: &mut ZinesState, pen: &str, name: &str, d: ZineDraft, now: u64) -> String {
        s.write(pen, name, d, now).unwrap().0
    }

    /// Revise a post, returning the revision it now holds.
    fn revise(s: &mut ZinesState, id: &str, pen: &str, name: &str, d: ZineDraft, now: u64) -> u64 {
        s.write(pen, name, revising(id, d), now).unwrap().1
    }

    /// The same draft, aimed at an existing post.
    fn revising(id: &str, d: ZineDraft) -> ZineDraft {
        ZineDraft {
            zine_id: Some(id.to_string()),
            ..d
        }
    }

    fn with_zine() -> ZinesState {
        let mut s = ZinesState::default();
        create(&mut s, PEN, "ada", draft("First", "<p>hello</p>"), NOW);
        s
    }

    /// The id of the only post in `s`.
    fn the_id(s: &ZinesState) -> String {
        assert_eq!(s.zines.len(), 1, "this helper expects exactly one post");
        s.zines[0].descriptor.zine_id.clone()
    }

    #[test]
    fn an_authored_zine_is_minted_a_read_key_at_create() {
        // The key exists before anything is published, so a later publish is a
        // seal-and-send rather than a migration of every zine already written.
        let s = with_zine();
        let file = s.zines.first().unwrap();
        let key = file.read_key.clone().expect("an authored zine has a read key");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&key)
                .unwrap()
                .len(),
            32,
            "32 bytes of key material, base64: {key:?}"
        );

        // A fresh mint per vault: the key is random, not derived from the id.
        let other = with_zine();
        assert_ne!(
            other.zines.first().unwrap().read_key,
            file.read_key,
            "two vaults do not share a read key"
        );
    }

    #[test]
    fn a_zine_written_before_the_key_existed_is_backfilled_once() {
        // The upgrade path: a zine authored by an older build has no read key,
        // and the read path mints one. The second call is a no-op, so the key
        // does not move under a zine that has already been shared.
        let mut s = with_zine();
        let id = the_id(&s);
        s.zines[0].read_key = None;

        assert!(s.backfill_read_keys(), "the missing key is filled in");
        let minted = s.get(&id).unwrap().read_key.clone().unwrap();
        assert!(!s.backfill_read_keys(), "the second pass has nothing to do");
        assert_eq!(s.get(&id).unwrap().read_key.as_deref(), Some(minted.as_str()));

        // And it survives a round trip, which is what makes it the key Phase 2
        // publishes rather than a value that changes on every load.
        let back = decode(Some(&s.encode().unwrap())).unwrap();
        assert_eq!(back.get(&id).unwrap().read_key.as_deref(), Some(minted.as_str()));
    }

    /// A held post: written by someone else, so this vault can read it and not
    /// edit it. Built by hand because the create path always authors.
    fn held_post(id: &str, pen_id: &str) -> ZineFile {
        ZineFile {
            descriptor: ZineDescriptor {
                zine_id: id.to_string(),
                pen_id: pen_id.to_string(),
                pen_name: "someone else".to_string(),
                title: "Theirs".to_string(),
                sections: Vec::new(),
                created_ms: NOW,
                revision: 1,
                prev_hash: String::new(),
                body: String::new(),
                comments: Vec::new(),
            },
            history: Vec::new(),
            authored: false,
            read_key: None,
            unreadable: true,
            muted: false,
            updated_ms: NOW,
        }
    }

    #[test]
    fn a_held_zine_is_not_given_a_read_key() {
        // Only the author holds the plaintext, so only the author can mint the
        // key. A held zine with no key is `unreadable`, not silently readable.
        let mut s = ZinesState::default();
        s.zines.push(held_post("bb22", "pen-bob"));
        assert!(!s.backfill_read_keys(), "a held zine is left as it was");
        assert!(s.get("bb22").unwrap().read_key.is_none());
    }

    #[test]
    fn empty_and_missing_sidecars_decode_to_default() {
        assert!(decode(None).unwrap().zines.is_empty());
        assert!(decode(Some(b"")).unwrap().zines.is_empty());
        assert!(
            decode(Some(b"garbage")).is_err(),
            "corrupt state is an error, not a reset"
        );
    }

    #[test]
    fn a_create_lands_at_revision_one_under_a_minted_id() {
        let s = with_zine();
        let z = s.zines.first().unwrap();
        assert_eq!(z.descriptor.revision, 1);
        assert_eq!(z.descriptor.pen_id, PEN, "the identity is the pen name");
        assert_ne!(
            z.descriptor.zine_id, PEN,
            "the id addresses the post, not the author: deriving it from the pen \
             name is what limited a pen name to one post"
        );
        assert!(!z.descriptor.zine_id.is_empty(), "every post has an address");
        assert_eq!(z.descriptor.pen_name, "ada");
        assert!(z.descriptor.prev_hash.is_empty(), "nothing precedes revision 1");
        assert!(z.authored, "this vault wrote it");
        assert_eq!(z.descriptor.created_ms, NOW);
    }

    #[test]
    fn a_pen_name_can_hold_many_posts() {
        let mut s = ZinesState::default();
        let first = create(&mut s, PEN, "ada", draft("First", "<p>one</p>"), NOW);
        let second = create(&mut s, PEN, "ada", draft("Second", "<p>two</p>"), NOW + 10);

        assert_ne!(first, second, "each post gets its own address");
        assert_eq!(s.zines.len(), 2, "the first is not replaced by the second");
        assert!(
            s.zines.iter().all(|z| z.descriptor.pen_id == PEN && z.authored),
            "both are this pen name's"
        );
        assert!(
            s.zines.iter().all(|z| z.descriptor.revision == 1),
            "a new post is not a revision of the one before it"
        );
        assert_eq!(s.index(None, None).len(), 2, "both are in the index");
    }

    #[test]
    fn each_post_carries_its_own_read_key() {
        // One key per zine, and a zine is one post: granting a reader one post
        // must not open the rest of the corpus.
        let mut s = ZinesState::default();
        create(&mut s, PEN, "ada", draft("First", "<p>one</p>"), NOW);
        create(&mut s, PEN, "ada", draft("Second", "<p>two</p>"), NOW + 10);
        let a = s.zines[0].read_key.clone().unwrap();
        let b = s.zines[1].read_key.clone().unwrap();
        assert_ne!(a, b, "two posts do not share a read key");
    }

    #[test]
    fn revising_one_post_leaves_the_other_alone() {
        let mut s = ZinesState::default();
        let first = create(&mut s, PEN, "ada", draft("First", "<p>one</p>"), NOW);
        let second = create(&mut s, PEN, "ada", draft("Second", "<p>two</p>"), NOW + 10);
        let head = content_hash(&s.get(&first).unwrap().descriptor);

        assert_eq!(
            revise(&mut s, &first, PEN, "ada", draft("First, revised", "<p>one v2</p>"), NOW + 20),
            2
        );
        let a = s.get(&first).unwrap();
        assert_eq!(a.descriptor.prev_hash, head, "the chain points at its own head");
        assert_eq!(a.history.len(), 1, "only the revised post has history");
        let b = s.get(&second).unwrap();
        assert_eq!(b.descriptor.revision, 1, "the other post did not move");
        assert_eq!(b.descriptor.title, "Second");
        assert!(b.history.is_empty(), "and gained no history");
    }

    #[test]
    fn an_edit_bumps_the_revision_and_chains_to_the_one_before() {
        let mut s = with_zine();
        let id = the_id(&s);
        let first = content_hash(&s.get(&id).unwrap().descriptor);
        let rev = revise(&mut s, &id, PEN, "ada", draft("Second", "<p>bye</p>"), NOW + 5);
        assert_eq!(rev, 2);
        let z = s.get(&id).unwrap();
        assert_eq!(z.descriptor.prev_hash, first, "the chain points at the head it replaced");
        assert_eq!(z.descriptor.title, "Second");
        assert_eq!(z.history.len(), 1);
        assert_eq!(z.history[0].revision, 1);
        assert_eq!(z.history[0].title, "First");
        assert_eq!(z.history[0].hash, first);
    }

    #[test]
    fn history_is_capped_newest_first() {
        let mut s = with_zine();
        let id = the_id(&s);
        for i in 0..HISTORY_CAP + 5 {
            revise(
                &mut s,
                &id,
                PEN,
                "ada",
                draft(&format!("t{i}"), "<p>x</p>"),
                NOW + i as u64,
            );
        }
        let z = s.get(&id).unwrap();
        assert_eq!(z.history.len(), HISTORY_CAP);
        assert_eq!(
            z.history[0].revision,
            z.descriptor.revision - 1,
            "the newest superseded revision is first"
        );
        assert!(
            z.history.iter().all(|h| h.revision >= 2),
            "the oldest fell off rather than being kept forever"
        );
    }

    #[test]
    fn a_revision_of_a_post_this_vault_only_holds_is_refused() {
        let mut s = ZinesState::default();
        s.zines.push(held_post("bb22", "pen-bob"));
        let err = s
            .write(PEN, "ada", revising("bb22", draft("Mine now", "<p>y</p>")), NOW)
            .unwrap_err();
        assert!(err.contains("only the pen name"), "{err}");
        assert_eq!(s.get("bb22").unwrap().descriptor.title, "Theirs");
    }

    #[test]
    fn a_revision_of_an_unknown_post_is_refused_not_created() {
        // The guard that matters now that a create mints its own id: an edit
        // naming an id this vault does not hold is a stale UI, and treating it
        // as a create would silently resurrect a forgotten post under that id.
        let mut s = ZinesState::default();
        let err = s
            .write(PEN, "ada", revising("nope", draft("x", "<p>b</p>")), NOW)
            .unwrap_err();
        assert!(err.contains("no post with that id"), "{err}");
        assert!(s.zines.is_empty(), "a refused revision stores nothing");

        // An empty id is a caller bug rather than "no id given", which is what
        // an absent field means: it must not fall through to a create either.
        let err = s
            .write(PEN, "ada", revising("", draft("x", "<p>b</p>")), NOW)
            .unwrap_err();
        assert!(err.contains("id of the post"), "{err}");
        assert!(s.zines.is_empty(), "a refused revision stores nothing");
    }

    #[test]
    fn a_body_is_sanitized_at_the_boundary() {
        let mut s = ZinesState::default();
        let id = create(
            &mut s,
            PEN,
            "ada",
            draft("x", "<p>hi</p><script>alert(1)</script><img src=x onerror=1>"),
            NOW,
        );
        let body = &s.get(&id).unwrap().descriptor.body;
        assert!(body.contains("<p>hi</p>"), "{body}");
        assert!(!body.contains("script"), "{body}");
        assert!(!body.contains("onerror"), "{body}");
        assert!(!body.contains("<img"), "{body}");
    }

    #[test]
    fn the_marks_the_toolbar_offers_survive_sanitizing() {
        let mut s = ZinesState::default();
        let id = create(
            &mut s,
            PEN,
            "ada",
            draft(
                "x",
                "<p><strong>b</strong><em>i</em><u>u</u><s>s</s></p><ul><li>l</li></ul><h2>h</h2><blockquote>q</blockquote>",
            ),
            NOW,
        );
        let body = &s.get(&id).unwrap().descriptor.body;
        for tag in ["<strong>", "<em>", "<u>", "<s>", "<ul>", "<li>", "<h2>", "<blockquote>"] {
            assert!(body.contains(tag), "{tag} was stripped from {body}");
        }
    }

    #[test]
    fn an_over_cap_body_is_refused_with_a_reason() {
        let mut s = ZinesState::default();
        let huge = "x".repeat(BODY_SOFT_CAP + 1);
        let err = s.write(PEN, "ada", draft("x", &huge), NOW).unwrap_err();
        assert!(err.contains("cap"), "{err}");
        assert!(s.zines.is_empty(), "a refused write stores nothing");
    }

    #[test]
    fn sections_are_normalised_deduped_and_capped() {
        let mut s = ZinesState::default();
        let id = create(
            &mut s,
            PEN,
            "ada",
            ZineDraft {
                zine_id: None,
                title: "x".into(),
                sections: vec![
                    "  Rust  ".into(),
                    "rust".into(),
                    "Distributed   Systems".into(),
                    "".into(),
                    "   ".into(),
                ],
                body: "<p>b</p>".into(),
            },
            NOW,
        );
        let sections = &s.get(&id).unwrap().descriptor.sections;
        assert_eq!(sections, &vec!["rust".to_string(), "distributed systems".to_string()]);
    }

    #[test]
    fn a_section_label_is_length_capped() {
        let mut s = ZinesState::default();
        let long = "a".repeat(SECTION_MAX_CHARS + 50);
        let id = create(
            &mut s,
            PEN,
            "ada",
            ZineDraft {
                zine_id: None,
                title: "x".into(),
                sections: vec![long],
                body: "<p>b</p>".into(),
            },
            NOW,
        );
        assert_eq!(
            s.get(&id).unwrap().descriptor.sections[0].chars().count(),
            SECTION_MAX_CHARS
        );
    }

    #[test]
    fn the_section_index_counts_and_sorts() {
        let mut s = ZinesState::default();
        create(
            &mut s,
            PEN,
            "ada",
            ZineDraft { zine_id: None, title: "a".into(), sections: vec!["rust".into(), "systems".into()], body: "<p>x</p>".into() },
            NOW,
        );
        create(
            &mut s,
            "pen-bob",
            "bob",
            ZineDraft { zine_id: None, title: "b".into(), sections: vec!["rust".into()], body: "<p>x</p>".into() },
            NOW + 1,
        );
        assert_eq!(
            s.sections(),
            vec![("rust".to_string(), 2), ("systems".to_string(), 1)]
        );
    }

    #[test]
    fn the_index_is_newest_first_and_filters_by_section_and_pen() {
        let mut s = ZinesState::default();
        create(&mut s, PEN, "ada", ZineDraft { zine_id: None, title: "old".into(), sections: vec!["rust".into()], body: "<p>a</p>".into() }, NOW);
        create(&mut s, "pen-bob", "bob", ZineDraft { zine_id: None, title: "new".into(), sections: vec!["go".into()], body: "<p>b</p>".into() }, NOW + 10);

        let all = s.index(None, None);
        assert_eq!(all[0].title, "new", "newest first");
        assert_eq!(all[1].title, "old");

        let rust = s.index(Some("  RUST "), None);
        assert_eq!(rust.len(), 1);
        assert_eq!(rust[0].title, "old");

        let by_pen = s.index(None, Some("AD"));
        assert_eq!(by_pen.len(), 1);
        assert_eq!(by_pen[0].pen_name, "ada");

        // The identity, not the address: a post id is random, so filtering on it
        // would make "everything by this pen name" unanswerable.
        let by_id = s.index(None, Some("pen-bob"));
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].pen_name, "bob");
    }

    #[test]
    fn a_forged_future_timestamp_is_clamped_on_write() {
        let mut s = ZinesState::default();
        let id = create(&mut s, PEN, "ada", draft("x", "<p>b</p>"), NOW);
        // A descriptor arriving from a peer with a timestamp far in the future
        // must not be able to pin itself to the top of the index forever.
        let mut z = s.zines.remove(0);
        z.descriptor.created_ms = u64::MAX;
        s.zines.push(z);
        let clamped = clamp_ms(s.get(&id).unwrap().descriptor.created_ms, NOW);
        assert_eq!(clamped, NOW);
        // And the write path applies the same clamp to a create.
        let mut s2 = ZinesState::default();
        let id2 = create(&mut s2, PEN, "ada", draft("x", "<p>b</p>"), NOW);
        assert!(s2.get(&id2).unwrap().descriptor.created_ms <= NOW + MAX_CLOCK_SKEW_MS);
    }

    #[test]
    fn comments_merge_once_and_read_oldest_first() {
        let mut s = with_zine();
        let id = the_id(&s);
        let comment = |hash: &str, body: &str, at: u64| ZineComment {
            hash: hash.into(),
            author: "bob".into(),
            body: body.into(),
            created_ms: at,
            merged_ms: 0,
        };
        s.merge_comment(&id, comment("h2", "<p>second</p>", NOW + 20), NOW)
            .unwrap();
        s.merge_comment(&id, comment("h1", "<p>first</p>", NOW + 10), NOW)
            .unwrap();
        // The same blob arriving on every drain must not grow the thread.
        s.merge_comment(&id, comment("h1", "<p>first</p>", NOW + 10), NOW)
            .unwrap();
        let comments = &s.get(&id).unwrap().descriptor.comments;
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].hash, "h1", "a thread reads in the order written");
        assert_eq!(comments[1].hash, "h2");
        assert_eq!(comments[0].merged_ms, NOW, "the merge time is local");
    }

    #[test]
    fn an_empty_comment_is_refused_and_a_hostile_one_is_sanitized() {
        let mut s = with_zine();
        let id = the_id(&s);
        let err = s
            .merge_comment(
                &id,
                ZineComment { hash: "h".into(), author: "b".into(), body: "   ".into(), created_ms: NOW, merged_ms: 0 },
                NOW,
            )
            .unwrap_err();
        assert!(err.contains("empty"), "{err}");

        s.merge_comment(
            &id,
            ZineComment {
                hash: "h".into(),
                author: "b".into(),
                body: "<p>ok</p><script>bad()</script>".into(),
                created_ms: NOW,
                merged_ms: 0,
            },
            NOW,
        )
        .unwrap();
        let body = &s.get(&id).unwrap().descriptor.comments[0].body;
        assert!(body.contains("ok"));
        assert!(!body.contains("script"), "{body}");
    }

    #[test]
    fn a_comment_without_a_hash_is_refused() {
        let mut s = with_zine();
        let id = the_id(&s);
        let err = s
            .merge_comment(
                &id,
                ZineComment { hash: String::new(), author: "b".into(), body: "<p>x</p>".into(), created_ms: NOW, merged_ms: 0 },
                NOW,
            )
            .unwrap_err();
        assert!(err.contains("hash"), "{err}");
    }

    #[test]
    fn comments_are_capped_with_a_reason() {
        let mut s = with_zine();
        let id = the_id(&s);
        for i in 0..COMMENTS_CAP {
            s.merge_comment(
                &id,
                ZineComment { hash: format!("h{i}"), author: "b".into(), body: "<p>x</p>".into(), created_ms: NOW + i as u64, merged_ms: 0 },
                NOW,
            )
            .unwrap();
        }
        let err = s
            .merge_comment(
                &id,
                ZineComment { hash: "one-more".into(), author: "b".into(), body: "<p>x</p>".into(), created_ms: NOW, merged_ms: 0 },
                NOW,
            )
            .unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn muting_is_local_and_idempotent() {
        let mut s = with_zine();
        let id = the_id(&s);
        assert!(s.set_muted(&id, true, NOW));
        assert!(s.get(&id).unwrap().muted);
        assert!(!s.set_muted(&id, true, NOW), "setting it again changes nothing");
        assert!(s.index(None, None)[0].muted, "the index reports it");
        assert!(!s.set_muted("nope", true, NOW));
    }

    #[test]
    fn forgetting_removes_the_zine_and_is_idempotent() {
        let mut s = with_zine();
        let id = the_id(&s);
        assert!(s.forget(&id));
        assert!(s.get(&id).is_none());
        assert!(!s.forget(&id));
    }

    #[test]
    fn create_over_the_cap_is_refused() {
        let mut s = ZinesState::default();
        for _ in 0..ZINES_CAP {
            create(&mut s, PEN, "ada", draft("t", "<p>b</p>"), NOW);
        }
        let err = s
            .write(PEN, "ada", draft("t", "<p>b</p>"), NOW)
            .unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn a_zine_without_a_pen_name_is_refused() {
        let mut s = ZinesState::default();
        let err = s.write("", "ada", draft("t", "<p>b</p>"), NOW).unwrap_err();
        assert!(err.contains("pen name"), "{err}");
    }

    #[test]
    fn renaming_the_byline_moves_no_identity_and_hits_every_post() {
        let mut s = ZinesState::default();
        let first = create(&mut s, PEN, "ada", draft("First", "<p>one</p>"), NOW);
        let second = create(&mut s, PEN, "ada", draft("Second", "<p>two</p>"), NOW + 1);

        assert!(s.rename_byline(PEN, "Ada Lovelace", NOW + 2));
        for id in [&first, &second] {
            let z = s.get(id).unwrap();
            assert_eq!(z.descriptor.pen_name, "Ada Lovelace");
            assert_eq!(z.descriptor.zine_id, *id, "the address did not move");
            assert_eq!(z.descriptor.pen_id, PEN, "nor did the identity");
        }
        assert!(
            !s.rename_byline(PEN, "Ada Lovelace", NOW + 3),
            "no-op reports false"
        );
        // Another pen name's byline is not this vault's to move.
        assert!(!s.rename_byline("pen-bob", "Bob", NOW + 4));
    }

    #[test]
    fn the_preview_strips_tags_and_collapses_whitespace() {
        assert_eq!(
            preview_of("<h1>Title</h1>\n<p>one   two</p>"),
            "Title one two"
        );
        assert_eq!(preview_of(""), "");
    }

    #[test]
    fn the_content_hash_ignores_the_chain_field_it_cannot_cover() {
        let mut s = with_zine();
        let id = the_id(&s);
        let a = content_hash(&s.get(&id).unwrap().descriptor);
        s.get_mut(&id).unwrap().descriptor.prev_hash = "deadbeef".into();
        let b = content_hash(&s.get(&id).unwrap().descriptor);
        assert_eq!(a, b, "prev_hash is not part of what it hashes");

        s.get_mut(&id).unwrap().descriptor.body = "<p>different</p>".into();
        let c = content_hash(&s.get(&id).unwrap().descriptor);
        assert_ne!(a, c, "a changed body changes the hash");
    }

    #[test]
    fn state_roundtrips_through_bytes() {
        let mut s = with_zine();
        let id = the_id(&s);
        revise(&mut s, &id, PEN, "ada", draft("Second", "<p>bye</p>"), NOW + 5);
        s.merge_comment(
            &id,
            ZineComment { hash: "h1".into(), author: "bob".into(), body: "<p>hi</p>".into(), created_ms: NOW, merged_ms: 0 },
            NOW,
        )
        .unwrap();
        let back = decode(Some(&s.encode().unwrap())).unwrap();
        let z = back.get(&id).unwrap();
        assert_eq!(z.descriptor.title, "Second");
        assert_eq!(z.descriptor.revision, 2);
        assert_eq!(z.history.len(), 1);
        assert_eq!(z.descriptor.comments.len(), 1);
        assert!(z.authored);
    }

    #[test]
    fn a_post_written_before_the_reshape_is_re_addressed_once() {
        // The legacy shape: the record's id *was* the pen name, which is exactly
        // what limited a pen name to one post. `pen_id` did not exist, so it
        // decodes as empty — that is the marker the migration keys on.
        let mut s = ZinesState::default();
        let mut legacy = held_post(PEN, "");
        legacy.authored = true;
        legacy.unreadable = false;
        legacy.read_key = None;
        s.zines.push(legacy);

        assert!(s.migrate(), "the legacy record is upgraded");
        let z = s.get(PEN);
        assert!(z.is_none(), "the identity is no longer an address");
        let z = s.zines.first().unwrap();
        assert_eq!(z.descriptor.pen_id, PEN, "the identity moved to pen_id");
        assert_ne!(z.descriptor.zine_id, PEN);
        assert_eq!(z.descriptor.pen_name, "someone else", "nothing else moved");
        assert!(z.read_key.is_some(), "the same pass mints the read key");

        let id = the_id(&s);
        assert!(!s.migrate(), "the second pass has nothing to do");
        assert_eq!(s.get(&id).unwrap().descriptor.pen_id, PEN);

        // It survives a round trip, which is what makes it a migration rather
        // than a value that changes on every load.
        let back = decode(Some(&s.encode().unwrap())).unwrap();
        assert_eq!(back.get(&id).unwrap().descriptor.pen_id, PEN);
        assert!(back.get(PEN).is_none());
    }

    #[test]
    fn a_held_post_before_the_reshape_keeps_the_address_its_blob_carries() {
        // A held post is addressed by the blob that carries it: re-addressing it
        // here would make this vault's copy unrecognisable to its author. The
        // identity is still recovered from the legacy id.
        let mut s = ZinesState::default();
        s.zines.push(held_post("bb22", ""));
        assert!(s.migrate());
        let z = s.get("bb22").unwrap();
        assert_eq!(z.descriptor.pen_id, "bb22", "the identity came from the id");
        assert_eq!(z.descriptor.zine_id, "bb22", "the address did not move");
        assert!(z.read_key.is_none(), "a held post still has no key");
        assert!(!s.migrate(), "nothing left to do");
    }
}
