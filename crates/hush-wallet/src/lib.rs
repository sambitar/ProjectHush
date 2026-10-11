//! Self-custody wallet for Hush.
//!
//! The wallet holds one BIP39 phrase and derives every chain's keys from it:
//! Bitcoin through BDK, Ethereum and its ERC-20 tokens through alloy (behind
//! the `ethereum` feature). Nothing here talks to a Hush server: the phrase is
//! generated and used locally, and chain data is fetched over Tor by default.
//!
//! The phrase is only ever held in memory while the vault is unlocked. The
//! daemon seals the whole [`WalletState`] with the vault master key before it
//! touches disk.

pub mod bitcoin;
pub mod chain;
#[cfg(feature = "ethereum")]
pub mod ethereum;
pub mod error;
pub mod net;
pub mod seed;
pub mod store;

pub use chain::{amount_string, parse_amount, ChainId, ChainView, SendRequest, TokenView};
pub use error::{Result, WalletError};
pub use net::NetworkMode;
pub use store::{BitcoinState, EthereumState, TokenEntry, WalletState, STATE_FILE, STATE_VERSION};
