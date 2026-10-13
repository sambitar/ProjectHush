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
    #[error("conclave not found")]
    ConclaveNotFound,
    #[error("a conclave needs a name and at least one trustee")]
    InvalidConclave,
    #[error("only the conclave's convener can do that")]
    NotConclaveConvener,
    #[error("this conclave has ended")]
    ConclaveEnded,
    #[error("only an ended conclave can be deleted")]
    ConclaveNotEnded,
    #[error("you are not a trustee of this conclave")]
    NotConclaveTrustee,
    #[error("no pending invite for that conclave")]
    InviteNotFound,
    #[error("that person is already a trustee of this conclave")]
    AlreadyConclaveTrustee,
    #[error("malformed drop container")]
    BadDrop,
    #[error("unsupported drop container version")]
    UnsupportedDrop,
    #[error("this drop was not sealed to this drop key")]
    DropNotForThisKey,
    #[error("not a valid drop key (expected hush-drop-pk-v1: or hush-drop-sk-v1:)")]
    InvalidDropKey,
    #[error("a drop key already exists — rotate it instead")]
    DropKeyExists,
    #[error("no drop key exists yet")]
    DropKeyMissing,
    #[error("a pen name already exists — rename it instead")]
    PenNameExists,
    #[error("no pen name exists yet")]
    PenNameMissing,
    #[error("this vault holds the most pen names it can")]
    PenNameCap,
    #[error("a public identity already exists")]
    PublicIdentityExists,
    #[error("a voice session is already open in this conclave")]
    VoiceSessionExists,
    #[error("no voice session is open in this conclave")]
    VoiceNoSession,
    #[error("the voice room is full")]
    VoiceCapReached,
    #[error("you are not in this voice session")]
    VoiceNotJoined,
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
