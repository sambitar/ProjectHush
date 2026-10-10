export interface StatusResponse {
  locked: boolean;
  vault_dir: string;
  protocol_name: string | null;
  vault_bytes: number;
  contacts: number;
  recovery_acknowledged: boolean;
  last_backup_unix: number;
  relay: string;
  relay_source: string;
  socket: string | null;
  tcp_addr: string | null;
  /** This member's mail handle (local part only), if set. */
  mail_handle: string | null;
  /** This vault's mail suffix — the vault name chosen at first start. */
  vault_name: string | null;
  /** Whether the relay path is switched to Tor (onion + SOCKS). */
  relay_tor: boolean;
}

/** `GET /api/relay` — the relay switch's state, readable while locked.
 * `tor_mode` is the user's choice (prefs); `applied` is whether it is in
 * effect on the vault's traffic right now. */
export interface RelayConfigResponse {
  tor_mode: boolean;
  applied: boolean;
  onion_url: string | null;
  socks: string | null;
  /** Whether the relay answered a health probe: true reachable, false not,
   * null not yet known. The connection light must reflect this, not Tor's
   * bootstrap phase. */
  reachable: boolean | null;
}

export interface ContactDto {
  nickname: string;
  protocol_name: string;
  inbound_ready: boolean;
  online: boolean;
  /** This contact's mail handle (local part only), if they published one. */
  mail_handle?: string | null;
  /** The vault suffix of the contact's address, learned from their invite. */
  mail_vault?: string | null;
}

export interface MailAttachmentDto {
  name: string;
  size: number;
  blob_id: string;
}

export interface ReceivedDto {
  from: string;
  kind: string;
  text?: string;
  file_name?: string;
  file_size?: number;
  blob_id?: string;
  /** Mail only: the thread this message belongs to. */
  thread_id?: string | null;
  /** Mail only: the subject line, if the sender set one. */
  subject?: string | null;
  /** Mail only: attachment references (bytes via `/api/blob/{id}`). */
  attachments?: MailAttachmentDto[];
  /** Conclave only: the id the message's conclave is keyed on. */
  conclave_id?: string | null;
  /** Conclave only: the conclave's name. */
  conclave_name?: string | null;
  /** Conclave only: the sender's joined trustee nicknames. */
  conclave_trustees?: string[];
}

/** A file shared into a conclave: only the vault blob reference travels. */
export interface ConclaveFileDto {
  name: string;
  blob_id_hex: string;
  size: number;
}

/** One message in a conclave's own log. */
export interface ConclaveMessageDto {
  id: string;
  outgoing: boolean;
  /** The sender's nickname as this vault knows it; absent for our own messages. */
  from?: string | null;
  body: string;
  /** Present when the message carried a file. */
  file?: ConclaveFileDto | null;
  ts_ms: number;
}

/**
 * A person in a conclave. `invited` has been asked and not yet answered;
 * `joined` receives the conclave's messages; `left` and `kicked` are terminal.
 */
export type TrusteeStateDto = "invited" | "joined" | "left" | "kicked";

/** One trustee in a conclave, keyed on the stable account id. */
export interface ConclaveTrusteeDto {
  /** The trustee's account id (hex of the identity key). */
  account_id: string;
  /** The local nickname for this trustee; a display hint only. */
  nick: string;
  state: TrusteeStateDto;
}

/**
 * A conclave: a local record in the vault, not a relay-side room.
 *
 * Group chat is sender-side fan-out over the trustees' 1:1 sessions. Membership
 * is consensual: naming someone sends an invite they must accept, and only the
 * convener may invite, kick, or end.
 */
export interface ConclaveDto {
  id: string;
  name: string;
  /** The convener's account id: the only authority for invite, kick and end. */
  convener: string;
  /** The trustees, with per-trustee state. */
  trustees: ConclaveTrusteeDto[];
  created_ms: number;
  messages: ConclaveMessageDto[];
  /** True once the convener has ended it: read-only on every peer. */
  ended: boolean;
}

/** An invitation received but not yet accepted or declined. */
export interface PendingInviteDto {
  conclave_id: string;
  name: string;
  /** The convener's account id. */
  convener: string;
  /** The account id of whoever sent the invite. */
  from_account: string;
  /** The sender's nickname as this vault knows it. */
  from_nick: string;
  /** The trustees the invite carried. */
  trustees: ConclaveTrusteeDto[];
  received_ms: number;
}

export interface ChatRecordDto {
  id: string;
  contact_nick: string;
  outgoing: boolean;
  ts_ms: number;
  payload:
    | { t: "text"; body: string }
    | { t: "file"; name: string; blob_id_hex: string; size: number }
    | {
        t: "mail";
        thread_id: string;
        subject?: string | null;
        body: string;
        attachments: { name: string; blob_id_hex: string; size: number }[];
        /** `queued` means the relay refused it and the Outbox is holding it. */
        state: "sent" | "queued";
      }
    | {
        t: "draft";
        thread_id?: string | null;
        to: string;
        subject?: string | null;
        body: string;
        attachments: { name: string; blob_id_hex: string; size: number }[];
        updated_ms: number;
      }
    | {
        t: "drop";
        /** The rendezvous slot it arrived on — which published key claimed it. */
        rendezvous_id: string;
        subject?: string | null;
        body: string;
        attachments: { name: string; blob_id_hex: string; size: number }[];
        /** When the dropper sealed it, which is not when it arrived. */
        created_unix: number;
      };
}

/**
 * `GET /api/mail`: messages, drafts and drops together, so the three cannot
 * disagree. Drops are kept out of `mail` so ordinary inbox rendering never has
 * to know the variant exists.
 */
export interface MailboxDto {
  mail: ChatRecordDto[];
  drafts: ChatRecordDto[];
  drops: ChatRecordDto[];
}

/** `GET`/`POST /api/drops/key` and `POST /api/drops/rotate`. */
export interface DropsKeyDto {
  /** Whether the account has a drop key yet. */
  exists: boolean;
  /** The pasteable public key, `hush-drop-pk-v1:…`; null when none. */
  public_key: string | null;
  /** The drop page's URL on the vault's preferred relay base. */
  page_url: string | null;
}

/** `POST /api/drops/drain` — what one pass did. */
export interface DropsDrainDto {
  /** Rendezvous slots a container was found on this pass. */
  hit_slots: string[];
  /** Containers left parked (unopenable), for the next pass to retry. */
  remaining: number;
}

export interface RecvResponse {
  messages: ReceivedDto[];
  warnings: string[];
}

export interface InviteResponse {
  invite: string;
}

export interface ChatMessage {
  id: string;
  contactNick: string;
  outgoing: boolean;
  kind: "text" | "file";
  text?: string;
  fileName?: string;
  fileSize?: number;
  fileData?: Uint8Array;
  savedPath?: string;
  blobId?: string;
  createdAt: number;
  reactions: string[];
  sendProgress?: number;
}

// --- Reads (Hacker News reader) -------------------------------------------------

export interface ReadsStoryDto {
  id: string;
  title: string;
  url: string | null;
  domain: string | null;
  author: string;
  points: number;
  num_comments: number;
  /** Unix seconds. */
  created_at: number;
  hn_url: string;
}

export interface ReadsCommentDto {
  id: string;
  author: string;
  /** Unix seconds. */
  created_at: number;
  /** Sanitized comment HTML from the daemon; injected as-is. */
  html: string;
  children: ReadsCommentDto[];
}

export interface ReadsStoryDetailDto {
  story: ReadsStoryDto;
  /** Sanitized story text for text posts (Ask HN), when present. */
  text_html: string | null;
  comments: ReadsCommentDto[];
}

export interface ReadsReaderDto {
  url: string;
  title: string | null;
  /** Sanitized article HTML from the daemon; injected as-is. */
  html: string;
}

export interface ReadsSavedStoryDto {
  story: ReadsStoryDto;
  /** Unix seconds. */
  saved_at: number;
}

export interface ReadsHistoryEntryDto {
  id: string;
  /** Unix seconds. */
  at: number;
  title: string;
}

export interface ReadsStateDto {
  /** User-added RSS feeds; the built-in HK News source is not state. */
  sources: { id: string; url: string; title: string; added_at: number }[];
  saved: ReadsSavedStoryDto[];
  hidden: string[];
  read: { id: string; at: number }[];
  history: ReadsHistoryEntryDto[];
}

/** One browsable source: the built-in HK News feed or a user RSS feed. */
export interface ReadsSourceDto {
  id: string;
  title: string;
  url: string;
  /** Built-ins cannot be removed; the UI hides the affordance. */
  built_in: boolean;
}

/** One story in a feed page, annotated with the viewer's markers. */
export type ReadsStoryListItem = ReadsStoryDto & {
  saved: boolean;
  read: boolean;
};

export interface ReadsFeedResponse {
  stories: ReadsStoryListItem[];
  /** True when this page looks like the last one. */
  done: boolean;
}

// --- Torrents (TorBox-powered client) ------------------------------------------

/** Where a queued item's data came from. */
export type TorrentSourceKind = "magnet" | "link" | "file";

/** The pipeline state of one torrent, mirroring the daemon's `ItemStatus`. */
export type TorrentItemStatus =
  | "queued"
  | "sending"
  | "caching"
  | "ready"
  | "downloading"
  | "completed"
  | "paused"
  | "error";

/** One file inside a torrent. `path` may contain `/`; the Library nests on it. */
export interface TorrentFileDto {
  file_id: number;
  name: string;
  path: string;
  size: number;
  mimetype: string | null;
  blob_id: string | null;
  done: boolean;
}

/** One queued or in-progress torrent. `source_data` never reaches the UI. */
export interface TorrentItemDto {
  id: string;
  name: string;
  source: TorrentSourceKind;
  hash: string | null;
  torrent_id: number | null;
  status: TorrentItemStatus;
  /** TorBox caching progress, 0..1. */
  progress: number;
  /** TorBox's own download speed, bytes/s. */
  speed: number;
  seeds: number;
  size: number;
  cached: boolean;
  /** A human sentence when `status === "error"`. */
  message: string | null;
  files: TorrentFileDto[];
  dl_done: number;
  dl_total: number;
  dl_speed: number;
  added_at: number;
  retry: number;
}

/** A completed torrent in the Library; its files are real vault blobs. */
export interface TorrentLibraryEntryDto {
  id: string;
  name: string;
  hash: string | null;
  size: number;
  added_at: number;
  files: TorrentFileDto[];
}

/** `GET /api/torrents/state` — everything the pane renders, minus the API key. */
export interface TorrentsStateDto {
  has_key: boolean;
  plan: number | null;
  plan_label: string | null;
  email: string | null;
  max_concurrent: number;
  items: TorrentItemDto[];
  library: TorrentLibraryEntryDto[];
}

/** `POST /api/torrents/test` — a validated key's account info. */
export interface TorboxUserDto {
  plan: number;
  plan_label: string;
  email: string | null;
  expires: string | null;
}
