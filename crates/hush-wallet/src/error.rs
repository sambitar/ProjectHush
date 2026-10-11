//! Errors for the wallet crate.
//!
//! One error type for the whole crate keeps the daemon's mapping to HTTP
//! responses simple; the variants carry enough context to say what actually
//! went wrong without leaking key material.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, WalletError>;

#[derive(Debug, Error)]
pub enum WalletError {
    /// No wallet has been created or restored in this vault yet.
    #[error("no wallet exists yet")]
    NoWallet,

    /// The vault is locked, so the sealed seed cannot be read.
    #[error("the wallet is locked")]
    Locked,

    /// The supplied BIP39 phrase is not a valid mnemonic.
    #[error("that recovery phrase is not valid: {0}")]
    Mnemonic(String),

    /// A Bitcoin-layer failure (descriptor, signing, transaction build).
    #[error("bitcoin: {0}")]
    Bitcoin(String),

    /// The electrum server could not be reached or refused the request.
    #[error("electrum: {0}")]
    Electrum(String),

    /// An Ethereum-layer failure (RPC, signing, transaction build).
    #[error("ethereum: {0}")]
    Ethereum(String),

    /// The chain named in a request is not one this wallet knows.
    #[error("chain: {0}")]
    Chain(String),

    /// The destination address did not parse, or is for the wrong network.
    #[error("address: {0}")]
    Address(String),

    /// The amount was zero, negative, or not a number.
    #[error("amount: {0}")]
    Amount(String),

    /// The persisted wallet state could not be encoded or decoded.
    #[error("wallet state: {0}")]
    State(String),

    /// A network-mode or proxy setting was malformed.
    #[error("network: {0}")]
    Network(String),

    /// The requested operation is not available in this build or for this chain.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
