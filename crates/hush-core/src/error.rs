use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("passphrase must not be empty")]
    EmptyPassphrase,
    #[error("wrong passphrase")]
    WrongPassphrase,
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
