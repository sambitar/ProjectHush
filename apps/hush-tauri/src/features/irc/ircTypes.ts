/**
 * Types shared between the Rust IRC client and the pane.
 *
 * Field names are snake_case because that is what the Rust `Serialize` structs
 * emit; keeping them identical avoids a mapping layer that could drift.
 */

/** One styled run of a message body (mIRC control codes already resolved). */
export interface IrcSegment {
  text: string;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  reverse: boolean;
  /** mIRC palette index 0-15, or null for the default foreground. */
  color: number | null;
  bg: number | null;
}

export interface IrcNickEntry {
  nick: string;
  prefixes: string;
}

export interface IrcChannelState {
  name: string;
  topic: string;
  topic_who: string | null;
  names: IrcNickEntry[];
  modes: string;
  joined: boolean;
}

export interface IrcQueryState {
  nick: string;
}

export interface IrcServerSnapshot {
  id: string;
  host: string;
  port: number;
  tls: boolean;
  use_tor: boolean;
  nick: string;
  connected: boolean;
  registered: boolean;
  phase: string;
  channels: IrcChannelState[];
  queries: IrcQueryState[];
}

export interface SavedServer {
  id: string;
  name: string;
  host: string;
  port: number;
  tls: boolean;
  use_tor: boolean;
  nick: string;
  alt_nick: string;
  auto_join: string[];
}

export interface LayoutSettings {
  active_server: string | null;
  active_target: string | null;
  dark: boolean;
}

export interface IrcSettings {
  nick: string;
  alt_nick: string;
  username: string;
  realname: string;
  default_use_tor: boolean;
  servers: SavedServer[];
  layout: LayoutSettings;
}

export interface IrcDefaults {
  default_host: string;
  default_port: number;
  lobby_channel: string;
  libera_onion_host: string;
}

export interface SettingsBundle {
  settings: IrcSettings;
  defaults: IrcDefaults;
}

/** What `connect_server` takes. */
export interface ConnectRequest {
  id?: string;
  host: string;
  port: number;
  tls: boolean;
  use_tor: boolean;
  nick: string;
  alt_nick?: string;
  username?: string;
  realname?: string;
  server_password?: string;
  sasl?: { account: string; password: string };
  auto_join?: string[];
}

// --- Event payloads (each tagged with server_id and a target) ---

export interface IrcServerEvent {
  server_id: string;
  id: string;
  host: string;
  port: number;
  tls: boolean;
  use_tor: boolean;
  nick: string;
  connected: boolean;
  registered: boolean;
  phase: string;
}

export interface IrcStatusEvent {
  server_id: string;
  target: string;
  text: string;
  level: "info" | "error" | "motd";
}

export type IrcMessageKind = "say" | "action" | "notice";

export interface IrcMessageEvent {
  server_id: string;
  target: string;
  from: string;
  kind: IrcMessageKind;
  segments: IrcSegment[];
  highlight: boolean;
  ts: number;
}

export interface IrcJoinEvent {
  server_id: string;
  target: string;
  nick: string;
}

export interface IrcPartEvent {
  server_id: string;
  target: string;
  nick: string;
  reason: string;
}

export interface IrcQuitEvent {
  server_id: string;
  target: string;
  nick: string;
  reason: string;
}

export interface IrcNickEvent {
  server_id: string;
  target: string;
  old: string;
  new: string;
}

export interface IrcTopicEvent {
  server_id: string;
  target: string;
  topic: string;
  who: string | null;
}

export interface IrcNamesEvent {
  server_id: string;
  target: string;
  names: IrcNickEntry[];
}

export interface IrcModeEvent {
  server_id: string;
  target: string;
  by: string;
  modes: string;
  args: string;
}

export interface IrcErrorEvent {
  server_id: string;
  target: string;
  text: string;
}

export interface IrcListEvent {
  server_id: string;
  channel: string;
  users: number;
  topic: string;
}

export interface IrcListEndEvent {
  server_id: string;
}
