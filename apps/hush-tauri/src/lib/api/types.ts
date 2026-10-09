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
