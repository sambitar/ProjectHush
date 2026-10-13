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

// --- Notes (private sticky-paper board) ----------------------------------------

/** One tickable line on a note. */
export interface NoteChecklistItemDto {
  id: string;
  text: string;
  done: boolean;
}

/** One sticky note. `x`/`y`/`z` are unused until the canvas step lands. */
export interface NoteDto {
  id: string;
  board_id: string;
  title: string;
  body: string;
  color: string;
  x: number;
  y: number;
  z: number;
  created_at: number;
  updated_at: number;
  checklist: NoteChecklistItemDto[];
}

/** A labelled rectangle that gathers notes by position (canvas step). */
export interface NoteRegionDto {
  id: string;
  board_id: string;
  label: string;
  color: string;
  x: number;
  y: number;
  w: number;
  h: number;
  collapsed: boolean;
}

/** `GET /api/notes/state` — the whole board. */
export interface NotesStateDto {
  board_id: string;
  notes: NoteDto[];
  regions: NoteRegionDto[];
}

/** `POST /api/notes/note` — the new state plus the note that changed. */
export interface NotesNoteResponse {
  state: NotesStateDto;
  id: string;
}

/** A conclave paper's descriptor: who made it and where live editing anchors. */
export interface PaperDescriptorDto {
  paper_id: string;
  conclave_id: string;
  /** Account id of the trustee who created the paper. */
  creator: string;
  /** Account id of the DataChannel rendezvous (Phase 1: always the creator). */
  hub: string;
  title: string;
  created_ms: number;
}

/** One merged CRDT update in the paper's pending log. */
export interface PaperUpdateDto {
  rev: number;
  /** Account id of the authoring trustee. */
  origin: string;
  /** Base64 of the opaque v1 Yjs update. */
  update: string;
  at_ms: number;
}

/**
 * `GET`/`POST /api/paper/*` — one conclave's shared paper.
 *
 * `state` is the full merged document (base64 v1 Yjs update); `updates` are
 * the pending log entries after the last compaction, oldest first. `revision`
 * is gapless and never restarts, even across compaction.
 */
export interface PaperViewDto {
  paper: PaperDescriptorDto;
  revision: number;
  updated_ms: number;
  state: string;
  updates: PaperUpdateDto[];
  compacted: boolean;
}

/** One peer's live presence frame, opaque to the daemon. */
export interface PaperPresencePeerDto {
  /** Account id the frame came from. */
  account: string;
  /** Base64 of the editor's awareness update. */
  frame: string;
}

/**
 * `GET /api/paper/{id}/presence` — who else is reachable on the fast path.
 *
 * An empty list is a real answer, not an error: presence has no relay fallback
 * and no persistence, so "nobody" means nobody is on a channel right now.
 */
export interface PaperPresenceDto {
  peers: PaperPresencePeerDto[];
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
  /** TorBox's own `download_state` while caching ("queued", "downloading", …). */
  remote_state: string | null;
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

// --- Wallet (self-custody, Bitcoin and Ethereum) --------------------------------

/**
 * How the wallet reaches its chain-data source. Tor is forced on while the
 * relay is on Tor: the wallet must never be less private than the rest of Hush.
 */
export type WalletNetworkMode = "clearnet" | "tor";

/** A chain the wallet holds value on. */
export type WalletChain = "bitcoin" | "ethereum";

/**
 * One chain's state.
 *
 * Amounts are decimal strings in the chain's base unit, not numbers: one ether
 * is 1e18 wei, far past what a JSON number can carry exactly, so a number here
 * would be a rounded balance. Bitcoin's satoshi fits in a number, but it uses
 * the same shape so both chains render through one path.
 */
export interface WalletChainDto {
  chain: WalletChain;
  /** False when this daemon was built without an adapter for the chain. */
  available: boolean;
  /** The address to receive on; null before a wallet exists. */
  address: string | null;
  /** The base unit name: `"sat"` or `"wei"`. */
  unit: string;
  /** Spendable now, base units. */
  confirmed: string;
  /** Seen but not yet confirmed, base units. */
  pending: string;
}

/** An ERC-20 token the wallet tracks, with the balance from the last sync. */
export interface WalletTokenDto {
  address: string;
  symbol: string;
  decimals: number;
  /** Balance in the token's smallest unit. */
  balance: string;
}

/** `GET /api/wallet` — everything the pane renders. Never carries the phrase. */
export interface WalletStateDto {
  exists: boolean;
  /** The Bitcoin network name (`"mainnet"`, `"testnet"`, …). */
  network: string;
  /** The mode in effect: Tor whenever the relay is on Tor. */
  mode: WalletNetworkMode;
  /** True while the relay pins the wallet to Tor, so the toggle is locked on. */
  mode_locked: boolean;
  label: string | null;
  /** Unix seconds. */
  created_at: number;
  /** One entry per chain, always in the same order: Bitcoin, then Ethereum. */
  chains: WalletChainDto[];
  tokens: WalletTokenDto[];
}

/** `POST /api/wallet/create` — the phrase is returned exactly once, here. */
export interface WalletCreateDto {
  state: WalletStateDto;
  mnemonic: string;
}

/** `POST /api/wallet/address` */
export interface WalletAddressDto {
  chain: WalletChain;
  address: string;
}

/** `POST /api/wallet/send` — the broadcast txid, and Bitcoin's fee. */
export interface WalletSendDto {
  chain: WalletChain;
  txid: string;
  /** Satoshi, and only for Bitcoin: an Ethereum fee is unknown until it mines. */
  fee_sat: number | null;
}

// --- Zines (a member's own publication, and the ones they hold) --------------
//
// Phase 1 is local and offline: this vault authors one zine under its pen name
// and holds whatever else it has been given. `zine_id` is the pen name's seal
// public key, so it is the address a comment is sealed to and the identity a
// revision chain is signed under — not a random id.

/** One entry in the local index. Deliberately not the whole article: a list of
 *  five hundred bodies would be megabytes of JSON for a screen of titles. */
export interface ZineSummaryDto {
  zine_id: string;
  /** The byline shown to readers. Chosen by the author, so it can change
   *  without the address moving. */
  pen_name: string;
  title: string;
  /** Normalised (lowercased, trimmed) tags. */
  sections: string[];
  /** Unix milliseconds. */
  created_ms: number;
  updated_ms: number;
  /** Monotonic per zine, from 1. */
  revision: number;
  comments: number;
  /** A short plain-text taste of the body, for the list row. */
  preview: string;
  /** Whether this vault is the author. Only the author's pen name can publish
   *  a new revision, because only it can sign one. */
  authored: boolean;
  /** Local mute: no wire effect, the zine is still held and still counted. */
  muted: boolean;
  /** Held but not yet openable — a blob arrived before its key did. */
  unreadable: boolean;
}

/** One comment the author has merged into the thread. */
export interface ZineCommentDto {
  hash: string;
  /** Whatever the commenter signed with. This vault records what was claimed
   *  and leaves the judgement to the reader. */
  author: string;
  body: string;
  created_ms: number;
  /** When this vault merged it: a local fact, never published. */
  merged_ms: number;
}

/** A superseded revision, kept so a mistake can be undone. */
export interface ZineRevisionDto {
  revision: number;
  hash: string;
  title: string;
  body: string;
  sections: string[];
  replaced_ms: number;
}

/** The article itself, as it is stored. */
export interface ZineDescriptorDto {
  /** This post's id: a fresh id per post, never an identity. */
  zine_id: string;
  /** The author's pen name: their seal public key, hex, stable across every post
   *  they write, and the address a comment is sealed to. */
  pen_id: string;
  pen_name: string;
  title: string;
  sections: string[];
  created_ms: number;
  revision: number;
  /** The content hash of the previous revision; empty at revision 1. */
  prev_hash: string;
  /** Sanitized HTML. Never injected without a trip through `bodyToDisplayHtml`. */
  body: string;
  comments: ZineCommentDto[];
}

/** One zine in this vault: the head, its history, and the local facts about
 *  holding it. */
export interface ZineFileDto {
  descriptor: ZineDescriptorDto;
  /** Superseded revisions, newest first. */
  history: ZineRevisionDto[];
  authored: boolean;
  read_key: string | null;
  unreadable: boolean;
  muted: boolean;
  updated_ms: number;
}

/** The pen name this vault publishes under. No secret material crosses this
 *  boundary: the private half never leaves the vault. */
export interface PenNameDto {
  /** The seal public key, hex: the address a comment is sealed to. */
  id: string;
  name: string;
  /** The signing public key, hex. Provenance, published with a blob. */
  sign_pk: string;
}

/** One row of the section facet. */
export interface ZineSectionDto {
  name: string;
  count: number;
}

/** The whole pane in one response: the index, the section facet, and the pen
 *  name. Three round trips to paint one screen would be three chances to
 *  render a pane that disagrees with itself. */
export interface ZinesViewDto {
  zines: ZineSummaryDto[];
  sections: ZineSectionDto[];
  /** Absent until a byline has been chosen: publishing before then is refused
   *  with a reason rather than minting one silently. */
  pen_name: PenNameDto | null;
}

/** `GET /api/zines/{id}` */
export interface ZineViewDto {
  zine: ZineFileDto;
}

/** `POST /api/zines`. An absent `zine_id` publishes a new post; a present one
 *  revises that post, and is refused rather than treated as a create when this
 *  vault does not hold it. */
export interface ZineDraftDto {
  zine_id?: string;
  title: string;
  sections: string[];
  /** Sanitized HTML. */
  body: string;
}

/** `POST /api/zines`: the index, plus which post was written. A create mints the
 *  id, so the answer is the only way the caller learns what it just published. */
export interface ZinesWriteDto extends ZinesViewDto {
  zine_id: string;
  revision: number;
}
