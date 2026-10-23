/**
 * What a keyed region's key is made of.
 *
 * A region earns a key only when something inside it must not be detached (see
 * `regions.ts`), and the key says when that element may be rebuilt anyway. It
 * has to cover exactly what the region draws that nothing else brings up to
 * date, and the two ways of getting that wrong are not symmetrical:
 *
 * - too little, and a region that has moved on is kept as it was — a pane
 *   showing something that is no longer true, which is the one failure this
 *   machinery must not have;
 * - too much, and the region is rebuilt on a change its patch could have made
 *   in place — and every rebuild is a lost caret, scroll position and hover
 *   state, which is the thing the key exists to avoid.
 *
 * So each function below names what is drawn and unpatched, and says which
 * parts it deliberately leaves out and who owns them. That list is the whole of
 * its reasoning, and it is why these are functions with tests rather than a
 * template string written where the region is registered.
 *
 * The parts of a key are joined with a newline, the same separator `filterText`
 * uses and for the same reason: a name is user-chosen, and a newline is the one
 * character a name cannot contain.
 */

/**
 * One row of a list, as the region draws it: which row it is, and the state
 * that its own builder — not its patch — is responsible for.
 */
export interface RowShape {
  /** The name the row shows. The displayed name, not the id behind it. */
  name: string;
  /** Everything else the row was drawn from that no patch rewrites. */
  shape: string;
}

function rowShapes(rows: readonly RowShape[]): string {
  return rows.map((r) => `${r.name}\n${r.shape}`).join("\n");
}

/**
 * The rows of the Chats list, as its key sees them.
 *
 * The name is the name the row shows — a local rename is drawn text and the
 * row's search haystack — and the shape is the protocol its avatar is drawn
 * from. The reach is in the input, and deliberately not read: the presence dot
 * it decides is rewritten in place by `patchConvRows`, and a reach that moves
 * off `full` takes the row off this list altogether, so the row set already
 * carries it. Reading it here would make every reach move — which is a render,
 * because a reach move rewrites menus — rebuild the column the search field is
 * in, and take the caret with it. The test beside this varies the reach and
 * asserts the shapes do not move.
 */
export function chatRowShapes(
  rows: readonly { shownName: string; protocol: string; reach: string }[],
): RowShape[] {
  return rows.map((r) => ({ name: r.shownName, shape: r.protocol }));
}

/**
 * The rail: which button is current, and the one badge nothing patches.
 *
 * The Chats, Mail and Conclaves badges are not here. `patchRailBadges` rewrites
 * them in place on every background delta, so an unread count moving must not
 * rebuild the rail. The torrents badge
 * has no writer: the rail's first paint draws it and so does the Torrents pane,
 * so a count that moved is a count the rail has to redraw. The relay shield is
 * not here either — the poll repaints its state into the element it finds.
 */
export function railKey(rail: string, activeTorrents: number): string {
  return `${rail}\n${activeTorrents}`;
}

/** What the Chats list draws where rows would go, when it draws no rows. */
export type ConvListBody = "rows" | "no-trusts" | "mail-only";

/**
 * The Chats list: which chats are listed, what each row was drawn for, and
 * which of the two nothings stands in place of rows.
 *
 * Not here: presence, unread rings, previews and times. `patchConvRows` owns
 * them and rewrites them in place, which is what keeps the search field and the
 * caret in it — and it owns the row's selected mark for the same reason, since
 * clicking a row is the one change that must not rebuild the field the user may
 * be typing in. The query is not here either — it hides rows, it does not change
 * which ones are drawn — and the two nothings are here because they are two
 * different sentences, so a key that could not tell them apart would keep one
 * where the other belongs.
 *
 * The name in a row is the name the row shows, not the nickname behind it: a
 * local rename is drawn text and the row's search haystack, and a key made of
 * ids would keep the old one.
 */
export function convListKey(
  rows: readonly RowShape[],
  body: ConvListBody,
  appearOffline: boolean,
): string {
  return [body, appearOffline ? "offline" : "online", rowShapes(rows)].join(
    "\n",
  );
}

/**
 * What a Trusts row was drawn from, beyond the name it shows: the reach on both
 * sides, and the address the row may carry.
 *
 * One definition with three readers — the row itself, the pane's region key,
 * and the patch that decides whether a kept row is still the row it drew. A
 * reach that moves rewrites the row's badge and the actions in its menu, so a
 * shape the key could not see is a badge and a menu that are no longer true.
 */
export function trustRowShape(c: {
  reach: string;
  my_reach: string;
  mail_handle?: string | null;
  mail_vault?: string | null;
  signals?: string;
  payments?: string;
  peer_payments?: string;
  torrents?: string;
  peer_torrents?: string;
}): string {
  return [
    c.reach,
    c.my_reach,
    c.mail_handle ?? "",
    c.mail_vault ?? "",
    c.signals ?? "",
    c.payments ?? "",
    c.peer_payments ?? "",
    c.torrents ?? "",
    c.peer_torrents ?? "",
  ].join("\n");
}

/** Which of the two shapes the Conclaves column is in. */
export type ConclaveListBody = "empty" | "list";

/**
 * What a Conclaves card was drawn from, beyond the name it shows.
 *
 * One definition with two readers, like `trustRowShape`: the region key, and the
 * test beside this that says what it covers. The three facts are the ones the
 * card draws and no patch rewrites — whether the meeting has ended, how many
 * trustees it shows, and whether a voice session is live in it. The unread pill,
 * the last-message preview and the selected mark are rewritten in place by
 * `patchConclaveCards`, so a shape that carried them would rebuild the column —
 * and take the caret out of its filter field — for a number changing.
 */
export function conclaveCardShape(card: {
  ended: boolean;
  trustees: number;
  voiceLive: boolean;
}): string {
  return [
    card.ended ? "ended" : "active",
    String(card.trustees),
    card.voiceLive ? "voice" : "quiet",
  ].join("\n");
}

/**
 * The Conclaves column: which shape it is in, whether there is any trust at all,
 * and each card's name and shape.
 *
 * The head's "New conclave" button — disabled, and titled differently, when
 * there is nobody to meet with — and the empty state's copy and button are both
 * drawn from that one flag, and neither is patched, so it is here.
 */
export function conclavesKey(
  body: ConclaveListBody,
  hasContacts: boolean,
  rows: readonly RowShape[],
): string {
  return [
    body,
    hasContacts ? "contacts" : "no-contacts",
    rowShapes(rows),
  ].join("\n");
}

/**
 * The Mail rail's folder column: the address line, and nothing else.
 *
 * The folder set and its labels are written out in the builder, so they cannot
 * move, and the two things about a folder that do move are patched: the counts
 * and unread marks by `patchMailFolderCounts`, the active folder's mark by the
 * region update. Neither is here, which is what lets a folder switch repaint one
 * class instead of rebuilding the column and dropping the focus off the button
 * the user just pressed.
 *
 * An address is `handle@vault`, so the sentinel cannot collide with one.
 */
export function mailFoldersKey(address: string | null): string {
  return `mailFolders:${address ?? "none"}`;
}

/**
 * The ring prompt: the ring it is showing, and the two names on it.
 *
 * Pinned exactly like the other keys, and for the same reason: what is *not*
 * here matters as much as what is. The room's joined count and the ring's
 * remaining time are both in `ringSignature` — they are what makes the poll
 * render at all — and neither is drawn on the prompt, so neither belongs in its
 * key. A key that grew either one would rebuild the prompt, and take the focus
 * off "Answer", every time someone else joined the call.
 */
export function ringPromptKey(ring: RingFacts | null): string {
  if (!ring) return "none";
  return `${ring.sessionId}\n${ring.who}\n${ring.where}`;
}

/** The parts of a ring the prompt is drawn from. */
export interface RingFacts {
  /** The ringing room. A second ring on the same room is the same prompt. */
  sessionId: string;
  /** The opener, by the name the prompt shows. */
  who: string;
  /** The conclave it rings in, as the prompt names it. */
  where: string;
}

/**
 * The Trusts pane: which contacts are listed, and what each row was drawn for.
 *
 * A row's shape is everything the row and its menu were drawn from and nothing
 * patches: the reach on both sides, and the mail address the row may show. The
 * reach is here for every row, not only for the one that moved, because a row's
 * menu is built from the trust list rather than from its own contact: a reach
 * that changes anywhere can change what another row's menu offers, and that menu
 * is drawn once and never patched. The presence dot is not here —
 * `patchTrustRows` toggles it in place — and neither is the filter's query,
 * which hides rows rather than changing them.
 */
export function trustsKey(rows: readonly RowShape[]): string {
  return rowShapes(rows);
}

// --- The Reads rail ---

/**
 * One source row of the Reads column, as the column draws it.
 *
 * The title is the row's label, the url is its tooltip, and `builtIn` is what
 * decides whether the row carries a remove button at all — three drawn facts
 * that no patch rewrites.
 */
export interface ReadsSourceShape {
  id: string;
  title: string;
  url: string;
  builtIn: boolean;
}

/**
 * The Reads column: which feeds it lists, and what each row shows.
 *
 * Not here: which row is current, and the Saved button's count. The current mark
 * is on the very button that was clicked, so it is rewritten in place by the
 * region's update — a key carrying it would rebuild the column on every feed
 * switch and take the focus with it — and the count arrives from the store
 * asynchronously, so the same update refreshes it.
 */
export function readsNavKey(sources: readonly ReadsSourceShape[]): string {
  return `readsNav:${sources
    .map((s) => [s.id, s.title, s.url, s.builtIn ? "built-in" : "added"].join("\n"))
    .join("\n\n")}`;
}

/**
 * One story row of the Reads list, as the list draws it.
 *
 * `meta` is the rendered line under the title, not the fields behind it, because
 * the age in that line moves on its own: a shape built from `created_at` would be
 * the same string while the row on screen says "2 min ago" and then should say
 * "3 min ago". Reading the rendered text is what makes the row honest, and it is
 * cheap — the row's builder and this both go through one `readsMetaText`.
 */
export interface ReadsStoryShape {
  id: string;
  title: string;
  meta: string;
  read: boolean;
  saved: boolean;
  open: boolean;
}

/**
 * The Reads list's identity: which view it is showing.
 *
 * The identity and not the drawn facts, because the update redraws every row it
 * holds — the story list is what changes when a feed moves — while the element
 * itself is what must survive: it is the list's scroll box, so coming back from
 * an article or from a page of "More" has to find the reader where they were.
 * The rows themselves are the shape's business, not the key's: a key that
 * carried them would replace the scroll box on every story that arrived.
 */
export function readsListKey(mode: string, source: string): string {
  return `readsList:${mode}\n${source}`;
}

/**
 * Everything the Reads list draws around and instead of its rows.
 *
 * Paired with the shape below rather than with the key: the key decides whether
 * the scroll box is replaced, this decides whether the update has anything to do.
 * Both the chrome and the rows are here, because the update redraws all of them
 * — and the chrome is here even though it is mostly still, because a "More"
 * button that came and went is exactly the kind of stale the key would not catch.
 */
export interface ReadsListChrome {
  error: string | null;
  loading: boolean;
  done: boolean;
}

export function readsListShape(
  chrome: ReadsListChrome,
  rows: readonly ReadsStoryShape[],
): string {
  return [
    chrome.error ?? "",
    chrome.loading ? "loading" : "idle",
    chrome.done ? "done" : "more",
    rows
      .map((r) =>
        [
          r.id,
          r.title,
          r.meta,
          r.read ? "read" : "unread",
          r.saved ? "saved" : "unsaved",
          r.open ? "open" : "closed",
        ].join("\n"),
      )
      .join("\n\n"),
  ].join("\n");
}

/**
 * The Reads reader's identity: the story it is open on, and which of the two
 * views of it is showing.
 *
 * The view is in the key because switching to the discussion and back should
 * start the new body at its top, not at the article's scroll offset. The story
 * is in it for the same reason the mail reader keys on its thread id: a different
 * story means the pane on screen belongs to the wrong one.
 */
export function readsMainKey(id: string | null, view: string): string {
  return `readsMain:${id ?? "none"}\n${view}`;
}

/**
 * Everything the Reads reader draws, for its update to compare against.
 *
 * The head line is the rendered text, for the same reason as the list's: the age
 * in it moves. The body is reduced to what it was drawn from — whether the
 * article is loading, its error, whether it is there at all, and the comment
 * tree's size and first id — because the update re-sanitizes the article HTML
 * and rebuilds the tree, which is far too much work to do on a poll tick that
 * changed nothing.
 */
export interface ReadsMainFacts {
  id: string | null;
  view: string;
  title: string;
  meta: string;
  saved: boolean;
  /** Whether the story has a url, and so whether "Read here" is drawn. */
  hasUrl: boolean;
  /** Whether it has a discussion, and so whether that button is drawn. */
  hasDiscussion: boolean;
  readerLoading: boolean;
  readerError: string | null;
  /** Whether an article body is drawn at all. */
  readerHtml: boolean;
  detailLoading: boolean;
  comments: number;
  firstComment: string | null;
}

export function readsMainShape(f: ReadsMainFacts): string {
  return [
    f.id ?? "",
    f.view,
    f.title,
    f.meta,
    f.saved ? "saved" : "unsaved",
    f.hasUrl ? "url" : "no-url",
    f.hasDiscussion ? "hn" : "no-hn",
    f.readerLoading ? "reader-loading" : "reader-idle",
    f.readerError ?? "",
    f.readerHtml ? "article" : "no-article",
    f.detailLoading ? "detail-loading" : "detail-idle",
    String(f.comments),
    f.firstComment ?? "",
  ].join("\n");
}

// --- The Zines rail ---

/**
 * The Zines column: which pen it shows, which view controls are on, and how the
 * public lane is doing.
 *
 * A constant key, because the column has no identity of its own to switch — it
 * is the same column whatever the page beside it is showing — so its shape is
 * what decides when it is redrawn. Every fact here is drawn and none is patched,
 * and the three switches are checkboxes: redrawing them takes the focus off one
 * the user may have just pressed with the keyboard, which is the reason for the
 * shape rather than an update that always runs.
 *
 * Not here: the "Explore publications" button, which is drawn from nothing that
 * moves, and the pen name's *label*, which follows its id.
 */
export interface ZinesPaneFacts {
  /** The pen name, by id and label together, or null when there is none yet. */
  pen: string | null;
  /** Whether a draft is open, which is what hides "Write a post". */
  editing: boolean;
  mineOnly: boolean;
  showMuted: boolean;
  /** The public lane: its switch, its note, and its Refresh button. */
  publicShown: boolean;
  publicError: string | null;
  publicLoading: boolean;
}

export function zinesPaneShape(f: ZinesPaneFacts): string {
  return [
    f.pen ?? "",
    f.editing ? "editing" : "browsing",
    f.mineOnly ? "mine" : "all",
    f.showMuted ? "muted" : "unmuted",
    f.publicShown ? "public" : "private",
    f.publicError ?? "",
    f.publicLoading ? "reading" : "idle",
  ].join("\n");
}

/**
 * Which of the Zines page's three bodies is up.
 *
 * One definition with three readers — the page's builder, its region key and its
 * shape — so the three cannot disagree about which body is showing, the way
 * `mailBody` keeps the Mail rail's dispatch and its key together.
 *
 * The front page carries its filters because they are what its identity is made
 * of: picking a section or a byline is a different page, and the code already
 * says so by clearing the scroll it would otherwise carry back.
 */
export type ZinesPageBody =
  | {
      kind: "front";
      lane: string;
      section: string | null;
      pen: string | null;
      mineOnly: boolean;
      showMuted: boolean;
    }
  | { kind: "story"; id: string; source: string }
  | { kind: "draft"; zineId: string | null };

export function zinesPageKey(body: ZinesPageBody): string {
  if (body.kind === "front") {
    return [
      "zinesPage:front",
      body.lane,
      body.section ?? "",
      body.pen ?? "",
      body.mineOnly ? "mine" : "all",
      body.showMuted ? "muted" : "unmuted",
    ].join("\n");
  }
  if (body.kind === "story") {
    return ["zinesPage:story", body.id, body.source].join("\n");
  }
  return ["zinesPage:draft", body.zineId ?? "new"].join("\n");
}

/**
 * Everything the Zines page draws, in the terms its shape compares.
 *
 * `corpus` and `cards` are rendered by the caller, and they are the *whole* of
 * what each part of the front page reads rather than a list of fields: the page
 * draws from the corpus, the pen filter, the public lane and the clock, and a
 * shape naming those one by one would be a second description of the page whose
 * missing field is a page that is silently wrong — the one failure this
 * machinery must not have. The cards' lines are rendered through the same helper
 * the cards themselves use, so the "3 hours ago" in them is compared as text.
 *
 * The draft's facts are deliberately this small. Its title and sections live in
 * the DOM while it is open, read back into the state on each keystroke, and its
 * errors and its Publish button are written in place — so a shape carrying any of
 * them would redraw the form on the keystroke that changed it and take the caret
 * out of the field being typed in.
 */
export interface ZinesPageFacts {
  /** The masthead's date line, which rolls over at midnight. */
  dateLine: string;
  front: {
    error: string | null;
    loading: boolean;
    corpus: string;
    cards: string;
  };
  story: {
    /** The detail's identity: which zine, which revision, and its own flags. */
    zine: string;
    /** The public document's identity, when that is what is open. */
    post: string;
    error: string | null;
    loading: boolean;
  };
  draft: {
    zineId: string | null;
  };
}

export function zinesPageShape(f: ZinesPageFacts): string {
  return [
    f.dateLine,
    f.front.error ?? "",
    f.front.loading ? "loading" : "idle",
    f.front.corpus,
    f.front.cards,
    f.story.zine,
    f.story.post,
    f.story.error ?? "",
    f.story.loading ? "loading" : "idle",
    f.draft.zineId ?? "new",
  ].join("\n");
}

// --- The Drops rail ---

/**
 * Everything the Drops pane draws.
 *
 * The pane is the rail's whole surface, so its shape is the whole of its state:
 * the header's count and the Tor chips that carry the phase, the open-Drop
 * banner, the inline error, the two switches over the controls, the connect
 * button's own state, and the card whose Open request is in flight. Every one of
 * them is drawn and none is patched, and the pane is a scroll box, so a delta
 * that reached nothing here would cost the reader their place.
 *
 * What is deliberately *not* here is the registry itself. The directory is a
 * constant in the binary — `visibleDrops` filters a frozen list — so which cards
 * exist moves only with the two switches above, which are here. A card's name,
 * organization, country and protocol cannot move at all, and a shape that listed
 * them would be a second description of a static table: the kind of field list
 * whose missing entry is a pane that is silently wrong.
 */
export interface DropsPaneFacts {
  /** The header's count line, from the registry's verified entries. */
  verified: number;
  /** Whether this shell can open a Drop at all; the chips say so instead of Tor. */
  shell: boolean;
  /** Tor's phase, which every chip in the pane is drawn from. */
  phase: string;
  /**
   * Tor's message, which the chips carry as a tooltip while the phase is a
   * failure. Drawn text, so it is here.
   */
  message: string | null;
  /** The Drop open in its own window, or null: the banner, and its own card. */
  openId: string | null;
  /** The Drop whose Open request is in flight, or null: that card's button. */
  openingId: string | null;
  /** True while an explicit connect is in flight: the button's label and state. */
  torBusy: boolean;
  /** The last error, drawn inline rather than as a toast. */
  error: string | null;
  /** Whether unverified entries are revealed, and with them their cards. */
  showUnverified: boolean;
  /** Whether development Drops are enabled, and with them the test card. */
  devAllowed: boolean;
}

export function dropsPaneShape(f: DropsPaneFacts): string {
  return [
    String(f.verified),
    f.shell ? "shell" : "browser",
    f.phase,
    f.message ?? "",
    f.openId ?? "",
    f.openingId ?? "",
    f.torBusy ? "connecting" : "idle",
    f.error ?? "",
    f.showUnverified ? "unverified" : "verified-only",
    f.devAllowed ? "dev" : "no-dev",
  ].join("\n");
}

// --- The Torrents rail ---

/**
 * Which of the Torrents pane's three bodies is up.
 *
 * One definition with three readers — the pane's builder, its region key and the
 * update that dispatches on it — so the three cannot disagree about which body
 * is showing, the way `mailBody` keeps the Mail rail's dispatch and its key
 * together.
 *
 * The Library's order and layout are part of the body: they decide the row
 * order, the container's class and which toggle is marked, and the update
 * redraws the rows rather than the toolbar those live in. The query is
 * deliberately not here — it hides rows and is owned by the field itself.
 */
export type TorrentsPaneBody =
  | { kind: "no-key" }
  | { kind: "active" }
  | {
      kind: "library";
      /** The folder path: the torrent's id first, then subfolder names. */
      path: readonly string[];
      /** The first segment names a torrent that is no longer in the library. */
      missing: boolean;
      sort: string;
      view: string;
    };

export function torrentsPaneKey(body: TorrentsPaneBody): string {
  if (body.kind === "no-key") return "torrentsPane:no-key";
  if (body.kind === "active") return "torrentsPane:active";
  return [
    "torrentsPane:library",
    body.missing ? "missing" : "folder",
    body.sort,
    body.view,
    ...body.path,
  ].join("\n");
}

/**
 * One card in the queue, as the pane draws it.
 *
 * The status is here because the card's actions are drawn from it and the update
 * does not rewrite them: a status that moved takes that one card's key with it
 * and the card is rebuilt, rather than leaving yesterday's buttons under it.
 * The label and the meta line are the *rendered* text, because both carry a
 * clock and a speed — `readsTimeAgo` and the transfer rate — which move without
 * the store moving, and a shape that could not see them would leave "12 MB/s" on
 * a card that stopped.
 *
 * What is deliberately not here: the name, which cannot move for a given id, and
 * the actions themselves, which follow the status.
 */
export interface TorrentCardFacts {
  id: string;
  status: string;
  /** The badge's wording: "Downloading to vault (42%)" and its kin. */
  label: string;
  /** The muted line under the bar, as drawn. */
  meta: string;
  /** The bar's fill width, as the percentage the builder writes. */
  bar: string;
  /** The inline error under a failed card, or null when it draws none. */
  error: string | null;
}

export function torrentCardShape(f: TorrentCardFacts): string {
  return [f.id, f.status, f.label, f.meta, f.bar, f.error ?? ""].join("\n");
}

/**
 * One row of the Library, as the pane draws it.
 *
 * The update redraws rows as a set rather than patching one — a row here is a
 * name, a size and two buttons, and what moves is the folder's listing — so a
 * row's shape is the whole of what its builder reads: the size column's text, a
 * torrent's file-count-and-age line, a file's mimetype (which picks its icon),
 * and whether its blob is being decrypted right now. The age is the reason the
 * text is compared rather than the timestamp: it moves on its own.
 */
export interface TorrentLibraryRowFacts {
  /** The entry id, the folder name, or the file name: what the row is. */
  id: string;
  kind: "torrent" | "folder" | "file";
  /** The size column's text, or "" when the row draws none. */
  size: string;
  /** The line under a torrent's name, or "". */
  sub: string;
  /** A file row's mimetype, which picks its icon. */
  mime: string;
  /** A file row whose blob is being decrypted: its mark and its two buttons. */
  busy: boolean;
}

export function torrentLibraryShape(rows: readonly TorrentLibraryRowFacts[]): string {
  return rows
    .map((r) =>
      [r.id, r.kind, r.size, r.sub, r.mime, r.busy ? "busy" : "idle"].join("\n"),
    )
    .join("\n\n");
}

/**
 * Everything the Torrents pane's update redraws.
 *
 * The pane is the rail's scroll box, so its key is the body and its shape is
 * what the update writes inside it: the two tab labels, the Add bar's own drawn
 * state, and whichever of the two row sets is on screen. All of it is redrawn in
 * place, which is what keeps a running download from taking the reader's place
 * in the queue and the caret out of the two fields the pane holds.
 *
 * The Add field's text is here for one case only: a submit clears it in the
 * state, and the field itself is not rebuilt, so something has to write the
 * empty string back. While the reader is typing, the two agree and the write is
 * a no-op.
 *
 * The query is not here, and neither is `torrentsDragOver`: both are the
 * elements' own state, written by their own handlers, and a shape that carried
 * them would redraw the pane to arrive at the same DOM — or, worse, clear the
 * drag highlight mid-drag.
 */
export interface TorrentsPaneFacts {
  /** The two tab labels, as drawn: the queue's size and the library's. */
  activeTab: string;
  libraryTab: string;
  /** The Add button's label and state, and the inline error under the bar. */
  addBusy: boolean;
  addError: string | null;
  /** The Add field's text, which a submit clears behind the field's back. */
  addValue: string;
  /** The queue, when it is the body on screen. */
  cards: readonly TorrentCardFacts[];
  /** The Library's rows, when it is the body on screen. */
  rows: readonly TorrentLibraryRowFacts[];
}

export function torrentsPaneShape(f: TorrentsPaneFacts): string {
  return [
    f.activeTab,
    f.libraryTab,
    f.addBusy ? "adding" : "idle",
    f.addError ?? "",
    f.addValue,
    f.cards.map(torrentCardShape).join("\n\n"),
    torrentLibraryShape(f.rows),
  ].join("\n\n");
}

// --- Settings ---

/**
 * What the Settings page draws.
 *
 * The page's identity is a constant — it is Settings, and the nav's eleven
 * sections are a list written out in the builder — so the section is a drawn
 * fact rather than part of the key. That is what lets a section switch repaint
 * the body in place and leave the nav alone, with the focus on the button that
 * was just pressed; and it is why the update resets the pane's scroll by hand
 * instead of a fresh element starting at the top.
 *
 * `body` is everything the section's body draws, in one string, assembled by
 * the caller: each section's entry in `settingsBodyFacts` says what is in it and
 * which of its parts are deliberately absent because something else owns them
 * (a field's own text, which the update cannot carry without rebuilding the
 * field; Voice's device list, which the section fills in place).
 */
export interface SettingsFacts {
  section: string;
  body: string;
}

export function settingsShape(f: SettingsFacts): string {
  return [f.section, f.body].join("\n\n");
}

// --- The Vault rail ---

/**
 * Which of the Vault rail's five bodies is up.
 *
 * One definition with two readers — the pane's builder and its region key — so
 * the two cannot disagree about which body is showing, the way `mailBody` keeps
 * the Mail rail's dispatch and its key together. It is the same shape of
 * decision: five flags that decide which builder runs, with the sub-identity
 * (whose files, which note) carried along rather than re-read at the far end.
 *
 * `files.nick` is null for the contact list; `notes.editor` is null for the
 * board and the note's id — or "new" — while the editor is open. `ownfiles` is
 * the vault's own library, distinct from files exchanged with a contact.
 */
export type VaultBody =
  | { kind: "tile" }
  | { kind: "files"; nick: string | null }
  | { kind: "ownfiles" }
  | { kind: "notes"; editor: string | null }
  | { kind: "wallet" };

/**
 * The Vault rail's body key.
 *
 * The rail is one region whose body is swapped under it, so the key says which
 * body is up and nothing about what it draws — what it draws is the shape's
 * business, and a key that moved for a drawn fact would replace the pane and
 * lose the scroll it exists to keep.
 *
 * A sub-identity *is* part of the key rather than a drawn fact: opening a
 * contact's files or a note's editor is arriving somewhere new, and a pane that
 * starts at the top is what the reader expects — the same rule the zines page
 * follows when it keys itself by the story it is showing. The Wallet has no
 * sub-identity: the chain capsule lives inside it, and a redraw brings the
 * cards forward under the reader where they are rather than replacing the pane.
 */
export function vaultBodyKey(body: VaultBody): string {
  switch (body.kind) {
    case "tile":
      return "vaultBody:tile";
    case "files":
      return body.nick === null ? "vaultBody:files" : `vaultBody:files\n${body.nick}`;
    case "ownfiles":
      return "vaultBody:ownfiles";
    case "notes":
      return body.editor === null
        ? "vaultBody:notes"
        : `vaultBody:notes\neditor:${body.editor}`;
    case "wallet":
      return "vaultBody:wallet";
  }
}
