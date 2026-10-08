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
  /** Fixed mail domain suffix, so the UI never hardcodes it. */
  mail_domain: string;
}

export interface ContactDto {
  nickname: string;
  protocol_name: string;
  inbound_ready: boolean;
  online: boolean;
  /** This contact's mail handle (local part only), if they published one. */
  mail_handle?: string | null;
}

export interface MailAttachmentDto {
  name: string;
  size: number;
  blob_id: string;
}

/**
 * A drop: a message sent to an address with no Hush account.
 *
 * There is no contact and no session, so the record carries the typed address
 * and the relay capability the pickup link is built from. The link itself is
 * composed by the daemon, which knows the deployment's base URL.
 */
export interface ExternalMailDto {
  to: string;
  delivery_id: string;
  expires_unix: number;
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
  /** Meeting only: the id the message's meeting is keyed on. */
  meeting_id?: string | null;
  /** Meeting only: the meeting's name, for materialising it on first sight. */
  meeting_name?: string | null;
  /** Meeting only: the sender's member list, for materialising it on first sight. */
  meeting_members?: string[];
}

/** One message in a meeting's own log. */
export interface MeetingMessageDto {
  id: string;
  outgoing: boolean;
  /** The sender's nickname as this vault knows it; absent for our own messages. */
  from?: string | null;
  body: string;
  ts_ms: number;
}

/**
 * A meeting: a local record in the vault, not a relay-side room.
 *
 * Group chat is sender-side fan-out over the members' 1:1 sessions, and each
 * message carries this descriptor so a recipient can materialise the meeting.
 * The member list is the creator's local view and is not shared.
 */
export interface MeetingDto {
  id: string;
  name: string;
  members: string[];
  created_ms: number;
  messages: MeetingMessageDto[];
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
        /** Set when this went to a non-member address instead of a contact. */
        external?: ExternalMailDto | null;
      }
    | {
        t: "draft";
        thread_id?: string | null;
        to: string;
        subject?: string | null;
        body: string;
        attachments: { name: string; blob_id_hex: string; size: number }[];
        updated_ms: number;
      };
}

/** `GET /api/mail`: messages and drafts together, so the two cannot disagree. */
export interface MailboxDto {
  mail: ChatRecordDto[];
  drafts: ChatRecordDto[];
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
