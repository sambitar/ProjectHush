import { api, AppError, getApiBase } from "./lib/api/daemon";
import type { ContactDto, StatusResponse } from "./lib/api/types";
import {
  addOutgoingFile,
  addOutgoingText,
  allFileMessages,
  clearChatStore,
  loadHistory,
  deliveryLabel,
  getThread,
  hasUnread,
  ingestIncoming,
  lastPreview,
  markOutgoingDelivered,
  markRead,
  setSendProgress,
  toggleReaction,
} from "./lib/chatStore";
import { MAX_FILE_BYTES, RECV_POLL_MS, AUTO_LOCK_MS } from "./lib/constants";
import { displayNick, getMockSettings, setBackupDone, setBlocked, setMuted, setNickOverride, setRecoveryRevealed, clearMockLocal, mockRecoveryCode, isMuted } from "./lib/api/mock";
import { fileToBase64, formatBytes, formatRelative } from "./lib/format";
import { groupMessages } from "./lib/messageGroups";
import { icon, type IconName } from "./lib/icons";
import { createAvatarEl } from "./components/ui/Avatar";
import { applyTheme, getThemeMode, setThemeMode, type ThemeMode } from "./lib/theme";
import QRCode from "qrcode";

type Rail = "chats" | "files" | "vault" | "settings";
type SettingsSection = "appearance" | "security" | "recovery" | "backup" | "about";

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
      this.status = await api.status();
      if (!this.status?.locked) {
        this.contacts = (await api.contacts()) ?? [];
        if (!this.selectedNick && this.contacts[0]) {
          this.selectedNick = this.contacts[0].nickname;
        }
        this.startPoll();
      } else {
        this.stopPoll();
      }
    } catch {
      this.status = { locked: true, vault_dir: "", protocol_name: null, vault_bytes: 0, contacts: 0 };
    }
  }

  private startPoll(): void {
    if (this.pollTimer) return;
    this.pollTimer = window.setInterval(() => this.pollRecv(), RECV_POLL_MS);
  }

  private stopPoll(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
  }

  private async pollRecv(): Promise<void> {
    if (!this.status || this.status.locked) return;
    try {
      const r = await api.recv();
      if (!r) return;
      for (const w of r.warnings) this.announce(w);
      for (const m of r.messages) {
        ingestIncoming(m.from, m);
        this.announce(`New message from ${m.from}`);
      }
      if (r.messages.length) {
        this.contacts = (await api.contacts()) ?? [];
        this.render();
      }
    } catch {
      /* daemon busy */
    }
  }

  private async lock(): Promise<void> {
    await api.lock();
    clearChatStore();
    clearMockLocal();
    this.selectedNick = null;
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
        const history = await api.messages();
        if (history) loadHistory(history);
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

  private railBtn(id: Rail, label: string, ic: IconName): HTMLElement {
    const b = document.createElement("button");
    b.className = `rail-btn pressable${this.rail === id ? " active" : ""}`;
    b.appendChild(icon(ic));
    const span = document.createElement("span");
    span.textContent = label;
    b.appendChild(span);
    b.onclick = () => {
      this.rail = id;
      this.render();
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
      const av = createAvatarEl(c.protocol_name, "sm", hasUnread(c.nickname));
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
    left.appendChild(createAvatarEl(contact.protocol_name, "lg"));
    const titles = document.createElement("div");
    const n = document.createElement("div");
    n.className = "text-17";
    n.style.fontWeight = "500";
    n.textContent = displayNick(nick);
    const sub = document.createElement("div");
    sub.className = "text-13 muted";
    sub.style.display = "flex";
    sub.style.alignItems = "center";
    sub.style.gap = "4px";
    sub.append(icon("lock"), document.createTextNode(`Only you and ${displayNick(nick)} can read this`));
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
          b.appendChild(fc);
        }
        if (tail && group.outgoing) {
          const d = document.createElement("div");
          d.className = "text-12 faint";
          d.style.marginTop = "4px";
          d.textContent = deliveryLabel(m.delivery);
          b.appendChild(d);
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

    const composer = this.renderComposer(nick);
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

  private renderComposer(nick: string): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "composer";
    const inner = document.createElement("div");
    inner.className = "composer-inner";
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
    ta.placeholder = "Message";
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
    send.onclick = () => void this.sendText(nick, ta.value.trim(), ta, send);
    inner.append(attach, fileIn, ta, send);
    wrap.appendChild(inner);
    syncSend();
    return wrap;
  }

  private async sendText(nick: string, text: string, ta: HTMLTextAreaElement, send: HTMLButtonElement): Promise<void> {
    if (!text) return;
    addOutgoingText(nick, text);
    ta.value = "";
    send.classList.remove("ready");
    this.render();
    try {
      await api.sendText(nick, text);
      const thread = getThread(nick);
      const last = thread[thread.length - 1];
      if (last) markOutgoingDelivered(nick, last.id);
      this.render();
    } catch {
      this.announce("Couldn't send. Try again.");
    }
  }

  private async sendFile(nick: string, file: File): Promise<void> {
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
      await api.sendFileData(nick, file.name, b64);
      markOutgoingDelivered(nick, msg.id);
      this.render();
    } catch {
      this.announce("Couldn't send. Try again.");
    }
  }

  private renderFiles(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane";
    pane.style.padding = "var(--space-5)";
    const h = document.createElement("h1");
    h.className = "text-22";
    h.textContent = "Files";
    const files = allFileMessages();
    const grid = document.createElement("div");
    grid.className = "bento";
    for (const m of files) {
      const t = document.createElement("div");
      t.className = "tile";
      t.innerHTML = `<div class="text-15">${m.fileName}</div><div class="muted text-13">${formatBytes(m.fileSize ?? 0)} · ${displayNick(m.contactNick)}</div>`;
      t.onclick = () => {
        this.rail = "chats";
        this.selectedNick = m.contactNick;
        this.render();
      };
      grid.appendChild(t);
    }
    if (!files.length) {
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "No files yet";
      grid.appendChild(p);
    }
    pane.append(h, grid);
    return pane;
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
    const remain = Math.max(0, AUTO_LOCK_MS - (Date.now() - this.unlockedAt));
    lock.innerHTML = `<div class="text-15">Auto-lock</div><div class="muted text-13">15 minutes</div><div class="text-13">${Math.ceil(remain / 60000)}m until lock</div><button class="btn-secondary pressable lock-now" style="margin-top:12px">Lock now</button>`;
    lock.querySelector(".lock-now")!.addEventListener("click", () => void this.lock());
    const backup = document.createElement("div");
    backup.className = "tile";
    const mock = getMockSettings();
    if (!mock.backupDone) backup.style.background = "var(--warning-soft)";
    backup.innerHTML = `<div class="text-15">Backup</div><div class="muted text-13">${mock.backupDone ? "Last backup recorded" : "Back up your vault so you never lose your chats"}</div><button class="btn-secondary pressable backup-btn" style="margin-top:12px">Back up now</button>`;
    backup.querySelector(".backup-btn")!.addEventListener("click", () => {
      setBackupDone(true);
      this.render();
    });
    grid.append(storage, lock, backup);
    pane.append(h, grid);
    return pane;
  }

  private renderSettings(): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "main-pane settings-layout";
    const nav = document.createElement("nav");
    nav.className = "settings-nav";
    const sections: { id: SettingsSection; label: string }[] = [
      { id: "appearance", label: "Appearance" },
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
          setThemeMode(mode);
          this.render();
        };
        seg.appendChild(b);
      }
      body.appendChild(seg);
    } else if (this.settingsSection === "recovery") {
      body.innerHTML = `<h2 class="text-22">Recovery</h2><p class="muted">Reveal your recovery code only in a private place.</p>`;
      const b = document.createElement("button");
      b.className = "btn-primary pressable";
      b.textContent = "I'm in a private place";
      const code = document.createElement("p");
      code.hidden = true;
      code.className = "text-15";
      b.onclick = () => {
        setRecoveryRevealed(true);
        code.hidden = false;
        code.textContent = mockRecoveryCode();
      };
      body.append(b, code);
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
          applyTheme(getThemeMode());
          setThemeMode(getThemeMode() === "dark" ? "light" : "dark");
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
      setNickOverride(nick, name.value);
      this.profileNick = null;
      this.render();
    };
    const mute = document.createElement("button");
    mute.className = "btn-secondary pressable";
    mute.textContent = isMuted(nick) ? "Unmute" : "Mute";
    mute.onclick = () => {
      setMuted(nick, !isMuted(nick));
      this.render();
    };
    const report = document.createElement("button");
    report.className = "btn-secondary pressable";
    report.style.color = "var(--danger)";
    report.textContent = "Report and remove";
    report.onclick = () => {
      if (confirm("Remove this contact from your list on this device?")) {
        setBlocked(nick, true);
        this.profileNick = null;
        this.render();
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

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
