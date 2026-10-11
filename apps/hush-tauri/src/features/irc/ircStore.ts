/**
 * The IRC pane's client-side store.
 *
 * The Rust client owns the sockets and the authoritative state; this store is
 * a cache fed entirely by `irc://*` events (plus one `get_state` at startup).
 * It is module-level and lives for the whole session, so unread counts and
 * scrollback survive switching rails and the pane never has to be re-mounted
 * to keep receiving.
 *
 * Memory is bounded: each window keeps at most `MAX_LINES` lines.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { isTauriShell } from "../../lib/tauriEnv";
import type {
  CertStatus,
  ConnectRequest,
  IrcDefaults,
  IrcJoinEvent,
  IrcListEndEvent,
  IrcListEvent,
  IrcMessageEvent,
  IrcModeEvent,
  IrcNamesEvent,
  IrcNickEntry,
  IrcNickEvent,
  IrcPartEvent,
  IrcQuitEvent,
  IrcSegment,
  IrcServerEvent,
  IrcServerSnapshot,
  IrcSettings,
  IrcStatusEvent,
  IrcTopicEvent,
  SettingsBundle,
} from "./ircTypes";

/** Lines retained per window. */
export const MAX_LINES = 2000;

/** The sentinel target for a server's status window. */
export const STATUS_TARGET = "*";

export type IrcLineKind =
  | "message"
  | "action"
  | "notice"
  | "join"
  | "part"
  | "quit"
  | "nick"
  | "topic"
  | "mode"
  | "status"
  | "error"
  | "motd";

export interface IrcLine {
  id: number;
  kind: IrcLineKind;
  ts: number;
  from?: string;
  segments?: IrcSegment[];
  text?: string;
  highlight?: boolean;
}

export type IrcWindowKind = "status" | "channel" | "query";

export interface IrcWindow {
  serverId: string;
  target: string;
  kind: IrcWindowKind;
  name: string;
  lines: IrcLine[];
  unread: number;
  mentioned: boolean;
  topic: string;
  topicWho: string | null;
  names: IrcNickEntry[];
  modes: string;
  joined: boolean;
}

export interface IrcServer {
  id: string;
  host: string;
  port: number;
  tls: boolean;
  useTor: boolean;
  nick: string;
  connected: boolean;
  registered: boolean;
  phase: string;
  order: string[];
  windows: Map<string, IrcWindow>;
}

export interface IrcChannelListEntry {
  channel: string;
  users: number;
  topic: string;
}

export interface IrcChannelList {
  entries: IrcChannelListEntry[];
  done: boolean;
}

export interface IrcUiState {
  servers: IrcServer[];
  activeServerId: string | null;
  activeTarget: string | null;
  settings: IrcSettings | null;
  defaults: IrcDefaults | null;
  dark: boolean;
  available: boolean;
  channelLists: Map<string, IrcChannelList>;
}

type Listener = (state: IrcUiState) => void;

const state: IrcUiState = {
  servers: [],
  activeServerId: null,
  activeTarget: null,
  settings: null,
  defaults: null,
  dark: false,
  available: isTauriShell(),
  channelLists: new Map(),
};

const listeners = new Set<Listener>();
let lineSeq = 1;
let initialized = false;
let unlisten: UnlistenFn[] = [];

export function getIrcState(): IrcUiState {
  return state;
}

export function subscribeIrc(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function notify(): void {
  for (const fn of listeners) fn(state);
}

function nowMs(): number {
  return Date.now();
}

// --- Window and server helpers ---

function ensureServer(id: string): IrcServer {
  let server = state.servers.find((s) => s.id === id);
  if (!server) {
    server = {
      id,
      host: id,
      port: 0,
      tls: true,
      useTor: false,
      nick: "",
      connected: false,
      registered: false,
      phase: "Connecting",
      order: [],
      windows: new Map(),
    };
    state.servers.push(server);
  }
  return server;
}

export function findServer(id: string | null): IrcServer | null {
  if (!id) return null;
  return state.servers.find((s) => s.id === id) ?? null;
}

function ensureWindow(
  server: IrcServer,
  target: string,
  kind?: IrcWindowKind,
): IrcWindow {
  let window = server.windows.get(target);
  if (window) return window;
  const resolvedKind: IrcWindowKind =
    kind ?? (target === STATUS_TARGET ? "status" : isChannelName(target) ? "channel" : "query");
  window = {
    serverId: server.id,
    target,
    kind: resolvedKind,
    name: resolvedKind === "status" ? "Status" : target,
    lines: [],
    unread: 0,
    mentioned: false,
    topic: "",
    topicWho: null,
    names: [],
    modes: "",
    joined: resolvedKind !== "channel",
  };
  server.windows.set(target, window);
  // Status first, then channels, then queries, each in insertion order.
  server.order.push(target);
  server.order.sort((a, b) => rankWindow(server, a) - rankWindow(server, b));
  return window;
}

function rankWindow(server: IrcServer, target: string): number {
  const w = server.windows.get(target);
  if (!w) return 99;
  return w.kind === "status" ? 0 : w.kind === "channel" ? 1 : 2;
}

function isChannelName(target: string): boolean {
  const c = target[0];
  return c === "#" || c === "&" || c === "!" || c === "+";
}

function isActive(serverId: string, target: string): boolean {
  return state.activeServerId === serverId && state.activeTarget === target;
}

function pushLine(serverId: string, target: string, line: Omit<IrcLine, "id" | "ts"> & { ts?: number }): void {
  const server = ensureServer(serverId);
  const window = ensureWindow(server, target);
  const full: IrcLine = { id: lineSeq++, ts: line.ts ?? nowMs(), ...line };
  window.lines.push(full);
  if (window.lines.length > MAX_LINES) {
    window.lines.splice(0, window.lines.length - MAX_LINES);
  }
  const isMessage = line.kind === "message" || line.kind === "action" || line.kind === "notice";
  if (!isActive(serverId, target)) {
    if (isMessage) window.unread += 1;
    if (line.highlight) window.mentioned = true;
  }
}

/** Add a line the backend did not send (slash-command errors, `/help`). */
export function pushLocalLine(
  serverId: string,
  target: string,
  kind: IrcLineKind,
  text: string,
): void {
  pushLine(serverId, target, { kind, text });
  notify();
}

/** A status line in the server's own status window. */
export function pushLocalStatus(serverId: string, text: string, kind: IrcLineKind = "status"): void {
  pushLocalLine(serverId, STATUS_TARGET, kind, text);
}

// --- Event application ---

function applyServerEvent(e: IrcServerEvent): void {
  const server = ensureServer(e.server_id);
  server.id = e.id;
  server.host = e.host;
  server.port = e.port;
  server.tls = e.tls;
  server.useTor = e.use_tor;
  server.nick = e.nick;
  server.connected = e.connected;
  server.registered = e.registered;
  server.phase = e.phase;
  ensureWindow(server, STATUS_TARGET, "status");
}

function applyStatusEvent(e: IrcStatusEvent): void {
  pushLine(e.server_id, e.target, {
    kind: e.level === "error" ? "error" : e.level === "motd" ? "motd" : "status",
    text: e.text,
  });
}

function applyMessageEvent(e: IrcMessageEvent): void {
  pushLine(e.server_id, e.target, {
    kind: e.kind === "action" ? "action" : e.kind === "notice" ? "notice" : "message",
    from: e.from,
    segments: e.segments,
    highlight: e.highlight,
    ts: e.ts,
  });
}

function applyJoinEvent(e: IrcJoinEvent): void {
  const server = ensureServer(e.server_id);
  const window = ensureWindow(server, e.target, "channel");
  window.joined = true;
  if (!window.names.some((n) => n.nick.toLowerCase() === e.nick.toLowerCase())) {
    window.names.push({ nick: e.nick, prefixes: "" });
  }
  pushLine(e.server_id, e.target, { kind: "join", text: e.nick });
  pushLine(e.server_id, STATUS_TARGET, { kind: "join", text: `${e.nick} joined ${e.target}` });
}

function applyPartEvent(e: IrcPartEvent): void {
  const server = ensureServer(e.server_id);
  const window = server.windows.get(e.target);
  if (window) {
    window.names = window.names.filter((n) => n.nick.toLowerCase() !== e.nick.toLowerCase());
  }
  const suffix = e.reason ? ` (${e.reason})` : "";
  pushLine(e.server_id, e.target, { kind: "part", text: `${e.nick} left ${e.target}${suffix}` });
  pushLine(e.server_id, STATUS_TARGET, { kind: "part", text: `${e.nick} left ${e.target}${suffix}` });
}

function applyQuitEvent(e: IrcQuitEvent): void {
  const server = ensureServer(e.server_id);
  const suffix = e.reason ? ` (${e.reason})` : "";
  for (const window of server.windows.values()) {
    if (window.kind !== "channel") continue;
    if (!window.names.some((n) => n.nick.toLowerCase() === e.nick.toLowerCase())) continue;
    window.names = window.names.filter((n) => n.nick.toLowerCase() !== e.nick.toLowerCase());
    pushLine(e.server_id, window.target, { kind: "quit", text: `${e.nick} quit${suffix}` });
  }
  pushLine(e.server_id, STATUS_TARGET, { kind: "quit", text: `${e.nick} quit${suffix}` });
}

function applyNickEvent(e: IrcNickEvent): void {
  const server = ensureServer(e.server_id);
  for (const window of server.windows.values()) {
    window.names = window.names.map((n) =>
      n.nick.toLowerCase() === e.old.toLowerCase() ? { ...n, nick: e.new } : n,
    );
  }
  pushLine(e.server_id, STATUS_TARGET, { kind: "nick", text: `${e.old} is now known as ${e.new}` });
  // The query window, if any, follows the rename.
  const query = server.windows.get(e.old);
  if (query && query.kind === "query") {
    server.windows.delete(e.old);
    query.target = e.new;
    query.name = e.new;
    server.windows.set(e.new, query);
    server.order = server.order.map((t) => (t === e.old ? e.new : t));
  }
}

function applyTopicEvent(e: IrcTopicEvent): void {
  const server = ensureServer(e.server_id);
  const window = ensureWindow(server, e.target, "channel");
  window.topic = e.topic;
  window.topicWho = e.who;
  const who = e.who ? ` by ${e.who}` : "";
  pushLine(e.server_id, e.target, {
    kind: "topic",
    text: e.topic ? `Topic${who}: ${e.topic}` : `Topic cleared${who}`,
  });
}

function applyNamesEvent(e: IrcNamesEvent): void {
  const server = ensureServer(e.server_id);
  const window = ensureWindow(server, e.target, "channel");
  window.names = e.names;
}

function applyModeEvent(e: IrcModeEvent): void {
  const line = `${e.by || "server"} sets mode ${e.modes}${e.args ? ` ${e.args}` : ""}`;
  pushLine(e.server_id, e.target, { kind: "mode", text: line });
  pushLine(e.server_id, STATUS_TARGET, { kind: "mode", text: `${e.target}: ${line}` });
}

function applyListEvent(e: IrcListEvent): void {
  let list = state.channelLists.get(e.server_id);
  if (!list) {
    list = { entries: [], done: false };
    state.channelLists.set(e.server_id, list);
  }
  list.entries.push({ channel: e.channel, users: e.users, topic: e.topic });
}

function applyListEndEvent(e: IrcListEndEvent): void {
  let list = state.channelLists.get(e.server_id);
  if (!list) {
    list = { entries: [], done: false };
    state.channelLists.set(e.server_id, list);
  }
  list.done = true;
}

/** Rebuild the whole store from a `get_state` snapshot (startup / reconnect). */
function applySnapshot(snapshots: IrcServerSnapshot[]): void {
  for (const snap of snapshots) {
    const server = ensureServer(snap.id);
    server.host = snap.host;
    server.port = snap.port;
    server.tls = snap.tls;
    server.useTor = snap.use_tor;
    server.nick = snap.nick;
    server.connected = snap.connected;
    server.registered = snap.registered;
    server.phase = snap.phase;
    ensureWindow(server, STATUS_TARGET, "status");
    for (const ch of snap.channels) {
      const window = ensureWindow(server, ch.name, "channel");
      window.topic = ch.topic;
      window.topicWho = ch.topic_who;
      window.names = ch.names;
      window.modes = ch.modes;
      window.joined = ch.joined;
    }
    for (const q of snap.queries) {
      ensureWindow(server, q.nick, "query");
    }
  }
}

// --- Public selectors ---

export function ircUnreadTotal(): number {
  let total = 0;
  for (const server of state.servers) {
    for (const window of server.windows.values()) {
      if (window.unread > 0) total += window.unread;
    }
  }
  return total;
}

export function ircMentionCount(): number {
  let total = 0;
  for (const server of state.servers) {
    for (const window of server.windows.values()) {
      if (window.mentioned) total += 1;
    }
  }
  return total;
}

export function activeIrcWindow(): IrcWindow | null {
  const server = findServer(state.activeServerId);
  if (!server || !state.activeTarget) return null;
  return server.windows.get(state.activeTarget) ?? null;
}

// --- Navigation ---

export function setActiveWindow(serverId: string, target: string): void {
  state.activeServerId = serverId;
  state.activeTarget = target;
  const server = findServer(serverId);
  const window = server?.windows.get(target);
  if (window) {
    window.unread = 0;
    window.mentioned = false;
  }
  persistLayoutSoon();
  notify();
}

export function cycleWindow(delta: number): void {
  const server = findServer(state.activeServerId);
  if (!server || server.order.length === 0) return;
  const index = Math.max(0, server.order.indexOf(state.activeTarget ?? STATUS_TARGET));
  const next = (index + delta + server.order.length) % server.order.length;
  setActiveWindow(server.id, server.order[next]);
}

export function setIrcDark(dark: boolean): void {
  state.dark = dark;
  persistLayoutSoon();
  notify();
}

// --- Commands ---

export async function loadIrcSettings(): Promise<void> {
  if (!state.available) return;
  const bundle = await invoke<SettingsBundle>("irc_settings_load");
  state.settings = bundle.settings;
  state.defaults = bundle.defaults;
  state.dark = bundle.settings.layout.dark;
  notify();
}

export async function refreshIrcState(): Promise<void> {
  if (!state.available) return;
  const snapshots = await invoke<IrcServerSnapshot[]>("get_state");
  applySnapshot(snapshots);
  notify();
}

export async function connectServer(request: ConnectRequest): Promise<string> {
  return invoke<string>("connect_server", { request });
}

/**
 * This device's certificate, as the connect dialog and `/cert` need it.
 *
 * Only the public half crosses this boundary: the private key stays in the
 * vault until a connection is opened, and never reaches the webview.
 */
export async function certStatus(): Promise<CertStatus> {
  return invoke<CertStatus>("irc_cert_status");
}

/** Mint a certificate and seal it in the vault. Returns its fingerprint. */
export async function certGenerate(): Promise<string> {
  return invoke<string>("irc_cert_generate");
}

/** Delete this device's certificate from the vault. */
export async function certForget(): Promise<void> {
  await invoke("irc_cert_forget");
}

/**
 * Ask NickServ to authorize this device's certificate, over a connection that
 * is already identified. The reply arrives in that server's status window.
 * Returns the fingerprint that was sent.
 */
export async function certAuthorize(serverId: string): Promise<string> {
  return invoke<string>("irc_cert_authorize", { serverId });
}

export async function disconnectServer(serverId: string, reason?: string): Promise<void> {
  await invoke("disconnect_server", { serverId, reason: reason ?? null });
  state.servers = state.servers.filter((s) => s.id !== serverId);
  if (state.activeServerId === serverId) {
    const next = state.servers[0];
    state.activeServerId = next?.id ?? null;
    state.activeTarget = next ? STATUS_TARGET : null;
  }
  notify();
}

export async function joinChannel(serverId: string, channel: string, key?: string): Promise<void> {
  await invoke("join_channel", { serverId, channel, key: key ?? null });
}

export async function partChannel(
  serverId: string,
  channel: string,
  reason?: string,
): Promise<void> {
  await invoke("part_channel", { serverId, channel, reason: reason ?? null });
}

export async function sendMessage(
  serverId: string,
  target: string,
  text: string,
  action = false,
): Promise<void> {
  await invoke("send_message", { serverId, target, text, action });
}

export async function sendRaw(serverId: string, line: string): Promise<void> {
  await invoke("send_raw", { serverId, line });
}

export async function changeNick(serverId: string, nick: string): Promise<void> {
  await invoke("change_nick", { serverId, nick });
}

/** Open (or focus) a query window locally and tell the backend. */
export function openQuery(serverId: string, nick: string): void {
  const server = ensureServer(serverId);
  ensureWindow(server, nick, "query");
  void invoke("open_query", { serverId, nick }).catch(() => {});
  setActiveWindow(serverId, nick);
}

/** Close a window: part a channel, drop a query, or (for status) disconnect. */
export async function closeWindow(serverId: string, target: string): Promise<void> {
  const server = findServer(serverId);
  const window = server?.windows.get(target);
  if (!window) return;
  if (window.kind === "channel") {
    await partChannel(serverId, target);
  } else if (window.kind === "status") {
    await disconnectServer(serverId);
    return;
  }
  server?.windows.delete(target);
  if (server) server.order = server.order.filter((t) => t !== target);
  if (isActive(serverId, target)) {
    state.activeTarget = server?.order[0] ?? null;
  }
  notify();
}

export function requestChannelList(serverId: string): void {
  state.channelLists.set(serverId, { entries: [], done: false });
  void invoke("list_channels", { serverId }).catch(() => {});
  notify();
}

/** Empty one window's scrollback (the `/clear` command). */
export function clearWindowLines(serverId: string, target: string): void {
  const server = findServer(serverId);
  const window = server?.windows.get(target);
  if (window) window.lines = [];
  notify();
}

export function channelList(serverId: string): IrcChannelList | null {
  return state.channelLists.get(serverId) ?? null;
}

// --- Settings persistence ---

let layoutTimer: number | null = null;

function persistLayoutSoon(): void {
  if (!state.available || !state.settings) return;
  if (layoutTimer !== null) window.clearTimeout(layoutTimer);
  layoutTimer = window.setTimeout(() => {
    layoutTimer = null;
    void persistSettings();
  }, 400);
}

export async function persistSettings(): Promise<void> {
  if (!state.available || !state.settings) return;
  state.settings.layout = {
    active_server: state.activeServerId,
    active_target: state.activeTarget,
    dark: state.dark,
  };
  await invoke("irc_settings_save", { settings: state.settings });
}

export async function saveSettings(settings: IrcSettings): Promise<void> {
  state.settings = settings;
  if (state.available) {
    await invoke("irc_settings_save", { settings });
  }
  notify();
}

// --- Wiring ---

/** Register the event listeners and pull the initial state. Idempotent. */
export async function initIrcStore(): Promise<void> {
  if (initialized) return;
  initialized = true;
  if (!state.available) return;

  const registrations: [string, (payload: unknown) => void][] = [
    ["irc://server", (p) => applyServerEvent(p as IrcServerEvent)],
    ["irc://status", (p) => applyStatusEvent(p as IrcStatusEvent)],
    ["irc://message", (p) => applyMessageEvent(p as IrcMessageEvent)],
    ["irc://join", (p) => applyJoinEvent(p as IrcJoinEvent)],
    ["irc://part", (p) => applyPartEvent(p as IrcPartEvent)],
    ["irc://quit", (p) => applyQuitEvent(p as IrcQuitEvent)],
    ["irc://nick", (p) => applyNickEvent(p as IrcNickEvent)],
    ["irc://topic", (p) => applyTopicEvent(p as IrcTopicEvent)],
    ["irc://names", (p) => applyNamesEvent(p as IrcNamesEvent)],
    ["irc://mode", (p) => applyModeEvent(p as IrcModeEvent)],
    ["irc://error", (p) => applyStatusEvent({ ...(p as IrcStatusEvent), level: "error" })],
    ["irc://list", (p) => applyListEvent(p as IrcListEvent)],
    ["irc://list_end", (p) => applyListEndEvent(p as IrcListEndEvent)],
  ];

  for (const [event, handler] of registrations) {
    unlisten.push(
      await listen(event, (e) => {
        handler(e.payload);
        notify();
      }),
    );
  }

  try {
    await loadIrcSettings();
    await refreshIrcState();
    const layout = state.settings?.layout;
    if (layout?.active_server && layout.active_target) {
      state.activeServerId = layout.active_server;
      state.activeTarget = layout.active_target;
    }
  } catch {
    /* settings are best-effort; the pane still works with defaults */
  }
  notify();
}

/** Tear down listeners (used only by tests / hot reload). */
export function disposeIrcStore(): void {
  for (const off of unlisten) off();
  unlisten = [];
  initialized = false;
}
