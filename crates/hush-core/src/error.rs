use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("passphrase must not be empty")]
    EmptyPassphrase,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("wrong recovery code")]
    WrongRecoveryCode,
    #[error("invalid backup archive")]
    InvalidBackup,
    #[error("vault already exists")]
    AlreadyExists,
    #[error("vault is missing or incomplete")]
    NotFound,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("crypto failed")]
    Crypto,
    #[error("blob not found")]
    BlobNotFound,
    #[error("corrupt vault metadata")]
    Corrupt,
    #[error("relay error: {0}")]
    Relay(String),
    #[error("signal session error")]
    Session,
    #[error("contact not found")]
    ContactNotFound,
    #[error("contact is offline — chat is live only; use mail when available")]
    ContactOffline,
    #[error("contact already exists")]
    ContactExists,
    #[error("unsupported invite version")]
    UnsupportedInvite,
    #[error("passphrase required: use --passphrase or set HUSH_PASSPHRASE")]
    PassphraseRequired,
    #[error("file too large (max {0} bytes)")]
    FileTooLarge(usize),
    #[error("invite relay {0} does not match this vault relay {1}")]
    RelayMismatch(String, String),
    #[error("handle must be 1-64 characters of a-z, 0-9, dot, underscore or dash")]
    InvalidHandle,
    #[error("too many attachments (max {0})")]
    TooManyAttachments(usize),
}

impl From<argon2::Error> for Error {
    fn from(_: argon2::Error) -> Self {
        Error::Crypto
    }
}

impl From<argon2::password_hash::Error> for Error {
    fn from(_: argon2::password_hash::Error) -> Self {
        Error::Crypto
    }
}
