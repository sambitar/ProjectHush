//! Chain-agnostic value types shared by the adapters and the daemon API.
//!
//! These are the only wallet types that cross the daemon/UI boundary, so they
//! are plain serde structs with no BDK or alloy types in them.

use serde::{Deserialize, Serialize};

/// Which chain a value belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainId {
    Bitcoin,
    Ethereum,
}

impl ChainId {
    pub fn as_str(self) -> &'static str {
        match self {
            ChainId::Bitcoin => "bitcoin",
            ChainId::Ethereum => "ethereum",
        }
    }
}

/// A receive address, tagged with its chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiveAddress {
    pub chain: ChainId,
    pub address: String,
}

/// A balance in the chain's base unit (satoshi, wei) plus the display unit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainBalance {
    pub chain: ChainId,
    /// Spendable now.
    pub confirmed: u64,
    /// Seen but not yet confirmed.
    pub pending: u64,
    /// The base unit name: "sat" or "wei".
    pub unit: String,
}

/// A request to send value, as it arrives from the UI.
#[derive(Clone, Debug, Deserialize)]
pub struct SendRequest {
    pub to: String,
    /// Amount in the chain's base unit (sat/wei). Zero is rejected.
    pub amount: u64,
    /// Fee rate in sat/vB (Bitcoin). Ignored by chains that do not use one.
    #[serde(default)]
    pub fee_rate: Option<u64>,
}
