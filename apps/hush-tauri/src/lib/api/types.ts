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
      };
}

export interface RecvResponse {
  messages: ReceivedDto[];
  warnings: string[];
}

export interface InviteResponse {
  invite: string;
}

export interface LinkDeviceStartResponse {
  link_id: string;
  expires_unix: number;
  payload: string;
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
