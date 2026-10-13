//! Zines — the members-only long-form corpus: every member's own blog, held in
//! this vault as an encrypted sidecar.
//!
//! Phase 1 was **offline and local**: a zine is authored here, stored here, and
//! read here. Phase 2 adds the wire, and the schema here is that wire schema:
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
//!   so a reader with the key can tell a superseded revision from the head. The
//!   chain is what makes "the newest revision a member holds wins" decidable.
//! - **Comments** are merged, not appended: a comment exists in this vault only
//!   once the author has accepted it. Moderation is entirely author-side, and an
//!   unaccepted comment sits in [`ZineFile::pending_comments`] until it is.
//!
//! What Phase 2 adds on top of that:
//!
//! - [`ZineBlob`] is what is sealed under a post's read key and published: the
//!   descriptor, the author's signing key, and a signature over the descriptor's
//!   canonical bytes. The signature is what makes a revision *the author's* —
//!   every reader holds the read key, so without it any reader could forge an
//!   edit. It is inside the sealed box rather than beside it, because a signing
//!   key in the clear on the relay would be a stable author pseudonym, and the
//!   bulletin is meant to be one flat opaque log.
//! - [`ZineCommentBlob`] is what a commenter seals to the author's pen name and
//!   publishes. Anyone who can read a post can write one, no session needed, and
//!   the author is the only one who can open it.
//! - [`HeldBlob`] is a sealed blob this vault keeps the *bytes* of, whether or
//!   not it can open it yet: unreadable is a state, not an error, because the
//!   key may still arrive and a re-post does not need one.
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
const COMMENTS_CAP: usize = 512;

/// Comments pulled from the bulletin and not yet accepted, per zine. Separate
/// from the merged thread because the author has not decided about these yet.
const PENDING_CAP: usize = 512;

/// Sealed blobs this vault keeps the bytes of, and their total size.
///
/// Both are needed: a count alone lets 512 maximum-size blobs sit in a sidecar
/// that is decrypted on every read, and a byte cap alone lets a flood of tiny
/// blobs grow an unbounded JSON array. Text keeps the real totals in low
/// megabytes; these are the ceiling that makes the worst case finite.
///
/// A cap rather than a limit on the corpus: a blob is the only copy of a
/// revision's bytes, so going over evicts rather than refuses — a drain that
/// refused would lose the blob for good, because the cursor moves on either way.
/// Superseded revisions go first; an unopened blob is worth the most, because a
/// key may still arrive and it may be the only copy.
pub const BLOBS_CAP: usize = 1024;
pub const BLOB_BYTES_CAP: usize = 32 * 1024 * 1024;

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
/// came from. `author` is the byline the commenter claimed: a comment arrives
/// through an anonymous sealed box, so there is no sender to verify and this
/// vault does not decide who they are. It records what was claimed and leaves
/// the judgement to the person reading.
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

/// What is sealed under a post's read key and published to the bulletin.
///
/// The descriptor is the article; `sign_pk` and `signature` are its provenance,
/// and they travel *inside* the sealed box rather than beside it. A signing key
/// in the clear would be a stable pseudonym for its author, which is exactly
/// what the bulletin must not offer: one flat log, nothing to group by.
///
/// The signature covers [`canonical_bytes`] of the descriptor — the descriptor
/// with `prev_hash` cleared, so a revision can be signed before it is chained,
/// and so the same article signed twice yields the same content hash.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineBlob {
    /// Envelope version, so a later change is a refusal rather than a misread.
    #[serde(default)]
    pub v: u32,
    /// The pen name's Ed25519 public key, hex.
    #[serde(default)]
    pub sign_pk: String,
    /// Base64 signature over the descriptor's canonical bytes.
    #[serde(default)]
    pub signature: String,
    pub descriptor: ZineDescriptor,
}

/// The envelope version this build writes and accepts.
pub const BLOB_VERSION: u32 = 1;

impl ZineBlob {
    /// Wrap a descriptor with its provenance, at the current version.
    pub fn new(descriptor: ZineDescriptor, sign_pk: String, signature: String) -> Self {
        Self {
            v: BLOB_VERSION,
            sign_pk,
            signature,
            descriptor,
        }
    }

    /// The bytes a signature is made over and checked against.
    pub fn signed_bytes(&self) -> Vec<u8> {
        canonical_bytes(&self.descriptor)
    }
}

/// What a commenter seals to the author's pen name and publishes.
///
/// No signature and no identity beyond the byline: the box is anonymous, so
/// there is nothing to verify and no sender to record. `zine_id` is inside the
/// sealed plaintext because the author holds many posts and one pen name is the
/// address for all of them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineCommentBlob {
    #[serde(default)]
    pub v: u32,
    pub zine_id: String,
    /// The byline the commenter claimed. Free text, never checked.
    #[serde(default)]
    pub author: String,
    /// Sanitized HTML.
    #[serde(default)]
    pub body: String,
    /// Commenter-declared, clamped by the author on ingest.
    #[serde(default)]
    pub created_ms: u64,
}

impl ZineCommentBlob {
    pub fn new(zine_id: String, author: String, body: String, created_ms: u64) -> Self {
        Self {
            v: BLOB_VERSION,
            zine_id,
            author,
            body,
            created_ms,
        }
    }
}

/// One sealed blob this vault holds the bytes of.
///
/// The bytes are kept, not just the parsed form, for two reasons: a re-post
/// sweep sends them again to keep a zine inside the relay's TTL, and a blob that
/// could not be opened yet is held until a key arrives. Neither needs the
/// plaintext, which is why unreadable is a flag rather than a reason to drop it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HeldBlob {
    /// SHA-256 of `body`, hex: the address the relay dedups on and the value a
    /// grant names.
    pub hash: String,
    /// The sealed bytes, base64.
    pub body: String,
    /// When the relay accepted it, ms, or the local time it was published.
    /// Drives the re-post sweep, oldest first.
    #[serde(default)]
    pub inserted_at: u64,
    /// Whether this vault has opened it. Unreadable is a state, not an error.
    #[serde(default)]
    pub readable: bool,
}

/// A superseded revision, kept so a mistake can be undone and so a holder can
/// answer "is this blob older than what I have?".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZineRevision {
    pub revision: u64,
    /// The content hash of this revision's canonical bytes.
    #[serde(default)]
    pub hash: String,
    /// The address of the sealed blob that carried this revision, when this
    /// vault has seen it. `hash` is derived from the plaintext, so it is only
    /// computable by someone holding the key; this is the value the relay and a
    /// grant name, and it is what a re-post sends.
    #[serde(default)]
    pub blob_hash: String,
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
    /// The address of the sealed blob carrying the head revision, when this
    /// vault has it. This is what a grant names, because a recipient can match
    /// it by hashing bytes rather than by opening anything.
    #[serde(default)]
    pub head_blob: String,
    /// The address a grant named as the newest revision the sender held, when
    /// this vault has not opened it yet. A drain that meets this address opens
    /// it directly with the granted key instead of trying keys on every blob,
    /// which is the whole point of the grant carrying a hash.
    #[serde(default)]
    pub expected_blob: String,
    /// Comments pulled from the bulletin and not yet accepted, newest last.
    /// Author-side only: a reader never sees these.
    #[serde(default)]
    pub pending_comments: Vec<ZineComment>,
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
    /// Sealed blobs this vault holds the bytes of, oldest first. Bounded by
    /// [`BLOBS_CAP`] and [`BLOB_BYTES_CAP`].
    #[serde(default)]
    pub blobs: Vec<HeldBlob>,
    /// The highest bulletin `seq` this vault has processed. The drain is
    /// resumable across restarts because of this: a member that was offline for
    /// a week reads the lane from where it stopped rather than from the start.
    #[serde(default)]
    pub bulletin_seq: i64,
    /// How many blobs the caps evicted since the caller last asked.
    ///
    /// Not persisted: it is a fact about this session's drains rather than about
    /// the corpus, and a count that survived a restart would report an eviction
    /// that happened in another life. A drop is a real loss of bytes, so it is
    /// counted here and reported by the drain instead of passing silently.
    #[serde(skip)]
    pub dropped_since_read: usize,
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
    /// Comments pulled and not yet accepted. Author-side only: it is the count
    /// the writer needs to see, and a reader never has any.
    #[serde(default)]
    pub pending: usize,
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
/// cannot carry markup the reader's sanitizer would strip anyway. Public because
/// the composer applies the same rule before sealing: a commenter should not be
/// able to send a body the author's ingest would have to change, or the hash the
/// thread names would not be the bytes that were written.
pub fn normalize_comment_body(raw: &str) -> String {
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
                pending: z.pending_comments.len(),
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
            advance(file, now_ms);
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
            head_blob: String::new(),
            expected_blob: String::new(),
            pending_comments: Vec::new(),
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

    /// Open a new revision without changing the text.
    ///
    /// What accepting a comment needs. The thread lives inside the descriptor,
    /// so a merge *is* a document change, and a change that kept its revision
    /// number would be refused by roughly half its readers: two heads at the
    /// same number are ordered by content hash, and a merge has no claim to the
    /// greater one. So a merge is a revision like any other edit, and the author
    /// republishes it through the same path.
    ///
    /// Answers with the new revision number, which is what the caller publishes.
    pub fn bump_revision(&mut self, zine_id: &str, now_ms: u64) -> Result<u64, String> {
        let file = self
            .get_mut(zine_id)
            .ok_or_else(|| "no post with that id is held".to_string())?;
        if !file.authored {
            return Err("only the pen name that wrote a zine can publish it".to_string());
        }
        advance(file, now_ms);
        Ok(file.descriptor.revision)
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

    // ---------------------------------------------------------------- blobs
    //
    // The pool is what makes "held, not discarded" true. Nothing here opens a
    // blob — this module holds no keys and does no crypto — so a blob arrives
    // already classified by the caller, and the only decisions made here are
    // whether it is new and whether the caps have to evict something.

    /// Whether this vault holds a blob's bytes, by address.
    pub fn holds_blob(&self, hash: &str) -> bool {
        !hash.is_empty() && self.blobs.iter().any(|b| b.hash == hash)
    }

    /// The bytes of a held blob.
    ///
    /// Test-only: production paths that need the bytes already have them (the
    /// drain holds the slice it is placing, and the re-post sweep takes them
    /// straight off the pool), so this exists to let a test assert that what a
    /// blob went in as is what it comes back out as.
    #[cfg(test)]
    pub fn blob_body(&self, hash: &str) -> Option<Vec<u8>> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let held = self.blobs.iter().find(|b| b.hash == hash)?;
        STANDARD.decode(&held.body).ok()
    }

    /// Keep a sealed blob's bytes, reporting what it cost.
    ///
    /// Idempotent on the address: the same blob arrives on every drain, and a
    /// re-post sends one this vault already has. The caps evict rather than
    /// refuse — a drain that refused would lose the blob for good, because the
    /// cursor moves on either way — so the caller is told how many went and can
    /// say so instead of pretending nothing happened. Eviction takes superseded
    /// revisions first and the newest last: a superseded revision's plaintext is
    /// already in `history`, so its bytes are worth the least, while an
    /// unopened blob may be the only copy of something a later key opens.
    pub fn hold_blob(
        &mut self,
        hash: &str,
        body: &[u8],
        inserted_at: u64,
        readable: bool,
    ) -> BlobHeld {
        use base64::{engine::general_purpose::STANDARD, Engine};
        if hash.is_empty() || body.is_empty() {
            return BlobHeld { added: false, dropped: 0 };
        }
        if self.holds_blob(hash) {
            // Not a no-op: a re-post refreshes the relay's TTL, and a blob that
            // was unreadable may have just been opened by a caller that learned
            // the key. Readable only ever goes up.
            if readable {
                if let Some(b) = self.blobs.iter_mut().find(|b| b.hash == hash) {
                    b.readable = true;
                }
            }
            return BlobHeld { added: false, dropped: 0 };
        }
        self.blobs.push(HeldBlob {
            hash: hash.to_string(),
            body: STANDARD.encode(body),
            inserted_at,
            readable,
        });
        BlobHeld {
            added: true,
            dropped: self.trim_blobs(),
        }
    }

    /// Drop blobs until both caps are met, oldest and least valuable first.
    fn trim_blobs(&mut self) -> usize {
        if self.blobs.len() <= BLOBS_CAP && self.blob_bytes() <= BLOB_BYTES_CAP {
            return 0;
        }
        let ranks: Vec<u8> = self
            .blobs
            .iter()
            .map(|b| self.blob_rank(&b.hash))
            .collect();
        let mut order: Vec<(u8, u64, usize)> = ranks
            .iter()
            .enumerate()
            .map(|(i, r)| (*r, self.blobs[i].inserted_at, i))
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut doomed: Vec<usize> = Vec::new();
        let mut count = self.blobs.len();
        let mut bytes = self.blob_bytes();
        for (_, _, i) in order {
            if count <= BLOBS_CAP && bytes <= BLOB_BYTES_CAP {
                break;
            }
            count -= 1;
            bytes = bytes.saturating_sub(self.blobs[i].body.len());
            doomed.push(i);
        }
        doomed.sort_unstable();
        for i in doomed.iter().rev() {
            self.blobs.remove(*i);
        }
        // Counted as well as returned: the cap is a real loss of held bytes, and
        // the drain reports it rather than letting a corpus quietly shrink.
        self.dropped_since_read += doomed.len();
        doomed.len()
    }

    /// How many blobs the caps evicted since this was last taken, resetting it.
    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped_since_read)
    }

    /// How much a blob is worth keeping: 0 superseded, 1 unknown or unopened,
    /// 2 the head of a post.
    fn blob_rank(&self, hash: &str) -> u8 {
        for z in &self.zines {
            if z.head_blob == hash {
                return 2;
            }
            if z.history.iter().any(|r| r.blob_hash == hash) {
                return 0;
            }
        }
        1
    }

    /// The base64 size of every held blob: what the byte cap actually bounds,
    /// because that is what the sidecar has to hold in memory and decrypt.
    fn blob_bytes(&self) -> usize {
        self.blobs.iter().map(|b| b.body.len()).sum()
    }

    /// Blobs due to be sent again: least recently published first, at most
    /// `limit`, and only those the relay has not seen for `after_ms`.
    ///
    /// The relay refreshes a row's TTL when the same bytes are published again,
    /// so a holder's sweep is what keeps a zine alive past the window without
    /// anyone re-authoring it.
    ///
    /// The age filter is what makes the sweep affordable. The relay's window is
    /// weeks, so re-posting on every poll would spend the member's whole publish
    /// budget on traffic nobody needs; waiting until a blob is a fraction of the
    /// way through the window keeps every held zine alive with one request per
    /// blob per window instead of one per poll.
    pub fn repost_due(&self, now_ms: u64, after_ms: u64, limit: usize) -> Vec<(String, Vec<u8>)> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let mut order: Vec<&HeldBlob> = self.blobs.iter().collect();
        order.sort_by(|a, b| a.inserted_at.cmp(&b.inserted_at).then_with(|| a.hash.cmp(&b.hash)));
        order
            .into_iter()
            .filter(|b| now_ms.saturating_sub(b.inserted_at) >= after_ms)
            .take(limit)
            .filter_map(|b| STANDARD.decode(&b.body).ok().map(|raw| (b.hash.clone(), raw)))
            .collect()
    }

    /// Record that the relay accepted these bytes at `at_ms`, which moves the
    /// blob to the back of the re-post queue.
    pub fn note_published(&mut self, hash: &str, at_ms: u64) {
        if let Some(b) = self.blobs.iter_mut().find(|b| b.hash == hash) {
            b.inserted_at = at_ms;
        }
    }

    /// Record the sealed blob a revision was published as.
    ///
    /// The publisher calls this after sealing and signing — this module holds no
    /// keys and cannot seal — so that a grant can name an address, a re-post can
    /// send bytes, and a reader that already has the blob can recognise it.
    pub fn record_published(
        &mut self,
        zine_id: &str,
        revision: u64,
        hash: &str,
        body: &[u8],
        at_ms: u64,
    ) -> Result<(), String> {
        let file = self
            .get_mut(zine_id)
            .ok_or_else(|| "no post with that id is held".to_string())?;
        if file.descriptor.revision == revision {
            file.head_blob = hash.to_string();
        } else if let Some(rev) = file
            .history
            .iter_mut()
            .find(|r| r.revision == revision)
        {
            rev.blob_hash = hash.to_string();
        }
        self.hold_blob(hash, body, at_ms, true);
        Ok(())
    }

    /// The address a grant should name for this post: the blob carrying its
    /// head, when this vault has it.
    pub fn head_blob(&self, zine_id: &str) -> String {
        self.get(zine_id).map(|z| z.head_blob.clone()).unwrap_or_default()
    }

    /// A post's read key, decoded, when this vault holds one.
    pub fn read_key(&self, zine_id: &str) -> Option<[u8; 32]> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let key = self.get(zine_id)?.read_key.as_deref()?;
        STANDARD.decode(key).ok()?.try_into().ok()
    }

    /// Every read key this vault holds, with the post it opens.
    ///
    /// What a drain needs: a blob off the bulletin says nothing about which post
    /// it belongs to, so the only way to place it is to try the keys, and this is
    /// the set to try. A post whose key has not arrived is simply not in it.
    pub fn read_keys(&self) -> Vec<(String, [u8; 32])> {
        self.zines
            .iter()
            .filter_map(|z| {
                self.read_key(&z.descriptor.zine_id)
                    .map(|k| (z.descriptor.zine_id.clone(), k))
            })
            .collect()
    }

    /// Blobs held that this vault has not opened. The honest status line: they
    /// are kept, they cost space, and a key may still make them readable.
    pub fn unreadable_blobs(&self) -> usize {
        self.blobs.iter().filter(|b| !b.readable).count()
    }

    // ------------------------------------------------------------- ingesting

    /// Take a read key from a grant.
    ///
    /// Returns whether anything changed. A key for a post this vault has never
    /// seen creates a placeholder rather than being dropped: the grant names the
    /// post, the key, and the address of the newest blob the sender held, and
    /// that blob may not have arrived yet. Until it does the post is
    /// [`ZineFile::unreadable`], which is exactly what that flag means.
    ///
    /// Nothing is verified, by design: any holder may re-share a key, and a
    /// grant can only confer read of a post the sender could already read.
    pub fn apply_grant(&mut self, zine_id: &str, key: &[u8], latest_hash: &str, now_ms: u64) -> bool {
        use base64::{engine::general_purpose::STANDARD, Engine};
        if zine_id.is_empty() || key.len() != 32 {
            return false;
        }
        let encoded = STANDARD.encode(key);
        if let Some(file) = self.get_mut(zine_id) {
            if file.read_key.is_none() {
                file.read_key = Some(encoded);
                return true;
            }
            // A second grant for a post already readable is the norm: every
            // holder may re-share, so the same key arrives from several
            // directions. Only a *newer* address is worth recording, because
            // that is the one a later drain can open without trying keys.
            if !latest_hash.is_empty()
                && file.expected_blob != latest_hash
                && file.head_blob != latest_hash
            {
                file.expected_blob = latest_hash.to_string();
                return true;
            }
            return false;
        }
        if self.zines.len() >= ZINES_CAP {
            return false;
        }
        self.zines.push(ZineFile {
            descriptor: ZineDescriptor {
                zine_id: zine_id.to_string(),
                // Unknown until the blob arrives: an empty pen name and a
                // revision of zero are what "not opened yet" looks like.
                pen_id: String::new(),
                pen_name: String::new(),
                title: String::new(),
                sections: Vec::new(),
                created_ms: now_ms,
                revision: 0,
                prev_hash: String::new(),
                body: String::new(),
                comments: Vec::new(),
            },
            history: Vec::new(),
            authored: false,
            read_key: Some(encoded),
            unreadable: true,
            head_blob: String::new(),
            expected_blob: latest_hash.to_string(),
            pending_comments: Vec::new(),
            muted: false,
            updated_ms: now_ms,
        });
        true
    }

    /// Take a signed revision out of the bulletin.
    ///
    /// The signature is checked *here* rather than by the caller, because this
    /// is the one place a revision is admitted and a caller that forgot to check
    /// would let any reader forge an edit: every reader holds the read key, so
    /// the signature is the only thing that makes a revision the author's.
    ///
    /// The key that opened the blob is passed in so a post this vault has never
    /// seen can be created from what the blob says, rather than from a grant
    /// that may not have arrived.
    pub fn ingest_blob(
        &mut self,
        blob: &ZineBlob,
        hash: &str,
        body: &[u8],
        key: [u8; 32],
        now_ms: u64,
    ) -> Result<Ingest, String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        if blob.v != BLOB_VERSION {
            return Err(format!("blob version {} is not understood", blob.v));
        }
        let signature = STANDARD
            .decode(&blob.signature)
            .map_err(|_| "the blob's signature is not base64".to_string())?;
        if !hush_core::verify_pen_signature(&blob.sign_pk, &blob.signed_bytes(), &signature) {
            return Err("the blob is not signed by the pen name it claims".to_string());
        }
        let descriptor = validate_descriptor(&blob.descriptor, now_ms)?;
        let id = descriptor.zine_id.clone();
        let existing = self.get(&id).map(|z| {
            (
                z.descriptor.revision,
                z.head_blob.clone(),
                content_hash(&z.descriptor),
            )
        });
        let Some((head_revision, head_blob, head_content)) = existing else {
            if self.zines.len() >= ZINES_CAP {
                return Err(format!("at most {ZINES_CAP} zines are held"));
            }
            self.hold_blob(hash, body, now_ms, true);
            self.zines.push(ZineFile {
                descriptor,
                history: Vec::new(),
                authored: false,
                read_key: Some(STANDARD.encode(key)),
                unreadable: false,
                head_blob: hash.to_string(),
                expected_blob: String::new(),
                pending_comments: Vec::new(),
                muted: false,
                updated_ms: now_ms,
            });
            return Ok(Ingest::NewPost);
        };

        if hash == head_blob {
            // Our own head coming back around the lane, or a blob this vault
            // already has. The bytes are still worth holding: that is what a
            // re-post sends.
            self.hold_blob(hash, body, now_ms, true);
            return Ok(Ingest::Duplicate);
        }
        if self
            .get(&id)
            .is_some_and(|z| z.history.iter().any(|r| r.blob_hash == hash))
        {
            // A superseded revision this vault already has: the bytes are worth
            // keeping for a re-post, the record is already here.
            self.hold_blob(hash, body, now_ms, true);
            return Ok(Ingest::Duplicate);
        }
        let file = self.get_mut(&id).expect("checked above");
        if !file.descriptor.pen_id.is_empty() && file.descriptor.pen_id != descriptor.pen_id {
            return Err("that post id belongs to another pen name".to_string());
        }
        let newer = match descriptor.revision.cmp(&head_revision) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            // Two devices, one post, the same revision number: the author has
            // forked. Nothing can order them by time — both are signed and
            // honest — so the greater content hash wins. Arbitrary, but
            // deterministic, which is what makes two readers agree.
            std::cmp::Ordering::Equal => {
                let incoming = content_hash(&descriptor);
                incoming > head_content
            }
        };
        if !newer {
            // An older revision is kept, not discarded: it is the undo history
            // and it answers "is this blob older than what I have?".
            let already = file
                .history
                .iter()
                .any(|r| r.revision == descriptor.revision);
            if !already && descriptor.revision > 0 {
                file.history.insert(
                    0,
                    ZineRevision {
                        revision: descriptor.revision,
                        hash: content_hash(&descriptor),
                        blob_hash: hash.to_string(),
                        title: descriptor.title.clone(),
                        body: descriptor.body.clone(),
                        sections: descriptor.sections.clone(),
                        replaced_ms: now_ms,
                    },
                );
                file.history.truncate(HISTORY_CAP);
            }
            self.hold_blob(hash, body, now_ms, true);
            return Ok(Ingest::Superseded);
        }
        // A placeholder — a key that arrived before its blob — is filled, not
        // superseded: there is no revision to keep as history and nothing was
        // known about the post to lose.
        let placeholder = file.descriptor.revision == 0;
        if !placeholder {
            let superseded = ZineRevision {
                revision: file.descriptor.revision,
                hash: head_content,
                blob_hash: head_blob,
                title: file.descriptor.title.clone(),
                body: file.descriptor.body.clone(),
                sections: file.descriptor.sections.clone(),
                replaced_ms: now_ms,
            };
            // A vault cloned onto two devices can hold two revisions under one
            // number. Whichever loses is worth keeping once, not twice.
            if !file
                .history
                .iter()
                .any(|r| r.revision == superseded.revision)
            {
                file.history.insert(0, superseded);
            }
            file.history.truncate(HISTORY_CAP);
        }
        file.descriptor = descriptor;
        file.head_blob = hash.to_string();
        file.expected_blob = String::new();
        file.unreadable = false;
        file.updated_ms = now_ms;
        self.hold_blob(hash, body, now_ms, true);
        Ok(if placeholder {
            Ingest::NewPost
        } else {
            Ingest::Advanced
        })
    }

    /// Hold a comment for a post this vault wrote. Not merged: the author
    /// decides, and an unaccepted comment is never in the thread.
    ///
    /// Idempotent on the blob's address, because the drain runs every few
    /// seconds and a pile that grows on each pass is a pile nobody can read.
    /// `Ok(false)` means the comment was already here, which is the common case.
    pub fn pull_comment(
        &mut self,
        comment: &ZineCommentBlob,
        hash: &str,
        now_ms: u64,
    ) -> Result<bool, String> {
        if comment.v != BLOB_VERSION {
            return Err(format!("comment version {} is not understood", comment.v));
        }
        let body = normalize_comment_body(&comment.body);
        if body.trim().is_empty() {
            return Err("an empty comment is not kept".to_string());
        }
        let file = self
            .get_mut(&comment.zine_id)
            .ok_or_else(|| "no post with that id is held".to_string())?;
        if !file.authored {
            return Err("only the pen name that wrote a post takes its comments".to_string());
        }
        if file
            .descriptor
            .comments
            .iter()
            .chain(file.pending_comments.iter())
            .any(|c| c.hash == hash)
        {
            return Ok(false);
        }
        if file.pending_comments.len() >= PENDING_CAP {
            return Err(format!("at most {PENDING_CAP} comments wait to be read"));
        }
        file.pending_comments.push(ZineComment {
            hash: hash.to_string(),
            author: comment.author.trim().to_string(),
            body,
            created_ms: clamp_ms(comment.created_ms, now_ms),
            merged_ms: 0,
        });
        file.pending_comments
            .sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.hash.cmp(&b.hash)));
        file.updated_ms = now_ms;
        Ok(true)
    }

    /// Accept a comment that was pulled: merge it into the thread and open a
    /// revision to publish it in.
    ///
    /// The thread lives inside the descriptor, so accepting one *is* a document
    /// change, and a change that kept its revision number would be refused by
    /// roughly half its readers: two heads at the same number are ordered by
    /// content hash, and a merge has no claim to the greater one. So a merge
    /// advances the revision like any edit, and the caller publishes it with the
    /// same call an edit uses.
    ///
    /// The bump happens *before* the merge, which is what keeps the chain honest:
    /// the superseded revision is then the one that was actually published, so a
    /// reader holding it computes the same hash this vault records as its
    /// predecessor. Answers with the revision now in force, which the caller
    /// publishes. Accepting a comment already merged is a no-op and does not open
    /// a second revision.
    pub fn accept_comment(&mut self, zine_id: &str, hash: &str, now_ms: u64) -> Result<u64, String> {
        // Everything that can refuse is checked before anything moves: a comment
        // that cannot be merged must not leave a revision behind.
        let comment = {
            let file = self
                .get_mut(zine_id)
                .ok_or_else(|| "no post with that id is held".to_string())?;
            if !file.authored {
                return Err("only the pen name that wrote a post merges its comments".to_string());
            }
            if file.descriptor.comments.iter().any(|c| c.hash == hash) {
                return Ok(file.descriptor.revision);
            }
            let Some(at) = file.pending_comments.iter().position(|c| c.hash == hash) else {
                return Err("no comment with that hash is waiting".to_string());
            };
            if file.descriptor.comments.len() >= COMMENTS_CAP {
                return Err(format!("at most {COMMENTS_CAP} comments are kept"));
            }
            file.pending_comments.remove(at)
        };
        let revision = self.bump_revision(zine_id, now_ms)?;
        self.merge_comment(zine_id, comment, now_ms)?;
        Ok(revision)
    }

    /// Refuse a comment that was pulled: drop it from the pile.
    ///
    /// Not a retraction of anything published — nothing was published — and
    /// `true` only says one was waiting.
    pub fn discard_comment(&mut self, zine_id: &str, hash: &str) -> bool {
        let Some(file) = self.get_mut(zine_id) else {
            return false;
        };
        let before = file.pending_comments.len();
        file.pending_comments.retain(|c| c.hash != hash);
        file.pending_comments.len() != before
    }
}

/// What a drain learned from one blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingest {
    /// A post this vault had never seen.
    NewPost,
    /// A revision newer than the head: it is the head now.
    Advanced,
    /// An older revision than the head, kept in history.
    Superseded,
    /// A blob this vault already had.
    Duplicate,
}

/// What holding a blob cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobHeld {
    /// False when this vault already held that address.
    pub added: bool,
    /// How many older blobs the caps evicted to make room.
    pub dropped: usize,
}

/// Normalise a descriptor that arrived from the wire.
///
/// A received descriptor is not trusted: the signature proves *who* wrote it,
/// not that it is sane, and the author may be running a hostile build. So every
/// field goes through the same boundary the local write path uses — sanitized
/// HTML, normalized sections, a clamped clock, a capped thread — and the
/// identity fields are required rather than defaulted.
fn validate_descriptor(raw: &ZineDescriptor, now_ms: u64) -> Result<ZineDescriptor, String> {
    if raw.zine_id.trim().is_empty() {
        return Err("a blob without a post id is refused".to_string());
    }
    if raw.pen_id.trim().is_empty() {
        return Err("a blob without a pen name is refused".to_string());
    }
    if raw.revision == 0 {
        return Err("a revision chain starts at 1".to_string());
    }
    let body = crate::reads::sanitize_rich_text(&raw.body);
    if body.len() > BODY_SOFT_CAP {
        return Err(format!(
            "the article is over the {} KiB cap",
            BODY_SOFT_CAP / 1024
        ));
    }
    let mut comments: Vec<ZineComment> = raw
        .comments
        .iter()
        .filter(|c| !c.hash.is_empty())
        .take(COMMENTS_CAP)
        .map(|c| ZineComment {
            hash: c.hash.clone(),
            author: c.author.trim().to_string(),
            body: normalize_comment_body(&c.body),
            created_ms: clamp_ms(c.created_ms, now_ms),
            merged_ms: 0,
        })
        .filter(|c| !c.body.trim().is_empty())
        .collect();
    comments.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.hash.cmp(&b.hash)));
    Ok(ZineDescriptor {
        zine_id: raw.zine_id.trim().to_string(),
        pen_id: raw.pen_id.trim().to_string(),
        pen_name: raw.pen_name.trim().to_string(),
        title: raw.title.trim().to_string(),
        sections: normalize_sections(&raw.sections),
        created_ms: clamp_ms(raw.created_ms, now_ms),
        revision: raw.revision,
        prev_hash: raw.prev_hash.clone(),
        body,
        comments,
    })
}

/// Move the current head into history and open the next revision.
///
/// The one place a revision advances, because two callers need it: an edit (new
/// text) and a comment merge (same text, longer thread). Both keep the old head
/// — it is the undo history, and it answers "is this blob older than what I
/// have?" — and both chain the new revision to what the old one hashed to. Two
/// copies of that would be two chances to get the chain wrong.
///
/// The caller mutates the text after this, so `prev_hash` is set before the
/// descriptor changes: it names the revision being replaced, not the one being
/// written.
fn advance(file: &mut ZineFile, now_ms: u64) {
    let head_content = content_hash(&file.descriptor);
    // A vault cloned onto two devices can already hold a revision under this
    // number. Whichever loses is worth keeping once, not twice.
    if !file
        .history
        .iter()
        .any(|r| r.revision == file.descriptor.revision)
    {
        let superseded = ZineRevision {
            revision: file.descriptor.revision,
            hash: head_content.clone(),
            blob_hash: file.head_blob.clone(),
            title: file.descriptor.title.clone(),
            body: file.descriptor.body.clone(),
            sections: file.descriptor.sections.clone(),
            replaced_ms: now_ms,
        };
        file.history.insert(0, superseded);
        file.history.truncate(HISTORY_CAP);
    }
    file.descriptor.prev_hash = head_content;
    file.descriptor.revision += 1;
    file.updated_ms = now_ms;
}

/// The bytes a revision is hashed and signed over.
///
/// Canonical means the serialized form with the one field it cannot cover taken
/// out: `prev_hash`. Clearing it is what lets a hash be computed before the
/// revision is chained, and what lets a signature be made by a layer that does
/// not know the chain. One function, because a hash and a signature over
/// *nearly* the same bytes is a bug waiting to happen.
pub fn canonical_bytes(descriptor: &ZineDescriptor) -> Vec<u8> {
    let canonical = ZineDescriptor {
        prev_hash: String::new(),
        ..descriptor.clone()
    };
    serde_json::to_vec(&canonical).unwrap_or_default()
}

/// The content hash of a descriptor's canonical bytes.
///
/// This is derived from the *plaintext*, so only someone holding the read key
/// can compute it — which is what it is for: the revision chain, where both
/// ends hold the key. The relay's address for a blob is the hash of its sealed
/// bytes instead ([`blob_hash`]), because a holder that cannot open a blob can
/// still hash it, and a grant has to be matchable by one.
pub fn content_hash(descriptor: &ZineDescriptor) -> String {
    hex::encode(hush_core::sha256(&canonical_bytes(descriptor)))
}

/// The address of a sealed blob: SHA-256 of the bytes as they travel.
pub fn blob_hash(body: &[u8]) -> String {
    hex::encode(hush_core::sha256(body))
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

    /// A real Ed25519 pen name for the tests that publish a blob. Fixed, so a
    /// failure is reproducible, and a real keypair, because a signature that was
    /// never really made proves nothing about the check.
    const SIGN_SK: [u8; 32] = [7u8; 32];

    fn sign_pk() -> String {
        use ed25519_dalek::SigningKey;
        hex::encode(SigningKey::from_bytes(&SIGN_SK).verifying_key().to_bytes())
    }

    fn signature_over(bytes: &[u8]) -> String {
        use base64::engine::general_purpose::STANDARD;
        use ed25519_dalek::Signer;
        let sig = ed25519_dalek::SigningKey::from_bytes(&SIGN_SK).sign(bytes);
        STANDARD.encode(sig.to_bytes())
    }

    /// A descriptor as it would arrive from the wire, at revision `rev`.
    fn descriptor(id: &str, rev: u64, body: &str) -> ZineDescriptor {
        ZineDescriptor {
            zine_id: id.to_string(),
            pen_id: PEN.to_string(),
            pen_name: "ada".to_string(),
            title: format!("Revision {rev}"),
            sections: vec!["Notes".to_string()],
            created_ms: NOW,
            revision: rev,
            prev_hash: String::new(),
            body: body.to_string(),
            comments: Vec::new(),
        }
    }

    /// Sign and seal a descriptor the way the publisher does, returning the
    /// envelope and the bytes a reader would hash.
    fn sealed(d: ZineDescriptor) -> (ZineBlob, Vec<u8>, [u8; 32]) {
        let key = [3u8; 32];
        let signature = signature_over(&canonical_bytes(&d));
        let blob = ZineBlob::new(d, sign_pk(), signature);
        let plain = serde_json::to_vec(&blob).unwrap();
        let body = hush_core::seal_symmetric(&key, &plain).unwrap();
        (blob, body, key)
    }

    /// Ingest a revision the way the drain does.
    fn ingest(s: &mut ZinesState, d: ZineDescriptor, now: u64) -> Result<Ingest, String> {
        let (blob, body, key) = sealed(d);
        let hash = blob_hash(&body);
        s.ingest_blob(&blob, &hash, &body, key, now)
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
            head_blob: String::new(),
            expected_blob: String::new(),
            pending_comments: Vec::new(),
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

    // ------------------------------------------------------------- blobs

    #[test]
    fn a_held_blob_is_kept_by_address_and_a_second_copy_changes_nothing() {
        let mut s = ZinesState::default();
        let first = s.hold_blob("aa", b"one", 10, false);
        assert!(first.added && first.dropped == 0);
        assert!(s.holds_blob("aa"));
        assert_eq!(s.blob_body("aa").unwrap(), b"one");
        assert_eq!(s.unreadable_blobs(), 1);

        // The same address again: the drain sees it every few seconds, and a
        // re-post sends one this vault already has.
        let again = s.hold_blob("aa", b"one", 20, false);
        assert!(!again.added, "the address was already held");
        assert_eq!(s.blobs.len(), 1, "not a second copy");
        assert_eq!(s.blobs[0].inserted_at, 10, "and not a new clock");

        // A blob that is opened later only ever goes from unreadable to read.
        s.hold_blob("aa", b"one", 10, true);
        assert_eq!(s.unreadable_blobs(), 0);

        assert!(!s.hold_blob("", b"x", 10, false).added, "no address, no blob");
        assert!(!s.hold_blob("bb", b"", 10, false).added, "no bytes, no blob");
    }

    #[test]
    fn the_blob_caps_evict_superseded_revisions_before_the_head() {
        // A pool has to be bounded, and eviction is not free: a superseded
        // revision's plaintext is already in `history`, while the head is what a
        // reader needs and what a re-post keeps alive.
        let mut s = with_zine();
        let id = the_id(&s);
        s.get_mut(&id).unwrap().head_blob = "head".to_string();
        s.hold_blob("head", b"head", 500, true);
        s.hold_blob("old", b"old", 100, true);
        s.get_mut(&id).unwrap().history.push(ZineRevision {
            revision: 1,
            hash: "h1".into(),
            blob_hash: "old".into(),
            title: String::new(),
            body: String::new(),
            sections: Vec::new(),
            replaced_ms: NOW,
        });

        // Fill the pool past the count cap and force one eviction.
        for i in 0..BLOBS_CAP {
            s.hold_blob(&format!("filler-{i}"), b"x", 200 + i as u64, false);
        }
        assert!(s.blobs.len() <= BLOBS_CAP, "the cap is enforced");
        assert!(s.holds_blob("head"), "the head survives");
        assert!(!s.holds_blob("old"), "the superseded revision went first");
    }

    #[test]
    fn a_repost_batch_is_oldest_first_and_a_publish_moves_a_blob_to_the_back() {
        let mut s = ZinesState::default();
        s.hold_blob("a", b"a", NOW - 30, true);
        s.hold_blob("b", b"b", NOW - 50, true);
        s.hold_blob("c", b"c", NOW - 40, true);
        // Nothing is due until a blob has been on the relay for the interval.
        assert!(
            s.repost_due(NOW, 100, 2).is_empty(),
            "a fresh corpus is not re-posted"
        );
        let batch: Vec<String> = s
            .repost_due(NOW, 20, 2)
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(batch, vec!["b", "c"], "the oldest go first");
        s.note_published("b", NOW);
        let batch: Vec<String> = s
            .repost_due(NOW, 20, 1)
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(batch, vec!["c"], "a re-posted blob goes to the back");
        assert_eq!(s.blob_body("b").unwrap(), b"b", "the bytes come back whole");
    }

    #[test]
    fn every_read_key_is_listed_with_the_post_it_opens() {
        // What a drain has to try against a blob that says nothing about itself.
        let mut s = with_zine();
        let id = the_id(&s);
        let keys = s.read_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, id);
        assert_eq!(Some(keys[0].1), s.read_key(&id));
        // A post with no key is not in the set, rather than a zero key that
        // would silently fail to open anything.
        s.zines[0].read_key = None;
        assert!(s.read_keys().is_empty());
    }

    #[test]
    fn accepting_a_comment_opens_a_revision_chained_to_the_published_one() {
        // A merge changes the descriptor, and a change that kept its revision
        // number would be refused by whichever readers hold the greater hash.
        let mut s = with_zine();
        let id = the_id(&s);
        // What readers hold: revision 1 with no comments. Its hash is the chain
        // link, and it is computable by anyone holding the read key.
        let published_hash = content_hash(&s.get(&id).unwrap().descriptor);
        let blob = ZineCommentBlob::new(id.clone(), "bob".into(), "<p>nice</p>".into(), NOW);
        assert!(s.pull_comment(&blob, "c1", NOW).unwrap());
        assert!(
            s.get(&id).unwrap().descriptor.comments.is_empty(),
            "a pulled comment is not merged"
        );

        assert_eq!(s.accept_comment(&id, "c1", NOW + 5).unwrap(), 2);
        let file = s.get(&id).unwrap();
        assert_eq!(file.descriptor.comments.len(), 1, "the merge is in the head");
        assert_eq!(
            file.descriptor.prev_hash, published_hash,
            "the new revision is chained to the one that was published"
        );
        assert_eq!(
            file.history[0].hash, published_hash,
            "history records the published revision, not the merge"
        );
        assert!(file.pending_comments.is_empty(), "it left the pile");

        // Accepting the same comment again does not open a second revision.
        assert_eq!(s.accept_comment(&id, "c1", NOW + 6).unwrap(), 2);
        assert_eq!(s.get(&id).unwrap().descriptor.revision, 2);
    }

    #[test]
    fn a_comment_that_cannot_be_merged_leaves_no_revision_behind() {
        let mut s = with_zine();
        let id = the_id(&s);
        assert!(s.accept_comment(&id, "nope", NOW).is_err());
        assert_eq!(
            s.get(&id).unwrap().descriptor.revision,
            1,
            "a refused accept is not an edit"
        );
    }

    #[test]
    fn a_holder_cannot_publish_someone_elses_post() {
        let mut s = ZinesState::default();
        let (blob, body, key) = sealed(descriptor("z1", 1, "<p>theirs</p>"));
        let hash = blob_hash(&body);
        s.ingest_blob(&blob, &hash, &body, key, NOW).unwrap();
        assert!(s.bump_revision("z1", NOW).is_err(), "read is not write");
    }

    // --------------------------------------------------------- ingesting

    #[test]
    fn a_grant_for_an_unknown_post_waits_for_the_blob_and_then_opens_it() {
        // The common shape: the key arrives over a session before the blob
        // arrives over the bulletin, so the post exists as a placeholder rather
        // than not at all.
        let mut s = ZinesState::default();
        let key = [3u8; 32];
        let (blob, body, _) = sealed(descriptor("z1", 1, "<p>hello</p>"));
        let hash = blob_hash(&body);

        assert!(s.apply_grant("z1", &key, &hash, NOW));
        let z = s.get("z1").unwrap();
        assert!(z.unreadable, "the key is here, the article is not");
        assert_eq!(z.expected_blob, hash, "the grant named the blob to open");
        assert_eq!(z.descriptor.revision, 0, "nothing is known about it yet");
        assert!(!s.apply_grant("z1", &key, &hash, NOW), "the second is a no-op");
        assert!(!s.apply_grant("z1", &[0u8; 7], &hash, NOW), "a short key is not a key");
        assert!(!s.apply_grant("", &key, &hash, NOW), "an id is needed");

        assert_eq!(s.ingest_blob(&blob, &hash, &body, key, NOW).unwrap(), Ingest::NewPost);
        let z = s.get("z1").unwrap();
        assert!(!z.unreadable, "opened");
        assert!(!z.authored, "held, not written here");
        assert_eq!(z.descriptor.title, "Revision 1");
        assert_eq!(z.head_blob, hash);
        assert_eq!(z.expected_blob, "", "nothing left to wait for");
        assert_eq!(s.read_key("z1").unwrap(), key);
    }

    #[test]
    fn an_unsigned_or_foreign_revision_is_refused() {
        // The read key is shared with every reader, so a signature is the only
        // thing that makes a revision the author's. This is the check that has
        // to hold, and it holds in the store rather than in a caller.
        let mut s = ZinesState::default();
        let key = [3u8; 32];
        let d = descriptor("z1", 1, "<p>hello</p>");

        let unsigned = ZineBlob {
            v: BLOB_VERSION,
            sign_pk: sign_pk(),
            signature: String::new(),
            descriptor: d.clone(),
        };
        let err = s.ingest_blob(&unsigned, "h", b"x", key, NOW).unwrap_err();
        assert!(err.contains("not signed"), "{err}");

        // A body edited after it was signed: the signature is over the bytes
        // that were signed, so the edit is what fails the check.
        let mut edited = d.clone();
        edited.title = "Tampered".into();
        let forged = ZineBlob::new(edited, sign_pk(), signature_over(&canonical_bytes(&d)));
        let err = s.ingest_blob(&forged, "h", b"x", key, NOW).unwrap_err();
        assert!(err.contains("not signed"), "{err}");

        // A signature that is real, but by someone else.
        let other = ZineBlob::new(
            d.clone(),
            hex::encode([9u8; 32]),
            signature_over(&canonical_bytes(&d)),
        );
        assert!(s.ingest_blob(&other, "h", b"x", key, NOW).is_err());

        // An envelope version this build does not understand is refused rather
        // than guessed at.
        let mut future = ZineBlob::new(d.clone(), sign_pk(), signature_over(&canonical_bytes(&d)));
        future.v = BLOB_VERSION + 1;
        assert!(s.ingest_blob(&future, "h", b"x", key, NOW).is_err());

        assert!(s.zines.is_empty(), "nothing was admitted");
    }

    #[test]
    fn a_descriptor_off_the_wire_is_validated_before_it_is_kept() {
        let mut s = ZinesState::default();
        let key = [3u8; 32];

        // A body with a script tag is sanitized, not stored as it arrived: the
        // UI renders this HTML.
        let mut hostile = descriptor("z1", 1, "<p>ok</p><script>alert(1)</script>");
        hostile.sections = vec!["  Notes  ".into(), "notes".into(), "".into()];
        hostile.created_ms = NOW + 10 * 24 * 60 * 60 * 1000;
        let (blob, body, _) = sealed(hostile);
        let hash = blob_hash(&body);
        assert_eq!(s.ingest_blob(&blob, &hash, &body, key, NOW).unwrap(), Ingest::NewPost);
        let z = s.get("z1").unwrap();
        assert!(!z.descriptor.body.contains("script"), "{}", z.descriptor.body);
        assert_eq!(z.descriptor.sections, vec!["notes"], "one section, normalized");
        assert_eq!(z.descriptor.created_ms, NOW, "a future clock is pulled back");

        // A blob with no pen name, and one with no revision, are refused.
        let mut anon = descriptor("z2", 1, "<p>x</p>");
        anon.pen_id = "  ".into();
        assert!(ingest(&mut s, anon, NOW).is_err());
        let mut zero = descriptor("z3", 0, "<p>x</p>");
        zero.revision = 0;
        assert!(ingest(&mut s, zero, NOW).is_err());
    }

    #[test]
    fn a_newer_revision_becomes_the_head_and_the_old_one_goes_to_history() {
        let mut s = ZinesState::default();
        let key = [3u8; 32];
        let (b1, body1, _) = sealed(descriptor("z1", 1, "<p>first</p>"));
        let h1 = blob_hash(&body1);
        assert_eq!(s.ingest_blob(&b1, &h1, &body1, key, NOW).unwrap(), Ingest::NewPost);

        // The same bytes again: the drain runs on a timer.
        assert_eq!(s.ingest_blob(&b1, &h1, &body1, key, NOW).unwrap(), Ingest::Duplicate);
        assert_eq!(s.get("z1").unwrap().descriptor.revision, 1);

        let (b2, body2, _) = sealed(descriptor("z1", 2, "<p>second</p>"));
        let h2 = blob_hash(&body2);
        assert_eq!(s.ingest_blob(&b2, &h2, &body2, key, NOW).unwrap(), Ingest::Advanced);
        let z = s.get("z1").unwrap();
        assert_eq!(z.descriptor.revision, 2);
        assert_eq!(z.descriptor.body, "<p>second</p>");
        assert_eq!(z.head_blob, h2);
        assert_eq!(z.history.len(), 1, "the old head is the undo history");
        assert_eq!(z.history[0].revision, 1);
        assert_eq!(z.history[0].blob_hash, h1, "and it remembers which blob it was");

        // An older revision arriving late is kept, and the head does not move.
        let (b3, body3, _) = sealed(descriptor("z1", 3, "<p>third</p>"));
        let h3 = blob_hash(&body3);
        assert_eq!(s.ingest_blob(&b3, &h3, &body3, key, NOW).unwrap(), Ingest::Advanced);
        let (old, old_body, _) = sealed(descriptor("z1", 1, "<p>first</p>"));
        let old_hash = blob_hash(&old_body);
        assert_eq!(
            s.ingest_blob(&old, &old_hash, &old_body, key, NOW).unwrap(),
            Ingest::Superseded
        );
        assert_eq!(s.get("z1").unwrap().descriptor.revision, 3, "the head stands");
        assert!(s.holds_blob(&old_hash), "and the older bytes are still held");
    }

    #[test]
    fn two_revisions_under_one_number_resolve_the_same_way_everywhere() {
        // A vault cloned onto two devices can publish the same revision twice,
        // and both are honestly signed. Nothing can order them by time, so the
        // rule is deterministic instead: the greater content hash wins. The
        // property that matters is not *which* wins but that two readers agree,
        // so this asserts convergence rather than picking a winner — which also
        // keeps it honest about the descriptor being normalized on the way in.
        let key = [3u8; 32];
        let a = descriptor("z1", 1, "<p>aaaa</p>");
        let b = descriptor("z1", 1, "<p>bbbb</p>");
        let (blob_a, body_a, _) = sealed(a);
        let (blob_b, body_b, _) = sealed(b);
        let hash_a = blob_hash(&body_a);
        let hash_b = blob_hash(&body_b);

        let mut one = ZinesState::default();
        assert_eq!(
            one.ingest_blob(&blob_a, &hash_a, &body_a, key, NOW).unwrap(),
            Ingest::NewPost
        );
        let second = one.ingest_blob(&blob_b, &hash_b, &body_b, key, NOW).unwrap();
        assert_ne!(second, Ingest::NewPost, "the same revision is not a new post");

        let mut two = ZinesState::default();
        assert_eq!(
            two.ingest_blob(&blob_b, &hash_b, &body_b, key, NOW).unwrap(),
            Ingest::NewPost
        );
        assert_ne!(
            two.ingest_blob(&blob_a, &hash_a, &body_a, key, NOW).unwrap(),
            Ingest::NewPost
        );

        let head_one = &one.get("z1").unwrap().descriptor;
        let head_two = &two.get("z1").unwrap().descriptor;
        assert_eq!(head_one.body, head_two.body, "the two vaults agree");
        assert_eq!(head_one.revision, head_two.revision);
        assert_eq!(head_one.title, head_two.title);
        // And the revision that lost is kept as history in both, not thrown away.
        assert_eq!(one.get("z1").unwrap().history.len(), 1);
        assert_eq!(two.get("z1").unwrap().history.len(), 1);
    }

    #[test]
    fn a_post_id_cannot_be_revised_by_another_pen_name() {
        let mut s = ZinesState::default();
        let key = [3u8; 32];
        let (b1, body1, _) = sealed(descriptor("z1", 1, "<p>mine</p>"));
        let h1 = blob_hash(&body1);
        s.ingest_blob(&b1, &h1, &body1, key, NOW).unwrap();

        let mut stolen = descriptor("z1", 2, "<p>not mine</p>");
        stolen.pen_id = "pen-eve".into();
        let (b2, body2, _) = sealed(stolen);
        let h2 = blob_hash(&body2);
        let err = s.ingest_blob(&b2, &h2, &body2, key, NOW).unwrap_err();
        assert!(err.contains("another pen name"), "{err}");
        assert_eq!(s.get("z1").unwrap().descriptor.body, "<p>mine</p>");
    }

    // ---------------------------------------------------------- comments

    #[test]
    fn a_pulled_comment_waits_to_be_accepted_and_is_merged_only_then() {
        let mut s = with_zine();
        let id = the_id(&s);
        let blob = ZineCommentBlob::new(id.clone(), "bob".into(), "<p>nice one</p>".into(), NOW - 1000);

        assert!(s.pull_comment(&blob, "c1", NOW).unwrap());
        assert!(!s.pull_comment(&blob, "c1", NOW).unwrap(), "idempotent on the address");
        let z = s.get(&id).unwrap();
        assert_eq!(z.pending_comments.len(), 1);
        assert_eq!(z.pending_comments[0].merged_ms, 0, "not merged yet");
        assert!(z.descriptor.comments.is_empty(), "and not in the thread");

        assert!(s.accept_comment(&id, "c1", NOW).is_ok());
        let z = s.get(&id).unwrap();
        assert!(z.pending_comments.is_empty(), "accepted, so no longer waiting");
        assert_eq!(z.descriptor.comments.len(), 1);
        assert_eq!(z.descriptor.comments[0].body, "<p>nice one</p>");
        assert_eq!(z.descriptor.comments[0].merged_ms, NOW);
        assert_eq!(
            s.accept_comment(&id, "c1", NOW).unwrap(),
            2,
            "accepting again is a no-op, not a second revision"
        );
        let z = s.get(&id).unwrap();
        assert_eq!(z.descriptor.comments.len(), 1, "the comment is not duplicated");
        assert_eq!(z.descriptor.revision, 2);
        assert!(
            s.accept_comment(&id, "never-pulled", NOW).is_err(),
            "a hash that was never pulled is refused"
        );

        // A discarded comment is dropped, and that is not a retraction of
        // anything: nothing was ever published.
        assert!(s.pull_comment(&blob, "c2", NOW).unwrap());
        assert!(s.discard_comment(&id, "c2"));
        assert!(!s.discard_comment(&id, "c2"));
        assert!(s.get(&id).unwrap().pending_comments.is_empty());
    }

    #[test]
    fn a_comment_is_only_held_for_a_post_this_vault_wrote() {
        // A reader that pulled a comment would be holding something it can
        // neither merge nor republish: the thread lives in the author's
        // descriptor and only the author can sign a revision of it.
        let mut s = ZinesState::default();
        let key = [3u8; 32];
        let (blob, body, _) = sealed(descriptor("z1", 1, "<p>theirs</p>"));
        let hash = blob_hash(&body);
        s.ingest_blob(&blob, &hash, &body, key, NOW).unwrap();

        let comment = ZineCommentBlob::new("z1".into(), "bob".into(), "<p>hi</p>".into(), NOW);
        let err = s.pull_comment(&comment, "c1", NOW).unwrap_err();
        assert!(err.contains("takes its comments"), "{err}");
        assert!(s.get("z1").unwrap().pending_comments.is_empty());

        // An empty body, and a comment for a post this vault has never seen.
        let empty = ZineCommentBlob::new("z1".into(), "bob".into(), "  ".into(), NOW);
        assert!(s.pull_comment(&empty, "c2", NOW).is_err());
        let stray = ZineCommentBlob::new("nope".into(), "bob".into(), "<p>hi</p>".into(), NOW);
        assert!(s.pull_comment(&stray, "c3", NOW).is_err());
    }

    #[test]
    fn a_published_revision_records_the_address_a_grant_names() {
        let mut s = with_zine();
        let id = the_id(&s);
        assert_eq!(s.head_blob(&id), "", "nothing published yet");
        s.record_published(&id, 1, "hh", b"bytes", NOW).unwrap();
        assert_eq!(s.head_blob(&id), "hh");
        assert!(s.holds_blob("hh"), "the bytes are held for a re-post");
        assert_eq!(s.blob_body("hh").unwrap(), b"bytes");
        assert!(s.record_published("nope", 1, "hh", b"bytes", NOW).is_err());
    }

    #[test]
    fn the_whole_pool_survives_a_round_trip_through_the_sidecar() {
        let mut s = with_zine();
        let id = the_id(&s);
        let (blob, body, key) = sealed(descriptor("held", 2, "<p>theirs</p>"));
        let hash = blob_hash(&body);
        s.apply_grant("held", &key, &hash, NOW);
        s.ingest_blob(&blob, &hash, &body, key, NOW).unwrap();
        s.record_published(&id, 1, "own", b"own bytes", NOW).unwrap();
        s.bulletin_seq = 42;
        s.hold_blob("unopened", b"sealed", NOW, false);

        let back = decode(Some(&s.encode().unwrap())).unwrap();
        assert_eq!(back.bulletin_seq, 42, "the drain resumes where it stopped");
        assert_eq!(back.get(&id).unwrap().head_blob, "own");
        assert_eq!(back.get("held").unwrap().head_blob, hash);
        assert_eq!(back.read_key("held").unwrap(), key, "the key came through");
        assert_eq!(back.blobs.len(), 3);
        assert_eq!(back.unreadable_blobs(), 1);
        assert_eq!(back.blob_body("unopened").unwrap(), b"sealed");
    }
}
