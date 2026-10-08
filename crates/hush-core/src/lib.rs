mod account;
mod paths;
mod backup;
mod chat_log;
pub mod config;
mod crypto;
mod device;
mod drop;
mod error;
mod invite;
mod lockfile;
mod mail;
mod mailer;
mod message;
mod payload;
pub mod prekey;
mod recovery;
mod relay_client;
mod signal;
mod signal_persist;
mod vault;
mod wire;

pub use account::{Account, Contact, MailOutcome, OutsiderOutcome, DROP_TTL_SECS, MIN_DROP_PIN_LEN};
pub use paths::{default_data_dir, default_runtime_dir, default_relay_db_path, resolve_data_dir};
pub use config::{
    ClientConfig, ConfigSource, DEFAULT_DAEMON_ADDR, DEFAULT_RELAY_URL, ENV_DAEMON_ADDR,
    ENV_DAEMON_SOCKET, ENV_RELAY_URL, SOCKET_FILE_NAME, config_path, default_socket_path,
    resolve_daemon_addr, resolve_daemon_addr_with_source, resolve_daemon_socket,
    resolve_daemon_socket_with_source, resolve_relay_url, resolve_relay_url_with_source,
};
pub use backup::{export_vault, import_vault, BACKUP_FORMAT_VERSION};
pub use device::{DeviceKeys, DEVICE_ID};
pub use drop::{
    decode_private_key as decode_drop_private_key, decode_public_key as decode_drop_public_key,
    encode_private_key as encode_drop_private_key, encode_public_key as encode_drop_public_key,
    generate_keypair as generate_drop_keypair, is_container, open as open_drop,
    open_with_key as open_drop_with_key, seal as seal_drop, seal_to_key as seal_drop_to_key,
    DropAttachment, DropKeypair, DropPackage, DROP_HEADER_LEN, DROP_PRIVATE_KEY_PREFIX,
    DROP_PUBLIC_KEY_PREFIX, DROP_V2_HEADER_LEN, MAX_DROP_BYTES,
};
pub use recovery::{generate_recovery_code, normalize_recovery_code, recovery_files_exist};
pub use chat_log::{
    ChatRecord, ChatRecordPayload, ExternalMail, MailAttachmentRecord, MailState, MeetingMessage,
    MeetingRecord,
};
pub use error::Error;
pub use invite::InvitePayload;
pub use mail::{format_address, normalize_handle, MAIL_DOMAIN, MAX_HANDLE_LEN};
pub use mailer::{notification_body, notification_body_for_key, FileMailer, Mailer, NullMailer};
pub use message::{InboxResult, ReceivedAttachment, ReceivedMessage};
pub use payload::{MailAttachment, MessagePayload, MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS};
pub use prekey::{OneTimePreKey, PreKeyRecord};
pub use relay_client::{normalize_relay_base, normalize_relay_base_with, HttpPolicy, RelayClient};
pub use vault::{BlobId, IdentityPub, Vault};

/// Queue credential derivation, shared by the client and the relay.
///
/// The relay has to recognise a read request, so the derivation cannot live on
/// the client side alone. Exposed as one function rather than the whole module:
/// the rest of `crypto` handles key material and has no business being public.
pub use crypto::queue_delivery_token;
