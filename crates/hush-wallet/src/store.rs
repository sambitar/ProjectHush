//! The durable wallet state: the sealed phrase, the network mode, and the
//! Bitcoin sync change set.
//!
//! This whole struct is serialized to JSON and handed to the daemon, which
//! seals it with the vault master key and stores it as a sidecar
//! (`wallet-state.enc`). Nothing here is written to disk in the clear. The
//! phrase lives in the struct only while the vault is unlocked.
//!
//! The schema is versioned: a future change that cannot read an old file must
//! bump [`STATE_VERSION`] and refuse rather than silently misread.

use serde::{Deserialize, Serialize};

use crate::error::{Result, WalletError};
use crate::net::NetworkMode;

/// The vault sidecar file name the daemon reads and writes.
pub const STATE_FILE: &str = "wallet-state.enc";

/// Bumped when the on-disk shape changes incompatibly.
pub const STATE_VERSION: u32 = 1;

/// The persisted wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletState {
    pub version: u32,
    /// The BIP39 phrase. Sealed at rest; present in memory only while unlocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
    /// `"mainnet"` or `"testnet"`. Chosen at creation and not changed by a
    /// restore (a phrase works on either; the wallet is pinned to one).
    #[serde(default = "default_network")]
    pub network: String,
    /// Clearnet or Tor, as chosen in the UI (subject to the relay lock).
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    /// Bitcoin-specific state, absent until the chain is first used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitcoin: Option<BitcoinState>,
    /// Ethereum-specific state. Always present in memory; a state file written
    /// before Ethereum existed decodes to the empty default, which is a wallet
    /// with no tokens on mainnet, account 0.
    #[serde(default)]
    pub ethereum: EthereumState,
}

fn default_network() -> String {
    "mainnet".to_string()
}

/// Bitcoin state that must survive a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BitcoinState {
    /// The BDK `ChangeSet`, opaque here. Kept as a JSON value so this crate
    /// does not have to track BDK's internal schema, and so a change-set shape
    /// change fails loudly at the BDK boundary instead of corrupting the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset: Option<serde_json::Value>,
}

/// Ethereum state that must survive a restart.
///
/// There is no local chain data to keep, so this is only what the user chose:
/// which network, which account, and which tokens to watch. Balances are read
/// from the node and are never stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EthereumState {
    /// `"mainnet"` or `"sepolia"`.
    #[serde(default = "default_network")]
    pub network: String,
    /// The BIP44 account (`m/44'/60'/0'/0/{index}`) this wallet spends from.
    #[serde(default)]
    pub account_index: u32,
    /// ERC-20 tokens to track, with the metadata read from each contract once.
    #[serde(default)]
    pub tokens: Vec<TokenEntry>,
}

impl Default for EthereumState {
    fn default() -> Self {
        Self {
            network: default_network(),
            account_index: 0,
            tokens: Vec::new(),
        }
    }
}

/// One tracked ERC-20 token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenEntry {
    /// The contract address, checksummed.
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
}

impl WalletState {
    /// A brand-new wallet holding `mnemonic`, on `network`, in `mode`.
    pub fn new(mnemonic: String, network: &str, mode: NetworkMode) -> Self {
        Self {
            version: STATE_VERSION,
            mnemonic: Some(mnemonic),
            network: network.to_string(),
            mode,
            label: None,
            created_at: now_secs(),
            bitcoin: None,
            ethereum: EthereumState::default(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| WalletError::State(e.to_string()))
    }

    /// The phrase, or [`WalletError::NoWallet`] if none is stored.
    pub fn mnemonic(&self) -> Result<&str> {
        self.mnemonic
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or(WalletError::NoWallet)
    }

    pub fn has_wallet(&self) -> bool {
        self.mnemonic.as_deref().is_some_and(|s| !s.trim().is_empty())
    }
}

impl Default for WalletState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            mnemonic: None,
            network: default_network(),
            mode: NetworkMode::default(),
            label: None,
            created_at: 0,
            bitcoin: None,
            ethereum: EthereumState::default(),
        }
    }
}

/// Decode sidecar bytes. `None` (never saved) is the empty default; a corrupt
/// file is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<WalletState> {
    match bytes {
        None | Some([]) => Ok(WalletState::default()),
        Some(b) => {
            let state: WalletState =
                serde_json::from_slice(b).map_err(|e| WalletError::State(e.to_string()))?;
            if state.version > STATE_VERSION {
                return Err(WalletError::State(format!(
                    "wallet state is version {} but this build understands up to {}",
                    state.version, STATE_VERSION
                )));
            }
            Ok(state)
        }
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_the_phrase_and_mode() {
        let state = WalletState::new(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
            "testnet",
            NetworkMode::Clearnet,
        );
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.mnemonic().unwrap(), state.mnemonic().unwrap());
        assert_eq!(back.network, "testnet");
        assert_eq!(back.mode, NetworkMode::Clearnet);
        assert!(back.has_wallet());
    }

    #[test]
    fn missing_and_empty_decode_to_an_empty_wallet() {
        assert!(!decode(None).unwrap().has_wallet());
        assert!(!decode(Some(b"")).unwrap().has_wallet());
        // A fresh default is Tor, not clearnet.
        assert_eq!(decode(None).unwrap().mode, NetworkMode::Tor);
        assert!(decode(Some(b"not json")).is_err());
    }

    #[test]
    fn a_future_version_is_refused_not_misread() {
        let mut state = WalletState::new("x".into(), "mainnet", NetworkMode::Tor);
        state.version = STATE_VERSION + 1;
        let bytes = serde_json::to_vec(&state).unwrap();
        assert!(decode(Some(&bytes)).is_err());
    }

    #[test]
    fn a_state_without_a_phrase_is_no_wallet() {
        let mut state = WalletState::default();
        assert!(!state.has_wallet());
        assert!(matches!(state.mnemonic(), Err(WalletError::NoWallet)));
        state.mnemonic = Some("   ".into());
        assert!(!state.has_wallet());
    }

    #[test]
    fn a_file_written_before_ethereum_existed_decodes_to_the_empty_default() {
        // The shape a v1 file had: no `ethereum` key at all.
        let old = r#"{"version":1,"mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about","network":"mainnet","mode":"tor","created_at":1}"#;
        let state = decode(Some(old.as_bytes())).unwrap();
        assert!(state.has_wallet());
        assert_eq!(state.ethereum.network, "mainnet");
        assert_eq!(state.ethereum.account_index, 0);
        assert!(state.ethereum.tokens.is_empty());
    }

    #[test]
    fn tokens_and_the_account_index_round_trip() {
        let mut state = WalletState::new(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
            "mainnet",
            NetworkMode::Tor,
        );
        state.ethereum.network = "sepolia".into();
        state.ethereum.account_index = 2;
        state.ethereum.tokens.push(TokenEntry {
            address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
            symbol: "USDC".into(),
            decimals: 6,
        });
        let back = decode(Some(&state.encode().unwrap())).unwrap();
        assert_eq!(back.ethereum.network, "sepolia");
        assert_eq!(back.ethereum.account_index, 2);
        assert_eq!(back.ethereum.tokens.len(), 1);
        assert_eq!(back.ethereum.tokens[0].symbol, "USDC");
        assert_eq!(back.ethereum.tokens[0].decimals, 6);
    }
}
