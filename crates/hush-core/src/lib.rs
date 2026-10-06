mod account;
mod paths;
mod backup;
mod chat_log;
pub mod config;
mod crypto;
mod error;
mod invite;
mod lockfile;
mod message;
mod payload;
mod recovery;
mod relay_client;
mod signal;
mod signal_persist;
mod vault;
mod wire;

pub use account::{Account, Contact};
pub use paths::{default_data_dir, default_runtime_dir, default_relay_db_path, resolve_data_dir};
pub use config::{
    ClientConfig, ConfigSource, DEFAULT_DAEMON_ADDR, DEFAULT_RELAY_URL, ENV_DAEMON_ADDR,
    ENV_RELAY_URL, config_path, resolve_daemon_addr, resolve_daemon_addr_with_source,
    resolve_relay_url, resolve_relay_url_with_source,
};
pub use backup::{export_vault, import_vault, BACKUP_FORMAT_VERSION};
pub use recovery::{generate_recovery_code, normalize_recovery_code, recovery_files_exist};
pub use chat_log::{ChatRecord, ChatRecordPayload};
pub use error::Error;
pub use invite::InvitePayload;
pub use message::{InboxResult, ReceivedMessage};
pub use payload::{MessagePayload, MAX_INLINE_FILE_BYTES};
pub use relay_client::{normalize_relay_base, RelayClient};
pub use vault::{BlobId, IdentityPub, Vault};
