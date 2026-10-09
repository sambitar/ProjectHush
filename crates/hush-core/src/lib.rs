mod account;
mod paths;
mod backup;
mod chat_log;
pub mod config;
mod crypto;
mod device;
mod error;
mod invite;
mod lockfile;
mod mail;
mod message;
mod payload;
pub mod prekey;
mod recovery;
mod relay_client;
mod signal;
mod signal_persist;
mod vault;
mod wire;

pub use account::{Account, Contact, MailOutcome, PendingInvite};
pub use paths::{default_data_dir, default_runtime_dir, default_relay_db_path, resolve_data_dir};
pub use config::{
    ClientConfig, ConfigSource, DEFAULT_DAEMON_ADDR, DEFAULT_RELAY_URL, ENV_DAEMON_ADDR,
    ENV_DAEMON_SOCKET, ENV_RELAY_URL, SOCKET_FILE_NAME, config_path, default_socket_path,
    resolve_daemon_addr, resolve_daemon_addr_with_source, resolve_daemon_socket,
    resolve_daemon_socket_with_source, resolve_relay_url, resolve_relay_url_with_source,
};
pub use backup::{export_vault, import_vault, BACKUP_FORMAT_VERSION};
pub use device::{DeviceKeys, DEVICE_ID};
pub use recovery::{generate_recovery_code, normalize_recovery_code, recovery_files_exist};
pub use chat_log::{
    ChatRecord, ChatRecordPayload, ConclaveMessage, ConclaveRecord, MailAttachmentRecord,
    MailState,
};
pub use error::Error;
pub use invite::InvitePayload;
pub use mail::{
    format_address, normalize_handle, normalize_vault_name, DEFAULT_VAULT_NAME, MAX_HANDLE_LEN,
};
pub use message::{InboxResult, ReceivedAttachment, ReceivedMessage};
pub use payload::{
    ConclaveDescriptor, ConclaveTrustee, MessagePayload, TrusteeState, MailAttachment,
    MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS,
};
pub use prekey::{OneTimePreKey, PreKeyRecord};
pub use relay_client::{normalize_relay_base, normalize_relay_base_with, HttpPolicy, RelayClient};
pub use vault::{BlobId, IdentityPub, Vault};

/// Queue credential derivation, shared by the client and the relay.
///
/// The relay has to recognise a read request, so the derivation cannot live on
/// the client side alone. Exposed as one function rather than the whole module:
/// the rest of `crypto` handles key material and has no business being public.
pub use crypto::queue_delivery_token;
