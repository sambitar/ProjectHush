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
  /** Whether this device holds a vault at all. False is a first run: there is
   * nothing to unlock yet, so the UI creates a vault instead. */
  vault_present: boolean;
}

/** `POST /api/vault/create` — a vault created on this device.
 *
 * `recovery_code` is shown once and only here: the vault keeps a hash of it, so
 * no later unlock can produce it again. `null` when the core had none to give. */
export interface VaultCreateResponse {
  recovery_code: string | null;
  vault_name: string;
  relay: string;
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

/**
 * How far a trustee may reach this vault (Task 24).
 *
 * `full` is mail plus 1:1 chat and conclaves; `mail_only` is mail alone. A
 * relationship gets the narrower of the two sides' choices, so `reach` on a
 * contact is the effective value while `my_reach` is this vault's own.
 */
export type TrusteeReach = "mail_only" | "full";

/**
 * Whether a trustee relationship carries signals.
 *
 * Signals are the non-chat sideband: typed control payloads that never appear
 * in the chat or mail stream. `allow` is the default for a trustee; `deny`
 * turns the sideband off in both directions.
 */
export type SignalPolicy = "allow" | "deny";

/**
 * Whether a trustee relationship may carry payments.
 *
 * Off by default and local: when `allow`, wallet addresses are exchanged and
 * the chat offers a payment action; when `deny`, neither happens.
 */
export type PaymentPolicy = "allow" | "deny";

/**
 * Whether a trustee relationship may carry torrent offers.
 *
 * Off by default on both sides. An offer is a pointer, but accepting it makes
 * the recipient's TorBox fetch the content, so it is opt-in like payments.
 */
export type TorrentPolicy = "allow" | "deny";

/** One kind of signal body on the wire. */
export interface SignalBodyDto {
  /** `ping` is the reference kind; future kinds add their own fields. */
  kind: string;
  note?: string | null;
  [key: string]: unknown;
}

/** One accepted signal in a trustee's audit log. */
export interface SignalRecordDto {
  id: string;
  from_account: string;
  from_nick: string;
  kind: string;
  received_ms: number;
  expires_ms?: number | null;
  supported: boolean;
}

/** A signal handed to the shell by `/api/recv`. */
export interface ReceivedSignalDto {
  from_account: string;
  from_nick: string;
  id: string;
  sent_ms?: number | null;
  expires_ms?: number | null;
  body: SignalBodyDto;
}

/** One acknowledgement of a signal this vault sent. */
export interface ReceivedSignalAckDto {
  from_account: string;
  from_nick: string;
  id: string;
  status: string;
}

export interface SendSignalResponse {
  id: string;
}

/** How many trustees a wallet-address announcement reached. */
export interface WalletAnnounceResult {
  sent: number;
  failed: number;
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
  /**
   * The effective reach for this relationship: the narrower of my choice and
   * theirs. When `mail_only`, chat and conclaves are hidden because they
   * cannot work — the other side (or we) excluded them.
   */
  reach: TrusteeReach;
  /** This vault's own choice of reach. What the toggle edits. */
  my_reach: TrusteeReach;
  /** Account id this contact was vouched under ("place of …"), when set. */
  place_of?: string | null;
  /** Whether this relationship carries trustee signals. */
  signals: SignalPolicy;
  /** Whether this relationship may carry payments. */
  payments: PaymentPolicy;
  /**
   * The peer's own payment choice, as last announced. `deny` means they are not
   * accepting payments; this side cannot pay them and says so.
   */
  peer_payments: PaymentPolicy;
  /** Whether this relationship may carry torrent offers. */
  torrents: TorrentPolicy;
  /** The peer's torrent-sharing choice, as last announced. */
  peer_torrents: TorrentPolicy;
  /** The Bitcoin receive address this contact announced, if any. */
  wallet_bitcoin?: string | null;
  /** The Ethereum receive address this contact announced, if any. */
  wallet_ethereum?: string | null;
  /** When the contact last announced a wallet address, Unix ms. 0 = never. */
  wallet_updated_ms: number;
}

/** One row of Settings → My places. */
export interface PlaceDto {
  account_id: string;
  nickname: string;
  label: string;
  role: "minted_by_me" | "minted_me" | string;
  /**
   * Whether the vouch is counter-signed by the place it names. A place we
   * minted is unconfirmed until its confirmation message arrives, and cannot
   * be introduced to a contact until then.
   */
  confirmed: boolean;
  added_unix: number;
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

/** One expiring chat line. The daemon holds it only for this poll. */
export interface ExpireMessageDto {
  from: string;
  /** The sender's message id, used to key the line on both sides. */
  id: string;
  text: string;
  sent_ms: number;
  expires_ms: number;
}

/** `POST /api/send-expire` — the id and deadline of the line just sent. */
export interface SendExpireResponse {
  id: string;
  expires_ms: number;
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
        /** A payment, drawn as a system line rather than a bubble. */
        t: "payment";
        /** The transferred amount in the token's smallest unit, decimal. */
        amount: string;
        symbol: string;
        txid: string;
      }
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
  /**
   * Live chat lines that expire. Never written to either vault; the store keeps
   * them only until `expires_ms`.
   */
  expire_messages: ExpireMessageDto[];
  /** Accepted trustee signals, out of band. Never chat rows. */
  signals: ReceivedSignalDto[];
  /** Acknowledgements for signals this vault sent. */
  signal_acks: ReceivedSignalAckDto[];
  warnings: string[];
  /**
   * True when the poll wrote a chat record that is not in `messages` (today
   * only a verified incoming payment), so the UI re-reads its history.
   */
  chat_appended?: boolean;
}

export interface InviteResponse {
  invite: string;
}

export interface ChatMessage {
  id: string;
  contactNick: string;
  outgoing: boolean;
  kind: "text" | "file" | "payment";
  /**
   * Unix ms when an ephemeral message must be forgotten. Absent for ordinary
   * messages, which live in the vault.
   */
  expiresAt?: number;
  text?: string;
  /** Payment lines: base-unit amount, token symbol, and the transaction hash. */
  amount?: string;
  symbol?: string;
  txid?: string;
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
  /**
   * Until when an inbound open should ring this vault, absent (or past) when
   * there is no live ring. A daemon that predates rings omits the field, so
   * absence is the same as "no ring".
   */
  ringing_until_ms?: number | null;
}

/** `GET /api/voice/state` — every live room, keyed by conclave id. */
export interface VoiceStateDto {
  rooms: VoiceViewDto[];
}

/** One side of the voice device pair, as the daemon's thread opened it. */
export interface VoiceDeviceHealthDto {
  input_name: string | null;
  output_name: string | null;
  input_live: boolean;
  output_live: boolean;
  in_rate: number;
  in_channels: number;
  /** Peak |sample| of the latest input callback, 0.0–1.0. */
  input_level: number;
  last_error: string | null;
}

/** `GET /api/voice/devices` — what is available, selected, and in use. */
export interface VoiceDevicesDto {
  /** False when the daemon's voice runs the synthetic test tone. */
  device_mode: boolean;
  inputs: string[];
  outputs: string[];
  selected_input: string | null;
  selected_output: string | null;
  /** A live room's device thread, when a room exists. */
  session: VoiceDeviceHealthDto | null;
  /** The Settings probe's device thread, when one is open. */
  probe: VoiceDeviceHealthDto | null;
}

/** `GET /api/voice/devices/meter` — the input meter's snapshot. */
export interface VoiceMeterDto {
  input_live: boolean;
  input_name: string | null;
  level: number;
}

/** `POST /api/voice/devices/test` — the speaker test's outcome. */
export interface VoiceTestDto {
  audible: boolean;
  output_name: string | null;
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

/** Where an incoming torrent offer stands. */
export type TorrentOfferStatus = "pending" | "accepted" | "declined";

/** One torrent a trustee has offered. The content is fetched, not carried. */
export interface TorrentOfferDto {
  offer_id: string;
  from_nick: string;
  from_account: string;
  name: string;
  hash: string;
  size: number;
  /** Unix seconds. */
  received_at: number;
  status: TorrentOfferStatus;
  item_id: string | null;
}

/** `POST /api/torrents/share` — what the sender queued on the sideband. */
export interface TorrentShareDto {
  offer_id: string;
  signal_id: string;
  name: string;
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
  offers: TorrentOfferDto[];
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
  /** The Ethereum network name (`"mainnet"`, `"sepolia"`). */
  ethereum_network: string;
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

/** `POST /api/wallet/pay-trustee` — a USDC payment to a trustee. */
export interface PayTrusteeResult {
  chain: WalletChain;
  txid: string;
  fee_sat?: number | null;
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
  /** This post's public life, once it has one. Absent until the first publish,
   *  so a row can say "not published" rather than guessing. */
  public?: ZinePublicDto | null;
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

/** One zine's public life on the open social web, once it has one.
 *
 *  Absent until the first publish. Local only: the `at://` address links this
 *  private post to a public record, so it never travels inside the sealed blob.
 *  The record key is not here either — it is derived from the zine id, so it can
 *  always be recomputed and a lost sidecar cannot orphan a record. */
export interface ZinePublicDto {
  /** The account whose repo the record lives in. Already public: it is in the
   *  record's own address. */
  did: string;
  /** The `at://` address of the record, so a link and a re-publish both have
   *  somewhere to point. */
  uri: string;
  /** The content hash the PDS assigned to the last write. */
  cid: string;
  /** When it first went public, unix milliseconds. Set once and never moved: an
   *  edit is a new write at the same address, and the lexicon's `publishedAt` is
   *  the first publish rather than the latest. */
  published_at_ms: number;
  /** When this vault last wrote it, unix milliseconds. A local fact, never
   *  published. */
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
  /** This zine's public record, once it has one. */
  public: ZinePublicDto | null;
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

// --- AT Protocol: the public lane (Task 22) -----------------------------------
//
// One dialect of the open social web: the Standard.site lexicons that
// `pckt.blog`, `leaflet.pub` and `offprint.app` agreed on. The identity is a DID
// the member already owns and the credential is a revocable app password, so
// this vault is a guest and never the holder of a key. Nothing private has
// anywhere to go in a record: a title, a plaintext body, tags and a date.

/** The account this vault speaks as.
 *
 *  The app password and the token pair are deliberately absent: the webview has
 *  no use for them, and a token in a response is a token in a log. */
export interface AtprotoAccountDto {
  did: string;
  handle: string;
  /** The PDS the session was minted against. */
  pds: string;
}

/** The blog this vault's documents are filed under. */
export interface AtprotoPublicationDto {
  /** The `at://` address of the publication record. */
  uri: string;
  /** The masthead. */
  name: string;
  /** The member's own web address. The lexicon combines it with each document's
   *  path to build a reader's link; Hush neither hosts it nor can invent one. */
  url: string;
}

/** One publication this vault reads. */
export interface AtprotoFollowDto {
  /** The account whose repo is read. The stable half: a handle can move. */
  did: string;
  /** The handle at the time it was added, for display only. */
  handle: string;
  /** What to show when the handle is not known. */
  name: string;
}

/** One document in the index, without its body.
 *
 *  A page of fifty full articles would be megabytes of JSON for a screen that
 *  shows titles, so the body is one request away and the row carries a taste. */
export interface AtprotoDocumentDto {
  /** The record's `at://` address, which is also its identity in the cache. */
  uri: string;
  did: string;
  handle: string;
  title: string;
  tags: string[];
  /** The author's own string, kept verbatim for display. */
  published_at: string;
  /** The same instant as a sortable number, parsed at ingest. */
  published_secs: number;
  path: string;
  /** The author's own summary, when they wrote one. Empty otherwise. */
  description: string;
  /** The document's own page on the author's site, when it has one.
   *
   *  Standard.site lets a document be a *pointer*: a title, a description and a
   *  `path` under the publication's `url`, with the words living on the author's
   *  own site rather than in the record. Resolved at ingest, so a reader with an
   *  empty body has somewhere to send the member instead of showing a bare
   *  title. Empty when the record is not a pointer or the publication could not
   *  be read — never guessed, because a wrong link is worse than no link. */
  url: string;
  /** A plain-text taste of the body. */
  preview: string;
  /** The Bluesky thread this document was announced as, when there is one.
   *
   *  Standard.site has no comment record, so the comments on a post live on a
   *  Bluesky thread the author's client linked. Absent when no thread was
   *  linked, which is also the case for posts whose comments cannot be found. */
  bsky_post_ref?: BskyPostRefDto | null;
  /** The publication this document belongs to, as an `at://` URI. Empty when
   *  the record names none. */
  site: string;
}

/** The Bluesky post a document was announced as: where its comments are. */
export interface BskyPostRefDto {
  uri: string;
  /** Empty when the author's client linked a thread without one. */
  cid: string;
}

/** A section facet: the tag and how many cached documents carry it. */
export interface AtprotoTagDto {
  name: string;
  count: number;
}

/** The whole public pane in one response: who this vault speaks as, where its
 *  posts are filed, who it reads, and what it has read. */
export interface AtprotoViewDto {
  account?: AtprotoAccountDto | null;
  publication?: AtprotoPublicationDto | null;
  follows: AtprotoFollowDto[];
  documents: AtprotoDocumentDto[];
  tags: AtprotoTagDto[];
}

/** `POST /api/atproto/refresh`: the pane, plus what the pass did.
 *
 *  `changed` rather than `documents`, because `documents` is already the index
 *  this response carries. `failed` is reported rather than raised: one
 *  publication whose PDS is down must not cost the member every other page. */
export interface AtprotoRefreshDto extends AtprotoViewDto {
  changed: number;
  failed: string[];
}

/** `GET /api/atproto/document`: one document with its body.
 *
 *  Served from the cache, so opening a page costs no network and no PDS learns
 *  which article was opened. */
export interface AtprotoDocumentFullDto extends AtprotoDocumentDto {
  /** The body as published: plaintext, because the lexicon's field forbids
   *  markup. Render it as text, never as markup. */
  text: string;
}

/** One publication the crawl found, as its own record declares it.
 *
 *  `uri` is the publication's address and `did` is the account hosting it:
 *  following uses the `did`, which needs no resolver and therefore no account. */
export interface AtprotoExploreEntryDto {
  uri: string;
  did: string;
  handle: string;
  name: string;
  description: string;
  url: string;
  has_icon: boolean;
  /** The seed this was reached through. Every entry came from somewhere. */
  via: string;
}

/** `GET /api/atproto/explore`: the cached discovery directory.
 *
 *  `crawled_unix` is `0` before the first crawl, which is a state and not a
 *  fault. Reading this costs no network; `POST /explore/refresh` is the crawl. */
export interface AtprotoExploreDto {
  directory: AtprotoExploreEntryDto[];
  crawled_unix: number;
  /** Accounts that could not be probed. Reported rather than raised. */
  failed?: string[];
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

/** `POST /api/zines/{id}/publish`: what went out, and the pane either way. */
export interface ZinePublishViewDto extends ZineViewDto {
  zine_id: string;
  /** Whether the PDS accepted the record. `false` is not a lost article: the
   *  zine is untouched and the publish can be tried again. */
  published: boolean;
  /** Why it did not, when it did not. Empty on success. */
  publish_error: string;
  /** The record's `at://` address. Empty when nothing was written. */
  uri: string;
  cid: string;
  /** The record key. Derived from the zine id, so it is the same on every
   *  publish and a retry replaces the record rather than adding one. */
  rkey: string;
  /** The web address a reader opens. Empty when no publication is set. */
  url: string;
}
