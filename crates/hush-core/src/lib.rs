mod account;
mod paths;
mod backup;
mod chat_log;
pub mod config;
mod crypto;
mod device;
pub mod device_list;
mod error;
mod invite;
mod lockfile;
mod message;
mod payload;
pub mod prekey;
mod recovery;
mod relay_client;
mod signal;
mod signal_persist;
mod vault;
mod wire;

pub use account::{Account, Contact, ContactDevice};
pub use paths::{default_data_dir, default_runtime_dir, default_relay_db_path, resolve_data_dir};
pub use config::{
    ClientConfig, ConfigSource, DEFAULT_DAEMON_ADDR, DEFAULT_RELAY_URL, ENV_DAEMON_ADDR,
    ENV_DAEMON_SOCKET, ENV_RELAY_URL, SOCKET_FILE_NAME, config_path, default_socket_path,
    resolve_daemon_addr, resolve_daemon_addr_with_source, resolve_daemon_socket,
    resolve_daemon_socket_with_source, resolve_relay_url, resolve_relay_url_with_source,
};
pub use backup::{export_vault, import_vault, BACKUP_FORMAT_VERSION};
pub use device::DeviceKeys;
pub use device_list::{DeviceList, DeviceRecord};
pub use recovery::{generate_recovery_code, normalize_recovery_code, recovery_files_exist};
pub use chat_log::{ChatRecord, ChatRecordPayload};
pub use error::Error;
pub use invite::InvitePayload;
pub use message::{InboxResult, ReceivedMessage};
pub use payload::{MessagePayload, MAX_INLINE_FILE_BYTES};
pub use prekey::{OneTimePreKey, PreKeyRecord};
pub use relay_client::{normalize_relay_base, normalize_relay_base_with, HttpPolicy, RelayClient};
pub use vault::{BlobId, IdentityPub, Vault};

/// Queue credential derivation, shared by the client and the relay.
///
/// The relay has to recognise a read request, so the derivation cannot live on
/// the client side alone. Exposed as one function rather than the whole module:
/// the rest of `crypto` handles key material and has no business being public.
pub use crypto::queue_delivery_token;
