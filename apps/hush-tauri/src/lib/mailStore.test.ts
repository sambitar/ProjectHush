import { test } from "node:test";
import assert from "node:assert/strict";
import {
  dropSearchText,
  folderBadge,
  loadMailbox,
  mailAttachmentCounts,
  mailSearchText,
  setKnownContacts,
  type DropMessage,
  type MailFlags,
  type MailMessage,
} from "./mailStore.ts";
import type { ChatRecordDto } from "./api/types.ts";

// What a mail or drop search looks at. The list stamps this on each row and the
// row filter matches the query against it, so these are the assertions that keep
// the row filter from quietly narrowing what search means.
//
// Bodies here are plain text. `htmlToText` strips tags through a detached
// element, which needs a DOM this runner does not have; the tag-stripping path
// is `htmlToText`'s own behaviour, and it is only reached when a body has tags.

const mail = (over: Partial<MailMessage> = {}): MailMessage => ({
  id: "m1",
  threadId: "t1",
  contactNick: "alice",
  outgoing: false,
  subject: "Quarterly report",
  body: "The numbers are in.",
  attachments: [],
  createdAt: 1,
  state: "sent",
  ...over,
});

const drop = (over: Partial<DropMessage> = {}): DropMessage => ({
  id: "d1",
  rendezvousId: "0".repeat(32),
  subject: "Anonymous tip",
  body: "Look at the pier.",
  attachments: [],
  createdAt: 1,
  receivedAt: 2,
  ...over,
});

test("a mail row matches on who it is with, the subject, and the body", () => {
  const hay = mailSearchText(mail());
  assert.ok(hay.includes("alice"));
  assert.ok(hay.includes("quarterly report"));
  assert.ok(hay.includes("the numbers are in."));
});

test("a mail haystack is lowercased, so a query is compared like with like", () => {
  assert.equal(mailSearchText(mail()), mailSearchText(mail()).toLowerCase());
});

test("a mail with no subject still matches on its sender and body", () => {
  const hay = mailSearchText(mail({ subject: undefined }));
  assert.ok(hay.includes("alice"));
  assert.ok(hay.includes("the numbers are in."));
  // The absent subject leaves no empty line, so no query can match across the
  // seam it would have made.
  assert.equal(hay.includes("\n\n"), false);
});

test("a drop matches on its subject and body", () => {
  const hay = dropSearchText(drop());
  assert.ok(hay.includes("anonymous tip"));
  assert.ok(hay.includes("look at the pier."));
});

test("a drop has no sender to match on", () => {
  // Every drop's who column reads "Anonymous", so a haystack carrying that
  // literal would make the word match every drop in the folder. The subject and
  // body here do not contain it, so neither may the haystack.
  const hay = dropSearchText(drop({ subject: "Pier", body: "Look at the pier." }));
  assert.equal(hay.includes("anonymous"), false);
  assert.ok(hay.includes("pier"));
});

test("a drop with no subject still matches on its body", () => {
  const hay = dropSearchText(drop({ subject: undefined }));
  assert.ok(hay.includes("look at the pier."));
  assert.equal(hay.includes("\n\n"), false);
});

// `mailAttachmentCounts` replaced a per-contact helper that both callers asked
// once per contact, which re-read the whole store each time. Its job is to be
// the same number for every contact, computed once — so the tests pin the folder
// arithmetic and the two ways a message could be counted twice or counted when
// it should not be.
//
// These are the first tests in this file to touch the store itself rather than
// free functions, and the store is module state: `loadMailbox` replaces it and
// `setKnownContacts` replaces the contact list for everything that follows in
// the file. Every test below therefore sets both for itself. A test appended
// after these cannot assume the default empty contact list.

const noFlags = (over: Partial<MailFlags> = {}): MailFlags => ({
  read: new Set(),
  starred: new Set(),
  archived: new Set(),
  trashed: new Set(),
  ...over,
});

/** A mail record as the daemon hands it over. `state` is `sent` unless asked
 *  otherwise, because `queued` is what puts a message in the Outbox. */
const record = (
  id: string,
  nick: string,
  ts: number,
  attachments: number,
  over: { outgoing?: boolean; state?: "sent" | "queued" } = {},
): ChatRecordDto => ({
  id,
  contact_nick: nick,
  outgoing: over.outgoing ?? false,
  ts_ms: ts,
  payload: {
    t: "mail",
    thread_id: `t-${id}`,
    subject: id,
    body: "body",
    attachments: Array.from({ length: attachments }, (_, i) => ({
      name: `${id}-${i}`,
      size: 1,
      blob_id_hex: `${id}${i}`.padEnd(64, "0"),
    })),
    state: over.state ?? "sent",
  },
});

test("attachments are summed per contact across inbox, sent and archive", () => {
  setKnownContacts(["alice", "bob", "erin"]);
  loadMailbox(
    [
      record("a1", "alice", 1, 2),
      record("a2", "alice", 2, 1, { outgoing: true }),
      record("b1", "bob", 3, 3),
      record("e1", "erin", 4, 4, { outgoing: true }),
    ],
    [],
  );
  const counts = mailAttachmentCounts(noFlags());
  assert.equal(counts.get("alice"), 3);
  assert.equal(counts.get("bob"), 3);
  assert.equal(counts.get("erin"), 4);
});

test("a message that sits in two folders contributes its attachments once", () => {
  setKnownContacts(["erin"]);
  // A sent mail that was also archived is in `sent` *and* `archive`, because
  // `inFolder` answers for each folder independently. Counting the folders
  // without deduplicating by id would report this contact's four attachments as
  // eight.
  loadMailbox([record("e1", "erin", 1, 4, { outgoing: true })], [], []);
  const counts = mailAttachmentCounts(noFlags({ archived: new Set(["e1"]) }));
  assert.equal(counts.get("erin"), 4);
});

test("a contact whose mail has no attachments is absent, not zero", () => {
  setKnownContacts(["carol"]);
  loadMailbox([record("c1", "carol", 1, 0)], [], []);
  const counts = mailAttachmentCounts(noFlags());
  // Absent rather than present-with-zero: the callers read it with `?? 0`, so
  // the two are equivalent, and a stored zero would be a second representation
  // of the same fact.
  assert.equal(counts.has("carol"), false);
});

test("outbox mail is not exchanged yet, so it does not count", () => {
  setKnownContacts(["dave"]);
  // Queued means the relay has not taken it: the Outbox is holding it, so no
  // file has been exchanged. `sent` requires `state === "sent"`.
  loadMailbox([record("d1", "dave", 1, 5, { outgoing: true, state: "queued" })], [], []);
  assert.equal(mailAttachmentCounts(noFlags()).has("dave"), false);
});

test("trashed mail is disowned, so it does not count", () => {
  setKnownContacts(["alice"]);
  loadMailbox([record("a1", "alice", 1, 2)], [], []);
  assert.equal(mailAttachmentCounts(noFlags()).get("alice"), 2);
  // Trash wins over every folder, so the same store with the message trashed
  // reports nothing.
  assert.equal(
    mailAttachmentCounts(noFlags({ trashed: new Set(["a1"]) })).has("alice"),
    false,
  );
});

test("mail from someone no longer in the circle does not count", () => {
  setKnownContacts(["alice"]);
  loadMailbox([record("a1", "alice", 1, 2), record("z1", "zoe", 2, 9)], [], []);
  const counts = mailAttachmentCounts(noFlags());
  assert.equal(counts.get("alice"), 2);
  assert.equal(counts.has("zoe"), false);
});

// --- The folder badges ---
//
// The folder column draws one number per folder, and a mail reload patches
// those numbers in place instead of repainting the shell. Both read
// `folderBadge`, so these pin the read rather than either caller.

/** A drop record as the daemon hands it over, which is not the `DropMessage`
 *  the search tests use: this is what `loadMailbox` consumes. */
const dropRecord = (id: string, ts: number): ChatRecordDto => ({
  id,
  contact_nick: "",
  outgoing: false,
  ts_ms: ts,
  payload: {
    t: "drop",
    rendezvous_id: "0".repeat(32),
    subject: id,
    body: "body",
    attachments: [],
    created_unix: 1,
  },
});

/** A draft record, likewise: `loadMailbox` reads drafts from their own payload
 *  kind, so a `mail` record in the drafts argument is skipped rather than
 *  counted. */
const draftRecord = (id: string, ts: number): ChatRecordDto => ({
  id,
  contact_nick: "alice",
  outgoing: true,
  ts_ms: ts,
  payload: {
    t: "draft",
    to: "alice",
    subject: id,
    body: "body",
    attachments: [],
    updated_ms: ts,
  },
});

test("the Inbox badge leads with what is unread in it", () => {
  setKnownContacts(["alice"]);
  loadMailbox([record("a1", "alice", 1, 0), record("a2", "alice", 2, 0)], [], []);
  // Two in the box and neither read, so the badge is 2 and it reads as unread.
  assert.deepEqual(folderBadge("inbox", noFlags()), { count: 2, unread: true });
});

test("the Inbox badge falls back to the whole box once it has all been read", () => {
  setKnownContacts(["alice"]);
  loadMailbox([record("a1", "alice", 1, 0), record("a2", "alice", 2, 0)], [], []);
  // Read mail still belongs to the folder, so the number stays; what changes is
  // that it is a count rather than a summons, which is the `unread` flag.
  assert.deepEqual(folderBadge("inbox", noFlags({ read: new Set(["a1", "a2"]) })), {
    count: 2,
    unread: false,
  });
});

test("a folder with nothing to show reports zero, which is no badge at all", () => {
  setKnownContacts([]);
  loadMailbox([], [], []);
  assert.deepEqual(folderBadge("inbox", noFlags()), { count: 0, unread: false });
  assert.deepEqual(folderBadge("trash", noFlags()), { count: 0, unread: false });
});

test("the Drops badge counts only the unopened drops the folder still shows", () => {
  setKnownContacts([]);
  loadMailbox(
    [],
    [],
    [dropRecord("d1", 1), dropRecord("d2", 2), dropRecord("d3", 3)],
  );
  // d1 filed away and d2 opened: one drop is left and it is unopened, so the
  // badge is 1 rather than 3 or 2.
  assert.deepEqual(
    folderBadge("drops", noFlags({ archived: new Set(["d1"]), read: new Set(["d2"]) })),
    { count: 1, unread: true },
  );
});

test("a folder with no unread notion shows its whole contents, unread or not", () => {
  // `listDrafts` drops drafts addressed to someone outside the circle, so the
  // contact has to be known for the draft to be in the folder at all.
  setKnownContacts(["alice"]);
  loadMailbox([], [draftRecord("dr1", 1)], []);
  assert.deepEqual(folderBadge("drafts", noFlags()), { count: 1, unread: false });
});
