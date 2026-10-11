/**
 * The IRC pane: an imperative controller that owns its DOM.
 *
 * The rest of the app rebuilds whole panes on render, but a chat pane cannot:
 * incoming messages must not rebuild the tree, steal the caret out of the
 * input, or reset the scroll position. So this class builds its skeleton once,
 * subscribes to the store, and patches only what changed. Input history and
 * per-window scroll positions live at module scope so they survive the rare
 * full remount (a rail switch).
 */

import { isSlashCommand, runSlashCommand } from "./ircCommands";
import { nickColor, renderSegments } from "./ircFormat";
import {
  activeIrcWindow,
  certForget,
  certGenerate,
  certStatus,
  channelList,
  closeWindow,
  connectServer,
  cycleWindow,
  disconnectServer,
  findServer,
  getIrcState,
  joinChannel,
  openQuery,
  partChannel,
  persistSettings,
  pushLocalLine,
  requestChannelList,
  saveSettings,
  sendMessage,
  sendRaw,
  setActiveWindow,
  setIrcDark,
  subscribeIrc,
  type IrcLine,
  type IrcServer,
  type IrcWindow,
} from "./ircStore";
import type { IrcNickEntry, SaslMechanism, SavedServer } from "./ircTypes";

/** Row height of the virtualized nick list, in pixels. */
const NICK_ROW_H = 16;

/** Input history is shared across remounts. */
const inputHistory: string[] = [];
const scrollPositions = new Map<string, { atBottom: boolean; top: number }>();

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `[${hh}:${mm}]`;
}

/** The highest channel prefix the current user holds in a channel. */
function myPrefixes(server: IrcServer | null, channel: string): string {
  if (!server) return "";
  const window = server.windows.get(channel);
  if (!window) return "";
  const me = window.names.find((n) => n.nick.toLowerCase() === server.nick.toLowerCase());
  return me?.prefixes ?? "";
}

function hasOps(prefixes: string): boolean {
  return /[~&@]/.test(prefixes);
}

export class IrcPane {
  private host: HTMLElement;
  private root!: HTMLElement;
  private treeEl!: HTMLElement;
  private switchbarEl!: HTMLElement;
  private topicEl!: HTMLElement;
  private messagesEl!: HTMLElement;
  private newMsgBtn!: HTMLButtonElement;
  private inputEl!: HTMLInputElement;
  private nickViewport!: HTMLElement;
  private nickSizer!: HTMLElement;
  private nickRows!: HTMLElement;
  private nickNames: IrcNickEntry[] = [];

  private unsubscribe: (() => void) | null = null;
  private renderedKey: string | null = null;
  private renderedLines = 0;
  private historyIndex = -1;
  private draft = "";
  private tabPrefix: string | null = null;
  private tabMatches: string[] = [];
  private tabIndex = 0;
  private menuEl: HTMLElement | null = null;
  private overlayEl: HTMLElement | null = null;
  private overlayUnsub: (() => void) | null = null;
  private pasteResolver: ((ok: boolean) => void) | null = null;

  constructor(host: HTMLElement) {
    this.host = host;
  }

  mount(): void {
    this.root = el("div", "irc-root");
    this.root.appendChild(this.buildToolbar());
    const body = el("div", "irc-body");

    this.treeEl = el("div", "irc-tree");
    body.appendChild(this.treeEl);

    const center = el("div", "irc-center");
    this.topicEl = el("div", "irc-topic");
    this.topicEl.title = "Double-click to change the topic (ops only)";
    this.topicEl.addEventListener("dblclick", () => this.promptTopic());
    center.appendChild(this.topicEl);

    const messagesWrap = el("div", "irc-messages-wrap");
    this.messagesEl = el("div", "irc-messages");
    this.messagesEl.addEventListener("scroll", () => this.onMessagesScroll());
    messagesWrap.appendChild(this.messagesEl);
    this.newMsgBtn = el("button", "irc-new-messages pressable", "New messages ↓");
    this.newMsgBtn.type = "button";
    this.newMsgBtn.hidden = true;
    this.newMsgBtn.addEventListener("click", () => this.scrollToBottom());
    messagesWrap.appendChild(this.newMsgBtn);
    center.appendChild(messagesWrap);

    this.inputEl = el("input", "irc-input");
    this.inputEl.type = "text";
    this.inputEl.spellcheck = false;
    this.inputEl.autocomplete = "off";
    this.inputEl.placeholder = "Type a message, or /help for commands";
    this.inputEl.addEventListener("keydown", (e) => this.onInputKey(e));
    this.inputEl.addEventListener("paste", (e) => this.onPaste(e));
    center.appendChild(this.inputEl);
    body.appendChild(center);

    this.nickViewport = el("div", "irc-nicks");
    this.nickSizer = el("div", "irc-nicks-sizer");
    this.nickRows = el("div", "irc-nicks-rows");
    this.nickSizer.appendChild(this.nickRows);
    this.nickViewport.appendChild(this.nickSizer);
    this.nickViewport.addEventListener("scroll", () => this.renderVisibleNicks());
    body.appendChild(this.nickViewport);

    this.root.appendChild(body);

    this.switchbarEl = el("div", "irc-switchbar");
    this.root.appendChild(this.switchbarEl);

    this.host.appendChild(this.root);
    this.unsubscribe = subscribeIrc(() => this.update());
    this.update(true);
  }

  unmount(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.closeMenu();
    this.closeOverlay();
  }

  /** Cycle the active window (called by the app's keyboard handler). */
  cycle(delta: number): void {
    cycleWindow(delta);
  }

  closeActive(): void {
    const window = activeIrcWindow();
    if (window) void closeWindow(window.serverId, window.target);
  }

  // ---------------------------------------------------------------- toolbar

  private buildToolbar(): HTMLElement {
    const bar = el("div", "irc-toolbar");
    const add = (label: string, onClick: () => void): HTMLButtonElement => {
      const b = el("button", "irc-tool-btn pressable", label);
      b.type = "button";
      b.addEventListener("click", onClick);
      bar.appendChild(b);
      return b;
    };
    add("Connect", () => this.openConnectDialog());
    add("Disconnect", () => this.disconnectActive());
    add("Channels List", () => this.openChannelList());
    add("Options", () => this.openOptions());
    return bar;
  }

  private disconnectActive(): void {
    const server = findServer(getIrcState().activeServerId);
    if (!server) return;
    this.confirm(`Disconnect from ${server.host}?`, () => void disconnectServer(server.id));
  }

  // ---------------------------------------------------------------- update

  private update(force = false): void {
    const state = getIrcState();
    const server = findServer(state.activeServerId);
    const window = activeIrcWindow();

    this.root.classList.toggle("irc-dark", state.dark);

    if (!state.available) {
      this.renderUnavailable();
      return;
    }

    this.renderTree(state.servers);
    this.renderSwitchbar(server);

    const key = window ? `${window.serverId}\u0000${window.target}` : null;
    const windowChanged = force || key !== this.renderedKey;

    if (!window) {
      this.topicEl.textContent = server
        ? `${server.host}:${server.port} — ${server.phase}`
        : "Not connected. Use Connect to add a server.";
      if (windowChanged) {
        this.messagesEl.textContent = "";
        this.renderedLines = 0;
        this.renderedKey = key;
      }
      this.nickNames = [];
      this.renderNicks();
      this.inputEl.disabled = true;
      return;
    }

    this.inputEl.disabled = false;
    this.updateTopic(server, window);

    if (windowChanged) {
      this.saveScroll();
      this.messagesEl.textContent = "";
      for (const line of window.lines) this.messagesEl.appendChild(this.renderLine(line));
      this.renderedLines = window.lines.length;
      this.renderedKey = key;
      if (key) this.restoreScroll(key);
    } else if (window.lines.length !== this.renderedLines) {
      // `/clear` shrinks the list; a normal event appends.
      if (window.lines.length < this.renderedLines) {
        this.messagesEl.textContent = "";
        for (const line of window.lines) this.messagesEl.appendChild(this.renderLine(line));
      } else {
        for (let i = this.renderedLines; i < window.lines.length; i += 1) {
          this.messagesEl.appendChild(this.renderLine(window.lines[i]));
        }
        const pos = scrollPositions.get(key!);
        if (pos && !pos.atBottom) {
          this.newMsgBtn.hidden = false;
        } else {
          this.scrollToBottom();
        }
      }
      this.renderedLines = window.lines.length;
    }

    this.nickNames = window.names;
    this.renderNicks();
  }

  private renderUnavailable(): void {
    this.treeEl.textContent = "";
    this.switchbarEl.textContent = "";
    this.topicEl.textContent = "IRC needs the desktop app.";
    this.messagesEl.textContent = "";
    const note = el(
      "div",
      "irc-status-line",
      "The IRC client runs in the Hush desktop shell. Open the app to connect.",
    );
    this.messagesEl.appendChild(note);
    this.inputEl.disabled = true;
  }

  private updateTopic(server: IrcServer | null, window: IrcWindow): void {
    if (window.kind === "channel") {
      const who = window.topicWho ? ` (set by ${window.topicWho})` : "";
      this.topicEl.textContent = window.topic
        ? `${window.target} — ${window.topic}${who}`
        : `${window.target} — no topic set`;
    } else if (window.kind === "query") {
      this.topicEl.textContent = `Private chat with ${window.target}`;
    } else {
      const phase = server ? `${server.host}:${server.port} — ${server.phase}` : "";
      this.topicEl.textContent = `Status — ${phase}`;
    }
  }

  // ---------------------------------------------------------------- tree

  private renderTree(servers: IrcServer[]): void {
    this.treeEl.textContent = "";
    const header = el("div", "irc-tree-header", "Channel Folder");
    this.treeEl.appendChild(header);
    if (servers.length === 0) {
      const empty = el("div", "irc-tree-empty", "No servers yet. Click Connect.");
      this.treeEl.appendChild(empty);
      return;
    }
    for (const server of servers) {
      const serverRow = el("div", "irc-tree-server");
      serverRow.textContent = `${server.host} (${server.phase})`;
      serverRow.addEventListener("click", () => {
        const first = server.order[0];
        if (first) setActiveWindow(server.id, first);
      });
      serverRow.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.openTreeMenu(e, server.id, null);
      });
      this.treeEl.appendChild(serverRow);

      const state = getIrcState();
      for (const target of server.order) {
        const window = server.windows.get(target);
        if (!window) continue;
        const row = el("div", "irc-tree-row");
        const isActive =
          state.activeServerId === server.id && state.activeTarget === target;
        if (isActive) row.classList.add("active");
        if (window.unread > 0) row.classList.add("unread");
        if (window.mentioned) row.classList.add("mentioned");

        const indent = window.kind === "status" ? "irc-tree-status" : "irc-tree-channel";
        row.classList.add(indent);
        const label = window.kind === "query" ? `@ ${window.name}` : window.name;
        row.appendChild(el("span", "irc-tree-label", label));
        if (window.unread > 0) row.appendChild(el("span", "irc-tree-count", String(window.unread)));

        row.addEventListener("click", () => setActiveWindow(server.id, target));
        row.addEventListener("dblclick", () => {
          if (window.kind === "channel") {
            joinChannel(server.id, target).catch(() => {});
          }
          setActiveWindow(server.id, target);
        });
        row.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          this.openTreeMenu(e, server.id, target);
        });
        this.treeEl.appendChild(row);
      }
    }
  }

  private openTreeMenu(event: MouseEvent, serverId: string, target: string | null): void {
    const server = findServer(serverId);
    if (!server) return;
    const window = target ? server.windows.get(target) : null;
    const items: MenuItem[] = [];
    if (window?.kind === "channel") {
      items.push({ label: "Join", run: () => joinChannel(serverId, window.target) });
      items.push({ label: "Part", run: () => partChannel(serverId, window.target) });
      items.push({ label: "Close", run: () => closeWindow(serverId, window.target) });
      items.push({ label: "Copy name", run: () => void copyText(window.target) });
    } else if (window?.kind === "query") {
      items.push({ label: "Close", run: () => closeWindow(serverId, window.target) });
      items.push({ label: "Copy name", run: () => void copyText(window.target) });
    }
    items.push({ label: "Join channel…", run: () => this.openJoinPrompt() });
    items.push({
      label: "Reconnect",
      run: () => {
        void disconnectServer(serverId).then(() => this.openConnectDialog(server));
      },
    });
    items.push({ label: "Disconnect", run: () => void disconnectServer(serverId) });
    this.openMenu(event, items);
  }

  // ---------------------------------------------------------------- switchbar

  private renderSwitchbar(server: IrcServer | null): void {
    this.switchbarEl.textContent = "";
    if (!server) return;
    const state = getIrcState();
    for (const target of server.order) {
      const window = server.windows.get(target);
      if (!window) continue;
      const btn = el("button", "irc-switch-btn pressable");
      btn.type = "button";
      if (state.activeTarget === target) btn.classList.add("active");
      if (window.unread > 0) btn.classList.add("unread");
      if (window.mentioned) btn.classList.add("mentioned");
      const label = window.kind === "query" ? window.name : window.name;
      btn.appendChild(el("span", "irc-switch-label", label));
      const close = el("span", "irc-switch-close", "×");
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        void closeWindow(server.id, target);
      });
      btn.appendChild(close);
      btn.addEventListener("click", () => setActiveWindow(server.id, target));
      this.switchbarEl.appendChild(btn);
    }
  }

  // ---------------------------------------------------------------- messages

  private renderLine(line: IrcLine): HTMLElement {
    const row = el("div", `irc-line irc-line-${line.kind}`);
    if (line.highlight) row.classList.add("irc-line-mention");
    row.appendChild(el("span", "irc-ts", fmtTime(line.ts)));

    switch (line.kind) {
      case "message": {
        const nick = el("span", "irc-nick", `<${line.from ?? ""}>`);
        nick.style.color = nickColor(line.from ?? "");
        row.appendChild(nick);
        row.appendChild(document.createTextNode(" "));
        row.appendChild(renderSegments(line.segments ?? []));
        break;
      }
      case "action": {
        const nick = el("span", "irc-action-nick", `* ${line.from ?? ""}`);
        nick.style.color = nickColor(line.from ?? "");
        row.appendChild(nick);
        row.appendChild(document.createTextNode(" "));
        row.appendChild(renderSegments(line.segments ?? []));
        break;
      }
      case "notice": {
        const nick = el("span", "irc-notice-nick", `-${line.from ?? ""}-`);
        row.appendChild(nick);
        row.appendChild(document.createTextNode(" "));
        row.appendChild(renderSegments(line.segments ?? []));
        break;
      }
      default: {
        // Event and status lines carry plain text, prefixed like mIRC.
        const prefix =
          line.kind === "join" ||
          line.kind === "part" ||
          line.kind === "quit" ||
          line.kind === "nick" ||
          line.kind === "mode" ||
          line.kind === "topic"
            ? "* "
            : "";
        row.appendChild(el("span", "irc-event", `${prefix}${line.text ?? ""}`));
      }
    }
    return row;
  }

  private onMessagesScroll(): void {
    const key = this.renderedKey;
    if (!key) return;
    const atBottom =
      this.messagesEl.scrollHeight - this.messagesEl.scrollTop - this.messagesEl.clientHeight < 40;
    scrollPositions.set(key, { atBottom, top: this.messagesEl.scrollTop });
    if (atBottom) this.newMsgBtn.hidden = true;
  }

  private saveScroll(): void {
    if (!this.renderedKey) return;
    const atBottom =
      this.messagesEl.scrollHeight - this.messagesEl.scrollTop - this.messagesEl.clientHeight < 40;
    scrollPositions.set(this.renderedKey, { atBottom, top: this.messagesEl.scrollTop });
  }

  private restoreScroll(key: string): void {
    const pos = scrollPositions.get(key);
    if (!pos || pos.atBottom) {
      this.scrollToBottom();
      this.newMsgBtn.hidden = true;
    } else {
      this.messagesEl.scrollTop = Math.min(pos.top, this.messagesEl.scrollHeight);
      this.newMsgBtn.hidden = false;
    }
  }

  private scrollToBottom(): void {
    this.messagesEl.scrollTop = this.messagesEl.scrollHeight;
    if (this.renderedKey) {
      scrollPositions.set(this.renderedKey, { atBottom: true, top: this.messagesEl.scrollTop });
    }
    this.newMsgBtn.hidden = true;
  }

  // ---------------------------------------------------------------- nick list

  private renderNicks(): void {
    this.nickSizer.style.height = `${this.nickNames.length * NICK_ROW_H}px`;
    this.renderVisibleNicks();
  }

  private renderVisibleNicks(): void {
    this.nickRows.textContent = "";
    const total = this.nickNames.length;
    if (total === 0) return;
    const scrollTop = this.nickViewport.scrollTop;
    const height = this.nickViewport.clientHeight || NICK_ROW_H * 20;
    const first = Math.max(0, Math.floor(scrollTop / NICK_ROW_H) - 2);
    const last = Math.min(total, Math.ceil((scrollTop + height) / NICK_ROW_H) + 2);
    for (let i = first; i < last; i += 1) {
      const entry = this.nickNames[i];
      const row = el("div", "irc-nick-row");
      row.style.top = `${i * NICK_ROW_H}px`;
      row.style.height = `${NICK_ROW_H}px`;
      if (entry.prefixes) {
        row.appendChild(el("span", "irc-nick-prefix", entry.prefixes));
      }
      const name = el("span", "irc-nick-name", entry.nick);
      name.style.color = nickColor(entry.nick);
      row.appendChild(name);
      row.addEventListener("dblclick", () => openQuery(this.activeServerId(), entry.nick));
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.openNickMenu(e, entry);
      });
      this.nickRows.appendChild(row);
    }
  }

  private activeServerId(): string {
    return getIrcState().activeServerId ?? "";
  }

  private openNickMenu(event: MouseEvent, entry: IrcNickEntry): void {
    const server = findServer(this.activeServerId());
    const window = activeIrcWindow();
    if (!server) return;
    const channel = window?.kind === "channel" ? window.target : null;
    const ops = hasOps(myPrefixes(server, channel ?? ""));
    const items: MenuItem[] = [
      { label: "Query", run: () => openQuery(server.id, entry.nick) },
      { label: "Whois", run: () => this.sendRaw(`WHOIS ${entry.nick}`) },
    ];
    if (channel) {
      items.push({
        label: "Op",
        disabled: !ops,
        run: () => this.sendRaw(`MODE ${channel} +o ${entry.nick}`),
      });
      items.push({
        label: "Deop",
        disabled: !ops,
        run: () => this.sendRaw(`MODE ${channel} -o ${entry.nick}`),
      });
      items.push({
        label: "Voice",
        disabled: !ops,
        run: () => this.sendRaw(`MODE ${channel} +v ${entry.nick}`),
      });
      items.push({
        label: "Devoice",
        disabled: !ops,
        run: () => this.sendRaw(`MODE ${channel} -v ${entry.nick}`),
      });
      items.push({
        label: "Kick",
        disabled: !ops,
        run: () => this.sendRaw(`KICK ${channel} ${entry.nick}`),
      });
      items.push({
        label: "Ban",
        disabled: !ops,
        run: () => this.sendRaw(`MODE ${channel} +b ${entry.nick}`),
      });
    }
    items.push({ label: "Copy nick", run: () => void copyText(entry.nick) });
    this.openMenu(event, items);
  }

  private sendRaw(line: string): void {
    const serverId = this.activeServerId();
    if (serverId) sendRaw(serverId, line).catch(() => {});
  }

  // ---------------------------------------------------------------- input

  private onInputKey(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      this.submitInput();
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      this.navigateHistory(-1);
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      this.navigateHistory(1);
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      this.completeNick();
      return;
    }
    // Any other key ends a completion cycle.
    this.tabPrefix = null;
  }

  private submitInput(): void {
    const value = this.inputEl.value.trim();
    if (!value) return;
    const window = activeIrcWindow();
    const serverId = this.activeServerId();
    if (!window || !serverId) return;

    inputHistory.push(value);
    if (inputHistory.length > 200) inputHistory.shift();
    this.historyIndex = inputHistory.length;
    this.inputEl.value = "";
    this.tabPrefix = null;

    if (isSlashCommand(value)) {
      const handled = runSlashCommand(value, {
        serverId,
        target: window.target,
        isChannel: window.kind === "channel",
        channel: window.kind === "channel" ? window.target : null,
      });
      if (handled) return;
    }
    sendMessage(serverId, window.target, value).catch((e) => {
      pushLocalLine(serverId, window.target, "error", String(e));
    });
  }

  private navigateHistory(delta: number): void {
    if (inputHistory.length === 0) return;
    if (this.historyIndex === inputHistory.length) this.draft = this.inputEl.value;
    this.historyIndex = Math.min(
      inputHistory.length,
      Math.max(0, this.historyIndex + delta),
    );
    this.inputEl.value =
      this.historyIndex === inputHistory.length
        ? this.draft
        : inputHistory[this.historyIndex];
    this.inputEl.setSelectionRange(this.inputEl.value.length, this.inputEl.value.length);
  }

  private completeNick(): void {
    const window = activeIrcWindow();
    if (!window || window.names.length === 0) return;
    const value = this.inputEl.value;
    const caret = this.inputEl.selectionStart ?? value.length;
    const before = value.slice(0, caret);
    const match = /(\S*)$/.exec(before);
    const prefix = match ? match[1] : "";
    if (prefix.startsWith("/")) return;
    const start = caret - prefix.length;

    if (this.tabPrefix !== prefix || this.tabMatches.length === 0) {
      this.tabPrefix = prefix;
      this.tabMatches = window.names
        .map((n) => n.nick)
        .filter((n) => n.toLowerCase().startsWith(prefix.toLowerCase()));
      this.tabIndex = 0;
    } else {
      this.tabIndex = (this.tabIndex + 1) % this.tabMatches.length;
    }
    if (this.tabMatches.length === 0) return;
    const completion = this.tabMatches[this.tabIndex];
    const suffix = start === 0 ? ": " : " ";
    const after = value.slice(caret);
    const next = value.slice(0, start) + completion + suffix + after;
    this.inputEl.value = next;
    const pos = start + completion.length + suffix.length;
    this.inputEl.setSelectionRange(pos, pos);
  }

  private onPaste(event: ClipboardEvent): void {
    const text = event.clipboardData?.getData("text") ?? "";
    const lines = text.split(/\r?\n/).filter((l) => l.trim().length > 0);
    if (lines.length <= 5) return; // ordinary paste, let the browser handle it
    event.preventDefault();
    this.confirm(`Send ${lines.length} pasted lines one at a time?`, () => {
      const window = activeIrcWindow();
      const serverId = this.activeServerId();
      if (!window || !serverId) return;
      for (const line of lines) {
        sendMessage(serverId, window.target, line).catch(() => {});
      }
    });
  }

  // ---------------------------------------------------------------- topic

  private promptTopic(): void {
    const server = findServer(this.activeServerId());
    const window = activeIrcWindow();
    if (!server || !window || window.kind !== "channel") return;
    if (!hasOps(myPrefixes(server, window.target))) {
      pushLocalLine(server.id, window.target, "status", "You need channel operator status to set the topic.");
      return;
    }
    this.prompt("Set channel topic", window.topic, (value) => {
      this.sendRaw(`TOPIC ${window.target} :${value}`);
    });
  }

  // ---------------------------------------------------------------- menus

  private openMenu(event: MouseEvent, items: MenuItem[]): void {
    this.closeMenu();
    const menu = el("div", "irc-menu");
    for (const item of items) {
      const row = el("button", "irc-menu-item pressable", item.label);
      row.type = "button";
      if (item.disabled) row.disabled = true;
      row.addEventListener("click", () => {
        this.closeMenu();
        item.run();
      });
      menu.appendChild(row);
    }
    document.body.appendChild(menu);
    const x = Math.min(event.clientX, window.innerWidth - menu.offsetWidth - 8);
    const y = Math.min(event.clientY, window.innerHeight - menu.offsetHeight - 8);
    menu.style.left = `${x}px`;
    menu.style.top = `${y}px`;
    this.menuEl = menu;
    const dismiss = (e: Event): void => {
      if (menu.contains(e.target as Node)) return;
      this.closeMenu();
      document.removeEventListener("mousedown", dismiss, true);
      document.removeEventListener("keydown", onKey, true);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") dismiss(e);
    };
    document.addEventListener("mousedown", dismiss, true);
    document.addEventListener("keydown", onKey, true);
  }

  private closeMenu(): void {
    this.menuEl?.remove();
    this.menuEl = null;
  }

  // ---------------------------------------------------------------- overlays

  private buildOverlay(title: string): { backdrop: HTMLElement; modal: HTMLElement } {
    this.closeOverlay();
    const backdrop = el("div", "irc-modal-backdrop");
    const modal = el("div", "irc-modal");
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-label", title);
    const head = el("div", "irc-modal-title", title);
    modal.appendChild(head);
    backdrop.appendChild(modal);
    backdrop.addEventListener("mousedown", (e) => {
      if (e.target === backdrop) this.closeOverlay();
    });
    this.root.appendChild(backdrop);
    this.overlayEl = backdrop;
    return { backdrop, modal };
  }

  private closeOverlay(): void {
    this.overlayUnsub?.();
    this.overlayUnsub = null;
    this.overlayEl?.remove();
    this.overlayEl = null;
    this.pasteResolver = null;
  }

  /** A message with a single OK button, for errors that have no window yet. */
  private alert(message: string): void {
    const { modal } = this.buildOverlay("IRC");
    modal.appendChild(el("p", "irc-modal-text", message));
    const actions = el("div", "irc-modal-actions");
    const ok = el("button", "irc-btn irc-btn-primary pressable", "OK");
    ok.type = "button";
    ok.addEventListener("click", () => this.closeOverlay());
    actions.appendChild(ok);
    modal.appendChild(actions);
  }

  /** Surface an error in the active window, or in a dialog when there is none. */
  private reportError(message: string): void {
    const serverId = this.activeServerId();
    const window = activeIrcWindow();
    if (serverId && window) {
      pushLocalLine(serverId, window.target, "error", message);
    } else {
      this.alert(message);
    }
  }

  private confirm(message: string, onOk: () => void): void {
    const { modal } = this.buildOverlay("Confirm");
    modal.appendChild(el("p", "irc-modal-text", message));
    const actions = el("div", "irc-modal-actions");
    const cancel = el("button", "irc-btn pressable", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => this.closeOverlay());
    const ok = el("button", "irc-btn irc-btn-primary pressable", "OK");
    ok.type = "button";
    ok.addEventListener("click", () => {
      this.closeOverlay();
      onOk();
    });
    actions.append(cancel, ok);
    modal.appendChild(actions);
  }

  private prompt(title: string, initial: string, onSubmit: (value: string) => void): void {
    const { modal } = this.buildOverlay(title);
    const input = el("input", "irc-field");
    input.type = "text";
    input.value = initial;
    modal.appendChild(input);
    const actions = el("div", "irc-modal-actions");
    const cancel = el("button", "irc-btn pressable", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => this.closeOverlay());
    const ok = el("button", "irc-btn irc-btn-primary pressable", "OK");
    ok.type = "button";
    const submit = (): void => {
      const value = input.value;
      this.closeOverlay();
      onSubmit(value);
    };
    ok.addEventListener("click", submit);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") submit();
    });
    actions.append(cancel, ok);
    modal.appendChild(actions);
    input.focus();
    input.select();
  }

  private openJoinPrompt(): void {
    const serverId = this.activeServerId();
    if (!serverId) {
      this.openConnectDialog();
      return;
    }
    this.prompt("Join channel", "#", (value) => {
      const channel = value.trim();
      if (channel) joinChannel(serverId, channel).catch(() => {});
    });
  }

  private openConnectDialog(prefill?: IrcServer): void {
    const state = getIrcState();
    const defaults = state.defaults;
    const saved: SavedServer[] = state.settings?.servers ?? [];
    const { modal } = this.buildOverlay("Connect to IRC server");

    const form = el("div", "irc-form");

    const serverSelect = el("select", "irc-field");
    const manualOption = el("option", undefined, "Other server…");
    manualOption.value = "__manual__";
    serverSelect.appendChild(manualOption);
    for (const s of saved) {
      const option = el("option", undefined, s.name || `${s.host}:${s.port}`);
      option.value = s.id;
      serverSelect.appendChild(option);
    }
    form.appendChild(labelled("Saved server", serverSelect));

    const hostInput = el("input", "irc-field");
    hostInput.type = "text";
    hostInput.value = prefill?.host ?? defaults?.default_host ?? "irc.libera.chat";
    const portInput = el("input", "irc-field");
    portInput.type = "number";
    portInput.value = String(prefill?.port ?? defaults?.default_port ?? 6697);
    const nickInput = el("input", "irc-field");
    nickInput.type = "text";
    nickInput.value = prefill?.nick || state.settings?.nick || defaultNick();
    const passInput = el("input", "irc-field");
    passInput.type = "password";
    passInput.placeholder = "optional";
    const saslAccount = el("input", "irc-field");
    saslAccount.type = "text";
    saslAccount.placeholder = "NickServ account (optional)";
    const saslPassword = el("input", "irc-field");
    saslPassword.type = "password";
    saslPassword.placeholder = "SASL password (not saved)";
    const saslMechanism = el("select", "irc-field");
    for (const [value, label] of [
      ["plain", "Password (PLAIN)"],
      ["external", "This device's certificate (EXTERNAL)"],
    ] as const) {
      const option = el("option", undefined, label);
      option.value = value;
      saslMechanism.appendChild(option);
    }
    // A reconnect prefills this dialog from the live server, whose id is the
    // same `host:port` the saved entry uses. The mechanism decides which
    // credential is sent, so it has to come from the saved entry: defaulting
    // the select to PLAIN would send a password to a server saved as EXTERNAL
    // (the Libera onion), which refuses passwords outright.
    const savedMechanism = saved.find((s) => s.id === prefill?.id)?.sasl_mechanism;
    saslMechanism.value = savedMechanism === "external" ? "external" : "plain";

    // The certificate section. Libera's Tor onion accepts only public-key SASL,
    // so EXTERNAL is the only way in there, and the fingerprint below is the one
    // thing the account has to authorize (once, from an identified connection).
    const certRow = el("div", "irc-cert");
    const certText = el("p", "irc-cert-text");
    const certFingerprint = el("code", "irc-cert-fingerprint");
    const certActions = el("div", "irc-cert-actions");
    const generateButton = el("button", "irc-btn pressable", "Generate certificate");
    generateButton.type = "button";
    const forgetButton = el("button", "irc-btn pressable", "Forget");
    forgetButton.type = "button";
    certActions.append(generateButton, forgetButton);
    certRow.append(certText, certFingerprint, certActions);

    const refreshCert = async (): Promise<void> => {
      try {
        const status = await certStatus();
        if (status.exists && status.fingerprint_sha512) {
          certText.textContent =
            "Authorize this fingerprint once, from a connection that is already identified: open that server's status window and run /cert authorize.";
          certFingerprint.textContent = status.fingerprint_sha512;
          generateButton.textContent = "Replace certificate";
          forgetButton.hidden = false;
        } else {
          certText.textContent =
            "This device has no certificate yet. Libera's Tor onion refuses passwords, so it needs one before it will let you in.";
          certFingerprint.textContent = "";
          generateButton.textContent = "Generate certificate";
          forgetButton.hidden = true;
        }
      } catch (e) {
        certText.textContent = `This device's certificate is unavailable: ${String(e)}`;
        certFingerprint.textContent = "";
        forgetButton.hidden = true;
      }
    };

    generateButton.addEventListener("click", () => {
      generateButton.disabled = true;
      generateButton.textContent = "Generating…";
      certGenerate()
        .then(() => refreshCert())
        .catch((e) => {
          certText.textContent = String(e);
        })
        .finally(() => {
          generateButton.disabled = false;
        });
    });

    forgetButton.addEventListener("click", () => {
      forgetButton.disabled = true;
      certForget()
        .then(() => refreshCert())
        .catch((e) => {
          certText.textContent = String(e);
        })
        .finally(() => {
          forgetButton.disabled = false;
        });
    });

    const saslAccountRow = labelled("SASL account", saslAccount);
    const saslPasswordRow = labelled("SASL password", saslPassword);

    // Only one of the two credentials applies at a time, so the dialog shows
    // exactly the one the chosen mechanism will use.
    const syncMechanism = (): void => {
      const external = saslMechanism.value === "external";
      saslPasswordRow.hidden = external;
      certRow.hidden = !external;
      saslAccount.placeholder = external
        ? "NickServ account (optional: the fingerprint identifies you)"
        : "NickServ account (optional)";
    };
    saslMechanism.addEventListener("change", syncMechanism);

    const tlsToggle = checkbox("Use TLS (port 6697)");
    tlsToggle.input.checked = prefill?.tls ?? true;
    const torToggle = checkbox("Route through Tor (protects your IP)");
    torToggle.input.checked = prefill?.useTor ?? state.settings?.default_use_tor ?? true;
    const autoJoinInput = el("input", "irc-field");
    autoJoinInput.type = "text";
    autoJoinInput.placeholder = defaults?.lobby_channel ?? "#your-lobby";

    form.append(
      labelled("Host", hostInput),
      labelled("Port", portInput),
      labelled("Nickname", nickInput),
      labelled("Server password", passInput),
      labelled("SASL mechanism", saslMechanism),
      saslAccountRow,
      saslPasswordRow,
      certRow,
      labelled("Auto-join channels", autoJoinInput),
      tlsToggle.wrap,
      torToggle.wrap,
    );
    syncMechanism();
    void refreshCert();

    serverSelect.addEventListener("change", () => {
      const chosen = saved.find((s) => s.id === serverSelect.value);
      if (!chosen) return;
      hostInput.value = chosen.host;
      portInput.value = String(chosen.port);
      tlsToggle.input.checked = chosen.tls;
      torToggle.input.checked = chosen.use_tor;
      nickInput.value = chosen.nick || nickInput.value;
      autoJoinInput.value = chosen.auto_join.join(" ");
      saslMechanism.value = chosen.sasl_mechanism === "external" ? "external" : "plain";
      syncMechanism();
    });

    modal.appendChild(form);

    const error = el("p", "irc-modal-error");
    modal.appendChild(error);

    const actions = el("div", "irc-modal-actions");
    const cancel = el("button", "irc-btn pressable", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => this.closeOverlay());
    const connect = el("button", "irc-btn irc-btn-primary pressable", "Connect");
    connect.type = "button";
    connect.addEventListener("click", () => {
      const host = hostInput.value.trim();
      const port = Number(portInput.value) || 6697;
      const nick = nickInput.value.trim() || defaultNick();
      if (!host) {
        error.textContent = "Host is required.";
        return;
      }
      const autoJoin = autoJoinInput.value
        .split(/\s+/)
        .map((s) => s.trim())
        .filter(Boolean);
      const account = saslAccount.value.trim();
      const mechanism: SaslMechanism =
        saslMechanism.value === "external" ? "external" : "plain";
      // EXTERNAL sends no secret, so it is configured even with an empty
      // account (the authzid is optional: the fingerprint is the identity).
      // PLAIN without both halves would be a silent no-op, so it is left off.
      const sasl =
        mechanism === "external"
          ? { mechanism, account }
          : account && saslPassword.value
            ? { mechanism, account, password: saslPassword.value }
            : undefined;
      this.closeOverlay();
      connectServer({
        host,
        port,
        tls: tlsToggle.input.checked,
        use_tor: torToggle.input.checked,
        nick,
        alt_nick: `${nick}_`,
        server_password: passInput.value || undefined,
        sasl,
        auto_join: autoJoin,
      })
        .then((id) => {
          setActiveWindow(id, "*");
        })
        .catch((e) => this.reportError(String(e)));
    });
    actions.append(cancel, connect);
    modal.appendChild(actions);
    hostInput.focus();
  }

  private openOptions(): void {
    const state = getIrcState();
    if (!state.settings) {
      this.confirm("Settings are only available in the desktop app.", () => {});
      return;
    }
    const settings = state.settings;
    const { modal } = this.buildOverlay("IRC options");
    const form = el("div", "irc-form");
    const nick = el("input", "irc-field");
    nick.value = settings.nick;
    const alt = el("input", "irc-field");
    alt.value = settings.alt_nick;
    const user = el("input", "irc-field");
    user.value = settings.username;
    const real = el("input", "irc-field");
    real.value = settings.realname;
    const tor = checkbox("Route new connections through Tor by default");
    tor.input.checked = settings.default_use_tor;
    const dark = checkbox("Classic dark colour scheme");
    dark.input.checked = state.dark;
    form.append(
      labelled("Nickname", nick),
      labelled("Alternate nickname", alt),
      labelled("Username", user),
      labelled("Real name", real),
      tor.wrap,
      dark.wrap,
    );
    modal.appendChild(form);

    const actions = el("div", "irc-modal-actions");
    const cancel = el("button", "irc-btn pressable", "Close");
    cancel.type = "button";
    cancel.addEventListener("click", () => this.closeOverlay());
    const save = el("button", "irc-btn irc-btn-primary pressable", "Save");
    save.type = "button";
    save.addEventListener("click", () => {
      settings.nick = nick.value.trim();
      settings.alt_nick = alt.value.trim();
      settings.username = user.value.trim();
      settings.realname = real.value.trim();
      settings.default_use_tor = tor.input.checked;
      setIrcDark(dark.input.checked);
      void saveSettings(settings).then(() => void persistSettings());
      this.closeOverlay();
    });
    actions.append(cancel, save);
    modal.appendChild(actions);
  }

  private openChannelList(): void {
    const serverId = this.activeServerId();
    if (!serverId) {
      this.openConnectDialog();
      return;
    }
    const { modal } = this.buildOverlay("Channel list");
    const search = el("input", "irc-field");
    search.placeholder = "Filter channels…";
    modal.appendChild(search);
    const table = el("div", "irc-chanlist");
    modal.appendChild(table);
    let sortByUsers = true;
    const sortBtn = el("button", "irc-btn pressable", "Sort by users");
    sortBtn.type = "button";
    const status = el("p", "irc-modal-text", "");
    const actions = el("div", "irc-modal-actions");
    const refresh = el("button", "irc-btn pressable", "Refresh");
    refresh.type = "button";
    const close = el("button", "irc-btn pressable", "Close");
    close.type = "button";
    close.addEventListener("click", () => this.closeOverlay());

    const paint = (): void => {
      const list = channelList(serverId);
      table.textContent = "";
      if (!list) {
        status.textContent = "Loading…";
        return;
      }
      const filter = search.value.trim().toLowerCase();
      let entries = list.entries.filter(
        (e) =>
          !filter ||
          e.channel.toLowerCase().includes(filter) ||
          e.topic.toLowerCase().includes(filter),
      );
      if (sortByUsers) {
        entries = [...entries].sort((a, b) => b.users - a.users);
      } else {
        entries = [...entries].sort((a, b) => a.channel.localeCompare(b.channel));
      }
      status.textContent = `${entries.length} channels${list.done ? "" : " (still loading…)"}`;
      for (const entry of entries.slice(0, 500)) {
        const row = el("div", "irc-chanlist-row");
        row.appendChild(el("span", "irc-chanlist-name", entry.channel));
        row.appendChild(el("span", "irc-chanlist-users", String(entry.users)));
        row.appendChild(el("span", "irc-chanlist-topic", entry.topic));
        row.addEventListener("dblclick", () => {
          joinChannel(serverId, entry.channel).catch(() => {});
          this.closeOverlay();
        });
        table.appendChild(row);
      }
    };

    search.addEventListener("input", paint);
    sortBtn.addEventListener("click", () => {
      sortByUsers = !sortByUsers;
      sortBtn.textContent = sortByUsers ? "Sort by name" : "Sort by users";
      paint();
    });
    refresh.addEventListener("click", () => {
      requestChannelList(serverId);
      paint();
    });
    actions.append(sortBtn, refresh, close);
    modal.appendChild(actions);
    modal.appendChild(status);
    // The list streams in over many `irc://list` events, so repaint the dialog
    // as they land rather than only once at open time.
    this.overlayUnsub = subscribeIrc(() => paint());
    requestChannelList(serverId);
    paint();
    search.focus();
  }
}

interface MenuItem {
  label: string;
  run: () => void;
  disabled?: boolean;
}

function labelled(label: string, field: HTMLElement): HTMLElement {
  const wrap = el("label", "irc-field-row");
  wrap.appendChild(el("span", "irc-field-label", label));
  wrap.appendChild(field);
  return wrap;
}

function checkbox(label: string): { wrap: HTMLElement; input: HTMLInputElement } {
  const wrap = el("label", "irc-checkbox");
  const input = document.createElement("input");
  input.type = "checkbox";
  wrap.appendChild(input);
  wrap.appendChild(el("span", undefined, label));
  return { wrap, input };
}

function defaultNick(): string {
  const digits = Math.floor(Math.random() * 10000)
    .toString()
    .padStart(4, "0");
  return `Guest${digits}`;
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    /* clipboard may be unavailable; nothing else to do */
  }
}
