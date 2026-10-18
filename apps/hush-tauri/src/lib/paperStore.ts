import * as Y from "yjs";
import {
  ySyncPlugin,
  yUndoPlugin,
  yUndoPluginKey,
  ySyncPluginKey,
  yCursorPlugin,
} from "y-prosemirror";
import {
  Awareness,
  applyAwarenessUpdate,
  encodeAwarenessUpdate,
} from "y-protocols/awareness";
import { EditorState, Plugin, type Command } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { keymap } from "prosemirror-keymap";
import { baseKeymap, chainCommands, toggleMark } from "prosemirror-commands";
import { splitListItem, liftListItem, sinkListItem } from "prosemirror-schema-list";
import { AppError, api } from "./api/daemon";
import { conclaveById } from "./conclaveStore";
import {
  buildToolbar,
  richTextSchema as schema,
  toolbarButtons,
} from "./proseSchema";
import { xmlFragmentToPlainText } from "./paperText";
import {
  fromBase64,
  isDecodable,
  mergeBatch,
  relabelAwareness,
  shouldApplyState,
  toBase64,
} from "./paperWire";

/**
 * The conclave Paper bridge: the webview half of the shared document.
 *
 * The **editor's** Yjs doc lives here; the daemon keeps the merged CRDT state
 * (opaque `yrs` updates) in the vault's encrypted sidecar and, from Phase 2,
 * fans updates out to the other trustees over their Signal sessions. The
 * protocol is deliberately dumb: the editor posts merged update bytes and
 * polls for merged update bytes. Yjs makes re-applying an update a no-op, so
 * one code path serves our own edits, a peer's edits, and a full catch-up —
 * nothing here needs to know which it just received.
 *
 * Phase 1 is offline: polling talks only to the local daemon, and an update's
 * origin is always this vault. The wire fan-out (Phase 2) and the DataChannel
 * fast path (Phase 3) swap the transport, not this shape.
 *
 * **Presence** (Task 7 Step 2) rides the same DataChannel and nothing else: the
 * editor's awareness — cursor as a Yjs relative position, plus the author's
 * name and colour — is published to the daemon, forwarded along the star, and
 * polled back as opaque frames. There is deliberately no relay fallback and no
 * persistence: a cursor that arrives a second and a half late is worse than no
 * cursor, and a stale one is a lie about who is reading. `yCursorPlugin` draws
 * them, which is also what keeps a name and a colour on each caret.
 */

/** Poll cadence while a paper is open; a live channel replaces this later. */
const PAPER_POLL_MS = 1000;
/** Local edits are batched this long before the daemon sees them. */
const PAPER_FLUSH_MS = 400;
/**
 * Local presence is batched this long before it is published.
 *
 * Shorter than an edit's window because a cursor is the thing being watched: a
 * caret that lags half a second behind the typing reads as broken, and a frame
 * is a few dozen bytes.
 */
const PAPER_PRESENCE_FLUSH_MS = 150;
/** How often the daemon is asked who else is on the fast path. */
const PAPER_PRESENCE_POLL_MS = 500;

/** The Yjs type the editor binds to; the daemon never parses it. */
const FRAGMENT_NAME = "prosemirror";

/** Fixed palette; a trustee's colour is theirs on every device. */
const AUTHOR_COLORS = [
  "#b7791f",
  "#2b6cb0",
  "#2f855a",
  "#805ad5",
  "#c53030",
  "#00838f",
  "#d0508f",
  "#5f6f1f",
] as const;

export function authorColor(accountId: string): string {
  let h = 0;
  for (let i = 0; i < accountId.length; i++) {
    h = (h * 31 + accountId.charCodeAt(i)) >>> 0;
  }
  return AUTHOR_COLORS[h % AUTHOR_COLORS.length];
}

/**
 * Apply state that came from the daemon (our own echo, or later a peer).
 *
 * Zero bytes are not a decodable Yjs update — `Y.applyUpdate` throws
 * `Unexpected end of array` — so "nothing yet" is skipped rather than thrown
 * at. The daemon sends the canonical empty update for a blank paper, but a
 * future transport must not be able to break the editor by sending nothing.
 */
function applyRemote(ydoc: Y.Doc, b64: string): void {
  const bytes = fromBase64(b64);
  if (!isDecodable(bytes)) return;
  Y.applyUpdate(ydoc, bytes, "remote");
}

// --- The session: one paper open at a time (one conclave is on screen). ----

/** Everything the Paper tab needs while it is open. */
export interface PaperSession {
  conclaveId: string;
  /** This vault's account id, set when the editor mounts. Presence publishes it
   *  as the author identity and uses it to spot our own frame. */
  me: string;
  ydoc: Y.Doc;
  /** Awareness for live cursors. Local presence is published from here. */
  awareness: Awareness;
  /** The merged doc state the daemon last acknowledged. */
  lastRev: number;
  /** Whether the conclave has a paper yet (any trustee may start one). */
  exists: boolean;
  title: string;
  /** Local updates not yet acknowledged by the daemon. */
  pending: Uint8Array[];
  flushTimer: number | null;
  pollTimer: number | null;
  /** Set once the editor is mounted; presence only matters when it is on screen. */
  presenceFlushTimer: number | null;
  presencePollTimer: number | null;
  /** Whether the awareness listener and poll have been started. */
  presenceStarted: boolean;
}

let session: PaperSession | null = null;
/** The live editor view, if a Paper tab is open. */
let editor: EditorView | null = null;

export function paperSession(): PaperSession | null {
  return session;
}

/**
 * Arm the session's polling loops, unless the window is hidden.
 *
 * Visibility is read from the document rather than remembered in a flag: the
 * shell pauses these loops from a `visibilitychange` handler that returns early
 * while the vault is locked, so a remembered "paused" could outlive the pause
 * and leave a paper that is opened later with no loops at all.
 */
function startLoops(s: PaperSession): void {
  if (document.visibilityState !== "visible") return;
  if (s.exists && s.pollTimer === null) {
    s.pollTimer = window.setInterval(() => void pollPaper(s), PAPER_POLL_MS);
  }
  if (s.presenceStarted && s.presencePollTimer === null) {
    s.presencePollTimer = window.setInterval(
      () => void pollPresence(s),
      PAPER_PRESENCE_POLL_MS,
    );
  }
}

/** Stop the session's polling loops, leaving the one-shot flush timers alone. */
function stopLoops(s: PaperSession): void {
  if (s.pollTimer !== null) window.clearInterval(s.pollTimer);
  if (s.presencePollTimer !== null) window.clearInterval(s.presencePollTimer);
  s.pollTimer = null;
  s.presencePollTimer = null;
}

/**
 * Stop polling while the window is hidden, and start again when it is back.
 *
 * The loops are the editor's only link to the merged state and to the peers'
 * cursors, and neither is looked at while the window is away — so leaving them
 * running is three daemon requests a second for a document nobody can see.
 *
 * Nothing is lost by the gap: an edit cannot be made while the window is
 * hidden, `pollPaper` re-reads the merged state and Yjs folds it idempotently
 * however much of it is new, and `pollPresence` re-reads the peers whose frames
 * have not expired. The one-shot flush timers are deliberately left running: an
 * edit made just before the window went away must still reach the daemon, and
 * that is a single request rather than a loop.
 *
 * Resume polls once immediately, so the document and the cursors are current
 * before the user looks at them rather than one interval later.
 */
export function pausePaperSync(): void {
  if (session) stopLoops(session);
}

export function resumePaperSync(): void {
  const s = session;
  if (!s) return;
  startLoops(s);
  if (s.exists) void pollPaper(s);
  if (s.presenceStarted) void pollPresence(s);
}

function stopTimers(s: PaperSession): void {
  stopLoops(s);
  if (s.flushTimer !== null) window.clearTimeout(s.flushTimer);
  if (s.presenceFlushTimer !== null) window.clearTimeout(s.presenceFlushTimer);
  s.flushTimer = null;
  s.presenceFlushTimer = null;
}

/** Close the session: stop syncing and drop the doc. Called when the tab
 *  closes, the conclave changes, the rail changes, or the vault locks. */
export function closePaper(): void {
  if (editor) {
    editor.destroy();
    editor = null;
  }
  if (session) {
    const s = session;
    session = null;
    // A local edit made inside the flush window has not reached the daemon yet,
    // and the document only survives inside the vault's encrypted sidecar — so
    // the pending batch goes out before the doc does. `mergeBatch` runs
    // synchronously, so the request is already built by the time the ydoc is
    // destroyed. A locked vault refuses it, which is why the shell flushes
    // before it locks (see `flushPaperEdits`); that refusal is then a crash,
    // not an ordinary lock.
    void flushPaper(s);
    stopTimers(s);
    // Tell the peers to drop our cursor before the awareness goes: presence has
    // no history to replay, so a frame that is not sent now is a cursor that
    // lingers until the daemon's TTL expires. Best-effort — a locked vault, a
    // closed window and a dead channel all end in the same place, which is
    // "nobody sees a cursor that is not there".
    try {
      s.awareness.setLocalState(null);
      const frame = encodeAwarenessUpdate(s.awareness, [s.awareness.clientID]);
      void api.paperPublishPresence(s.conclaveId, toBase64(frame)).catch(() => {});
    } catch {
      // Nothing to clean up if the awareness could not be read.
    }
    s.awareness.destroy();
    s.ydoc.destroy();
  }
}

function scheduleFlush(s: PaperSession): void {
  if (s.flushTimer !== null) return;
  s.flushTimer = window.setTimeout(() => {
    s.flushTimer = null;
    void flushPaper(s);
  }, PAPER_FLUSH_MS);
}

/**
 * Flush the open paper's pending edits now, without closing the session.
 *
 * Called by the shell immediately **before** it locks the vault: every paper
 * route answers 401 once the vault is locked, so an edit still inside its
 * 400 ms flush window would otherwise be dropped along with the editor's doc.
 * A no-op when no paper is open or nothing is pending.
 */
export function flushPaperEdits(): void {
  if (!session) return;
  void flushPaper(session);
}

async function flushPaper(s: PaperSession): Promise<void> {
  if (!s.pending.length || !s.exists) return;
  const batch = s.pending;
  s.pending = [];
  const merged = mergeBatch(batch);
  if (!merged) return;
  try {
    const view = await api.paperUpdate(s.conclaveId, toBase64(merged));
    if (view) s.lastRev = Math.max(s.lastRev, view.revision);
  } catch (e) {
    // Put the batch back: it was never acknowledged. The vault locking
    // surfaces as a 401 ("locked") and is handled by the shell.
    s.pending.unshift(...batch);
    if (!(e instanceof AppError && e.message === "locked")) {
      console.warn("paper flush failed", e);
    }
  }
}

// --- Presence: live cursors over the DataChannel only ----------------------

/**
 * Publish our awareness frame.
 *
 * The frame is the editor's own awareness update — cursor as a Yjs relative
 * position, plus the author's name and colour — and the daemon treats it as
 * opaque bytes, forwarding it along the star. There is no relay fallback: a
 * cursor that arrives a second and a half late is worse than no cursor, and
 * "nobody is live" is already a state the UI shows.
 */
function schedulePresenceFlush(s: PaperSession): void {
  if (s.presenceFlushTimer !== null) return;
  s.presenceFlushTimer = window.setTimeout(() => {
    s.presenceFlushTimer = null;
    if (session !== s) return;
    const frame = encodeAwarenessUpdate(s.awareness, [s.awareness.clientID]);
    void api
      .paperPublishPresence(s.conclaveId, toBase64(frame))
      .catch(() => {
        // Locked, or no channel: the next change tries again.
      });
  }, PAPER_PRESENCE_FLUSH_MS);
}

/**
 * Take one peer's frame.
 *
 * The frame is applied wholesale: awareness updates carry their own clock, so
 * an older one arriving after a newer one is ignored by the library rather than
 * by us. A peer that stops sending — a crash, a killed daemon — is dropped by
 * that same machinery after `outdatedTimeout` (30 s), which is deliberately the
 * daemon's own presence TTL: a cursor disappears when the frame behind it
 * expires, whichever end notices first.
 */
function applyPeerPresence(s: PaperSession, b64: string): void {
  const bytes = fromBase64(b64);
  if (!isDecodable(bytes)) return;
  try {
    // The frame names the author by account id, which is the identity the relay
    // authenticated; the caret's *label* comes from our own roster, so a peer
    // cannot name its caret after somebody else and the label matches the name
    // the member list and the chat show.
    applyAwarenessUpdate(
      s.awareness,
      relabelAwareness(bytes, (id) => {
        if (id === s.me) return "you";
        return conclaveById(s.conclaveId)?.trustees.find((t) => t.accountId === id)?.nick || null;
      }),
      "remote",
    );
  } catch {
    // A frame this build cannot read is dropped, like an unknown payload tag.
  }
}

async function pollPresence(s: PaperSession): Promise<void> {
  try {
    const seen = await api.paperPresence(s.conclaveId);
    if (!seen || session !== s) return;
    for (const peer of seen.peers) applyPeerPresence(s, peer.frame);
  } catch (e) {
    if (
      !(e instanceof AppError && (e.message === "locked" || e.message === "no paper in this conclave"))
    ) {
      console.warn("paper presence poll failed", e);
    }
  }
}

/** Start the presence loops. Called when the editor mounts, so a paper that is
 *  not on screen neither publishes a cursor nor polls for one. */
function startPresence(s: PaperSession, me: string): void {
  if (s.presenceStarted) return;
  s.presenceStarted = true;
  // The editor's cursor plugin writes into the local awareness state, and
  // `setLocalStateField` is a no-op while that state is null, so the identity
  // has to exist before the plugin is attached.
  s.awareness.setLocalState({ user: { name: me, color: authorColor(me) } });
  s.awareness.on(
    "update",
    (
      { added, updated }: { added: number[]; updated: number[] },
      origin: unknown,
    ) => {
      if (origin === "remote") return;
      // Only our own client is ours to publish: echoing a peer's frame back
      // would be traffic that says nothing.
      if (
        !added.includes(s.awareness.clientID) &&
        !updated.includes(s.awareness.clientID)
      ) {
        return;
      }
      schedulePresenceFlush(s);
    },
  );
  startLoops(s);
}

/** One sync pass: pull the merged state and apply what is new. Applying the
 *  full state is always safe — Yjs folds it idempotently. */
async function pollPaper(s: PaperSession): Promise<void> {
  try {
    const view = await api.paperGet(s.conclaveId);
    if (!view) return;
    if (shouldApplyState(s.lastRev, view.revision, view.compacted)) {
      applyRemote(s.ydoc, view.state);
      s.lastRev = view.revision;
    }
  } catch (e) {
    if (!(e instanceof AppError && (e.message === "locked" || e.message === "no paper in this conclave"))) {
      console.warn("paper poll failed", e);
    }
  }
}

/** Open (or re-open) the conclave's paper. Resolves even when the conclave
 *  has no paper yet — `exists` says which; the UI shows the start affordance. */
export async function openPaper(conclaveId: string): Promise<PaperSession> {
  closePaper();
  const ydoc = new Y.Doc();
  session = {
    conclaveId,
    me: "",
    ydoc,
    awareness: new Awareness(ydoc),
    lastRev: 0,
    exists: false,
    title: "Paper",
    pending: [],
    flushTimer: null,
    pollTimer: null,
    presenceFlushTimer: null,
    presencePollTimer: null,
    presenceStarted: false,
  };
  // The origin of a ydoc change says who made it: `"remote"` is our own poll
  // applying merged state (never re-sent), `ySyncPluginKey` is the editor,
  // and an UndoManager undo/redo is a local change peers must learn about —
  // so everything except `"remote"` goes out.
  ydoc.on("update", (update: Uint8Array, origin: unknown) => {
    if (origin === "remote" || !session) return;
    session.pending.push(update);
    scheduleFlush(session);
  });
  try {
    const view = await api.paperGet(conclaveId);
    if (view) {
      session.exists = true;
      session.title = view.paper.title;
      applyRemote(ydoc, view.state);
      session.lastRev = view.revision;
    }
  } catch (e) {
    if (!(e instanceof AppError && e.message === "no paper in this conclave")) {
      throw e;
    }
  }
  if (session.exists) {
    startLoops(session);
  }
  return session;
}

/** Start the paper for a conclave that does not have one yet (any joined
 *  trustee may; the first create wins and everyone else adopts it). */
export async function createPaper(
  conclaveId: string,
  title: string,
): Promise<PaperSession> {
  const created = await api.paperCreate(conclaveId, title);
  if (!created) throw new AppError("Couldn't start the paper.");
  const s = session && session.conclaveId === conclaveId ? session : await openPaper(conclaveId);
  s.exists = true;
  s.title = created.paper.title;
  s.lastRev = Math.max(s.lastRev, created.revision);
  if (s.pollTimer === null) {
    startLoops(s);
  }
  return s;
}

// --- Editor binding --------------------------------------------------------

/**
 * Build the editor (toolbar + ProseMirror surface) into `container`. The DOM
 * subtree stays stable across the app's wholesale re-renders: `render()` moves
 * this element, it never rebuilds it, so focus and selection survive.
 *
 * `readOnlyReason` is the refusal the daemon would give, in the words the
 * surface should show, or `null` when the paper is editable. There are two
 * reasons — an ended conclave, and a vault that is no longer a joined trustee —
 * and they are not the same sentence, so the note says which one applies.
 * Cursors still render either way: the document is still shared, it is just
 * frozen.
 */
export function attachEditor(
  s: PaperSession,
  container: HTMLElement,
  me: string,
  readOnlyReason: string | null = null,
): EditorView {
  const readOnly = readOnlyReason !== null;
  const fragment = s.ydoc.getXmlFragment(FRAGMENT_NAME);
  // The cursor plugin writes into the local awareness state, so the identity it
  // publishes has to exist before the plugin is attached.
  s.me = me;
  startPresence(s, me);

  const undoFrom = (state: EditorState): Y.UndoManager | undefined => {
    const pluginState = yUndoPluginKey.getState(state) as
      | { undoManager: Y.UndoManager }
      | undefined;
    return pluginState?.undoManager;
  };

  const withUndo =
    (action: (um: Y.UndoManager) => void): Command =>
    (state) => {
      const um = undoFrom(state);
      if (!um) return false;
      action(um);
      return true;
    };

  const buttons = toolbarButtons({
    undo: withUndo((um) => um.undo()),
    redo: withUndo((um) => um.redo()),
  });

  const keys: Record<string, Command> = {
    "Mod-b": (st, dispatch) => toggleMark(schema.marks.bold)(st, dispatch),
    "Mod-i": (st, dispatch) => toggleMark(schema.marks.italic)(st, dispatch),
    "Mod-u": (st, dispatch) => toggleMark(schema.marks.underline)(st, dispatch),
    "Mod-z": withUndo((um) => um.undo()),
    "Mod-y": withUndo((um) => um.redo()),
    "Mod-Shift-z": withUndo((um) => um.redo()),
    Enter: chainCommands(
      (st, dispatch, view) => splitListItem(schema.nodes.list_item)(st, dispatch, view),
      baseKeymap.Enter,
    ),
    Tab: (st, dispatch, view) => sinkListItem(schema.nodes.list_item)(st, dispatch, view),
    "Shift-Tab": (st, dispatch, view) => liftListItem(schema.nodes.list_item)(st, dispatch, view),
  };

  const wrap = document.createElement("div");
  wrap.className = "paper-editor";
  // The toolbar is built before the view exists, which is why it takes a
  // getter: nothing inside reads the view until a click or a state change,
  // both of which happen long after construction.
  const toolbar = buildToolbar(buttons, () => view, "Paper formatting");

  // The frame goes up in document order before the view exists: ProseMirror
  // appends its own DOM into the element it is given, so the toolbar has to be
  // in place first or it would end up underneath the editor.
  if (readOnlyReason !== null) {
    // No toolbar: every command it offers would be refused by the daemon.
    const note = document.createElement("p");
    note.className = "paper-readonly-note muted text-13";
    note.textContent = readOnlyReason;
    wrap.appendChild(note);
  } else {
    wrap.appendChild(toolbar.el);
  }

  const surface = document.createElement("div");
  surface.className = "paper-surface";
  surface.style.setProperty("--paper-caret", authorColor(me));
  // The scroll container goes into the frame *before* the view exists:
  // `new EditorView(place)` appends its own DOM into `place`, so appending
  // `view.dom` afterwards would move the editor back out and leave this
  // element detached — the surface would render with none of its styling.
  wrap.appendChild(surface);

  // The toolbar's pressed state follows the editor's state, refreshed from a
  // plugin view callback rather than a `dispatchTransaction` override:
  // y-prosemirror dispatches a transaction while the EditorView is *still
  // constructing* (adopting the Yjs fragment into the ProseMirror doc), so a
  // dispatch override closing over `view` reads it in its temporal dead zone
  // and throws "Cannot access 'view' before initialization" — which, because
  // render() has already cleared the root by then, blanks the whole app.
  // `update(view)` hands the view in, so nothing reads it early.
  const toolbarSync = new Plugin({
    view: () => ({
      update: () => toolbar.refresh(),
    }),
  });

  const view = new EditorView(surface, {
    state: EditorState.create({
      schema,
      plugins: [
        ySyncPlugin(fragment),
        yUndoPlugin(),
        yCursorPlugin(s.awareness),
        toolbarSync,
        keymap(keys),
        keymap(baseKeymap),
      ],
    }),
    editable: () => !readOnly,
    attributes: {
      role: "textbox",
      "aria-multiline": "true",
      "aria-label": "Shared paper",
      "aria-readonly": readOnly ? "true" : "false",
      spellcheck: readOnly ? "false" : "true",
    },
  });

  // Handlers bind after the view exists; clicking applies the command to the
  // live view and returns focus to the editor.
  toolbar.refresh();

  // `view.dom` is already inside `surface`; appending it here would move it.
  container.appendChild(wrap);
  editor = view;
  return view;
}

/** Whether the paper surface currently holds the caret. */
export function paperHasFocus(): boolean {
  return !!editor && editor.hasFocus();
}

/**
 * The paper's whole text, flattened the way a note reads: headings prefixed,
 * lists marked, nesting indented (see `paperText`). Works on a session that
 * is mounted or merely opened — the text lives in the ydoc, not the editor.
 */
export function paperPlainText(s: PaperSession): string {
  return xmlFragmentToPlainText(s.ydoc.getXmlFragment(FRAGMENT_NAME));
}
