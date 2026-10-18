import { test } from "node:test";
import assert from "node:assert/strict";
import {
  chatRowShapes,
  conclaveCardShape,
  conclavesKey,
  convListKey,
  dropsPaneShape,
  mailFoldersKey,
  railKey,
  readsListKey,
  readsListShape,
  readsMainKey,
  readsMainShape,
  readsNavKey,
  ringPromptKey,
  trustRowShape,
  trustsKey,
  settingsShape,
  torrentCardShape,
  torrentsPaneKey,
  torrentsPaneShape,
  vaultBodyKey,
  zinesPageKey,
  zinesPageShape,
  zinesPaneShape,
  type DropsPaneFacts,
  type ReadsListChrome,
  type ReadsMainFacts,
  type ReadsStoryShape,
  type RowShape,
  type TorrentCardFacts,
  type TorrentLibraryRowFacts,
  type TorrentsPaneFacts,
  type ZinesPageFacts,
  type ZinesPaneFacts,
} from "./regionKeys.ts";

const row = (name: string, shape: string): RowShape => ({ name, shape });

// --- The rail ---

test("the rail key is the current rail and the torrent count, and nothing else", () => {
  // Pinned exactly, on purpose. An unread count added to this key would rebuild
  // the rail on every arrival, and those counts are precisely what
  // `patchRailBadges` and the IRC writer keep up to date without a rebuild.
  assert.equal(railKey("chats", 0), "chats\n0");
  assert.equal(railKey("torrents", 3), "torrents\n3");
});

test("the rail key moves when the rail or the torrent count moves", () => {
  assert.notEqual(railKey("chats", 0), railKey("trusts", 0));
  assert.notEqual(railKey("chats", 0), railKey("chats", 1));
});

// --- The Chats list ---

test("the chats key is the body, the appear-offline flag and the rows", () => {
  // Pinned exactly, on purpose: the query is not in it, and neither is anything
  // `patchConvRows` rewrites. A field added here costs the search field its
  // caret, which is what the key is for.
  assert.equal(
    convListKey([row("ana", "full"), row("bo", "mail-only")], "rows", false),
    "rows\nonline\nana\nfull\nbo\nmail-only",
  );
});

test("a chat appearing, going, or reordering moves the chats key", () => {
  const one = convListKey([row("ana", "full")], "rows", false);
  const two = convListKey([row("ana", "full"), row("bo", "full")], "rows", false);
  assert.notEqual(one, two);
  assert.notEqual(
    one,
    convListKey([row("bo", "full")], "rows", false),
  );
  assert.notEqual(
    two,
    convListKey([row("bo", "full"), row("ana", "full")], "rows", false),
  );
});

test("a chat row's shape is the protocol its avatar is drawn from", () => {
  assert.deepEqual(
    chatRowShapes([{ shownName: "ana", protocol: "nostr", reach: "full" }]),
    [{ name: "ana", shape: "nostr" }],
  );
});

test("a reach moving does not move the chats key", () => {
  // The dot the reach decides is rewritten in place by `patchConvRows`, and a
  // reach that moves off `full` takes the row off this list altogether, so the
  // row set carries it. Reading the reach here would rebuild the column — and
  // take the caret out of its search field — on the very render a reach move
  // causes, which is the render this key has to survive.
  const full = chatRowShapes([
    { shownName: "ana", protocol: "nostr", reach: "full" },
  ]);
  const mailOnly = chatRowShapes([
    { shownName: "ana", protocol: "nostr", reach: "mail_only" },
  ]);
  assert.deepEqual(full, mailOnly);
  assert.equal(
    convListKey(full, "rows", false),
    convListKey(mailOnly, "rows", false),
  );
});

test("the two nothings are told apart from each other and from rows", () => {
  assert.notEqual(
    convListKey([], "rows", false),
    convListKey([], "no-trusts", false),
  );
  assert.notEqual(
    convListKey([], "no-trusts", false),
    convListKey([], "mail-only", false),
  );
});

test("appearing offline moves the chats key: that row is drawn above the search", () => {
  assert.notEqual(
    convListKey([], "rows", false),
    convListKey([], "rows", true),
  );
});

// --- The Mail rail's folder column ---

test("the folder column's key is the address line, and only that", () => {
  // Pinned exactly. The counts and the active mark are patched in place, and the
  // folder set and its labels are written out in the builder, so a folder switch
  // and a mail arriving must both leave this key where it is — which is what
  // keeps the focus on the folder button the user just pressed.
  assert.equal(mailFoldersKey("ana@home"), "mailFolders:ana@home");
  assert.equal(mailFoldersKey(null), "mailFolders:none");
});

test("an address set, changed or cleared moves the folder column's key", () => {
  assert.notEqual(mailFoldersKey(null), mailFoldersKey("ana@home"));
  assert.notEqual(mailFoldersKey("ana@home"), mailFoldersKey("ana@work"));
});

// --- The Conclaves column ---

test("a card's shape is the ended flag, the member count and the voice chip", () => {
  assert.equal(
    conclaveCardShape({ ended: false, trustees: 3, voiceLive: false }),
    "active\n3\nquiet",
  );
  assert.equal(
    conclaveCardShape({ ended: true, trustees: 0, voiceLive: true }),
    "ended\n0\nvoice",
  );
});

test("each of the three moves a card's shape, and none of them is a patch's", () => {
  // The three the card draws and `patchConclaveCards` does not rewrite. The
  // unread pill, the preview and the selected mark are the patch's, so they are
  // not here — a shape that carried them would rebuild the column, and take the
  // caret out of the filter field, for a number changing.
  const base = { ended: false, trustees: 3, voiceLive: false };
  assert.notEqual(
    conclaveCardShape(base),
    conclaveCardShape({ ...base, ended: true }),
  );
  assert.notEqual(
    conclaveCardShape(base),
    conclaveCardShape({ ...base, trustees: 2 }),
  );
  assert.notEqual(
    conclaveCardShape(base),
    conclaveCardShape({ ...base, voiceLive: true }),
  );
});

test("the conclaves key is the body, the trust flag and the cards", () => {
  assert.equal(
    conclavesKey("list", true, [row("Book club", "active\n3\nquiet")]),
    "list\ncontacts\nBook club\nactive\n3\nquiet",
  );
});

test("the empty state and the no-trusts case are keys of their own", () => {
  assert.notEqual(
    conclavesKey("empty", false, []),
    conclavesKey("empty", true, []),
  );
  assert.notEqual(conclavesKey("empty", true, []), conclavesKey("list", true, []));
  assert.equal(conclavesKey("empty", false, []), "empty\nno-contacts\n");
});

test("a card renamed, added or reordered moves the conclaves key", () => {
  const one = conclavesKey("list", true, [row("Book club", "active\n3\nquiet")]);
  assert.notEqual(
    one,
    conclavesKey("list", true, [row("Book club 2", "active\n3\nquiet")]),
  );
  assert.notEqual(
    one,
    conclavesKey("list", true, [
      row("Book club", "active\n3\nquiet"),
      row("Other", "active\n2\nquiet"),
    ]),
  );
  assert.notEqual(
    conclavesKey("list", true, [
      row("A", "active\n3\nquiet"),
      row("B", "active\n3\nquiet"),
    ]),
    conclavesKey("list", true, [
      row("B", "active\n3\nquiet"),
      row("A", "active\n3\nquiet"),
    ]),
  );
});

// --- The ring prompt ---

test("the ring prompt key is the ring and the two names on it", () => {
  // Pinned exactly. The room's joined count and the ring's remaining time are
  // deliberately absent: both make the poll render, neither is drawn on the
  // prompt, and a key that grew either one would rebuild the prompt — and take
  // the focus off "Answer" — every time someone else joined the call.
  assert.equal(
    ringPromptKey({ sessionId: "s1", who: "Ana", where: "Book club" }),
    "s1\nAna\nBook club",
  );
});

test("no ring is a key of its own", () => {
  assert.equal(ringPromptKey(null), "none");
  assert.notEqual(
    ringPromptKey(null),
    ringPromptKey({ sessionId: "none", who: "", where: "" }),
  );
});

test("a different ring, opener or conclave moves the ring prompt key", () => {
  const base = { sessionId: "s1", who: "Ana", where: "Book club" };
  assert.notEqual(ringPromptKey(base), ringPromptKey({ ...base, sessionId: "s2" }));
  assert.notEqual(ringPromptKey(base), ringPromptKey({ ...base, who: "Bo" }));
  assert.notEqual(ringPromptKey(base), ringPromptKey({ ...base, where: "Other" }));
});

// --- The Trusts pane ---

test("the trusts key is every row's shown name and reach", () => {
  assert.equal(
    trustsKey([row("Ana", "full"), row("Bo", "mail-only")]),
    "Ana\nfull\nBo\nmail-only",
  );
});

test("a reach moving anywhere moves the trusts key, not only on the row that moved", () => {
  // A row's menu is built from the trust list rather than from its own contact,
  // so one contact's reach can change what another row offers.
  assert.notEqual(
    trustsKey([row("Ana", "full"), row("Bo", "full")]),
    trustsKey([row("Ana", "mail-only"), row("Bo", "full")]),
  );
});

test("a contact appearing, going, or reordering moves the trusts key", () => {
  const one = trustsKey([row("Ana", "full")]);
  const two = trustsKey([row("Ana", "full"), row("Bo", "full")]);
  assert.notEqual(one, two);
  assert.notEqual(two, trustsKey([row("Bo", "full"), row("Ana", "full")]));
});

test("the empty pane has a key of its own", () => {
  // The pane draws an empty state instead of a filter and rows, so this key
  // cannot be confused with a pane holding one row whose name is empty.
  assert.equal(trustsKey([]), "");
  assert.notEqual(trustsKey([]), trustsKey([row("Ana", "full")]));
});

test("a trusts row's shape is both reaches and the mail address", () => {
  assert.equal(trustRowShape({ reach: "full", my_reach: "full" }), "full\nfull\n\n");
  assert.equal(
    trustRowShape({
      reach: "mail_only",
      my_reach: "full",
      mail_handle: "ana@example.org",
      mail_vault: "vault-1",
    }),
    "mail_only\nfull\nana@example.org\nvault-1",
  );
});

test("a reach moving on either side moves a trusts row's shape", () => {
  // The badge and the menu are drawn from both sides of it: mine is what the
  // toggle edits, theirs is what the other side allowed.
  const base = { reach: "full", my_reach: "full" } as const;
  assert.notEqual(
    trustRowShape(base),
    trustRowShape({ ...base, reach: "mail_only" }),
  );
  assert.notEqual(
    trustRowShape(base),
    trustRowShape({ ...base, my_reach: "mail_only" }),
  );
});

test("a mail address appearing moves a trusts row's shape", () => {
  const base = { reach: "full", my_reach: "full" } as const;
  assert.notEqual(
    trustRowShape(base),
    trustRowShape({ ...base, mail_handle: "ana@example.org" }),
  );
  assert.notEqual(
    trustRowShape({ ...base, mail_handle: "ana@example.org" }),
    trustRowShape({
      ...base,
      mail_handle: "ana@example.org",
      mail_vault: "vault-1",
    }),
  );
});

test("the trusts key pairs each shown name with its row's shape", () => {
  // What the caller does with `trustRowShape`: the pane's rows are the names it
  // shows, each carrying the shape its own row and menu were drawn from.
  assert.equal(
    trustsKey([{ name: "Ana", shape: trustRowShape({ reach: "full", my_reach: "full" }) }]),
    "Ana\nfull\nfull\n\n",
  );
});

// --- The Zines rail ---

test("the zines page's key is which body is up, and the front page's filters", () => {
  const front = {
    kind: "front",
    lane: "private",
    section: null,
    pen: null,
    mineOnly: false,
    showMuted: true,
  } as const;
  assert.notEqual(
    zinesPageKey(front),
    zinesPageKey({ ...front, section: "rust" }),
  );
  assert.notEqual(zinesPageKey(front), zinesPageKey({ ...front, pen: "ana" }));
  assert.notEqual(zinesPageKey(front), zinesPageKey({ ...front, mineOnly: true }));
  assert.notEqual(zinesPageKey(front), zinesPageKey({ ...front, showMuted: false }));
  assert.notEqual(zinesPageKey(front), zinesPageKey({ ...front, lane: "public" }));
  assert.notEqual(
    zinesPageKey(front),
    zinesPageKey({ kind: "story", id: "z1", source: "zine" }),
  );
  assert.notEqual(
    zinesPageKey({ kind: "story", id: "z1", source: "zine" }),
    zinesPageKey({ kind: "story", id: "z2", source: "zine" }),
  );
  assert.notEqual(
    zinesPageKey({ kind: "story", id: "z1", source: "zine" }),
    zinesPageKey({ kind: "story", id: "z1", source: "post" }),
  );
  assert.notEqual(
    zinesPageKey({ kind: "draft", zineId: "z1" }),
    zinesPageKey({ kind: "draft", zineId: null }),
  );
});

test("the zines column's shape moves with its pen, its switches and its lane", () => {
  const base: ZinesPaneFacts = {
    pen: null,
    editing: false,
    mineOnly: false,
    showMuted: true,
    publicShown: false,
    publicError: null,
    publicLoading: false,
  };
  assert.notEqual(zinesPaneShape(base), zinesPaneShape({ ...base, pen: "ana" }));
  assert.notEqual(zinesPaneShape(base), zinesPaneShape({ ...base, editing: true }));
  assert.notEqual(zinesPaneShape(base), zinesPaneShape({ ...base, mineOnly: true }));
  assert.notEqual(zinesPaneShape(base), zinesPaneShape({ ...base, showMuted: false }));
  assert.notEqual(zinesPaneShape(base), zinesPaneShape({ ...base, publicShown: true }));
  assert.notEqual(
    zinesPaneShape({ ...base, publicShown: true }),
    zinesPaneShape({ ...base, publicShown: true, publicLoading: true }),
  );
  assert.notEqual(
    zinesPaneShape({ ...base, publicShown: true }),
    zinesPaneShape({ ...base, publicShown: true, publicError: "no network" }),
  );
});

test("the zines page's shape compares whichever body is up", () => {
  const facts = (over: Partial<ZinesPageFacts> = {}): ZinesPageFacts => ({
    dateLine: "Sunday, October 18",
    front: { error: null, loading: false, corpus: "c", cards: "card" },
    story: { zine: "", post: "", error: null, loading: false },
    draft: { zineId: null },
    ...over,
  });
  const base = zinesPageShape(facts());
  assert.notEqual(base, zinesPageShape(facts({ dateLine: "Monday, October 19" })));
  assert.notEqual(
    base,
    zinesPageShape(facts({ front: { error: null, loading: false, corpus: "c2", cards: "card" } })),
  );
  assert.notEqual(
    base,
    zinesPageShape(facts({ front: { error: null, loading: false, corpus: "c", cards: "card2" } })),
  );
  assert.notEqual(
    base,
    zinesPageShape(facts({ story: { zine: "z1\n5\n2\nunmuted\nopen", post: "", error: null, loading: false } })),
  );
  // The draft's shape is its post and nothing else: its fields live in the DOM
  // while it is open, which is what keeps the caret in the one being typed in.
  assert.notEqual(
    zinesPageShape(facts({ draft: { zineId: null } })),
    zinesPageShape(facts({ draft: { zineId: "z1" } })),
  );
});

// --- The Reads rail ---

const source = (id: string, title: string, builtIn = false) => ({
  id,
  title,
  url: `https://example.org/${id}`,
  builtIn,
});

test("the reads column key lists its feeds and what each row shows", () => {
  assert.equal(
    readsNavKey([source("hk", "Hacker News", true), source("ana", "Ana's blog")]),
    "readsNav:hk\nHacker News\nhttps://example.org/hk\nbuilt-in\n\nana\nAna's blog\nhttps://example.org/ana\nadded",
  );
});

test("a feed added, removed, renamed or re-ordered moves the reads column", () => {
  const one = [source("hk", "Hacker News", true)];
  const two = [source("hk", "Hacker News", true), source("ana", "Ana's blog")];
  assert.notEqual(readsNavKey(one), readsNavKey(two));
  assert.notEqual(readsNavKey(two), readsNavKey(two.slice(0, 1)));
  assert.notEqual(
    readsNavKey(two),
    readsNavKey([two[0], source("ana", "Ana's notes")]),
  );
  // Reordering rebuilds rather than moves, so the order is part of the key.
  assert.notEqual(readsNavKey(two), readsNavKey([two[1], two[0]]));
});

test("which feed is current and how many are saved are not in the reads column key", () => {
  // Both are the update's: the mark is on the button that was clicked, and the
  // count arrives from the store after the column is drawn. The key cannot see
  // either of them — this test is here to say that is deliberate, and a shape
  // that grew one would rebuild the column and take the focus with it.
  const feeds = [source("hk", "Hacker News", true)];
  assert.equal(readsNavKey(feeds), readsNavKey([source("hk", "Hacker News", true)]));
});

const story = (over: Partial<ReadsStoryShape> = {}): ReadsStoryShape => ({
  id: "1",
  title: "A story",
  meta: "ana · 2 min ago",
  read: false,
  saved: false,
  open: false,
  ...over,
});

const chrome = (over: Partial<ReadsListChrome> = {}): ReadsListChrome => ({
  error: null,
  loading: false,
  done: true,
  ...over,
});

test("the reads list's key is the view, not the stories in it", () => {
  // The list's element is its scroll box, so it is replaced only when the view
  // changes. A story arriving is the update's business, not the key's.
  assert.equal(readsListKey("feed", "hk"), "readsList:feed\nhk");
  assert.notEqual(readsListKey("feed", "hk"), readsListKey("saved", "hk"));
  assert.notEqual(readsListKey("feed", "hk"), readsListKey("feed", "ana"));
});

test("a story's age moving redraws the list", () => {
  // The one drawn fact that moves without the store moving: the shape reads the
  // rendered line, so "2 min ago" and "3 min ago" are different lists.
  assert.notEqual(
    readsListShape(chrome(), [story({ meta: "ana · 2 min ago" })]),
    readsListShape(chrome(), [story({ meta: "ana · 3 min ago" })]),
  );
});

test("a story's read, saved and open marks redraw the list", () => {
  const base = readsListShape(chrome(), [story()]);
  assert.notEqual(base, readsListShape(chrome(), [story({ read: true })]));
  assert.notEqual(base, readsListShape(chrome(), [story({ saved: true })]));
  assert.notEqual(base, readsListShape(chrome(), [story({ open: true })]));
});

test("the list's chrome is in its shape, because the update redraws it too", () => {
  const rows = [story()];
  const base = readsListShape(chrome(), rows);
  assert.notEqual(base, readsListShape(chrome({ loading: true }), rows));
  assert.notEqual(base, readsListShape(chrome({ done: false }), rows));
  assert.notEqual(base, readsListShape(chrome({ error: "no network" }), rows));
});

test("a story arriving, leaving or being re-titled redraws the list", () => {
  const rows = [story()];
  assert.notEqual(
    readsListShape(chrome(), rows),
    readsListShape(chrome(), [story(), story({ id: "2", title: "Another" })]),
  );
  assert.notEqual(readsListShape(chrome(), rows), readsListShape(chrome(), []));
  assert.notEqual(
    readsListShape(chrome(), rows),
    readsListShape(chrome(), [story({ title: "Re-titled" })]),
  );
});

const facts = (over: Partial<ReadsMainFacts> = {}): ReadsMainFacts => ({
  id: "1",
  view: "reader",
  title: "A story",
  meta: "ana · 2 min ago",
  saved: false,
  hasUrl: true,
  hasDiscussion: false,
  readerLoading: false,
  readerError: null,
  readerHtml: true,
  detailLoading: false,
  comments: 0,
  firstComment: null,
  ...over,
});

test("the reads reader's key is the story and the view of it", () => {
  assert.equal(readsMainKey("1", "reader"), "readsMain:1\nreader");
  assert.equal(readsMainKey(null, "reader"), "readsMain:none\nreader");
  assert.notEqual(readsMainKey("1", "reader"), readsMainKey("2", "reader"));
  assert.notEqual(readsMainKey("1", "reader"), readsMainKey("1", "comments"));
});

test("the reader's shape moves with its body, its save button and its age", () => {
  const base = readsMainShape(facts());
  assert.notEqual(base, readsMainShape(facts({ readerLoading: true })));
  assert.notEqual(base, readsMainShape(facts({ readerHtml: false })));
  assert.notEqual(base, readsMainShape(facts({ readerError: "timed out" })));
  assert.notEqual(base, readsMainShape(facts({ saved: true })));
  assert.notEqual(base, readsMainShape(facts({ meta: "ana · 3 min ago" })));
});

test("the reader's shape moves with the comment tree it drew", () => {
  // The discussion is rebuilt from the detail, so the tree's identity is in the
  // shape: the same story with a different tree is a different pane.
  const base = readsMainShape(facts({ view: "comments", comments: 3, firstComment: "c1" }));
  assert.notEqual(
    base,
    readsMainShape(facts({ view: "comments", comments: 4, firstComment: "c1" })),
  );
  assert.notEqual(
    base,
    readsMainShape(facts({ view: "comments", comments: 3, firstComment: "c2" })),
  );
  assert.notEqual(base, readsMainShape(facts({ view: "comments", comments: 3, firstComment: "c1", detailLoading: true })));
});

// --- The Drops rail ---

const dropsFacts = (over: Partial<DropsPaneFacts> = {}): DropsPaneFacts => ({
  verified: 3,
  shell: true,
  phase: "ready",
  message: null,
  openId: null,
  openingId: null,
  torBusy: false,
  error: null,
  showUnverified: false,
  devAllowed: false,
  ...over,
});

test("the drops pane's shape moves with every chip it draws", () => {
  const base = dropsPaneShape(dropsFacts());
  assert.notEqual(base, dropsPaneShape(dropsFacts({ verified: 4 })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ shell: false })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ phase: "connecting" })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ phase: "failed", message: "no route" })));
});

test("the drops pane's shape moves with the open window and the request in flight", () => {
  // The banner is a whole element that comes and goes with the open Drop, and
  // the card whose Open is in flight says "Opening…" and disables its button.
  const base = dropsPaneShape(dropsFacts());
  assert.notEqual(base, dropsPaneShape(dropsFacts({ openId: "secure-drop-1" })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ openingId: "secure-drop-1" })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ torBusy: true })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ error: "Tor is not running." })));
});

test("the drops pane's shape moves with the two switches over the cards", () => {
  // These are the only things that change which cards are drawn: the registry
  // is a constant, so nothing else about a card can move.
  const base = dropsPaneShape(dropsFacts());
  assert.notEqual(base, dropsPaneShape(dropsFacts({ showUnverified: true })));
  assert.notEqual(base, dropsPaneShape(dropsFacts({ devAllowed: true })));
});

test("a drops pane redrawn from the same facts is not redrawn", () => {
  assert.equal(dropsPaneShape(dropsFacts()), dropsPaneShape(dropsFacts()));
  // A message on a phase that does not carry it as a tooltip is still a fact:
  // the shape compares what the pane was drawn from, not what it shows.
  assert.notEqual(
    dropsPaneShape(dropsFacts({ message: "warming up" })),
    dropsPaneShape(dropsFacts({ message: null })),
  );
});

// --- The Torrents rail ---

const torrentPaneFacts = (over: Partial<TorrentsPaneFacts> = {}): TorrentsPaneFacts => ({
  activeTab: "Active & Queue (1)",
  libraryTab: "Library (2)",
  addBusy: false,
  addError: null,
  addValue: "",
  cards: [],
  rows: [],
  ...over,
});

const card = (over: Partial<TorrentCardFacts> = {}): TorrentCardFacts => ({
  id: "t1",
  status: "downloading",
  label: "Downloading to vault (42%)",
  meta: "1.2 GB · 512 MB / 1.2 GB · 12 MB/s · 3 min ago",
  bar: "42",
  error: null,
  ...over,
});

const libraryRow = (
  over: Partial<TorrentLibraryRowFacts> = {},
): TorrentLibraryRowFacts => ({
  id: "t1",
  kind: "torrent",
  size: "1.2 GB",
  sub: "4 files · 3 min ago",
  mime: "",
  busy: false,
  ...over,
});

test("the torrents pane's key is the body, and the library's order and layout", () => {
  assert.equal(torrentsPaneKey({ kind: "no-key" }), "torrentsPane:no-key");
  assert.equal(torrentsPaneKey({ kind: "active" }), "torrentsPane:active");
  const folder = {
    kind: "library" as const,
    path: ["t1", "Season 1"],
    missing: false,
    sort: "date",
    view: "list",
  };
  // The path is what makes a folder a different pane; the pieces cannot collide
  // with a torrent id because they are joined as their own lines.
  assert.notEqual(torrentsPaneKey(folder), torrentsPaneKey({ ...folder, path: ["t1"] }));
  assert.notEqual(
    torrentsPaneKey(folder),
    torrentsPaneKey({ ...folder, missing: true }),
  );
  assert.notEqual(torrentsPaneKey(folder), torrentsPaneKey({ ...folder, sort: "name" }));
  assert.notEqual(torrentsPaneKey(folder), torrentsPaneKey({ ...folder, view: "grid" }));
  // The query is not in the key: the field owns it, and a key that carried it
  // would rebuild the pane — and the field — on every keystroke.
  assert.equal(torrentsPaneKey(folder), torrentsPaneKey({ ...folder }));
});

test("the torrents pane's shape moves with the two tab labels and the Add bar", () => {
  const base = torrentsPaneShape(torrentPaneFacts());
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ activeTab: "Active & Queue" })));
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ libraryTab: "Library" })));
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ addBusy: true })));
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ addError: "not a magnet" })));
  // The Add field's text is drawn by the update — it is how a cleared submit
  // reaches a field that is never rebuilt.
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ addValue: "magnet:?xt=…" })));
});

test("a card's shape moves with everything the card draws and the patch rewrites", () => {
  const base = torrentCardShape(card());
  assert.notEqual(base, torrentCardShape(card({ label: "Downloading to vault (43%)" })));
  assert.notEqual(base, torrentCardShape(card({ meta: "1.2 GB · 520 MB / 1.2 GB · 3 min ago" })));
  assert.notEqual(base, torrentCardShape(card({ bar: "43" })));
  // The status is in the shape because the actions are drawn from it, and the
  // card's key carries it too: a status that moved is a different card.
  assert.notEqual(base, torrentCardShape(card({ status: "completed", label: "Completed" })));
  assert.notEqual(base, torrentCardShape(card({ error: "no route" })));
});

test("the pane's shape carries the cards and the rows, so a moved row is redrawn", () => {
  const base = torrentsPaneShape(torrentPaneFacts({ cards: [card()] }));
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ cards: [card({ bar: "43" })] })));
  assert.notEqual(base, torrentsPaneShape(torrentPaneFacts({ cards: [card(), card({ id: "t2" })] })));
  const withRow = torrentsPaneShape(torrentPaneFacts({ rows: [libraryRow()] }));
  assert.notEqual(
    withRow,
    torrentsPaneShape(torrentPaneFacts({ rows: [libraryRow({ sub: "4 files · 4 min ago" })] })),
  );
  assert.notEqual(
    withRow,
    torrentsPaneShape(torrentPaneFacts({ rows: [libraryRow({ busy: true, kind: "file" })] })),
  );
  assert.notEqual(
    withRow,
    torrentsPaneShape(torrentPaneFacts({ rows: [libraryRow({ mime: "video/x-matroska" })] })),
  );
});

test("a torrents pane redrawn from the same facts is not redrawn", () => {
  const facts = torrentPaneFacts({
    cards: [card(), card({ id: "t2", status: "paused", label: "Paused" })],
    rows: [libraryRow()],
  });
  assert.equal(torrentsPaneShape(facts), torrentsPaneShape(torrentPaneFacts({ ...facts })));
});
// --- Settings ---

test("the settings shape is the section and its body, and nothing else", () => {
  // Pinned exactly. The pane's key is a constant, so the section has to be here
  // — a section switch that did not move the shape would leave the old body on
  // screen — and the body is the caller's string, assembled per section.
  assert.equal(
    settingsShape({ section: "appearance", body: "dark" }),
    "appearance\n\ndark",
  );
  assert.notEqual(
    settingsShape({ section: "appearance", body: "dark" }),
    settingsShape({ section: "profile", body: "dark" }),
  );
  assert.notEqual(
    settingsShape({ section: "appearance", body: "dark" }),
    settingsShape({ section: "appearance", body: "light" }),
  );
});

test("a settings pane redrawn from the same facts is not redrawn", () => {
  const facts = { section: "security", body: "15\nunlocked" };
  assert.equal(settingsShape(facts), settingsShape({ ...facts }));
});

// --- The Vault rail ---

test("the vault body key names the body and its sub-identity, exactly", () => {
  // Pinned exactly, on purpose. The pane is the rail's scroll box, so the key
  // has to be stable across everything the body draws — a drawn fact added here
  // would replace the pane and lose the scroll the key exists to keep.
  assert.equal(vaultBodyKey({ kind: "tile" }), "vaultBody:tile");
  assert.equal(vaultBodyKey({ kind: "wallet" }), "vaultBody:wallet");
  assert.equal(vaultBodyKey({ kind: "files", nick: null }), "vaultBody:files");
  assert.equal(vaultBodyKey({ kind: "files", nick: "sam" }), "vaultBody:files\nsam");
  assert.equal(vaultBodyKey({ kind: "notes", editor: null }), "vaultBody:notes");
  assert.equal(
    vaultBodyKey({ kind: "notes", editor: "n1" }),
    "vaultBody:notes\neditor:n1",
  );
  assert.equal(
    vaultBodyKey({ kind: "notes", editor: "new" }),
    "vaultBody:notes\neditor:new",
  );
});

test("the vault body key moves for every body and every sub-identity", () => {
  const keys = [
    vaultBodyKey({ kind: "tile" }),
    vaultBodyKey({ kind: "wallet" }),
    vaultBodyKey({ kind: "files", nick: null }),
    vaultBodyKey({ kind: "files", nick: "sam" }),
    vaultBodyKey({ kind: "notes", editor: null }),
    vaultBodyKey({ kind: "notes", editor: "new" }),
    vaultBodyKey({ kind: "notes", editor: "n1" }),
  ];
  assert.equal(new Set(keys).size, keys.length);
  // The board and the editor are different panes: a fresh note opens at the top.
  assert.notEqual(
    vaultBodyKey({ kind: "notes", editor: null }),
    vaultBodyKey({ kind: "notes", editor: "new" }),
  );
  assert.notEqual(
    vaultBodyKey({ kind: "files", nick: "sam" }),
    vaultBodyKey({ kind: "files", nick: "ada" }),
  );
});
