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

/**
 * An introduction request waiting on this vault.
 *
 * When `target` is this vault, the seeker wants our invite and consent is
 * asked; otherwise this vault is the courier being asked to relay the request.
 * The invite string itself never appears here: accepting or dismissing is a
 * decision the vault executes, not data the page handles.
 */
export interface PendingIntroRequestDto {
  conclave_id: string;
  conclave_name: string;
  /** The account that wants to meet the target. */
  seeker: string;
  seeker_nick: string;
  /** The account whose invite is wanted. */
  target: string;
  target_nick: string;
  /** The account the request travelled through. */
  via: string;
  received_ms: number;
}

/** A delivered introduction waiting on an accept or dismiss. */
export interface PendingIntroDto {
  conclave_id: string;
  conclave_name: string;
  /** The account the invite belongs to. */
  target: string;
  target_nick: string;
  /** The account who carried the invite here. */
  via: string;
  received_ms: number;
}

/** Both introduction queues in one GET. */
export interface IntroListsDto {
  requests: PendingIntroRequestDto[];
  intros: PendingIntroDto[];
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

/** `POST /api/chats/delete` — how many chat records the delete removed. */
export interface DeleteChatsResponse {
  deleted: number;
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

// --- Conclave voice (Task 23) -----------------------------------------------
// `GET`/`POST /api/voice/*` — the conclave's live voice room. Phase 1 is the
// signaling plumbing; the room state is real, the handshakes are stubs.

/** One trustee in a voice room. `muted` is advisory: capture stopped before
 * the flag was sent, so silence is the truth and this is the label. */
export interface VoiceMemberDto {
  account: string;
  muted: boolean;
}

/**
 * One conclave's voice room.
 *
 * `joined: []` with `i_am_in: false` is also the shape a successful leave
 * returns when the leave *ended* the room: the action succeeded and there is
 * nothing left to show.
 */
export interface VoiceViewDto {
  session_id: string;
  conclave_id: string;
  opener: string;
  started_ms: number;
  /** Joined trustees in join order; the opener is first. */
  joined: VoiceMemberDto[];
  i_am_in: boolean;
}

/** `GET /api/voice/state` — every live room, keyed by conclave id. */
export interface VoiceStateDto {
  rooms: VoiceViewDto[];
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
  /** Comments pulled and not yet accepted. Author-side only: the writer needs
   *  to see the pile, and a reader never has one. */
  pending: number;
  /** A short plain-text taste of the body, for the list row. */
  preview: string;
  /** Whether this vault is the author. Only the author's pen name can publish
   *  a new revision, because only it can sign one. */
  authored: boolean;
  /** Local mute: no wire effect, the zine is still held and still counted. */
  muted: boolean;
  /** Held but not yet openable — a blob arrived before its key did. */
  unreadable: boolean;
  /** This post's public life, once it has one. Absent until the first export,
   *  so a row can say "not published" rather than guessing. */
  export: ZineExportDto | null;
}

/** One comment the author has merged into the thread. */
export interface ZineCommentDto {  hash: string;
  /** Whatever the commenter signed with. This vault records what was claimed
   *  and leaves the judgement to the reader. */
  author: string;
  body: string;
  created_ms: number;
  /** When this vault merged it: a local fact, never published. */
  merged_ms: number;
}

/** A comment pulled from the lane and waiting for the author's decision.
 *
 *  Same shape as a merged one minus `merged_ms`: it is not in the thread, and
 *  accepting it is what puts it there. */
export interface ZinePendingCommentDto {
  hash: string;
  author: string;
  body: string;
  created_ms: number;
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

/** One zine's public life on the Nostr network, once it has one.
 *
 *  Absent until the first export. Local only: the `d` links this private post to
 *  a public article, so it never travels inside the sealed blob. */
export interface ZineExportDto {
  /** The NIP-23 `d` tag: the addressable id within the author's namespace.
   *  Minted on the first export and stable forever, because NIP-23 is editable
   *  by address and a new `d` would publish a second article instead of
   *  updating the first. Deliberately not the `zine_id`. */
  d: string;
  /** The public key the article is signed by, hex. Already public — it is in
   *  every event that went out — and held here so the `30023:pubkey:d` address
   *  can be shown without fetching the identity. */
  pubkey: string;
  /** When the article first went public, unix seconds. Set once. */
  published_at: number;
  /** The id of the event this vault published last, hex. */
  last_event_id: string;
  /** When this vault last published, unix milliseconds. Local fact. */
  published_ms: number;
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
  /** Pulled comments waiting for this vault's decision. Author-side only. */
  pending_comments: ZinePendingCommentDto[];
  muted: boolean;
  /** This zine's public export, once it has one. */
  export: ZineExportDto | null;
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

/** The public identity this vault publishes and comments under on the public
 *  Nostr network (neighborhoods). The public key only: the secret half never
 *  leaves the vault, because a Nostr event is signed inside it. */
export interface PublicIdentityDto {
  /** The x-only secp256k1 public key, hex. A Nostr event's `pubkey`. */
  pubkey: string;
  /** The bech32 `npub` handle. */
  npub: string;
}

/** The neighborhoods read lane's settings (Task 22). */
export interface NeighborhoodSettingsDto {
  /** The relays read from. A curated set with no Hush-run relay; user-editable. */
  allowlist: string[];
  /** The reading-time floor, in whole minutes. */
  min_minutes: number;
  /** Whether the front page shows public posts by default. Off by default,
   *  because showing them is what triggers egress. */
  default_show: boolean;
}

/** One public article in the neighborhoods index. No body: the index is a list. */
export interface NeighborhoodSummaryDto {
  /** The NIP-01 addressable id, the `a` tag: `30023:pubkey:d`. */
  address: string;
  /** The event id of the version this was mapped from. */
  event_id: string;
  pubkey: string;
  /** The `d` tag: the addressable id within the author's namespace. */
  d: string;
  title: string;
  summary: string | null;
  /** Normalised `t` tags. */
  sections: string[];
  created_at: number;
  published_at: number | null;
  /** Reading time in whole minutes, at 200 wpm. */
  reading_minutes: number;
}

/** One public article in full, with its Markdown body. */
export interface NeighborhoodArticleDto extends NeighborhoodSummaryDto {
  /** The Markdown body, as the relay sent it. Render through the one sanitizer. */
  content: string;
  /** The thread under this article, oldest first. Empty for an article whose
   *  replies this vault does not read — every article but the ones it published
   *  itself. */
  replies: NeighborhoodCommentDto[];
}

/** One public comment, as the page reads it.
 *
 *  `content` is plaintext, by the spec. Render it as text, never as markup: a
 *  comment is not Markdown, and a stranger's text is not markup here. */
export interface NeighborhoodCommentDto {
  event_id: string;
  /** The commenter's public key, hex. A key, not a name: the lane fetches no
   *  kind:0 metadata, so the page names the key it holds. */
  pubkey: string;
  content: string;
  created_at: number;
  /** The comment this answers, when it answers one rather than the article. */
  parent_id: string | null;
}

/** `POST /api/neighborhoods/{addr}/comment`: the thread as it now stands.
 *
 *  The thread rather than the one comment, because the page shows a thread and
 *  the caller should not have to splice its own words into a list it did not
 *  build. */
export interface NeighborhoodCommentViewDto {
  address: string;
  /** The id of the event that went out, hex. */
  event_id: string;
  /** Whether a relay took it. `false` means the comment is not public: unlike an
   *  export there is no address to retry, so it is not cached either. */
  published: boolean;
  /** Why the publish failed, when it did. Empty on success. */
  publish_error: string;
  replies: NeighborhoodCommentDto[];
}

/** A section facet: the tag and how many held articles carry it. */
export interface NeighborhoodSectionDto {
  name: string;
  count: number;
}

/** The neighborhoods index: what the page shows, plus its facets and settings. */
export interface NeighborhoodsViewDto {
  articles: NeighborhoodSummaryDto[];
  sections: NeighborhoodSectionDto[];
  settings: NeighborhoodSettingsDto;
}

/** A patch to the neighborhoods settings: send only what changed. */
export interface NeighborhoodSettingsPatch {
  allowlist?: string[];
  min_minutes?: number;
  default_show?: boolean;
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
  /** Sealed blobs this vault holds the bytes of, opened or not. These are what
   *  the re-post sweep keeps alive on the relay. */
  held_blobs: number;
  /** Of those, the ones no key has opened yet. Held on purpose, and a key may
   *  still arrive. */
  unreadable_blobs: number;
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
  /** Whether the sealed blob reached the relay. `false` is not a lost write:
   *  the bytes are held and the re-post sweep sends them again. */
  published: boolean;
  /** Why the publish failed, when it did. Empty on success. */
  publish_error: string;
}

/** `POST /api/zines/{id}/export`: the article's public address, and the post as
 *  it now stands. Flattened rather than nested, so the reader page can show
 *  "published" without a second request. */
export interface ZineExportViewDto extends ZineViewDto {
  zine_id: string;
  /** The NIP-23 `d` the article lives at. Stable across every re-export. */
  d: string;
  /** The NIP-01 address `30023:pubkey:d`. What a NIP-22 comment references. */
  address: string;
  /** The shareable handle, with the allowlist as relay hints. */
  naddr: string;
  /** The id of the event that was published, hex. */
  event_id: string;
  /** The public key the article is signed by, hex. */
  pubkey: string;
  /** When the article first went public, unix seconds. */
  published_at: number;
  /** Whether a relay took it. `false` is not a lost article: the address is
   *  recorded and the retry publishes the same one. */
  published: boolean;
  /** Why the publish failed, when it did. Empty on success. */
  publish_error: string;
}
