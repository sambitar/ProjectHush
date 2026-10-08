import { api, AppError, getApiBase } from "./lib/api/daemon";
import type { ContactDto, StatusResponse } from "./lib/api/types";
import {
  addOutgoingFile,
  addOutgoingText,
  contactsWithFiles,
  fileMessagesForContact,
  fileCountForContact,
  clearChatStore,
  loadHistory,
  getThread,
  hasUnread,
  ingestIncoming,
  lastPreview,
  markRead,
  removeOutgoingMessage,
  clearSendProgress,
  setMessageBlobId,
  setSendProgress,
  allFileMessages,
  toggleReaction,
} from "./lib/chatStore";
import { MAX_FILE_BYTES, RECV_POLL_MS } from "./lib/constants";
import { isTauriShell } from "./lib/tauriEnv";
import {
  clearMailStore,
  draftById,
  draftPreview,
  folderCount,
  getThread as getMailThread,
  hasUnreadMail,
  isUnread,
  listDrafts,
  listFolder,
  loadMailbox,
  preview,
  searchMessages,
  setKnownContacts,
  threadSubject,
  unreadCount,
  type MailAttachmentRef,
  type MailFlags,
  type MailFolder,
  type MailMessage,
} from "./lib/mailStore";
import {
  getAutoLockMs,
  getAutoLockMinutes,
  mailFlags,
  patchUiPrefs,
  setMailFlag,
} from "./lib/api/uiPrefs";
import { displayNick, getMockSettings, setMuted, setNickOverride, clearMockLocal, isMuted, refreshContactPrefs } from "./lib/api/mock";
import { clearContactUiPrefs } from "./lib/api/uiPrefs";
import { fileToBase64, formatAbsolute, formatBytes, formatRelative, formatUntil } from "./lib/format";
import {
  attachFileActions,
  downloadBase64File,
  readFileAsBase64,
  resolveMessageFileBytes,
} from "./lib/fileBytes";
import { groupMessages } from "./lib/messageGroups";
import { icon, type IconName } from "./lib/icons";
import { avatarWithPresence, createAvatarEl } from "./components/ui/Avatar";
import { RichTextEditor } from "./components/ui/RichTextEditor";
import { ContactPicker } from "./components/ui/ContactPicker";
import { bodyToDisplayHtml, isBlankHtml } from "./lib/richText";
import { applyTheme, getThemeMode, setThemeMode, type ThemeMode } from "./lib/theme";
import QRCode from "qrcode";

type Rail = "chats" | "trusts" | "mail" | "files" | "vault" | "settings";
type SettingsSection =
  | "appearance"
  | "profile"
  | "security"
  | "recovery"
  | "backup"
  | "about";

/** Matches the daemon: at most this many attachments, and this many bytes total. */
const MAX_MAIL_ATTACHMENTS = 8;
const MAX_MAIL_BYTES = 256 * 1024;

/**
 * Matches `MIN_DROP_PIN_LEN` in `hush-core`.
 *
 * Duplicated on purpose rather than fetched: the check is a courtesy that saves
 * a round trip, and the daemon enforces the real floor either way, so a stale
 * copy here can only ever be more permissive than the truth.
 */
const MIN_DROP_PIN_LEN = 8;

/**
 * Matches `DROP_PUBLIC_KEY_PREFIX` in `hush-core`.
 *
 * The drop public-key mode is a versioned, pasteable string. Checking the prefix
 * and shape here catches a pasted invite or a half-copied key before a round
 * trip; the daemon still validates the point itself.
 */
const DROP_PUBLIC_KEY_PREFIX = "hush-drop-pk-v1:";
/** 32 raw bytes, base64url without padding, is exactly 43 characters. */
const DROP_PUBLIC_KEY_BODY_LEN = 43;

const MAIL_FOLDER_LABEL: Record<MailFolder, string> = {
  inbox: "Inbox",
  sent: "Sent",
  outbox: "Outbox",
  drafts: "Drafts",
  starred: "Starred",
  archive: "Archive",
  trash: "Trash",
};

const MAIL_EMPTY: Record<MailFolder, string> = {
  inbox: "No mail yet — write to a contact.",
  sent: "Nothing sent yet.",
  outbox: "Nothing waiting. Mail the relay could not take would queue here.",
  drafts: "No drafts.",
  starred: "No starred mail.",
  archive: "Nothing archived.",
  trash: "Trash is empty.",
};

/** `Re:` a subject, without stacking prefixes on an existing reply. */
function reSubject(subject: string): string {
  return /^re:/i.test(subject.trim()) ? subject : `Re: ${subject}`;
}

export class App {
  private root: HTMLElement;
  private status: StatusResponse | null = null;
  private contacts: ContactDto[] = [];
  private rail: Rail = "chats";
  private selectedNick: string | null = null;
  private pollTimer: number | null = null;
  private unlockedAt = 0;
  private search = "";
  private settingsSection: SettingsSection = "appearance";
  private liveRegion!: HTMLElement;
  private addFriendOpen = false;
  private profileNick: string | null = null;
  private paletteOpen = false;
  private filesContactNick: string | null = null;
  private filesDirection: "received" | "sent" = "received";
  private recoveryModalCode: string | null = null;
  private mailFolder: MailFolder = "inbox";
  /** The conversation open in the reading pane. */
  private mailThreadId: string | null = null;
  /** The message the user clicked, highlighted inside its conversation. */
  private mailOpenId: string | null = null;
  private mailSearch = "";
  private mailComposeOpen = false;
  /** Set when compose is editing an existing draft rather than a new message. */
  private mailDraftId: string | null = null;
  /** Reply target and thread, when compose was opened from a conversation. */
  private mailReplyTo: string | null = null;
  private mailReplyThread: string | null = null;
  /** Subject to prefill, for a reply. */
  private mailSubjectSeed: string | null = null;
  /**
   * The open compose's fields.
   *
   * Held here rather than read back out of the DOM because compose re-renders
   * (adding an attachment, loading a draft's files) and a rebuilt field would
   * otherwise forget what was typed. The body is HTML from the rich-text
   * editor.
   */
  private mailComposeTo = "";
  private mailComposeSubject = "";
  private mailComposeBody = "";
  /**
   * Which kind of recipient compose is addressing.
   *
   * Internal picks a contact, and the message goes through the encrypted
   * session. External types an address, and the message is sealed under a PIN
   * and parked on the relay instead — a different transport, not a different
   * field.
   */
  private mailComposeMode: "internal" | "external" = "internal";
  /** The typed address for an external send. */
  private mailComposeExternalTo = "";
  /**
   * The delivery PIN for an external send.
   *
   * Held in memory only. It is never written to a draft and never sent to the
   * daemon except on the send itself, because the whole point is that it travels
   * to the recipient through a channel the link does not use.
   */
  private mailComposePin = "";
  /**
   * Which credential an external drop is sealed under.
   *
   * PIN is the original mode: the sender chooses a secret and shares it out of
   * band. Public key is the second: the recipient publishes a key and the
   * sender pastes it, so no shared secret ever changes hands.
   */
  private mailComposeDropMode: "pin" | "publickey" = "pin";
  /**
   * The recipient's pasteable drop public key for an external send.
   *
   * Held in memory only, like the PIN. It is a public value, but persisting it
   * would let a stale key outlive the send that used it.
   */
  private mailComposePublicKey = "";
  /**
   * The link from the last successful drop.
   *
   * Set instead of navigating away, because the send is not finished until the
   * sender has the link: the panel stays up until they say they have copied it.
   */
  private mailDropResult: {
    link: string;
    to: string;
    expiresUnix: number;
    /** Which credential the drop was sealed under, so the panel says the right thing. */
    mode: "pin" | "publickey";
  } | null = null;
  /**
   * Attachments held for the open compose.
   *
   * Bytes rather than refs, because both save and send post the bytes, and a
   * draft being edited has to carry its existing files forward.
   */
  private mailComposeFiles: { name: string; dataBase64: string; size: number }[] = [];
  private mailHandle: string | null = null;
  private mailDomain = "myserver.com";
  /**
   * The contact whose Delete confirmation is open, if any.
   *
   * Deletion lives here and nowhere else: the old remove button in the profile
   * modal was a second, buried way to do the same thing, which is exactly the
   * kind of split the Trusts rail exists to remove.
   */
  private trustDeleteNick: string | null = null;

  /**
   * What the user has typed into the delete dialog's confirmation field.
   *
   * Kept in memory only so a background re-render does not wipe it mid-typing;
   * it is cleared every time the dialog opens, so a previous confirmation can
   * never carry over to the next person.
   */
  private trustDeleteConfirm = "";

  /** Read/star/archive/trash, straight from `ui-prefs.json`. */
  private mailPrefs(): MailFlags {
    return mailFlags();
  }

  constructor(root: HTMLElement) {
    this.root = root;
  }

  async start(): Promise<void> {
    // The shell injects the transport before the page runs. A plain browser
    // never sets it, so waiting there only stalls every dev reload.
    if (isTauriShell()) {
      for (let i = 0; i < 30; i++) {
        if (window.__HUSH_API__) break;
        await sleep(50);
      }
    }
    this.liveRegion = document.createElement("div");
    this.liveRegion.setAttribute("aria-live", "polite");
    this.liveRegion.className = "sr-only";
    document.body.appendChild(this.liveRegion);
    document.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "k") {
        e.preventDefault();
        this.paletteOpen = !this.paletteOpen;
        this.render();
      }
      if (e.key === "Escape") {
        this.paletteOpen = false;
        this.addFriendOpen = false;
        this.profileNick = null;
        this.closeTrustDelete();
        if (this.filesContactNick) this.filesContactNick = null;
        this.render();
      }
    });
    await this.refreshStatus();
    this.render();
  }

  private announce(msg: string): void {
    this.liveRegion.textContent = msg;
  }

  private async refreshStatus(): Promise<void> {
    try {
      const status = await api.status();
      this.status = status;
      if (!status || status.locked) {
        this.stopPoll();
      } else {
        this.contacts = (await api.contacts()) ?? [];
        setKnownContacts(this.contacts.map((c) => c.nickname));
        if (!this.selectedNick && this.contacts[0]) {
          this.selectedNick = this.contacts[0].nickname;
        }
        this.mailHandle = status.mail_handle ?? null;
        this.mailDomain = status.mail_domain ?? "myserver.com";
        await this.reloadMailbox();
        this.startPoll();
      }
    } catch {
      this.status = {
        locked: true,
        vault_dir: "",
        protocol_name: null,
        vault_bytes: 0,
        contacts: 0,
        recovery_acknowledged: true,
        last_backup_unix: 0,
        relay: getApiBase(),
        relay_source: "unknown",
        socket: null,
        tcp_addr: null,
        mail_handle: null,
        mail_domain: "myserver.com",
      };
    }
  }

  private startPoll(): void {
    if (this.pollTimer) return;
    this.pollTimer = window.setInterval(() => this.pollRecv(), RECV_POLL_MS);
  }

  /** Pull the whole mailbox: messages and drafts arrive together so the two
   *  halves of the Mail rail can never disagree. Returns whether it changed. */
  private async reloadMailbox(): Promise<boolean> {
    const box = await api.mail();
    if (!box) return false;
    return loadMailbox(box.mail ?? [], box.drafts ?? []);
  }

  private stopPoll(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
  }

  private async pollRecv(): Promise<void> {
    if (!this.status || this.status.locked) return;
    try {
      await api.presenceHeartbeat();
      const r = await api.recv();
      const fresh = (await api.contacts()) ?? [];
      const prevOnline = new Map(
        this.contacts.map((c) => [c.nickname, c.online]),
      );
      const presenceChanged =
        fresh.length !== this.contacts.length ||
        fresh.some((c) => prevOnline.get(c.nickname) !== c.online);
      this.contacts = fresh;
      setKnownContacts(fresh.map((c) => c.nickname));
      if (!r) {
        if (presenceChanged) this.render();
        return;
      }
      for (const w of r.warnings) this.announce(w);
      let mailArrived = false;
      for (const m of r.messages) {
        if (m.kind === "mail") {
          // Reloaded from the daemon rather than appended here, so the message
          // carries the record id the read/unread flags are keyed on.
          mailArrived = true;
          this.announce(`New mail from ${m.from}`);
        } else {
          ingestIncoming(m.from, m);
          this.announce(`New message from ${m.from}`);
        }
      }
      // Mail also changes without an incoming message reaching this poll: the
      // daemon's Outbox flush moves a message from Queued to Sent, and another
      // device can file or delete one. While the Mail rail is up, re-read the
      // mailbox so the list is never stale.
      let mailboxChanged = false;
      if (mailArrived || this.rail === "mail") {
        mailboxChanged = await this.reloadMailbox();
      }
      // Never repaint over a half-written message.
      if ((r.messages.length || presenceChanged || mailboxChanged) && !this.mailComposeOpen) {
        this.render();
      }
    } catch {
      /* daemon busy */
    }
  }

  private async lock(): Promise<void> {
    await api.lock();
    clearChatStore();
    clearMailStore();
    clearMockLocal();
    this.selectedNick = null;
    this.filesContactNick = null;
    this.mailThreadId = null;
    this.mailOpenId = null;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.mailReplyTo = null;
    this.mailReplyThread = null;
    this.mailSearch = "";
    this.unlockedAt = 0;
    this.stopPoll();
    await this.refreshStatus();
    this.render();
  }

  private render(): void {
    this.root.innerHTML = "";
    if (!this.status || this.status.locked) {
      this.root.appendChild(this.renderUnlock());
      return;
    }
    const shell = document.createElement("div");
    shell.className = "shell";
    shell.appendChild(this.renderRail());
    if (this.rail === "chats") {
      shell.appendChild(this.renderConvList());
      shell.appendChild(this.renderMainChat());
    } else if (this.rail === "trusts") {
      shell.appendChild(this.renderTrusts());
    } else if (this.rail === "mail") {
      shell.appendChild(this.renderMailFolders());
      shell.appendChild(this.renderMailMain());
    } else if (this.rail === "files") {
      shell.appendChild(this.renderFiles());
    } else if (this.rail === "vault") {
      shell.appendChild(this.renderVault());
    } else {
      shell.appendChild(this.renderSettings());
    }
    this.root.appendChild(shell);
    if (this.paletteOpen) this.root.appendChild(this.renderPalette());
    if (this.addFriendOpen) this.root.appendChild(this.renderAddFriend());
    if (this.profileNick) this.root.appendChild(this.renderProfile());
    if (this.recoveryModalCode) this.root.appendChild(this.renderRecoveryModal());
    if (this.trustDeleteNick) this.root.appendChild(this.renderTrustDeleteModal());
  }

  private async maybeShowRecoveryCode(): Promise<void> {
    try {
      const pending = await api.recoveryPending();
      if (pending?.code) this.recoveryModalCode = pending.code;
    } catch {
      /* no pending code */
    }
  }

  private renderRecoveryModal(): HTMLElement {
    const code = this.recoveryModalCode!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", "Recovery code");
    modal.innerHTML = `<h2 class="text-22">Save your recovery code</h2><p class="muted text-13">Store this somewhere safe. It can unlock your vault if you forget your passphrase. We will only show it once.</p>`;
    const pre = document.createElement("p");
    pre.className = "text-15";
    pre.style.fontFamily = "monospace";
    pre.style.wordBreak = "break-all";
    pre.textContent = code;
    const btn = document.createElement("button");
    btn.className = "btn-primary pressable";
    btn.style.marginTop = "var(--space-4)";
    btn.textContent = "I've saved it";
    btn.onclick = async () => {
      await api.recoveryAcknowledge();
      this.recoveryModalCode = null;
      await this.refreshStatus();
      this.render();
    };
    modal.append(pre, btn);
    back.appendChild(modal);
    return back;
  }

  private renderUnlock(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "unlock-screen";
    const card = document.createElement("div");
    card.className = "unlock-card";
    const logoWrap = document.createElement("div");
    logoWrap.className = "logo-wrap";
    const ring = document.createElement("div");
    ring.className = "logo-ring breathe";
    const mark = document.createElement("div");
    mark.className = "logo-mark";
    mark.appendChild(icon("vault"));
    logoWrap.append(ring, mark);
    const title = document.createElement("h1");
    title.className = "text-28";
    title.textContent = "Welcome back";
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.textContent = "Your vault stays on this device.";
    const pass = document.createElement("input");
    pass.type = "password";
    pass.className = "input-field";
    pass.placeholder = "Passphrase";
    pass.autocomplete = "current-password";
    pass.style.marginTop = "var(--space-4)";
    const showRow = document.createElement("label");
    showRow.className = "text-13 muted";
    showRow.style.display = "flex";
    showRow.style.gap = "8px";
    showRow.style.marginTop = "8px";
    const showCb = document.createElement("input");
    showCb.type = "checkbox";
    showCb.onchange = () => {
      pass.type = showCb.checked ? "text" : "password";
    };
    showRow.append(showCb, document.createTextNode("Show passphrase"));
    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.hidden = true;
    const btn = document.createElement("button");
    btn.className = "btn-primary pressable";
    btn.style.marginTop = "var(--space-4)";
    btn.textContent = "Unlock";
    btn.onclick = async () => {
      err.hidden = true;
      try {
        await api.unlock(pass.value);
        pass.value = "";
        this.unlockedAt = Date.now();
        ring.classList.add("bloom");
        await sleep(400);
        await this.refreshStatus();
        await refreshContactPrefs();
        const history = await api.messages();
        if (history) loadHistory(history);
        await this.maybeShowRecoveryCode();
        this.render();
      } catch (e) {
        card.classList.add("shake");
        setTimeout(() => card.classList.remove("shake"), 300);
        err.hidden = false;
        err.textContent =
          e instanceof AppError ? e.message : "That passphrase didn't work. Try again.";
      }
    };
    const daemon = document.createElement("p");
    daemon.className = "faint text-12";
    daemon.style.marginTop = "var(--space-4)";
    daemon.textContent = getApiBase();
    card.append(logoWrap, title, hint, pass, showRow, err, btn, daemon);
    wrap.appendChild(card);
    return wrap;
  }

  private railBtn(id: Rail, label: string, ic: IconName, badge = false): HTMLElement {
    const b = document.createElement("button");
    b.className = `rail-btn pressable${this.rail === id ? " active" : ""}`;
    b.appendChild(icon(ic));
    const span = document.createElement("span");
    span.textContent = label;
    b.appendChild(span);
    if (badge) {
      const dot = document.createElement("span");
      dot.className = "rail-badge";
      dot.setAttribute("aria-label", "Unread");
      b.appendChild(dot);
    }
    b.onclick = () => {
      this.rail = id;
      this.render();
      // The Mail rail reads the encrypted log, so pick up anything that landed
      // while another rail was showing.
      if (id === "mail") {
        void this.reloadMailbox().then((changed) => {
          if (changed && this.rail === "mail" && !this.mailComposeOpen) this.render();
        });
      }
    };
    return b;
  }

  private renderRail(): HTMLElement {
    const rail = document.createElement("nav");
    rail.className = "rail";
    rail.setAttribute("aria-label", "Main");
    const logo = document.createElement("div");
    logo.className = "logo-mark";
    logo.style.width = "40px";
    logo.style.height = "40px";
    logo.appendChild(icon("vault"));
    rail.appendChild(logo);
    rail.appendChild(this.railBtn("chats", "Chats", "chats"));
    rail.appendChild(this.railBtn("trusts", "Trusts", "trusts"));
    rail.appendChild(this.railBtn("mail", "Mail", "mail", hasUnreadMail(this.mailPrefs())));
    const spacer = document.createElement("div");
    spacer.style.height = "40px";
    rail.appendChild(spacer);
    const flex = document.createElement("div");
    flex.className = "rail-spacer";
    rail.appendChild(flex);
    rail.appendChild(this.railBtn("files", "Files", "files"));
    rail.appendChild(this.railBtn("vault", "Vault", "vault"));
    rail.appendChild(this.railBtn("settings", "Settings", "settings"));
    return rail;
  }

  private renderConvList(): HTMLElement {
    const col = document.createElement("div");
    col.className = "conv-list";
    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Chats";
    const plus = document.createElement("button");
    plus.className = "btn-icon-round pressable";
    plus.appendChild(icon("plus"));
    plus.setAttribute("aria-label", "Add a trust");
    plus.onclick = () => {
      this.addFriendOpen = true;
      this.render();
    };
    head.append(h, plus);
    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const inp = document.createElement("input");
    inp.placeholder = "Search or press Ctrl+K";
    inp.value = this.search;
    inp.oninput = () => {
      this.search = inp.value;
      this.render();
    };
    search.appendChild(inp);
    const rows = document.createElement("div");
    rows.className = "conv-rows";
    const q = this.search.toLowerCase();
    const list = this.contacts.filter((c) =>
      displayNick(c.nickname).toLowerCase().includes(q),
    );
    if (!list.length) {
      const empty = document.createElement("p");
      empty.className = "muted";
      empty.style.padding = "var(--space-4)";
      empty.textContent = "No chats yet";
      rows.appendChild(empty);
    }
    for (const c of list) {
      const row = document.createElement("button");
      row.className = `conv-row pressable${this.selectedNick === c.nickname ? " selected" : ""}`;
      const av = avatarWithPresence(
        c.protocol_name,
        c.online,
        "sm",
        hasUnread(c.nickname),
      );
      const meta = document.createElement("div");
      meta.style.flex = "1";
      meta.style.minWidth = "0";
      const name = document.createElement("div");
      name.className = "text-15";
      name.style.fontWeight = "500";
      name.textContent = displayNick(c.nickname);
      const prev = lastPreview(c.nickname);
      const sub = document.createElement("div");
      sub.className = "text-13 muted";
      sub.style.overflow = "hidden";
      sub.style.textOverflow = "ellipsis";
      sub.style.whiteSpace = "nowrap";
      sub.textContent = prev?.text ?? (c.inbound_ready ? "Ready to chat" : "Waiting for them to add you");
      meta.append(name, sub);
      const time = document.createElement("span");
      time.className = "text-12 faint";
      time.textContent = prev ? formatRelative(prev.time) : "";
      row.append(av, meta, time);
      row.onclick = () => {
        this.selectedNick = c.nickname;
        markRead(c.nickname);
        this.render();
      };
      rows.appendChild(row);
    }
    col.append(head, search, rows);
    return col;
  }

  private renderMainChat(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.position = "relative";
    const nick = this.selectedNick;
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact || !nick) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      empty.innerHTML = `<svg width="120" height="80" viewBox="0 0 120 80" aria-hidden="true"><ellipse cx="60" cy="50" rx="40" ry="20" fill="var(--accent-soft)"/><circle cx="60" cy="35" r="18" fill="var(--accent)" opacity="0.3"/></svg>`;
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "Quiet in here";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Invite someone to start your first chat";
      const b = document.createElement("button");
      b.className = "btn-primary pressable";
      b.textContent = "Add a trust";
      b.onclick = () => {
        this.addFriendOpen = true;
        this.render();
      };
      empty.append(t, p, b);
      pane.appendChild(empty);
      return pane;
    }

    const header = document.createElement("div");
    header.className = "chat-header";
    const left = document.createElement("div");
    left.style.display = "flex";
    left.style.gap = "var(--space-3)";
    left.style.alignItems = "center";
    left.appendChild(avatarWithPresence(contact.protocol_name, contact.online, "lg"));
    const titles = document.createElement("div");
    const n = document.createElement("div");
    n.className = "text-17";
    n.style.fontWeight = "500";
    n.textContent = displayNick(nick);
    const sub = document.createElement("div");
    sub.className = "text-13 muted";
    sub.style.display = "flex";
    sub.style.alignItems = "center";
    sub.style.gap = "6px";
    sub.style.flexWrap = "wrap";
    const presence = document.createElement("span");
    presence.className = contact.online ? "presence-label online" : "presence-label";
    presence.textContent = contact.online ? "Online" : "Offline";
    sub.append(presence, document.createTextNode("·"));
    const lockLine = document.createElement("span");
    lockLine.style.display = "inline-flex";
    lockLine.style.alignItems = "center";
    lockLine.style.gap = "4px";
    lockLine.append(icon("lock"), document.createTextNode("End-to-end encrypted"));
    sub.append(lockLine);
    titles.append(n, sub);
    left.append(titles);
    const more = document.createElement("button");
    more.className = "pressable";
    more.appendChild(icon("more"));
    more.onclick = () => {
      this.profileNick = nick;
      this.render();
    };
    header.append(left, more);

    const scroll = document.createElement("div");
    scroll.className = "chat-messages";
    const msgs = getThread(nick);
    let lastDay = "";
    for (const group of groupMessages(msgs)) {
      if (group.day !== lastDay) {
        lastDay = group.day;
        const sep = document.createElement("div");
        sep.className = "day-sep";
        sep.textContent = group.day;
        scroll.appendChild(sep);
      }
      const gEl = document.createElement("div");
      gEl.className = `bubble-group ${group.outgoing ? "out" : "in"}`;
      group.messages.forEach((m, i) => {
        const tail = i === group.messages.length - 1;
        const b = document.createElement("div");
        b.className = `bubble ${group.outgoing ? "out" : "in"}${tail ? " tail" : ""}`;
        if (m.kind === "text") b.textContent = m.text ?? "";
        else {
          const fc = document.createElement("div");
          fc.className = "file-card";
          fc.innerHTML = `<div class="text-15" style="font-weight:500">${m.fileName}</div><div class="text-13 muted">${formatBytes(m.fileSize ?? 0)}</div><div class="text-12" style="color:var(--success-text);background:var(--success-soft);display:inline-block;padding:2px 8px;border-radius:var(--radius-pill);margin-top:8px">Sealed on your device</div>`;
          if (m.sendProgress !== undefined) {
            const bar = document.createElement("div");
            bar.style.height = "3px";
            bar.style.background = "var(--border)";
            bar.style.marginTop = "8px";
            bar.style.borderRadius = "2px";
            const fill = document.createElement("div");
            fill.style.height = "100%";
            fill.style.width = `${m.sendProgress}%`;
            fill.style.background = "var(--accent)";
            bar.appendChild(fill);
            fc.appendChild(bar);
          }
          const uploading =
            m.sendProgress !== undefined && m.sendProgress < 100;
          if (!uploading && (m.fileData || m.blobId)) {
            attachFileActions(fc, m.fileName ?? "file", () =>
              resolveMessageFileBytes(m),
            );
          } else if (!uploading && !m.fileData && !m.blobId) {
            const hint = document.createElement("p");
            hint.className = "text-12 muted";
            hint.style.marginTop = "var(--space-2)";
            hint.textContent =
              "File bytes are not on this device. Ask your contact to send again.";
            fc.appendChild(hint);
          }
          b.appendChild(fc);
        }
        if (m.reactions.length) {
          const pills = document.createElement("div");
          pills.style.display = "flex";
          pills.style.gap = "4px";
          pills.style.marginTop = "4px";
          m.reactions.forEach((r) => {
            const p = document.createElement("span");
            p.className = "text-12";
            p.style.background = "var(--surface)";
            p.style.padding = "2px 6px";
            p.style.borderRadius = "var(--radius-pill)";
            p.textContent = r;
            pills.appendChild(p);
          });
          b.appendChild(pills);
        }
        b.onmouseenter = () => {
          /* reaction bar simplified: double-click heart */
        };
        b.ondblclick = () => {
          toggleReaction(m.id, "❤");
          this.render();
        };
        gEl.appendChild(b);
      });
      scroll.appendChild(gEl);
    }
    scroll.scrollTop = scroll.scrollHeight;

    const composer = this.renderComposer(nick, contact.online);
    pane.append(header, scroll, composer);

    pane.ondragover = (e) => {
      e.preventDefault();
    };
    pane.ondrop = async (e) => {
      e.preventDefault();
      const f = e.dataTransfer?.files?.[0];
      if (f) await this.sendFile(nick, f);
    };

    return pane;
  }

  private renderComposer(nick: string, contactOnline: boolean): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "composer";
    if (!contactOnline) {
      const hint = document.createElement("p");
      hint.className = "text-13 muted composer-offline-hint";
      hint.textContent =
        `${displayNick(nick)} is offline. Chat and files need you both online — use Mail to reach them later.`;
      wrap.appendChild(hint);
    }
    const inner = document.createElement("div");
    inner.className = "composer-inner";
    if (!contactOnline) inner.classList.add("composer-disabled");
    const attach = document.createElement("button");
    attach.className = "pressable";
    attach.appendChild(icon("attach"));
    const fileIn = document.createElement("input");
    fileIn.type = "file";
    fileIn.hidden = true;
    attach.onclick = () => fileIn.click();
    fileIn.onchange = async () => {
      const f = fileIn.files?.[0];
      if (f) await this.sendFile(nick, f);
      fileIn.value = "";
    };
    const ta = document.createElement("textarea");
    ta.rows = 1;
    ta.placeholder = contactOnline ? "Message" : "Waiting for them to come online…";
    ta.disabled = !contactOnline;
    const send = document.createElement("button");
    send.className = "send-fab btn-icon-round pressable";
    send.appendChild(icon("send"));
    const syncSend = () => {
      send.classList.toggle("ready", ta.value.trim().length > 0);
    };
    ta.oninput = () => {
      syncSend();
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight}px`;
    };
    ta.onkeydown = (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void this.sendText(nick, ta.value.trim(), ta, send);
      }
    };
    send.disabled = !contactOnline;
    send.onclick = () => void this.sendText(nick, ta.value.trim(), ta, send);
    attach.disabled = !contactOnline;
    inner.append(attach, fileIn, ta, send);
    wrap.appendChild(inner);
    syncSend();
    return wrap;
  }

  private async sendText(nick: string, text: string, ta: HTMLTextAreaElement, send: HTMLButtonElement): Promise<void> {
    if (!text) return;
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact?.online) {
      this.announce(`${displayNick(nick)} is offline. Chat needs you both online — send Mail instead.`);
      return;
    }
    const msg = addOutgoingText(nick, text);
    ta.value = "";
    send.classList.remove("ready");
    this.render();
    try {
      await api.sendText(nick, text);
    } catch (e) {
      removeOutgoingMessage(nick, msg.id);
      this.announce(e instanceof AppError ? e.message : "Couldn't send. Try again.");
      this.render();
    }
  }

  private async sendFile(nick: string, file: File): Promise<void> {
    const contact = this.contacts.find((c) => c.nickname === nick);
    if (!contact?.online) {
      this.announce(`${displayNick(nick)} is offline. Chat needs you both online — send Mail instead.`);
      return;
    }
    if (file.size > MAX_FILE_BYTES) {
      this.announce(`Files are limited to ${Math.round(MAX_FILE_BYTES / 1024)} KB for now`);
      return;
    }
    const buf = new Uint8Array(await file.arrayBuffer());
    const msg = addOutgoingFile(nick, file.name, file.size, buf);
    this.render();
    try {
      for (let p = 20; p <= 100; p += 20) {
        setSendProgress(msg.id, p);
        this.render();
        await sleep(80);
      }
      const b64 = await fileToBase64(file);
      const resp = await api.sendFileData(nick, file.name, b64);
      if (resp?.blob_id) setMessageBlobId(nick, msg.id, resp.blob_id);
      clearSendProgress(nick, msg.id);
      this.render();
    } catch (e) {
      removeOutgoingMessage(nick, msg.id);
      this.announce(e instanceof AppError ? e.message : "Couldn't send. Try again.");
      this.render();
    }
  }

  // --------------------------------------------------------------- trusts

  /**
   * The Trusts rail: every contact as a card rather than a row.
   *
   * A card is a person, so it carries the same avatar and presence the Chats
   * rail already renders for them, and its actions are built from a list. Adding
   * an action is one entry in that list, not a layout change.
   */
  private renderTrusts(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane trusts-pane";

    const head = document.createElement("div");
    head.className = "trusts-head";
    const titles = document.createElement("div");
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Trusts";
    const sub = document.createElement("p");
    sub.className = "muted text-13";
    sub.style.marginTop = "var(--space-2)";
    sub.textContent = this.contacts.length
      ? `${this.contacts.length} ${this.contacts.length === 1 ? "person" : "people"} in your circle.`
      : "People you have exchanged invites with.";
    titles.append(h, sub);
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn-primary pressable trusts-add";
    add.textContent = "Add a trust";
    add.onclick = () => {
      this.addFriendOpen = true;
      this.render();
    };
    head.append(titles, add);
    pane.appendChild(head);

    // A filter over the cards, so a long list stays findable. It toggles the
    // rendered cards directly instead of re-rendering, so typing never loses
    // focus.
    const filter = document.createElement("div");
    filter.className = "search-pill trusts-filter";
    const filterInput = document.createElement("input");
    filterInput.type = "search";
    filterInput.placeholder = "Filter trusts";
    filterInput.setAttribute("aria-label", "Filter trusts");
    filter.append(icon("search"), filterInput);

    if (!this.contacts.length) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      const t = document.createElement("h2");
      t.className = "text-22";
      t.textContent = "No trusts yet";
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Add someone and they will appear here as a card.";
      const b = document.createElement("button");
      b.className = "btn-primary pressable";
      b.textContent = "Add a trust";
      b.onclick = () => {
        this.addFriendOpen = true;
        this.render();
      };
      empty.append(t, p, b);
      pane.appendChild(empty);
      return pane;
    }

    // Ordered by the name the card actually shows, so the grid stays scannable
    // as it grows. Sorted on a copy: the contact array's order is not ours to
    // change, and a comparison by nickname could not see the decoration.
    const ordered = [...this.contacts].sort((a, b) =>
      displayNick(a.nickname).localeCompare(displayNick(b.nickname), undefined, {
        sensitivity: "base",
      }),
    );
    const grid = document.createElement("div");
    grid.className = "bento trusts-grid";
    for (const c of ordered) grid.appendChild(this.trustCard(c));

    const noMatch = document.createElement("p");
    noMatch.className = "muted text-13 trusts-filter-empty";
    noMatch.hidden = true;
    noMatch.textContent = "No trusts match that filter.";

    filterInput.oninput = () => {
      const q = filterInput.value.trim().toLocaleLowerCase();
      let shown = 0;
      for (const el of Array.from(grid.children) as HTMLElement[]) {
        const match =
          !q || (el.dataset.trustName ?? "").toLocaleLowerCase().includes(q);
        el.hidden = !match;
        if (match) shown++;
      }
      noMatch.hidden = shown > 0;
    };

    pane.append(filter, grid, noMatch);
    return pane;
  }

  /** One contact card: avatar, name, presence, and the action row. */
  private trustCard(c: ContactDto): HTMLElement {
    const card = document.createElement("div");
    card.className = "tile trust-card";
    // The name as shown, so the filter matches what the user can see.
    card.dataset.trustName = displayNick(c.nickname);

    const top = document.createElement("div");
    top.className = "trust-card-top";
    top.appendChild(avatarWithPresence(c.protocol_name, c.online, "lg"));

    const meta = document.createElement("div");
    meta.className = "trust-card-meta";
    const name = document.createElement("div");
    name.className = "text-17 trust-card-name";
    name.textContent = displayNick(c.nickname);
    const presence = document.createElement("div");
    presence.className = `presence-label text-13${c.online ? " online" : ""}`;
    presence.textContent = c.online ? "Online" : "Offline";
    meta.append(name, presence);
    if (c.mail_handle) {
      const mail = document.createElement("div");
      mail.className = "muted text-12 trust-card-mail";
      mail.textContent = `${c.mail_handle}@${this.mailDomain}`;
      meta.appendChild(mail);
    }
    top.appendChild(meta);

    // The extensible action row. Each action is a label, an icon and a handler;
    // a future action drops in here without touching the card layout.
    const actions: {
      label: string;
      ic: IconName;
      run: () => void;
      danger?: boolean;
      /**
       * Destructive actions render icon-only, muted, and pushed away from the
       * primary ones. A delete that is the easiest thing on the card to hit is
       * a delete that gets hit by accident.
       */
      iconOnly?: boolean;
    }[] = [
      { label: "Chat", ic: "chats", run: () => this.openChat(c.nickname) },
      { label: "Email", ic: "mail", run: () => this.emailContact(c.nickname) },
      {
        label: "Delete",
        ic: "trash",
        danger: true,
        iconOnly: true,
        run: () => this.openTrustDelete(c.nickname),
      },
    ];
    const row = document.createElement("div");
    row.className = "trust-actions";
    for (const a of actions) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = `btn-secondary pressable trust-action${a.danger ? " danger" : ""}${
        a.iconOnly ? " icon-only" : ""
      }`;
      // Icon-only buttons still need a name for assistive tech and a tooltip
      // for everyone else.
      b.setAttribute("aria-label", a.label);
      b.title = a.label;
      b.appendChild(icon(a.ic));
      if (!a.iconOnly) {
        const s = document.createElement("span");
        s.textContent = a.label;
        b.appendChild(s);
      }
      b.onclick = a.run;
      row.appendChild(b);
    }

    card.append(top, row);
    return card;
  }

  /** Open the chat thread with a contact, from any rail. */
  private openChat(nick: string): void {
    this.rail = "chats";
    this.selectedNick = nick;
    markRead(nick);
    this.render();
  }

  /** Open the mail composer with a contact already chosen as the recipient. */
  private emailContact(nick: string): void {
    this.rail = "mail";
    this.openCompose({ to: nick });
  }

  /** Open the delete dialog for a contact, always with an empty confirmation. */
  private openTrustDelete(nick: string): void {
    this.trustDeleteNick = nick;
    this.trustDeleteConfirm = "";
    this.render();
  }

  /** Close the delete dialog and forget anything typed into it. */
  private closeTrustDelete(): void {
    this.trustDeleteNick = null;
    this.trustDeleteConfirm = "";
  }

  private renderTrustDeleteModal(): HTMLElement {
    const nick = this.trustDeleteNick!;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal trust-delete-modal";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    modal.setAttribute("aria-label", `Delete ${displayNick(nick)} from Trusts`);

    const h = document.createElement("h2");
    h.className = "text-22";
    h.textContent = `Delete ${displayNick(nick)}?`;
    const p = document.createElement("p");
    p.className = "muted text-13";
    p.style.marginTop = "var(--space-3)";
    p.textContent = `This removes ${displayNick(nick)} from Trusts. Your chat and mail history with them stays in your vault.`;
    const p2 = document.createElement("p");
    p2.className = "muted text-13";
    p2.style.marginTop = "var(--space-2)";
    p2.textContent =
      "They disappear from Chats, Mail and Trusts, and they can no longer message you. This cannot be undone.";
    // The one honest limit: a contact added before per-contact queues existed is
    // still on the shared inbox, which is not burned (that would cut off every
    // old contact at once). They become revocable the moment they pick up their
    // own private queue.
    const limit = document.createElement("p");
    limit.className = "faint text-12";
    limit.style.marginTop = "var(--space-2)";
    limit.textContent =
      "A contact you added before this update may still be able to send to the old shared inbox until they pick up their own private queue. Nothing they send will open on this device, and they become fully cut off once they do.";
    const note = document.createElement("p");
    note.className = "faint text-12";
    note.style.marginTop = "var(--space-2)";
    note.textContent =
      "You can add them back later with a fresh invite; their old messages stay in your vault.";

    const label = document.createElement("label");
    label.className = "trust-delete-field";
    const labelText = document.createElement("span");
    labelText.className = "text-13";
    labelText.textContent = "Type DELETE to confirm";
    const input = document.createElement("input");
    input.type = "text";
    input.className = "input-field";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocapitalize", "characters");
    input.setAttribute("spellcheck", "false");
    input.value = this.trustDeleteConfirm;
    label.append(labelText, input);

    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "btn-secondary pressable";
    cancel.textContent = "Cancel";
    cancel.onclick = () => {
      this.closeTrustDelete();
      this.render();
    };
    const del = document.createElement("button");
    del.type = "button";
    del.className = "btn-primary pressable trust-delete-confirm";
    del.style.background = "var(--danger)";
    del.textContent = "Delete";
    // Matched case-insensitively on trimmed text, and gated on the same check
    // for the button, the Enter key, and the click handler, so there is one
    // rule and no way around it.
    const confirmed = () => input.value.trim().toUpperCase() === "DELETE";
    const sync = () => {
      this.trustDeleteConfirm = input.value;
      del.disabled = !confirmed();
    };
    input.oninput = sync;
    input.onkeydown = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (confirmed()) void this.deleteTrust(nick);
      }
    };
    sync();
    del.onclick = () => {
      if (confirmed()) void this.deleteTrust(nick);
    };

    row.append(cancel, del);
    modal.append(h, p, p2, limit, note, label, row);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.closeTrustDelete();
        this.render();
      }
    };
    // Focus the field, not the destructive button, so a stray Enter cannot
    // delete anyone.
    queueMicrotask(() => input.focus());
    return back;
  }

  /**
   * Remove a contact and revoke their ability to message this account.
   *
   * History is deliberately not touched: the chat log and mail records stay in
   * the vault, and the contact simply stops being surfaced. What changed from a
   * plain removal is that the daemon now calls `revoke_contact`, which burns the
   * per-contact queue this account minted for them. The deleted person then has
   * no queue to deliver into, and anything already queued is dropped rather than
   * filed.
   *
   * The one limit: a contact added before per-contact queues existed is still on
   * the shared inbox, which is not burned because that would cut off every old
   * contact at once. Such a contact is not revocable until they receive their own
   * queue; the delete modal says so.
   */
  private async deleteTrust(nick: string): Promise<void> {
    try {
      await api.removeContact(nick);
      await clearContactUiPrefs(nick);
      this.contacts = (await api.contacts()) ?? [];
      setKnownContacts(this.contacts.map((c) => c.nickname));
      if (this.selectedNick === nick) this.selectedNick = null;
      if (this.filesContactNick === nick) this.filesContactNick = null;
      if (this.profileNick === nick) this.profileNick = null;
      // The open conversation and composer belong to the person being removed,
      // so neither may keep showing them.
      this.mailThreadId = null;
      this.mailOpenId = null;
      if (this.mailComposeMode === "internal" && this.mailComposeTo === nick) {
        this.mailComposeOpen = false;
        this.mailDraftId = null;
        this.mailComposeTo = "";
        this.mailComposeSubject = "";
        this.mailComposeBody = "";
        this.mailComposeFiles = [];
      }
      this.closeTrustDelete();
      await this.reloadMailbox();
      this.announce(`${nick} removed from your circle`);
      this.render();
    } catch (e) {
      this.closeTrustDelete();
      this.announce(e instanceof AppError ? e.message : "Could not remove contact");
      this.render();
    }
  }

  private renderFiles(): HTMLElement {
    if (this.filesContactNick) {
      return this.renderFilesForContact(this.filesContactNick);
    }
    return this.renderFilesContactList();
  }

  private renderFilesContactList(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.padding = "var(--space-5)";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Files";
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.style.marginTop = "var(--space-2)";
    hint.textContent =
      "Choose a contact to see files you’ve exchanged — stored only on your devices.";
    const list = document.createElement("div");
    list.style.marginTop = "var(--space-5)";
    list.style.display = "flex";
    list.style.flexDirection = "column";
    list.style.gap = "var(--space-2)";

    const nicks = contactsWithFiles(
      this.contacts.map((c) => c.nickname),
    );
    const also = this.contacts.filter((c) => !nicks.includes(c.nickname));

    for (const nick of [...nicks, ...also.map((c) => c.nickname)]) {
      const c = this.contacts.find((x) => x.nickname === nick);
      if (!c) continue;
      const count = fileCountForContact(nick);
      const row = document.createElement("button");
      row.type = "button";
      row.className = "conv-row pressable";
      row.style.width = "100%";
      row.style.textAlign = "left";
      const left = document.createElement("div");
      left.style.display = "flex";
      left.style.gap = "var(--space-3)";
      left.style.alignItems = "center";
      left.appendChild(createAvatarEl(c.protocol_name, "sm"));
      const meta = document.createElement("div");
      const title = document.createElement("div");
      title.className = "text-15";
      title.style.fontWeight = "500";
      title.textContent = displayNick(nick);
      const sub = document.createElement("div");
      sub.className = "text-13 muted";
      sub.textContent =
        count > 0
          ? `${count} file${count === 1 ? "" : "s"} exchanged`
          : "No files yet — open to send";
      meta.append(title, sub);
      left.append(meta);
      row.append(left);
      row.onclick = () => {
        this.filesContactNick = nick;
        this.filesDirection = "received";
        this.render();
      };
      list.appendChild(row);
    }

    if (!this.contacts.length) {
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Add a contact in Chats to exchange files.";
      list.appendChild(p);
    }

    pane.append(h, hint, list);
    return pane;
  }

  private renderFilesForContact(nick: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.padding = "var(--space-5)";

    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-secondary text-13 pressable";
    back.textContent = "← All contacts";
    back.onclick = () => {
      this.filesContactNick = null;
      this.render();
    };

    const h = document.createElement("h1");
    h.className = "text-22";
    h.style.marginTop = "var(--space-4)";
    h.textContent = displayNick(nick);
    const hint = document.createElement("p");
    hint.className = "muted text-13";
    hint.textContent = `Files you’ve exchanged with ${displayNick(nick)} — stored only on your devices.`;

    const tabs = document.createElement("div");
    tabs.className = "segmented";
    tabs.style.marginTop = "var(--space-4)";
    tabs.style.maxWidth = "280px";
    const recvTab = document.createElement("button");
    recvTab.type = "button";
    recvTab.className = this.filesDirection === "received" ? "active" : "";
    recvTab.textContent = "Received";
    recvTab.onclick = () => {
      this.filesDirection = "received";
      this.render();
    };
    const sentTab = document.createElement("button");
    sentTab.type = "button";
    sentTab.className = this.filesDirection === "sent" ? "active" : "";
    sentTab.textContent = "Sent";
    sentTab.onclick = () => {
      this.filesDirection = "sent";
      this.render();
    };
    tabs.append(recvTab, sentTab);

    const sendRow = document.createElement("div");
    sendRow.style.marginTop = "var(--space-4)";
    const sendBtn = document.createElement("button");
    sendBtn.type = "button";
    sendBtn.className = "btn-primary pressable";
    sendBtn.textContent = "Send file";
    const fileIn = document.createElement("input");
    fileIn.type = "file";
    fileIn.hidden = true;
    sendBtn.onclick = () => fileIn.click();
    fileIn.onchange = async () => {
      const f = fileIn.files?.[0];
      fileIn.value = "";
      if (f) await this.sendFile(nick, f);
    };
    sendRow.append(sendBtn, fileIn);

    const list = document.createElement("div");
    list.style.marginTop = "var(--space-4)";
    list.style.display = "flex";
    list.style.flexDirection = "column";
    list.style.gap = "var(--space-3)";

    const files = fileMessagesForContact(nick, this.filesDirection);
    for (const m of files) {
      const card = document.createElement("div");
      card.className = "tile";
      card.innerHTML = `<div class="text-15" style="font-weight:500">${m.fileName}</div><div class="text-13 muted">${formatBytes(m.fileSize ?? 0)} · ${formatRelative(m.createdAt)}</div>`;
      if (m.fileData || m.blobId) {
        attachFileActions(card, m.fileName ?? "file", () =>
          resolveMessageFileBytes(m),
        );
      }
      const chatLink = document.createElement("button");
      chatLink.type = "button";
      chatLink.className = "text-13 pressable";
      chatLink.style.marginTop = "var(--space-2)";
      chatLink.style.color = "var(--accent)";
      chatLink.textContent = "View in chat";
      chatLink.onclick = () => {
        this.rail = "chats";
        this.selectedNick = nick;
        this.filesContactNick = null;
        this.render();
      };
      card.appendChild(chatLink);
      list.appendChild(card);
    }

    if (!files.length) {
      const p = document.createElement("p");
      p.className = "muted text-13";
      p.textContent =
        this.filesDirection === "received"
          ? "Nothing received yet."
          : "You haven’t sent a file here yet.";
      list.appendChild(p);
    }

    pane.append(back, h, hint, tabs, sendRow, list);
    return pane;
  }

  private mailAddress(): string | null {
    return this.mailHandle ? `${this.mailHandle}@${this.mailDomain}` : null;
  }

  /** A 14px icon, for the inline marks in list rows. */
  private iconSmall(name: IconName): SVGElement {
    const el = icon(name);
    el.setAttribute("width", "14");
    el.setAttribute("height", "14");
    return el;
  }

  // ------------------------------------------------------------- sidebar

  private renderMailFolders(): HTMLElement {
    const col = document.createElement("div");
    col.className = "mail-folders";

    const head = document.createElement("div");
    head.className = "conv-list-header";
    const h = document.createElement("span");
    h.className = "text-22";
    h.textContent = "Mail";
    const compose = document.createElement("button");
    compose.type = "button";
    compose.className = "btn-icon-round pressable";
    compose.appendChild(icon("plus"));
    compose.setAttribute("aria-label", "Compose");
    compose.onclick = () => this.openCompose({});
    head.append(h, compose);

    const addr = document.createElement("p");
    addr.className = "muted text-12 mail-address";
    addr.textContent =
      this.mailAddress() ?? "No address yet — set one in Settings → Profile.";
    col.append(head, addr);

    const flags = this.mailPrefs();
    const folders: { id: MailFolder; ic: IconName }[] = [
      { id: "inbox", ic: "inbox" },
      { id: "sent", ic: "send" },
      { id: "outbox", ic: "outbox" },
      { id: "drafts", ic: "drafts" },
      { id: "starred", ic: "star" },
      { id: "archive", ic: "archive" },
      { id: "trash", ic: "trash" },
    ];

    const list = document.createElement("div");
    list.className = "mail-folder-list";
    for (const f of folders) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `mail-folder pressable${this.mailFolder === f.id ? " active" : ""}`;
      btn.appendChild(icon(f.ic));
      const label = document.createElement("span");
      label.className = "mail-folder-name";
      label.textContent = MAIL_FOLDER_LABEL[f.id];
      btn.appendChild(label);

      // Inbox shows how much is unread; the rest show how much is in them.
      const unread = f.id === "inbox" ? unreadCount(flags) : 0;
      const count = unread > 0 ? unread : folderCount(f.id, flags);
      if (count > 0) {
        const badge = document.createElement("span");
        badge.className = unread > 0 ? "mail-folder-count unread" : "mail-folder-count";
        badge.textContent = String(count);
        btn.appendChild(badge);
      }
      btn.onclick = () => this.openFolder(f.id);
      list.appendChild(btn);
    }
    col.appendChild(list);
    return col;
  }

  private openFolder(folder: MailFolder): void {
    this.mailFolder = folder;
    this.mailThreadId = null;
    this.mailOpenId = null;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.render();
  }

  // ----------------------------------------------------------- main pane

  private renderMailMain(): HTMLElement {
    if (this.mailComposeOpen) return this.renderMailCompose();
    if (this.mailThreadId) return this.renderMailThread(this.mailThreadId);
    return this.renderMailList();
  }

  private renderMailList(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";

    const bar = document.createElement("div");
    bar.className = "mail-toolbar";
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = MAIL_FOLDER_LABEL[this.mailFolder];
    const search = document.createElement("div");
    search.className = "search-pill";
    search.appendChild(icon("search"));
    const inp = document.createElement("input");
    inp.placeholder = "Search mail";
    inp.value = this.mailSearch;
    inp.oninput = () => {
      this.mailSearch = inp.value;
      this.render();
    };
    search.appendChild(inp);
    bar.append(title, search);
    pane.appendChild(bar);

    if (this.mailFolder === "drafts") {
      pane.appendChild(this.renderDraftList());
      return pane;
    }

    const flags = this.mailPrefs();
    const rows = searchMessages(
      listFolder(this.mailFolder, flags),
      this.mailSearch,
    );
    if (!rows.length) {
      pane.appendChild(
        this.mailEmpty(
          this.mailSearch
            ? "No mail matches that search."
            : MAIL_EMPTY[this.mailFolder],
        ),
      );
      return pane;
    }

    const list = document.createElement("div");
    list.className = "mail-list";
    for (const m of rows) list.appendChild(this.mailRow(m, flags));
    pane.appendChild(list);
    return pane;
  }

  /** One row in the message list: who, subject, snippet, when. */
  private mailRow(m: MailMessage, flags: MailFlags): HTMLElement {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `mail-row pressable${isUnread(m, flags) ? " unread" : ""}`;

    const dot = document.createElement("span");
    dot.className = "mail-row-dot";

    const who = document.createElement("span");
    who.className = "mail-row-who";
    who.textContent = m.outgoing
      ? `To ${displayNick(m.contactNick)}`
      : displayNick(m.contactNick);

    const main = document.createElement("div");
    main.className = "mail-row-main";
    const subject = document.createElement("div");
    subject.className = "mail-row-subject";
    subject.textContent = m.subject || "(no subject)";
    const snip = document.createElement("div");
    snip.className = "mail-row-snippet";
    snip.textContent = preview(m);
    main.append(subject, snip);

    const right = document.createElement("div");
    right.className = "mail-row-right";
    const time = document.createElement("span");
    time.className = "text-12 faint";
    time.textContent = formatRelative(m.createdAt);
    right.appendChild(time);
    if (m.state === "queued") {
      const queued = document.createElement("span");
      queued.className = "mail-row-queued";
      queued.textContent = "Queued";
      right.appendChild(queued);
    }
    if (flags.starred.has(m.id)) right.appendChild(this.iconSmall("star"));
    if (m.attachments.length) right.appendChild(this.iconSmall("attach"));

    row.append(dot, who, main, right);
    row.onclick = () => this.openMessage(m);
    return row;
  }

  private renderDraftList(): HTMLElement {
    const drafts = listDrafts();
    if (!drafts.length) return this.mailEmpty(MAIL_EMPTY.drafts);

    const list = document.createElement("div");
    list.className = "mail-list";
    for (const d of drafts) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "mail-row pressable";

      const dot = document.createElement("span");
      dot.className = "mail-row-dot";

      const who = document.createElement("span");
      who.className = "mail-row-who";
      who.textContent = d.to ? `To ${displayNick(d.to)}` : "No recipient";

      const main = document.createElement("div");
      main.className = "mail-row-main";
      const subject = document.createElement("div");
      subject.className = "mail-row-subject";
      subject.textContent = d.subject || "(no subject)";
      const snip = document.createElement("div");
      snip.className = "mail-row-snippet";
      snip.textContent = draftPreview(d);
      main.append(subject, snip);

      const right = document.createElement("div");
      right.className = "mail-row-right";
      const time = document.createElement("span");
      time.className = "text-12 faint";
      time.textContent = formatRelative(d.updatedAt);
      right.appendChild(time);
      if (d.attachments.length) right.appendChild(this.iconSmall("attach"));

      row.append(dot, who, main, right);
      row.onclick = () => this.openCompose({ draftId: d.id });
      list.appendChild(row);
    }
    return list;
  }

  /** Open a message: show its conversation, and mark what you have seen read. */
  private openMessage(m: MailMessage): void {
    this.mailThreadId = m.threadId;
    this.mailOpenId = m.id;
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.render();
    // The read flags are a prefs write, so the pane opens first and the unread
    // dot clears a moment later rather than holding up the click.
    void this.markThreadRead(m.threadId).then(() => {
      if (this.mailThreadId === m.threadId) this.render();
    });
  }

  private async markThreadRead(threadId: string): Promise<void> {
    const flags = this.mailPrefs();
    const unseen = getMailThread(threadId).filter((m) => isUnread(m, flags));
    if (!unseen.length) return;
    const read = new Set(flags.read);
    for (const m of unseen) read.add(m.id);
    await patchUiPrefs({ mail_read: [...read] });
  }

  private renderMailThread(threadId: string): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    const msgs = getMailThread(threadId);
    const contact =
      msgs.find((m) => !m.outgoing)?.contactNick ?? msgs[0]?.contactNick ?? "";
    const flags = this.mailPrefs();
    // Actions apply to the message you clicked, falling back to the newest one.
    const targetId = this.mailOpenId ?? msgs[msgs.length - 1]?.id ?? "";

    const head = document.createElement("div");
    head.className = "mail-read-head";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-icon-round pressable";
    back.appendChild(icon("back"));
    back.setAttribute("aria-label", `Back to ${MAIL_FOLDER_LABEL[this.mailFolder]}`);
    back.onclick = () => {
      this.mailThreadId = null;
      this.mailOpenId = null;
      this.render();
    };

    const titles = document.createElement("div");
    titles.className = "mail-read-titles";
    const subject = threadSubject(threadId);
    const h = document.createElement("h1");
    h.className = "text-17";
    h.textContent = subject;
    const sub = document.createElement("div");
    sub.className = "muted text-13";
    sub.textContent = `With ${displayNick(contact)} · ${msgs.length} message${msgs.length === 1 ? "" : "s"}`;
    titles.append(h, sub);

    const actions = document.createElement("div");
    actions.className = "mail-read-actions";
    if (flags.trashed.has(targetId)) {
      actions.append(
        this.mailAction("Restore to Inbox", "inbox", () =>
          void this.flagMail("mail_trashed", targetId, false),
        ),
        this.mailAction("Delete forever", "trash", () =>
          void this.deleteMailForever(targetId),
        ),
      );
    } else {
      const starred = flags.starred.has(targetId);
      actions.append(
        this.mailAction(starred ? "Unstar" : "Star", "star", () =>
          void this.flagMail("mail_starred", targetId, !starred), starred),
        this.mailAction("Archive", "archive", () =>
          void this.flagMail("mail_archived", targetId, true),
        ),
        this.mailAction("Move to Trash", "trash", () =>
          void this.flagMail("mail_trashed", targetId, true),
        ),
      );
    }
    actions.appendChild(
      this.mailAction("Reply", "reply", () =>
        this.openCompose({
          to: contact,
          threadId,
          subject: reSubject(threadSubject(threadId)),
        }),
      ),
    );
    head.append(back, titles, actions);
    pane.appendChild(head);

    const list = document.createElement("div");
    list.className = "mail-messages";
    for (const m of msgs) {
      const card = document.createElement("article");
      card.className = `mail-message${m.outgoing ? " outgoing" : ""}${m.id === this.mailOpenId ? " open" : ""}`;

      // Classic mail layout: a fixed avatar column, then everything else. The
      // message is not a card — messages are separated by a rule, and the avatar
      // is what tells you who wrote it without reading the header.
      card.appendChild(createAvatarEl(this.avatarKeyFor(m), "sm"));

      const content = document.createElement("div");
      content.className = "mail-message-content";

      // A genuine mail header: the fields a client shows, one per line, with
      // the labels forming a left column so the values line up. The subject
      // belongs to the thread, not this message, so it repeats down the stack.
      const msgHead = document.createElement("dl");
      msgHead.className = "mail-message-head";
      msgHead.append(
        ...this.mailHeaderField(
          "From",
          m.outgoing ? (this.mailAddress() ?? "You") : displayNick(m.contactNick),
          "mail-message-from",
        ),
        ...this.mailHeaderField(
          "To",
          m.outgoing
            ? (m.external?.to ?? displayNick(m.contactNick))
            : (this.mailAddress() ?? "you"),
        ),
        ...this.mailHeaderField("Date", this.mailDateEl(m.createdAt)),
        ...this.mailHeaderField("Subject", subject || "(no subject)"),
      );

      // A rule under the header is what separates the envelope from the letter.
      const rule = document.createElement("hr");
      rule.className = "mail-message-rule";

      const body = document.createElement("div");
      body.className = "text-15 mail-message-body";
      // The body may be rich HTML from the composer, or plain text from an
      // older record. `bodyToDisplayHtml` sanitizes either way.
      body.innerHTML = bodyToDisplayHtml(m.body);
      this.guardLinks(body);

      content.append(msgHead, rule, body);
      if (m.attachments.length) {
        // Attachments are their own section, not text that happens to sit under
        // the message, so a second rule introduces them.
        const attachRule = document.createElement("hr");
        attachRule.className = "mail-message-rule";
        content.appendChild(attachRule);
        for (const a of m.attachments) content.appendChild(this.mailAttachmentChip(a));
      }
      card.appendChild(content);
      list.appendChild(card);
    }
    pane.appendChild(list);
    return pane;
  }

  /**
   * One labelled line of a mail message header.
   *
   * Returns the `<dt>`/`<dd>` pair so the caller can append them in order; the
   * grid on `.mail-message-head` lays each pair out as a label column and a
   * value column. `valueClass` lets a single field (the sender) stand out.
   */
  private mailHeaderField(
    label: string,
    value: Node | string,
    valueClass?: string,
  ): [HTMLElement, HTMLElement] {
    const dt = document.createElement("dt");
    dt.className = "mail-message-label";
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.className = "mail-message-value";
    if (valueClass) dd.classList.add(valueClass);
    if (typeof value === "string") dd.textContent = value;
    else dd.appendChild(value);
    return [dt, dd];
  }

  /**
   * The moment a message was sent, as a real timestamp.
   *
   * A mail header shows the date; the relative form is kept on `title` so the
   * "3h ago" reading is still one hover away without being the visible label.
   */
  private mailDateEl(ts: number): HTMLElement {
    const el = document.createElement("time");
    el.dateTime = new Date(ts).toISOString();
    el.textContent = formatAbsolute(ts);
    el.title = formatRelative(ts);
    return el;
  }

  /**
   * The key an avatar's colour is derived from.
   *
   * An outgoing message is us, so it uses this vault's own protocol name. An
   * incoming one uses the contact's — the same key the chat rail uses, so the
   * same person is the same colour in both rails. A contact who has since been
   * removed falls back to the nickname, which is still stable per person.
   */
  private avatarKeyFor(m: MailMessage): string {
    if (m.outgoing) return this.status?.protocol_name ?? m.contactNick;
    return (
      this.contacts.find((c) => c.nickname === m.contactNick)?.protocol_name ??
      m.contactNick
    );
  }

  private mailAction(
    label: string,
    ic: IconName,
    onClick: () => void,
    active = false,
  ): HTMLElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `btn-icon-round pressable${active ? " active" : ""}`;
    b.appendChild(icon(ic));
    b.setAttribute("aria-label", label);
    b.title = label;
    b.onclick = onClick;
    return b;
  }

  private mailEmpty(text: string): HTMLElement {
    const empty = document.createElement("p");
    empty.className = "muted mail-empty";
    empty.textContent = text;
    return empty;
  }

  /**
   * Flip one folder flag.
   *
   * Folder membership is derived from these lists, so filing a message is a
   * prefs write rather than a rewrite of the encrypted log.
   */
  private async flagMail(
    flag: "mail_starred" | "mail_archived" | "mail_trashed",
    id: string,
    on: boolean,
  ): Promise<void> {
    if (!id) return;
    await setMailFlag(flag, id, on);
    // Filing the open message away drops you back to the list it left.
    if (on && (flag === "mail_trashed" || flag === "mail_archived")) {
      this.mailThreadId = null;
      this.mailOpenId = null;
    }
    this.render();
  }

  /**
   * Remove a message from the encrypted log for good.
   *
   * The log is the archive, so this really does drop the record — but the
   * attachment blobs it pointed at are left in the vault, which has no
   * collection pass. That is the same trade chat already makes.
   */
  private async deleteMailForever(id: string): Promise<void> {
    if (!id) return;
    try {
      await api.deleteMail(id);
    } catch (e) {
      this.announce(e instanceof AppError ? e.message : "Could not delete.");
      return;
    }
    // Drop the id from the flag lists too, or they accumulate ids for records
    // that no longer exist.
    const flags = this.mailPrefs();
    const drop = (s: Set<string>) => [...s].filter((x) => x !== id);
    await patchUiPrefs({
      mail_read: drop(flags.read),
      mail_starred: drop(flags.starred),
      mail_archived: drop(flags.archived),
      mail_trashed: drop(flags.trashed),
    });
    this.mailThreadId = null;
    this.mailOpenId = null;
    await this.reloadMailbox();
    this.announce("Message deleted");
    this.render();
  }

  private mailAttachmentChip(a: MailAttachmentRef): HTMLElement {
    const chip = document.createElement("div");
    chip.className = "mail-attachment";
    const name = document.createElement("span");
    name.className = "text-13";
    name.textContent = a.name;
    const size = document.createElement("span");
    size.className = "text-12 faint";
    size.textContent = formatBytes(a.size);
    chip.append(name, size);
    attachFileActions(chip, a.name, async () => {
      const r = await api.blob(a.blobId);
      if (!r?.data_base64) throw new Error("Could not load attachment from the vault");
      const bin = atob(r.data_base64);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    });
    return chip;
  }

  /**
   * Make a rendered rich body's links safe to click.
   *
   * The sanitizer allows `href` on `http(s)`/`mailto` links, but this app has no
   * external opener wired up, so a raw click would either do nothing or navigate
   * the webview away from the app. Intercepting the click and copying the URL
   * keeps the link useful without handing the shell a navigation it cannot
   * safely perform.
   */
  private guardLinks(root: HTMLElement): void {
    root.addEventListener("click", (e) => {
      const link = (e.target as HTMLElement).closest?.("a[href]");
      if (!link) return;
      e.preventDefault();
      const href = link.getAttribute("href") ?? "";
      void navigator.clipboard
        .writeText(href)
        .then(() => this.announce("Link copied to clipboard"))
        .catch(() => this.announce(href));
    });
  }

  // ------------------------------------------------------------- compose

  /** Open the full-pane composer for a new message, a reply, or a draft. */
  private openCompose(opts: {
    draftId?: string;
    to?: string;
    threadId?: string;
    subject?: string;
  }): void {
    this.mailComposeOpen = true;
    this.mailDraftId = opts.draftId ?? null;
    this.mailReplyTo = opts.to ?? null;
    this.mailReplyThread = opts.threadId ?? null;
    this.mailSubjectSeed = opts.subject ?? null;
    this.mailComposeFiles = [];
    // A brand-new message has no conversation to return to. Without this,
    // opening compose from a thread and then saving a draft or pressing Back
    // lands on the folder list while the reading pane still shows the thread
    // that happened to be open.
    if (!opts.draftId && !opts.threadId) {
      this.mailThreadId = null;
      this.mailOpenId = null;
    }
    // Every open starts internal. A reply is always internal, and a draft's
    // recipient is a contact nickname, so neither can arrive already external.
    this.mailComposeMode = "internal";
    this.mailComposePin = "";
    this.mailComposeDropMode = "pin";
    this.mailComposePublicKey = "";
    this.mailDropResult = null;
    // Seed the editable fields from the draft (or the reply) before rendering,
    // so the first paint already has the right values and a later re-render
    // keeps them.
    const draft = opts.draftId ? draftById(opts.draftId) : undefined;
    this.mailComposeTo = opts.to ?? draft?.to ?? "";
    this.mailComposeSubject = draft?.subject ?? opts.subject ?? "";
    this.mailComposeBody = draft?.body ?? "";
    this.render();

    // Editing a draft has to carry its attachments forward, and the log only
    // kept blob ids — so the bytes come back out of the vault.
    if (draft?.attachments.length) {
      void this.loadDraftAttachments(draft.attachments);
    }
  }

  private async loadDraftAttachments(refs: MailAttachmentRef[]): Promise<void> {
    const loaded: { name: string; dataBase64: string; size: number }[] = [];
    for (const a of refs) {
      try {
        const r = await api.blob(a.blobId);
        if (r?.data_base64) {
          loaded.push({ name: a.name, dataBase64: r.data_base64, size: a.size });
        }
      } catch {
        this.announce(`Could not load ${a.name} from the vault`);
      }
    }
    // The composer may have been closed while those fetches were in flight.
    if (!this.mailComposeOpen) return;
    this.mailComposeFiles = loaded;
    this.render();
  }

  private closeCompose(): void {
    this.mailComposeOpen = false;
    this.mailDraftId = null;
    this.mailReplyTo = null;
    this.mailReplyThread = null;
    this.mailSubjectSeed = null;
    this.mailComposeFiles = [];
    this.mailComposeTo = "";
    this.mailComposeSubject = "";
    this.mailComposeBody = "";
    this.mailComposeMode = "internal";
    this.mailComposeExternalTo = "";
    this.mailComposePin = "";
    this.mailComposeDropMode = "pin";
    this.mailComposePublicKey = "";
    this.mailDropResult = null;
    this.render();
  }

  private renderMailCompose(): HTMLElement {
    // A drop is not finished when it is sent — it is finished when the sender
    // has the link. Until they dismiss this, the pane shows the link rather
    // than the form.
    const dropResult = this.mailDropResult;
    if (dropResult) return this.renderDropResult(dropResult);

    const pane = document.createElement("div");
    pane.className = "main-pane";
    const draft = this.mailDraftId ? draftById(this.mailDraftId) : undefined;

    const head = document.createElement("div");
    head.className = "mail-toolbar";
    const back = document.createElement("button");
    back.type = "button";
    back.className = "btn-icon-round pressable";
    back.appendChild(icon("back"));
    back.setAttribute("aria-label", "Back to mail");
    back.onclick = () => this.closeCompose();
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = draft
      ? "Edit draft"
      : this.mailReplyThread
        ? "Reply"
        : "New mail";
    head.append(back, title);
    pane.appendChild(head);

    const form = document.createElement("div");
    form.className = "mail-compose";

    const isReply = !!this.mailReplyThread;
    const isExternal = this.mailComposeMode === "external";

    // The mode switch. Two buttons rather than a checkbox because neither state
    // is the "off" one: they are two transports with different rules.
    const mode = document.createElement("div");
    mode.className = "mail-to-mode";
    mode.setAttribute("role", "group");
    mode.setAttribute("aria-label", "Recipient kind");
    for (const value of ["internal", "external"] as const) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "mail-to-mode-btn pressable";
      b.textContent = value === "internal" ? "Internal" : "External";
      b.setAttribute("aria-pressed", String(this.mailComposeMode === value));
      // A reply goes back to the contact who wrote, so it cannot become a drop.
      if (isReply && value === "external") b.disabled = true;
      b.onclick = () => {
        if (this.mailComposeMode === value) return;
        this.mailComposeMode = value;
        // A PIN or key typed for the other mode would be a leftover, not an
        // intent, and the drop mode itself resets so the pane always reopens on
        // the original PIN flow.
        this.mailComposePin = "";
        this.mailComposeDropMode = "pin";
        this.mailComposePublicKey = "";
        this.render();
      };
      mode.appendChild(b);
    }

    // A keyboard-typeable picker: type to narrow, arrow to move, Enter to
    // choose. It reads and writes `this.mailComposeTo`, which is held outside
    // the DOM so that rebuilding this pane (an attachment load, a mode switch)
    // cannot forget the recipient.
    const toPicker = new ContactPicker({
      items: this.contacts.map((c) => ({
        nickname: c.nickname,
        label: c.mail_handle
          ? `${displayNick(c.nickname)} (${c.mail_handle}@${this.mailDomain})`
          : displayNick(c.nickname),
        // The raw nickname stays searchable even when a display-name override
        // hides it from the label.
        keywords: c.nickname,
      })),
      value: this.mailComposeTo,
      label: "To",
      placeholder: "Type a name…",
      // A reply goes back to the thread's contact; changing it would fork the
      // conversation, so the picker is fixed.
      locked: isReply,
      emptyMessage: "No contacts yet. Add someone in Trusts to mail them.",
      noMatchMessage: "No contacts match that name.",
      onChange: (nickname) => {
        this.mailComposeTo = nickname;
      },
    });

    // A typed address. `type=email` gets the platform keyboard and autofill on
    // desktop; the daemon re-validates regardless, because a DOM hint is not a
    // check.
    const externalTo = document.createElement("input");
    externalTo.type = "email";
    externalTo.className = "input-field mail-compose-external-to";
    externalTo.placeholder = "name@example.com";
    externalTo.value = this.mailComposeExternalTo;
    externalTo.oninput = () => {
      this.mailComposeExternalTo = externalTo.value;
    };

    const toField = document.createElement("div");
    toField.className = "mail-compose-to-field";
    toField.append(mode);
    toField.appendChild(isExternal ? externalTo : toPicker.element);
    form.appendChild(this.composeRow("To", toField));

    // The credential field. The PIN is shown, not masked: the sender has to read
    // it out or write it down, and a field they cannot read makes that
    // error-prone. It is never stored and never emailed. In public-key mode the
    // PIN is replaced by the recipient's pasteable key, which the sender got out
    // of band instead — the shape of the interaction is the same, only the
    // credential and the crypto differ.
    let pinInput: HTMLInputElement | null = null;
    let publicKeyInput: HTMLInputElement | null = null;
    if (isExternal) {
      // A second segmented switch, separate from To's. The To switch chooses a
      // transport; this one chooses the credential the drop is sealed under.
      const dropMode = document.createElement("div");
      dropMode.className = "mail-drop-mode";
      dropMode.setAttribute("role", "group");
      dropMode.setAttribute("aria-label", "How the drop is sealed");
      for (const value of ["pin", "publickey"] as const) {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "mail-to-mode-btn pressable";
        b.textContent = value === "pin" ? "PIN" : "Public key";
        b.setAttribute("aria-pressed", String(this.mailComposeDropMode === value));
        b.onclick = () => {
          if (this.mailComposeDropMode === value) return;
          this.mailComposeDropMode = value;
          // The other mode's credential is a leftover, not an intent.
          this.mailComposePin = "";
          this.mailComposePublicKey = "";
          this.render();
        };
        dropMode.appendChild(b);
      }

      const credential = document.createElement("div");
      credential.className = "mail-compose-to-field";
      credential.appendChild(dropMode);

      if (this.mailComposeDropMode === "pin") {
        pinInput = document.createElement("input");
        pinInput.type = "text";
        pinInput.className = "input-field mail-compose-pin";
        pinInput.placeholder = `At least ${MIN_DROP_PIN_LEN} characters`;
        pinInput.autocomplete = "off";
        pinInput.spellcheck = false;
        pinInput.value = this.mailComposePin;
        pinInput.oninput = () => {
          this.mailComposePin = pinInput!.value;
        };
        const hint = document.createElement("p");
        hint.className = "text-12 muted mail-compose-hint";
        hint.textContent =
          "Give this to the recipient separately — by phone, or in person. It is never emailed and never saved, so a lost PIN means sending again.";
        credential.append(pinInput, hint);
        form.appendChild(this.composeRow("PIN", credential));
      } else {
        publicKeyInput = document.createElement("input");
        publicKeyInput.type = "text";
        publicKeyInput.className = "input-field mail-compose-public-key";
        publicKeyInput.placeholder = `${DROP_PUBLIC_KEY_PREFIX}…`;
        publicKeyInput.autocomplete = "off";
        publicKeyInput.spellcheck = false;
        publicKeyInput.value = this.mailComposePublicKey;
        publicKeyInput.oninput = () => {
          this.mailComposePublicKey = publicKeyInput!.value;
        };
        const hint = document.createElement("p");
        hint.className = "text-12 muted mail-compose-hint";
        hint.textContent =
          "Paste the public key the recipient published. No PIN is needed — only their private key opens the drop.";
        credential.append(publicKeyInput, hint);
        form.appendChild(this.composeRow("Public key", credential));
      }
    }

    const subj = document.createElement("input");
    subj.className = "input-field mail-compose-subject";
    subj.placeholder = "(no subject)";
    subj.value = this.mailComposeSubject;
    subj.oninput = () => {
      this.mailComposeSubject = subj.value;
    };
    form.appendChild(this.composeRow("Subject", subj));

    // The body is rich text. `onChange` mirrors each edit into the field above
    // so that a rebuild of this pane — attaching a file, or the async load of a
    // draft's attachments — does not throw away what was typed.
    const editor = new RichTextEditor({
      value: this.mailComposeBody,
      placeholder: "Write your message",
      label: "Message body",
      onChange: () => {
        this.mailComposeBody = editor.getHtml();
      },
    });
    form.appendChild(editor.element);

    const preview = document.createElement("div");
    preview.className = "mail-attachment-preview";

    const attach = document.createElement("button");
    attach.type = "button";
    attach.className = "btn-secondary text-13 pressable";
    attach.onclick = () => files.click();

    // Rebuilt in place rather than through `this.render()`, so adding or
    // removing a file leaves the caret in the body where it was.
    const renderChips = () => {
      preview.innerHTML = "";
      this.mailComposeFiles.forEach((f, i) => {
        const chip = document.createElement("div");
        chip.className = "mail-attachment";
        const name = document.createElement("span");
        name.className = "text-13";
        name.textContent = f.name;
        const size = document.createElement("span");
        size.className = "text-12 faint";
        size.textContent = formatBytes(f.size);
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "btn-icon-round pressable";
        remove.appendChild(this.iconSmall("close"));
        remove.setAttribute("aria-label", `Remove ${f.name}`);
        remove.onclick = () => {
          this.mailComposeFiles.splice(i, 1);
          renderChips();
        };
        chip.append(name, size, remove);
        preview.appendChild(chip);
      });
      attach.textContent = this.mailComposeFiles.length
        ? "Add files"
        : "Attach files";
    };
    renderChips();

    const files = document.createElement("input");
    files.type = "file";
    files.multiple = true;
    files.className = "mail-file-input";
    files.onchange = () => {
      void this.addComposeFiles(Array.from(files.files ?? []), renderChips);
      // Reset so picking the same file twice still fires a change event.
      files.value = "";
    };

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.hidden = true;

    const saveDraft = document.createElement("button");
    saveDraft.type = "button";
    saveDraft.className = "btn-secondary pressable";
    saveDraft.textContent = "Save draft";
    // A drop cannot be drafted: a draft is stored in the vault, and the PIN (or
    // key) must not be. Saving the message without its credential would leave a
    // draft that cannot be sent, so the button is off rather than quietly lossy.
    if (isExternal) {
      saveDraft.disabled = true;
      saveDraft.title = "A drop cannot be saved as a draft — its PIN or key is never stored.";
    }
    saveDraft.onclick = () =>
      void this.saveDraft(saveDraft, {
        to: toPicker.value,
        subject: subj.value,
        body: editor.getHtml(),
        err,
      });

    const send = document.createElement("button");
    send.type = "button";
    send.className = "btn-primary pressable";
    send.textContent = isExternal
      ? "Seal and send"
      : isReply
        ? "Send reply"
        : "Send";
    send.onclick = () => {
      if (this.mailComposeMode === "external") {
        void this.sendToOutsider(send, {
          to: externalTo.value,
          subject: subj.value,
          body: editor.getHtml(),
          pin: pinInput?.value ?? this.mailComposePin,
          publicKey: publicKeyInput?.value ?? this.mailComposePublicKey,
          err,
        });
        return;
      }
      void this.sendMail(send, {
        to: toPicker.value,
        subject: subj.value,
        body: editor.getHtml(),
        err,
      });
    };

    const actions = document.createElement("div");
    actions.className = "mail-compose-actions";
    actions.append(attach, saveDraft);
    // Only a saved draft can be discarded. A brand new message is thrown away
    // by closing compose, which is what Back already does.
    if (this.mailDraftId) {
      const discard = document.createElement("button");
      discard.type = "button";
      discard.className = "btn-secondary pressable";
      discard.textContent = "Discard draft";
      discard.onclick = () => void this.discardDraft(discard, err);
      actions.appendChild(discard);
    }
    actions.appendChild(send);

    form.append(files, preview, actions, err);
    pane.appendChild(form);
    return pane;
  }

  private composeRow(label: string, field: HTMLElement): HTMLElement {
    const row = document.createElement("div");
    row.className = "mail-compose-row";
    const l = document.createElement("span");
    l.className = "mail-compose-label";
    l.textContent = label;
    row.append(l, field);
    return row;
  }

  private async addComposeFiles(
    picked: File[],
    after: () => void,
  ): Promise<void> {
    const next = [...this.mailComposeFiles];
    for (const f of picked) {
      next.push({
        name: f.name,
        dataBase64: await fileToBase64(f),
        size: f.size,
      });
    }
    this.mailComposeFiles = next;
    after();
  }

  /** The client-side half of the daemon's caps, so the limit is explained
   *  before a round trip rather than after one. */
  private composeError(
    to: string,
    body: string,
  ): string | null {
    if (!to) return "Add a contact first.";
    return this.attachmentOrBodyError(body);
  }

  /**
   * The drop half of the same checks.
   *
   * Mirrors `normalize_external_address` in `hush-core` closely enough to catch
   * the obvious mistakes without a round trip. It is deliberately not a full
   * address parser: the daemon is the boundary, and refusing an unusual but valid
   * address here would be worse than letting it through to be checked properly.
   */
  private dropError(to: string, pin: string, publicKey: string, body: string): string | null {
    const addr = to.trim();
    if (!addr) return "Type the recipient's email address.";
    if (/\s/.test(addr) || addr.split("@").length !== 2) {
      return "That does not look like an email address.";
    }
    const [local, domain] = addr.split("@");
    if (!local || !domain.includes(".") || domain.startsWith(".") || domain.endsWith(".")) {
      return "That does not look like an email address.";
    }
    if (domain.toLowerCase() === this.mailDomain.toLowerCase()) {
      // Sending to this server from the external field would be a drop to
      // somebody who could have received it as mail, under a weaker guarantee.
      return `Use Internal to reach @${this.mailDomain} — that goes through the encrypted session instead.`;
    }
    // Only the credential for the mode in use is checked, so the PIN floor never
    // blocks a public-key drop and a key never has to satisfy a PIN rule.
    const credentialError =
      this.mailComposeDropMode === "publickey"
        ? this.dropPublicKeyError(publicKey)
        : pin.length < MIN_DROP_PIN_LEN
          ? `The PIN needs at least ${MIN_DROP_PIN_LEN} characters — it is what protects the link if it is forwarded.`
          : null;
    if (credentialError) return credentialError;
    return this.attachmentOrBodyError(body);
  }

  /**
   * The public-key half of the credential check.
   *
   * Mirrors the shape `decode_public_key` in `hush-core` accepts: a versioned
   * prefix and 32 raw bytes as unpadded base64url. This is a courtesy that saves
   * a round trip — the daemon still parses the point and can reject a key whose
   * bytes are not a canonical X25519 public key.
   */
  private dropPublicKeyError(key: string): string | null {
    const k = key.trim();
    if (!k) return "Paste the recipient's public key.";
    if (!k.startsWith(DROP_PUBLIC_KEY_PREFIX)) {
      return `That does not look like a drop public key — it should start with ${DROP_PUBLIC_KEY_PREFIX}.`;
    }
    const body = k.slice(DROP_PUBLIC_KEY_PREFIX.length);
    if (!/^[A-Za-z0-9_-]+$/.test(body) || body.length !== DROP_PUBLIC_KEY_BODY_LEN) {
      return "That key is the wrong length or has characters that do not belong — check you copied the whole line.";
    }
    return null;
  }

  /** The checks both transports share: attachment caps, and not sending nothing. */
  private attachmentOrBodyError(body: string): string | null {
    if (this.mailComposeFiles.length > MAX_MAIL_ATTACHMENTS) {
      return `At most ${MAX_MAIL_ATTACHMENTS} attachments.`;
    }
    const total = this.mailComposeFiles.reduce((n, f) => n + f.size, 0);
    if (total > MAX_MAIL_BYTES) {
      return `Attachments are limited to ${Math.round(MAX_MAIL_BYTES / 1024)} KB in total.`;
    }
    // The body is HTML; an editor holding only an empty paragraph is not text.
    if (isBlankHtml(body) && !this.mailComposeFiles.length) {
      return "Write a message or attach a file.";
    }
    return null;
  }

  private async saveDraft(
    btn: HTMLButtonElement,
    input: { to: string; subject: string; body: string; err: HTMLElement },
  ): Promise<void> {
    input.err.hidden = true;
    // A draft with no recipient and no text is not worth keeping.
    if (
      !input.to &&
      isBlankHtml(input.body) &&
      !input.subject.trim() &&
      !this.mailComposeFiles.length
    ) {
      input.err.hidden = false;
      input.err.textContent = "Nothing to save yet.";
      return;
    }
    btn.disabled = true;
    try {
      await api.saveDraft({
        id: this.mailDraftId ?? undefined,
        thread_id: this.mailReplyThread ?? undefined,
        to: input.to,
        subject: input.subject.trim() || undefined,
        body: input.body,
        attachments: this.mailComposeFiles.map((f) => ({
          name: f.name,
          data_base64: f.dataBase64,
        })),
      });
      await this.reloadMailbox();
      this.mailFolder = "drafts";
      this.closeCompose();
      this.announce("Draft saved");
    } catch (e) {
      input.err.hidden = false;
      input.err.textContent =
        e instanceof AppError ? e.message : "Couldn't save the draft.";
    } finally {
      btn.disabled = false;
    }
  }

  /**
   * Throw the open draft away for good, dropping its record from the log.
   *
   * Only reachable from a saved draft; a new message is abandoned by leaving
   * compose, and nothing was written for it in the first place.
   */
  private async discardDraft(
    btn: HTMLButtonElement,
    err: HTMLElement,
  ): Promise<void> {
    const id = this.mailDraftId;
    if (!id) return;
    err.hidden = true;
    btn.disabled = true;
    try {
      await api.deleteDraft(id);
      await this.reloadMailbox();
      this.mailComposeOpen = false;
      this.mailDraftId = null;
      this.mailComposeFiles = [];
      this.announce("Draft discarded");
      this.render();
    } catch (e) {
      btn.disabled = false;
      err.hidden = false;
      err.textContent =
        e instanceof AppError ? e.message : "Couldn't discard the draft.";
    }
  }

  private async sendMail(
    btn: HTMLButtonElement,
    input: { to: string; subject: string; body: string; err: HTMLElement },
  ): Promise<void> {
    input.err.hidden = true;
    const problem = this.composeError(input.to, input.body);
    if (problem) {
      input.err.hidden = false;
      input.err.textContent = problem;
      return;
    }
    btn.disabled = true;
    const draftId = this.mailDraftId;
    const isReply = !!this.mailReplyThread;
    try {
      const resp = await api.sendMail(input.to, {
        thread_id: this.mailReplyThread ?? undefined,
        subject: input.subject.trim() || undefined,
        body: input.body,
        attachments: this.mailComposeFiles.map((f) => ({
          name: f.name,
          data_base64: f.dataBase64,
        })),
      });
      // Sending a draft consumes it, or the same message would sit in Drafts
      // forever after going out.
      if (draftId) {
        try {
          await api.deleteDraft(draftId);
        } catch {
          /* the mail is sent; a stale draft is not worth an error */
        }
      }
      await this.reloadMailbox();
      this.mailComposeOpen = false;
      this.mailDraftId = null;
      this.mailReplyTo = null;
      this.mailReplyThread = null;
      this.mailSubjectSeed = null;
      this.mailComposeFiles = [];
      // Show the message where it landed: Sent, or the Outbox if the relay
      // refused it — saying "sent" then filing it under Outbox would be a lie.
      this.mailFolder = resp?.queued ? "outbox" : "sent";
      this.mailThreadId = resp?.thread_id ?? null;
      this.mailOpenId = null;
      this.announce(
        resp?.queued
          ? "Queued — the relay is unreachable, it will send when it is back"
          : isReply
            ? "Reply sent"
            : "Mail sent",
      );
      this.render();
    } catch (e) {
      input.err.hidden = false;
      input.err.textContent =
        e instanceof AppError ? e.message : "Couldn't send. Try again.";
    } finally {
      btn.disabled = false;
    }
  }

  /**
   * Seal a message for someone with no account and show the link.
   *
   * There is no Outbox here: a drop either reached the relay or it did not, and
   * "did not" has no retry that could work — the PIN is gone from memory once
   * this returns, so a queued drop could never be opened by anyone. That is why
   * a failure is reported rather than deferred.
   */
  private async sendToOutsider(
    btn: HTMLButtonElement,
    input: {
      to: string;
      subject: string;
      body: string;
      pin: string;
      publicKey: string;
      err: HTMLElement;
    },
  ): Promise<void> {
    input.err.hidden = true;
    const problem = this.dropError(input.to, input.pin, input.publicKey, input.body);
    if (problem) {
      input.err.hidden = false;
      input.err.textContent = problem;
      return;
    }
    btn.disabled = true;
    // Sealing is deliberately slow in PIN mode — 600k PBKDF2 iterations — so the
    // button says what it is doing rather than just going dead.
    const label = btn.textContent;
    btn.textContent = "Sealing…";
    const isKeyMode = this.mailComposeDropMode === "publickey";
    try {
      const common = {
        to: input.to.trim(),
        subject: input.subject.trim() || undefined,
        body: input.body,
        attachments: this.mailComposeFiles.map((f) => ({
          name: f.name,
          data_base64: f.dataBase64,
        })),
      };
      // Exactly one credential, by construction: the API type refuses both.
      const resp = isKeyMode
        ? await api.sendToOutsider({ ...common, public_key: input.publicKey.trim() })
        : await api.sendToOutsider({ ...common, pin: input.pin });
      // The mailbox is reloaded so the message is already in Sent when the
      // sender leaves the link panel, rather than appearing a poll later.
      await this.reloadMailbox();
      this.mailComposeFiles = [];
      this.mailComposePin = "";
      this.mailComposePublicKey = "";
      // The credential is dropped here and nowhere else: the link panel is all
      // that is left of the send, and re-showing the message must not need it.
      this.mailDropResult = {
        link: resp?.link ?? "",
        // Lowercased to match the address the daemon recorded, so the panel and
        // the Sent row cannot disagree about who the message went to.
        to: input.to.trim().toLowerCase(),
        expiresUnix: resp?.expires_unix ?? 0,
        mode: isKeyMode ? "publickey" : "pin",
      };
      this.render();
    } catch (e) {
      input.err.hidden = false;
      input.err.textContent =
        e instanceof AppError ? e.message : "Couldn't send. Try again.";
    } finally {
      btn.disabled = false;
      btn.textContent = label;
    }
  }

  /**
   * The link panel, shown in place of the form after a drop goes out.
   *
   * It stays up until dismissed because the link is the deliverable: navigating
   * straight to Sent would hide the one thing the sender needs.
   */
  private renderDropResult(result: {
    link: string;
    to: string;
    expiresUnix: number;
    mode: "pin" | "publickey";
  }): HTMLElement {
    const isKeyMode = result.mode === "publickey";
    const pane = document.createElement("div");
    pane.className = "main-pane";

    const head = document.createElement("div");
    head.className = "mail-toolbar";
    const title = document.createElement("h1");
    title.className = "text-17";
    title.textContent = "Send the link";
    head.appendChild(title);
    pane.appendChild(head);

    const body = document.createElement("div");
    body.className = "mail-compose mail-drop-result";

    const lead = document.createElement("p");
    lead.className = "text-15";
    lead.textContent = isKeyMode
      ? `Your message is sealed and waiting. Send this link to ${result.to} — they open it with their private key. No PIN is needed.`
      : `Your message is sealed and waiting. Send this link to ${result.to}, and give them the PIN separately.`;
    body.appendChild(lead);
    const linkRow = document.createElement("div");
    linkRow.className = "mail-drop-link";
    const link = document.createElement("input");
    link.className = "input-field mail-drop-link-input";
    link.readOnly = true;
    // Select on focus so the whole link can be copied with one keystroke even if
    // the clipboard API is unavailable.
    link.onfocus = () => link.select();
    link.value = result.link;
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "btn-primary pressable";
    copy.textContent = "Copy link";
    copy.onclick = () => void this.copyDropLink(result.link, copy);
    linkRow.append(link, copy);
    body.appendChild(linkRow);

    const warn = document.createElement("p");
    warn.className = "text-13 muted mail-compose-hint";
    warn.textContent = isKeyMode
      ? `Anyone with this link and the recipient's private key can read the message. It expires ${formatUntil(result.expiresUnix * 1000)}, and it cannot be recalled.`
      : `Anyone with this link and the PIN can read the message. It expires ${formatUntil(result.expiresUnix * 1000)}, and it cannot be recalled.`;
    body.appendChild(warn);

    const actions = document.createElement("div");
    actions.className = "mail-compose-actions";
    const done = document.createElement("button");
    done.type = "button";
    done.className = "btn-secondary pressable";
    done.textContent = "Done";
    done.onclick = () => {
      this.mailDropResult = null;
      // Land on the message that just went out, so "where did it go?" is
      // answered by the screen rather than by the folder list.
      this.mailComposeOpen = false;
      this.mailComposeExternalTo = "";
      this.mailComposeSubject = "";
      this.mailComposeBody = "";
      this.mailFolder = "sent";
      const sent = listFolder("sent", this.mailPrefs())[0];
      this.mailThreadId = sent?.threadId ?? null;
      this.mailOpenId = null;
      this.announce("Drop sent");
      this.render();
    };
    actions.appendChild(done);
    body.appendChild(actions);

    pane.appendChild(body);
    return pane;
  }

  private async copyDropLink(link: string, btn: HTMLButtonElement): Promise<void> {
    try {
      await navigator.clipboard.writeText(link);
      this.announce("Link copied");
    } catch {
      // Clipboard access can be denied. The field is already selected and
      // read-only, so the user can still copy it by hand.
      this.announce("Copy the link from the field above");
      btn.blur();
    }
  }

  private renderVault(): HTMLElement {    const pane = document.createElement("div");
    pane.className = "main-pane";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.style.padding = "var(--space-5)";
    h.textContent = "Vault";
    const grid = document.createElement("div");
    grid.className = "bento";
    const s = this.status!;
    const storage = document.createElement("div");
    storage.className = "tile";
    storage.innerHTML = `<div class="text-15">Storage</div><div class="text-28">${formatBytes(s.vault_bytes)}</div><button class="btn-secondary pressable" style="margin-top:12px">Manage</button>`;
    const lock = document.createElement("div");
    lock.className = "tile";
    const mins = getAutoLockMinutes();
    const idleLabel =
      mins <= 0 ? "Idle lock off" : `${mins} minute${mins === 1 ? "" : "s"}`;
    const remain = Math.max(0, getAutoLockMs() - (Date.now() - this.unlockedAt));
    const countdown =
      mins <= 0
        ? "Only manual lock or suspend"
        : `${Math.ceil(remain / 60000)}m until lock`;
    lock.innerHTML = `<div class="text-15">Auto-lock</div><div class="muted text-13">${idleLabel}</div><div class="text-13">${countdown}</div><button class="btn-secondary pressable lock-now" style="margin-top:12px">Lock now</button>`;
    lock.querySelector(".lock-now")!.addEventListener("click", () => void this.lock());
    const backup = document.createElement("div");
    backup.className = "tile";
    const lastBackup = s.last_backup_unix ?? 0;
    if (!lastBackup) backup.style.background = "var(--warning-soft)";
    const backupHint =
      lastBackup > 0
        ? `Last backup ${formatRelative(lastBackup * 1000)}`
        : "Back up your vault so you never lose your chats";
    backup.innerHTML = `<div class="text-15">Backup</div><div class="muted text-13">${backupHint}</div><button type="button" class="btn-secondary pressable backup-btn" style="margin-top:12px">Back up now</button>`;
    backup.querySelector(".backup-btn")!.addEventListener("click", async () => {
      try {
        const r = await api.backupExport();
        if (r) await downloadBase64File(r.filename, r.data_base64);
        await this.refreshStatus();
        this.announce("Backup downloaded");
        this.render();
      } catch (e) {
        this.announce(e instanceof AppError ? e.message : "Backup failed");
      }
    });
    const filesTile = document.createElement("div");
    filesTile.className = "tile";
    const fileCount = allFileMessages().length;
    filesTile.innerHTML = `<div class="text-15">Exchanged files</div><div class="muted text-13">${fileCount} in your sealed vault (encrypted blobs)</div><button type="button" class="btn-secondary pressable files-rail-btn" style="margin-top:12px">Open Files</button>`;
    filesTile.querySelector(".files-rail-btn")!.addEventListener("click", () => {
      this.rail = "files";
      this.render();
    });
    grid.append(storage, lock, backup, filesTile);
    pane.append(h, grid);
    return pane;
  }

  /**
   * Rotate the vault passphrase.
   *
   * The daemon re-wraps the vault key rather than re-encrypting anything, so
   * this is fast regardless of vault size and the recovery code keeps working.
   * All three fields are validated here so an obvious typo never reaches the
   * daemon; the current passphrase itself can only be checked server-side.
   */
  private renderChangePassphraseForm(): HTMLElement {
    const form = document.createElement("form");
    form.style.marginTop = "var(--space-6)";

    const title = document.createElement("h3");
    title.className = "text-15";
    title.textContent = "Change passphrase";

    const note = document.createElement("p");
    note.className = "muted text-13";
    note.textContent =
      "Your messages, files and recovery code keep working — nothing is re-encrypted.";

    const fields: HTMLInputElement[] = [];
    const makeField = (placeholder: string, autocomplete: AutoFill) => {
      const input = document.createElement("input");
      input.type = "password";
      input.className = "input-field";
      input.placeholder = placeholder;
      input.autocomplete = autocomplete;
      input.style.marginTop = "var(--space-3)";
      fields.push(input);
      return input;
    };
    const current = makeField("Current passphrase", "current-password");
    const next = makeField("New passphrase", "new-password");
    const confirm = makeField("Confirm new passphrase", "new-password");

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.style.marginTop = "var(--space-2)";
    err.hidden = true;

    const submit = document.createElement("button");
    submit.type = "submit";
    submit.className = "btn-primary pressable";
    submit.style.marginTop = "var(--space-4)";
    submit.textContent = "Change passphrase";

    const showError = (message: string) => {
      err.textContent = message;
      err.hidden = false;
      form.classList.add("shake");
      setTimeout(() => form.classList.remove("shake"), 300);
    };

    form.onsubmit = async (ev) => {
      ev.preventDefault();
      err.hidden = true;
      if (fields.some((f) => !f.value)) {
        showError("Fill in all three fields.");
        return;
      }
      if (next.value !== confirm.value) {
        showError("The new passphrases don't match.");
        return;
      }
      if (next.value === current.value) {
        showError("The new passphrase must be different from the current one.");
        return;
      }
      submit.disabled = true;
      try {
        await api.changePassphrase(current.value, next.value);
        for (const f of fields) f.value = "";
        this.announce("Passphrase changed. Your recovery code still works.");
        this.render();
      } catch (e) {
        showError(
          e instanceof AppError ? e.message : "Could not change the passphrase.",
        );
      } finally {
        submit.disabled = false;
      }
    };

    form.append(title, note, current, next, confirm, err, submit);
    return form;
  }

  private renderSettings(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane settings-layout";
    const nav = document.createElement("nav");
    nav.className = "settings-nav";
    const sections: { id: SettingsSection; label: string }[] = [
      { id: "appearance", label: "Appearance" },
      { id: "profile", label: "Profile" },
      { id: "security", label: "Security" },
      { id: "recovery", label: "Recovery" },
      { id: "backup", label: "Backup" },
      { id: "about", label: "About" },
    ];
    for (const s of sections) {
      const b = document.createElement("button");
      b.className = this.settingsSection === s.id ? "active" : "";
      b.textContent = s.label;
      b.onclick = () => {
        this.settingsSection = s.id;
        this.render();
      };
      nav.appendChild(b);
    }
    const body = document.createElement("div");
    body.className = "settings-body";
    if (this.settingsSection === "appearance") {
      body.innerHTML = `<h2 class="text-22">Appearance</h2><p class="muted">Theme</p>`;
      const seg = document.createElement("div");
      seg.className = "segmented";
      for (const mode of ["system", "light", "dark"] as ThemeMode[]) {
        const b = document.createElement("button");
        b.className = getThemeMode() === mode ? "active" : "";
        b.textContent = mode.charAt(0).toUpperCase() + mode.slice(1);
        b.onclick = () => {
          void setThemeMode(mode).then(() => this.render());
        };
        seg.appendChild(b);
      }
      body.appendChild(seg);
    } else if (this.settingsSection === "profile") {
      body.innerHTML = `<h2 class="text-22">Profile</h2><p class="muted text-13">Your mail address is a label bound to your keys, shared inside your invite. There is no public directory — contacts learn it only when you add each other.</p>`;
      const current = document.createElement("p");
      current.className = "text-15";
      current.style.marginTop = "var(--space-4)";
      current.textContent = this.mailAddress()
        ? `Your address: ${this.mailAddress()}`
        : "No address yet";
      const label = document.createElement("p");
      label.className = "muted";
      label.style.marginTop = "var(--space-4)";
      label.textContent = "Address";
      const input = document.createElement("input");
      input.className = "input-field";
      input.placeholder = "sandy";
      input.value = this.mailHandle ?? "";
      const suffix = document.createElement("span");
      suffix.className = "muted text-13";
      suffix.textContent = `@${this.mailDomain}`;
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.gap = "var(--space-2)";
      row.style.alignItems = "center";
      row.style.marginTop = "var(--space-2)";
      row.append(input, suffix);
      const err = document.createElement("p");
      err.className = "text-13";
      err.style.color = "var(--danger)";
      err.hidden = true;
      const save = document.createElement("button");
      save.type = "button";
      save.className = "btn-primary pressable";
      save.style.marginTop = "var(--space-4)";
      save.textContent = "Save address";
      save.onclick = async () => {
        err.hidden = true;
        const raw = input.value.trim();
        if (!/^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$/.test(raw.toLowerCase())) {
          err.hidden = false;
          err.textContent =
            "Use 1–64 lowercase letters, digits, dot, dash, or underscore; no leading or trailing separator.";
          return;
        }
        save.disabled = true;
        try {
          await api.setMailHandle(raw);
          await this.refreshStatus();
          this.announce("Mail address saved");
          this.render();
        } catch (e) {
          err.hidden = false;
          err.textContent =
            e instanceof AppError ? e.message : "Couldn't save that address.";
        } finally {
          save.disabled = false;
        }
      };
      body.append(current, label, row, err, save);
    } else if (this.settingsSection === "security") {
      body.innerHTML = `<h2 class="text-22">Security</h2><p class="muted text-13">Idle auto-lock is enforced by the daemon. Suspend / sleep also locks the vault on Linux when logind is available.</p>`;
      const label = document.createElement("p");
      label.className = "muted";
      label.style.marginTop = "var(--space-4)";
      label.textContent = "Lock after inactivity";
      const sel = document.createElement("select");
      sel.className = "input-field";
      sel.style.marginTop = "var(--space-2)";
      const options: { v: number; t: string }[] = [
        { v: 5, t: "5 minutes" },
        { v: 15, t: "15 minutes" },
        { v: 30, t: "30 minutes" },
        { v: 60, t: "1 hour" },
        { v: 0, t: "Never (idle only)" },
      ];
      for (const o of options) {
        const opt = document.createElement("option");
        opt.value = String(o.v);
        opt.textContent = o.t;
        sel.appendChild(opt);
      }
      sel.value = String(getAutoLockMinutes());
      sel.onchange = () => {
        const v = Number(sel.value);
        void patchUiPrefs({ auto_lock_minutes: v }).then(() => this.render());
      };
      body.append(label, sel, this.renderChangePassphraseForm());
    } else if (this.settingsSection === "recovery") {
      body.innerHTML = `<h2 class="text-22">Recovery</h2><p class="muted text-13">Your recovery code was shown once when the vault was created or when recovery was first set up. It unlocks the vault if you forget your passphrase (with your backup file).</p>`;
      const ack = this.status?.recovery_acknowledged;
      const note = document.createElement("p");
      note.className = "text-13 muted";
      note.style.marginTop = "var(--space-3)";
      note.textContent = ack
        ? "Recovery code acknowledged. Keep your saved copy offline."
        : "You still need to save your recovery code when prompted.";
      body.appendChild(note);
    } else if (this.settingsSection === "backup") {
      body.innerHTML = `<h2 class="text-22">Backup</h2><p class="muted text-13">Download an encrypted archive of your vault. Restore by locking the app, then importing the file.</p>`;
      const exportBtn = document.createElement("button");
      exportBtn.type = "button";
      exportBtn.className = "btn-primary pressable";
      exportBtn.textContent = "Export backup";
      exportBtn.style.marginTop = "var(--space-4)";
      exportBtn.onclick = async () => {
        const r = await api.backupExport();
        if (r) await downloadBase64File(r.filename, r.data_base64);
        await this.refreshStatus();
        this.announce("Backup downloaded");
      };
      const importBtn = document.createElement("button");
      importBtn.type = "button";
      importBtn.className = "btn-secondary pressable";
      importBtn.style.marginTop = "var(--space-3)";
      importBtn.textContent = "Import backup (vault must be locked)";
      const fileIn = document.createElement("input");
      fileIn.type = "file";
      fileIn.accept = ".tar,application/x-tar";
      fileIn.hidden = true;
      importBtn.onclick = () => fileIn.click();
      fileIn.onchange = async () => {
        const f = fileIn.files?.[0];
        fileIn.value = "";
        if (!f) return;
        if (!this.status?.locked) {
          window.alert("Lock the vault before importing a backup.");
          return;
        }
        try {
          const b64 = await readFileAsBase64(f);
          await api.backupImport(b64);
          this.announce("Backup imported. Unlock with your passphrase.");
        } catch (e) {
          window.alert(e instanceof AppError ? e.message : "Import failed");
        }
      };
      body.append(exportBtn, importBtn, fileIn);
    } else if (this.settingsSection === "about") {
      body.innerHTML = `<h2 class="text-22">About</h2><p class="muted text-13">Where this client sends ciphertext, and where your vault lives. The relay value comes from the vault once it is unlocked; while locked it shows the configured default.</p>`;
      const rows: [string, string][] = [
        ["Relay", this.status?.relay ?? "—"],
        ["Relay source", this.status?.relay_source ?? "—"],
        ["Vault", this.status?.vault_dir ?? "—"],
        ["API", getApiBase()],
        [
          "TCP",
          this.status?.tcp_addr ??
            "off (Unix socket only — restart with --addr to enable)",
        ],
      ];
      const list = document.createElement("dl");
      list.className = "about-list";
      list.style.marginTop = "var(--space-4)";
      for (const [k, v] of rows) {
        const dt = document.createElement("dt");
        dt.className = "muted text-13";
        dt.textContent = k;
        const dd = document.createElement("dd");
        dd.className = "text-13";
        dd.style.wordBreak = "break-all";
        dd.textContent = v;
        list.append(dt, dd);
      }
      const hint = document.createElement("p");
      hint.className = "faint text-12";
      hint.style.marginTop = "var(--space-4)";
      hint.textContent =
        "Change the default with: hush-cli config set --relay <url>. Existing vaults keep the relay they were created with.";
      body.append(list, hint);
    } else {
      body.innerHTML = `<h2 class="text-22">${this.settingsSection}</h2><p class="muted">More options coming soon.</p>`;
    }
    pane.append(nav, body);
    return pane;
  }

  private renderPalette(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    back.onclick = (e) => {
      if (e.target === back) {
        this.paletteOpen = false;
        this.render();
      }
    };
    const modal = document.createElement("div");
    modal.className = "modal palette";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", "Command palette");
    const inp = document.createElement("input");
    inp.placeholder = "Type a command or contact…";
    const actions: { label: string; run: () => void }[] = [
      ...this.contacts.map((c) => ({
        label: `Open ${displayNick(c.nickname)}`,
        run: () => {
          this.selectedNick = c.nickname;
          this.rail = "chats";
        },
      })),
      {
        label: "Add a trust",
        run: () => {
          this.addFriendOpen = true;
        },
      },
      {
        label: "Lock vault",
        run: () => void this.lock(),
      },
      {
        label: "Toggle theme",
        run: () => {
          void setThemeMode(getThemeMode() === "dark" ? "light" : "dark").then(
            () => this.render(),
          );
        },
      },
    ];
    const list = document.createElement("div");
    const renderList = () => {
      list.innerHTML = "";
      const q = inp.value.toLowerCase();
      actions
        .filter((a) => a.label.toLowerCase().includes(q))
        .forEach((a) => {
          const btn = document.createElement("button");
          btn.className = "palette-item pressable";
          btn.textContent = a.label;
          btn.onclick = () => {
            a.run();
            this.paletteOpen = false;
            this.render();
          };
          list.appendChild(btn);
        });
    };
    inp.oninput = renderList;
    renderList();
    modal.append(inp, list);
    back.appendChild(modal);
    setTimeout(() => inp.focus(), 0);
    return back;
  }

  private renderAddFriend(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    const title = document.createElement("h2");
    title.className = "text-22";
    title.textContent = "Add a trust";
    title.style.marginBottom = "var(--space-3)";
    let tab = "show" as "show" | "add";
    const tabs = document.createElement("div");
    tabs.className = "tabs";
    const showTab = document.createElement("button");
    showTab.className = "tab active";
    showTab.textContent = "Show my invite";
    const addTab = document.createElement("button");
    addTab.className = "tab";
    addTab.textContent = "Add with invite";
    const body = document.createElement("div");
    const renderTab = async () => {
      body.innerHTML = "";
      showTab.classList.toggle("active", tab === "show");
      addTab.classList.toggle("active", tab === "add");
      if (tab === "show") {
        const card = document.createElement("div");
        card.className = "key-card";
        const inner = document.createElement("div");
        inner.className = "key-card-inner";
        const qr = document.createElement("div");
        qr.className = "qr-tile";
        try {
          const inv = await api.invite();
          if (inv) {
            const canvas = document.createElement("canvas");
            await QRCode.toCanvas(canvas, inv.invite, { width: 160, margin: 1 });
            qr.appendChild(canvas);
            const copy = document.createElement("button");
            copy.className = "btn-secondary pressable";
            copy.textContent = "Copy invite link";
            copy.style.marginTop = "12px";
            copy.onclick = () => navigator.clipboard.writeText(inv.invite);
            inner.append(qr, copy);
            const note = document.createElement("p");
            note.className = "text-13 muted";
            note.textContent =
              "Anyone with this card can start a chat with you. Single use.";
            inner.appendChild(note);
          }
        } catch {
          inner.textContent = "Couldn't load invite";
        }
        card.appendChild(inner);
        body.appendChild(card);
      } else {
        const nick = document.createElement("input");
        nick.className = "input-field";
        nick.placeholder = "Nickname";
        const inv = document.createElement("textarea");
        inv.className = "input-field";
        inv.placeholder = "Paste invite";
        inv.rows = 3;
        inv.style.marginTop = "8px";
        const err = document.createElement("p");
        err.style.color = "var(--danger)";
        err.hidden = true;
        const btn = document.createElement("button");
        btn.className = "btn-primary pressable";
        btn.textContent = "Add contact";
        btn.style.marginTop = "12px";
        btn.onclick = async () => {
          err.hidden = true;
          try {
            await api.addContact(nick.value.trim(), inv.value.trim());
            this.contacts = (await api.contacts()) ?? [];
            setKnownContacts(this.contacts.map((c) => c.nickname));
            this.addFriendOpen = false;
            this.announce("Names live only on your device.");
            this.render();
          } catch (e) {
            err.hidden = false;
            err.textContent =
              e instanceof AppError ? e.message : "Couldn't add contact";
          }
        };
        const hint = document.createElement("p");
        hint.className = "text-13 muted";
        hint.textContent = "Names live only on your device.";
        body.append(nick, inv, err, btn, hint);
      }
    };
    showTab.onclick = () => {
      tab = "show";
      void renderTab();
    };
    addTab.onclick = () => {
      tab = "add";
      void renderTab();
    };
    void renderTab();
    tabs.append(showTab, addTab);
    const close = document.createElement("button");
    close.className = "btn-secondary pressable";
    close.textContent = "Close";
    close.style.marginTop = "16px";
    close.onclick = () => {
      this.addFriendOpen = false;
      this.render();
    };
    modal.append(title, tabs, body, close);
    back.appendChild(modal);
    return back;
  }

  private renderProfile(): HTMLElement {
    const nick = this.profileNick!;
    const c = this.contacts.find((x) => x.nickname === nick);
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    if (c) modal.appendChild(createAvatarEl(c.protocol_name, "lg"));
    const name = document.createElement("input");
    name.className = "input-field";
    name.value = displayNick(nick);
    name.style.marginTop = "12px";
    const save = document.createElement("button");
    save.className = "btn-secondary pressable";
    save.textContent = "Save nickname";
    save.onclick = () => {
      void setNickOverride(nick, name.value).then(() => {
        this.profileNick = null;
        this.render();
      });
    };
    const mute = document.createElement("button");
    mute.className = "btn-secondary pressable";
    mute.textContent = isMuted(nick) ? "Unmute" : "Mute";
    mute.onclick = () => {
      void setMuted(nick, !isMuted(nick)).then(() => this.render());
    };
    // Deletion lives in Trusts only. This button is a pointer to it, not a
    // second implementation, so there is one place that decides what removing
    // a person does.
    const manage = document.createElement("button");
    manage.className = "btn-secondary pressable";
    manage.textContent = "Manage in Trusts";
    manage.onclick = () => {
      this.profileNick = null;
      this.rail = "trusts";
      this.render();
    };
    const close = document.createElement("button");
    close.className = "btn-primary pressable";
    close.textContent = "Done";
    close.onclick = () => {
      this.profileNick = null;
      this.render();
    };
    modal.append(name, save, mute, manage, close);
    back.appendChild(modal);
    return back;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
