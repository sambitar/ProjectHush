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
import {
  clearMailStore,
  draftById,
  folderCount,
  getThread as getMailThread,
  hasUnreadMail,
  isUnread,
  listDrafts,
  listFolder,
  loadMailbox,
  preview,
  previewText,
  searchMessages,
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
import { fileToBase64, formatBytes, formatRelative } from "./lib/format";
import {
  attachFileActions,
  downloadBase64File,
  readFileAsBase64,
  resolveMessageFileBytes,
} from "./lib/fileBytes";
import { groupMessages } from "./lib/messageGroups";
import { icon, type IconName } from "./lib/icons";
import { avatarWithPresence, createAvatarEl } from "./components/ui/Avatar";
import { applyTheme, getThemeMode, setThemeMode, type ThemeMode } from "./lib/theme";
import QRCode from "qrcode";

type Rail = "chats" | "mail" | "files" | "vault" | "settings";
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
  private linkDevicePayload: string | null = null;
  private unlockLinkMode = false;
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
   * Attachments held for the open compose.
   *
   * Bytes rather than refs, because both save and send post the bytes, and a
   * draft being edited has to carry its existing files forward.
   */
  private mailComposeFiles: { name: string; dataBase64: string; size: number }[] = [];
  private mailHandle: string | null = null;
  private mailDomain = "myserver.com";

  /** Read/star/archive/trash, straight from `ui-prefs.json`. */
  private mailPrefs(): MailFlags {
    return mailFlags();
  }

  constructor(root: HTMLElement) {
    this.root = root;
  }

  async start(): Promise<void> {
    for (let i = 0; i < 30; i++) {
      if (window.__HUSH_API__) break;
      await sleep(50);
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
    if (this.linkDevicePayload) {
      this.root.appendChild(this.renderLinkDeviceModal());
      return;
    }
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
    const linkToggle = document.createElement("button");
    linkToggle.type = "button";
    linkToggle.className = "text-13 pressable";
    linkToggle.style.marginTop = "var(--space-4)";
    linkToggle.style.color = "var(--accent)";
    linkToggle.textContent = this.unlockLinkMode
      ? "Back to unlock"
      : "Transfer from another device";
    linkToggle.onclick = () => {
      this.unlockLinkMode = !this.unlockLinkMode;
      this.render();
    };
    card.append(logoWrap, title, hint);
    if (this.unlockLinkMode) {
      const p = document.createElement("p");
      p.className = "muted text-13";
      p.textContent =
        "On your unlocked device: Settings → Security → Link a new device. Paste the pairing payload, or enter the source URL + link ID. This device must be locked. If the code shows 127.0.0.1 and this is a different machine, type the source PC's LAN URL below.";
      const payloadIn = document.createElement("textarea");
      payloadIn.className = "input-field";
      payloadIn.placeholder = 'Paste {"kind":"hush-link-device",...}';
      payloadIn.rows = 3;
      payloadIn.style.marginTop = "var(--space-3)";
      const sourceIn = document.createElement("input");
      sourceIn.className = "input-field";
      sourceIn.placeholder = "Source daemon URL (e.g. http://192.168.1.10:9876)";
      sourceIn.style.marginTop = "var(--space-2)";
      const idIn = document.createElement("input");
      idIn.className = "input-field";
      idIn.placeholder = "Link ID (if not using payload JSON)";
      idIn.style.marginTop = "var(--space-2)";
      const pullBtn = document.createElement("button");
      pullBtn.className = "btn-primary pressable";
      pullBtn.style.marginTop = "var(--space-3)";
      pullBtn.textContent = "Import vault copy";
      pullBtn.onclick = async () => {
        err.hidden = true;
        try {
          const parsed = parseLinkDevicePayload(payloadIn.value);
          const typedSource = sourceIn.value.trim();
          const source_api = typedSource || parsed?.source_api || "";
          const link_id = parsed?.link_id ?? idIn.value.trim();
          if (!source_api || !link_id) {
            throw new AppError("Enter a payload or both URL and link ID.");
          }
          await api.linkDevicePull(source_api, link_id);
          this.unlockLinkMode = false;
          await this.refreshStatus();
          this.announce("Vault copied. Unlock with your passphrase.");
          this.render();
        } catch (e) {
          err.hidden = false;
          err.textContent =
            e instanceof AppError ? e.message : "Transfer failed.";
        }
      };
      card.append(p, payloadIn, sourceIn, idIn, err, pullBtn, linkToggle, daemon);
    } else {
      card.append(pass, showRow, err, btn, linkToggle, daemon);
    }
    wrap.appendChild(card);
    return wrap;
  }

  private renderLinkDeviceModal(): HTMLElement {
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const modal = document.createElement("div");
    modal.className = "modal";
    modal.innerHTML = `<h2 class="text-22">Link a new device</h2><p class="muted text-13">Scan or copy this pairing code on the <strong>new</strong> device while it is locked. The transfer works once and expires in about 10 minutes. Both devices must reach each other on your network (use the source PC's LAN IP if not on the same machine).</p>`;
    const qr = document.createElement("div");
    qr.className = "qr-tile";
    qr.style.marginTop = "var(--space-4)";
    const payload = this.linkDevicePayload ?? "";
    const canvas = document.createElement("canvas");
    void QRCode.toCanvas(canvas, payload, { width: 200, margin: 1 });
    qr.appendChild(canvas);
    const pre = document.createElement("pre");
    pre.className = "text-12 muted";
    pre.style.marginTop = "var(--space-3)";
    pre.style.whiteSpace = "pre-wrap";
    pre.style.wordBreak = "break-all";
    pre.textContent = payload;
    const copy = document.createElement("button");
    copy.className = "btn-secondary pressable";
    copy.style.marginTop = "var(--space-3)";
    copy.textContent = "Copy pairing payload";
    copy.onclick = () => navigator.clipboard.writeText(payload);
    const done = document.createElement("button");
    done.className = "btn-primary pressable";
    done.style.marginTop = "var(--space-2)";
    done.textContent = "Done";
    done.onclick = () => {
      this.linkDevicePayload = null;
      this.render();
    };
    modal.append(qr, pre, copy, done);
    back.appendChild(modal);
    back.onclick = (e) => {
      if (e.target === back) {
        this.linkDevicePayload = null;
        this.render();
      }
    };
    return back;
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
    plus.setAttribute("aria-label", "Add a friend");
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
      b.textContent = "Add a friend";
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
      snip.textContent = previewText(d.body, d.attachments.length);
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
    const h = document.createElement("h1");
    h.className = "text-17";
    h.textContent = threadSubject(threadId);
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
      const card = document.createElement("div");
      card.className = `mail-message${m.outgoing ? " outgoing" : ""}${m.id === this.mailOpenId ? " open" : ""}`;
      const meta = document.createElement("div");
      meta.className = "text-12 faint";
      meta.textContent = `${m.outgoing ? "You" : displayNick(m.contactNick)} · ${formatRelative(m.createdAt)}`;
      const body = document.createElement("div");
      body.className = "text-15 mail-message-body";
      body.textContent = m.body;
      card.append(meta, body);
      for (const a of m.attachments) card.appendChild(this.mailAttachmentChip(a));
      list.appendChild(card);
    }
    pane.appendChild(list);
    return pane;
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
    this.render();

    // Editing a draft has to carry its attachments forward, and the log only
    // kept blob ids — so the bytes come back out of the vault.
    const draft = opts.draftId ? draftById(opts.draftId) : undefined;
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
    this.render();
  }

  private renderMailCompose(): HTMLElement {
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
    const toSel = document.createElement("select");
    toSel.className = "input-field mail-compose-to";
    for (const c of this.contacts) {
      const o = document.createElement("option");
      o.value = c.nickname;
      o.textContent = c.mail_handle
        ? `${displayNick(c.nickname)} (${c.mail_handle}@${this.mailDomain})`
        : displayNick(c.nickname);
      toSel.appendChild(o);
    }
    const wanted = this.mailReplyTo ?? draft?.to ?? "";
    if (wanted) toSel.value = wanted;
    // A reply goes back to the thread's contact; changing it would fork the
    // conversation, so the picker is fixed.
    toSel.disabled = isReply;
    form.appendChild(this.composeRow("To", toSel));

    const subj = document.createElement("input");
    subj.className = "input-field mail-compose-subject";
    subj.placeholder = "(no subject)";
    subj.value = draft?.subject ?? this.mailSubjectSeed ?? "";
    form.appendChild(this.composeRow("Subject", subj));

    const body = document.createElement("textarea");
    body.className = "input-field mail-compose-body";
    body.placeholder = "Write your message";
    body.value = draft?.body ?? "";
    form.appendChild(body);

    const files = document.createElement("input");
    files.type = "file";
    files.multiple = true;
    files.className = "mail-file-input";
    files.onchange = () => {
      void this.addComposeFiles(Array.from(files.files ?? []));
      // Reset so picking the same file twice still fires a change event.
      files.value = "";
    };

    const preview = document.createElement("div");
    preview.className = "mail-attachment-preview";
    for (const [i, f] of this.mailComposeFiles.entries()) {
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
        this.render();
      };
      chip.append(name, size, remove);
      preview.appendChild(chip);
    }

    const err = document.createElement("p");
    err.className = "text-13";
    err.style.color = "var(--danger)";
    err.hidden = true;

    const attach = document.createElement("button");
    attach.type = "button";
    attach.className = "btn-secondary text-13 pressable";
    attach.textContent = this.mailComposeFiles.length ? "Add files" : "Attach files";
    attach.onclick = () => files.click();

    const saveDraft = document.createElement("button");
    saveDraft.type = "button";
    saveDraft.className = "btn-secondary pressable";
    saveDraft.textContent = "Save draft";
    saveDraft.onclick = () =>
      void this.saveDraft(saveDraft, {
        to: toSel.value,
        subject: subj.value,
        body: body.value,
        err,
      });

    const send = document.createElement("button");
    send.type = "button";
    send.className = "btn-primary pressable";
    send.textContent = isReply ? "Send reply" : "Send";
    send.onclick = () =>
      void this.sendMail(send, {
        to: toSel.value,
        subject: subj.value,
        body: body.value,
        err,
      });

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

  private async addComposeFiles(picked: File[]): Promise<void> {
    const next = [...this.mailComposeFiles];
    for (const f of picked) {
      next.push({
        name: f.name,
        dataBase64: await fileToBase64(f),
        size: f.size,
      });
    }
    this.mailComposeFiles = next;
    this.render();
  }

  /** The client-side half of the daemon's caps, so the limit is explained
   *  before a round trip rather than after one. */
  private composeError(
    to: string,
    body: string,
  ): string | null {
    if (!to) return "Add a contact first.";
    if (this.mailComposeFiles.length > MAX_MAIL_ATTACHMENTS) {
      return `At most ${MAX_MAIL_ATTACHMENTS} attachments.`;
    }
    const total = this.mailComposeFiles.reduce((n, f) => n + f.size, 0);
    if (total > MAX_MAIL_BYTES) {
      return `Attachments are limited to ${Math.round(MAX_MAIL_BYTES / 1024)} KB in total.`;
    }
    if (!body.trim() && !this.mailComposeFiles.length) {
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
    if (!input.to && !input.body.trim() && !input.subject.trim() && !this.mailComposeFiles.length) {
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

  private renderVault(): HTMLElement {
    const pane = document.createElement("div");
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
      const linkBtn = document.createElement("button");
      linkBtn.type = "button";
      linkBtn.className = "btn-secondary pressable";
      linkBtn.style.marginTop = "var(--space-5)";
      linkBtn.textContent = "Link a new device";
      linkBtn.onclick = () => {
        void api.linkDeviceStart().then((r) => {
          if (r?.payload) {
            this.linkDevicePayload = r.payload;
            this.render();
          }
        }).catch((e) => {
          this.announce(e instanceof AppError ? e.message : "Could not start link");
        });
      };
      body.append(label, sel, linkBtn, this.renderChangePassphraseForm());
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
        label: "Add a friend",
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
    modal.append(tabs, body, close);
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
    const report = document.createElement("button");
    report.className = "btn-secondary pressable";
    report.style.color = "var(--danger)";
    report.textContent = "Remove contact";
    report.onclick = () => {
      if (
        confirm(
          `Remove ${displayNick(nick)} from your contact list? Chat history stays in your vault.`,
        )
      ) {
        void (async () => {
          try {
            await api.removeContact(nick);
            await clearContactUiPrefs(nick);
            this.contacts = (await api.contacts()) ?? [];
            if (this.selectedNick === nick) this.selectedNick = null;
            if (this.filesContactNick === nick) this.filesContactNick = null;
            this.profileNick = null;
            this.announce("Contact removed");
            this.render();
          } catch (e) {
            this.announce(
              e instanceof AppError ? e.message : "Could not remove contact",
            );
          }
        })();
      }
    };
    const close = document.createElement("button");
    close.className = "btn-primary pressable";
    close.textContent = "Done";
    close.onclick = () => {
      this.profileNick = null;
      this.render();
    };
    modal.append(name, save, mute, report, close);
    back.appendChild(modal);
    return back;
  }
}

function parseLinkDevicePayload(
  raw: string,
): { source_api: string; link_id: string } | null {
  const t = raw.trim();
  if (!t) return null;
  try {
    const j = JSON.parse(t) as {
      kind?: string;
      source_api?: string;
      link_id?: string;
    };
    if (
      j.kind === "hush-link-device" &&
      typeof j.source_api === "string" &&
      typeof j.link_id === "string"
    ) {
      return { source_api: j.source_api, link_id: j.link_id };
    }
  } catch {
    return null;
  }
  return null;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
