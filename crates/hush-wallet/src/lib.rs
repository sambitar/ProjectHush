//! Self-custody wallet for Hush.
//!
//! The wallet holds one BIP39 phrase and derives every chain's keys from it.
//! Bitcoin comes first, through BDK; Ethereum and its tokens are a later,
//! feature-gated addition. Nothing here talks to a Hush server: the phrase is
//! generated and used locally, and chain data is fetched over Tor by default.
//!
//! The phrase is only ever held in memory while the vault is unlocked. The
//! daemon seals the whole [`WalletState`] with the vault master key before it
//! touches disk.

pub mod bitcoin;
pub mod chain;
pub mod error;
pub mod net;
pub mod seed;
pub mod store;

pub use chain::{ChainBalance, ChainId, ReceiveAddress, SendRequest};
pub use error::{Result, WalletError};
pub use net::NetworkMode;
pub use store::{BitcoinState, WalletState, STATE_FILE, STATE_VERSION};
