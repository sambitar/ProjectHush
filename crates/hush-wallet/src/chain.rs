//! Chain-agnostic value types shared by the adapters and the daemon API.
//!
//! These are the only wallet types that cross the daemon/UI boundary, so they
//! are plain serde structs with no BDK or alloy types in them.
//!
//! Amounts cross that boundary as decimal strings, in the chain's base unit
//! (satoshi, wei, or a token's smallest unit). Ethereum's base unit is far
//! past JavaScript's safe integer range — one ether is 1e18 wei, and a JSON
//! number would silently round in the webview — so one string form for every
//! chain keeps the two adapters and the UI speaking the same language.

use serde::{Deserialize, Serialize};

use crate::error::{Result, WalletError};

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

    /// The chain named on the wire (`"bitcoin"`, `"ethereum"`).
    pub fn from_name(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "bitcoin" | "btc" => Ok(ChainId::Bitcoin),
            "ethereum" | "eth" => Ok(ChainId::Ethereum),
            other => Err(WalletError::Chain(format!("unknown chain {other:?}"))),
        }
    }
}

/// One chain's state, as the UI renders it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainView {
    pub chain: ChainId,
    /// False when this build has no adapter for the chain, so the UI can say
    /// so instead of showing an empty balance that looks like zero money.
    pub available: bool,
    /// The address to receive on. Absent before a wallet exists.
    pub address: Option<String>,
    /// The base unit name: `"sat"` or `"wei"`.
    pub unit: String,
    /// Spendable now, in base units, as a decimal string.
    pub confirmed: String,
    /// Seen but not yet confirmed, in base units.
    pub pending: String,
}

/// An ERC-20 token the wallet tracks, with its last known balance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenView {
    /// The contract address, checksummed.
    pub address: String,
    pub symbol: String,
    /// How many decimal places the token's smallest unit has. The balance is
    /// in those smallest units.
    pub decimals: u8,
    /// Balance in the token's smallest unit, as a decimal string.
    pub balance: String,
}

/// The outcome of checking a claimed payment against the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferCheck {
    /// The transaction is not in a block yet. Retrying is the right answer.
    Pending,
    /// Mined, successful, and the claimed transfer is really there.
    Confirmed {
        /// The sender as the chain recorded it, EIP-55 checksummed.
        from: String,
    },
    /// Mined, but not the transfer that was claimed. Never retry this one.
    Refused(String),
}

/// A request to send value, as it arrives from the UI.
#[derive(Clone, Debug, Deserialize)]
pub struct SendRequest {
    pub chain: ChainId,
    pub to: String,
    /// Amount in the chain's base unit (sat/wei), as a decimal string. Zero is
    /// rejected before anything touches the network.
    pub amount: String,
    /// Fee rate in sat/vB. Bitcoin only; ignored by Ethereum, which has the
    /// fee filled by the RPC's own estimate.
    #[serde(default)]
    pub fee_rate: Option<u64>,
    /// The ERC-20 contract to send instead of the native coin. Ethereum only.
    #[serde(default)]
    pub token: Option<String>,
}

/// Parse a base-unit amount typed or sent as a decimal string.
///
/// Rejects a sign, a fraction, whitespace inside the digits, and anything that
/// would overflow the widest amount any chain here uses. Zero is accepted by
/// this function and rejected by the callers that spend.
pub fn parse_amount(value: &str) -> Result<u128> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(WalletError::Amount("the amount is empty".into()));
    }
    if !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(WalletError::Amount(format!(
            "the amount must be a whole number of base units, not {trimmed:?}"
        )));
    }
    trimmed
        .parse::<u128>()
        .map_err(|_| WalletError::Amount(format!("that amount is too large: {trimmed}")))
}

/// A base-unit amount as the string the UI receives.
pub fn amount_string(value: u128) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_names_round_trip_and_accept_the_short_forms() {
        for (name, chain) in [
            ("bitcoin", ChainId::Bitcoin),
            ("BTC", ChainId::Bitcoin),
            ("ethereum", ChainId::Ethereum),
            (" ETH ", ChainId::Ethereum),
        ] {
            assert_eq!(ChainId::from_name(name).unwrap(), chain);
        }
        assert!(ChainId::from_name("litecoin").is_err());
        assert_eq!(ChainId::Bitcoin.as_str(), "bitcoin");
        assert_eq!(ChainId::Ethereum.as_str(), "ethereum");
    }

    #[test]
    fn amounts_parse_as_whole_base_units_only() {
        assert_eq!(parse_amount("0").unwrap(), 0);
        assert_eq!(parse_amount(" 1250000 ").unwrap(), 1_250_000);
        // An ether is 1e18 wei, well past what a f64 or a u64 can hold.
        assert_eq!(parse_amount("1000000000000000000").unwrap(), 10u128.pow(18));
        assert_eq!(
            parse_amount(&u128::MAX.to_string()).unwrap(),
            u128::MAX,
            "the widest amount any chain here uses must parse"
        );
    }

    #[test]
    fn a_signed_fractional_or_overlarge_amount_is_refused() {
        for bad in ["", "   ", "-1", "+1", "1.5", "1 000", "1e18", "0x10", "abc"] {
            assert!(
                parse_amount(bad).is_err(),
                "{bad:?} must not parse as an amount"
            );
        }
        // One past the widest value there is.
        let too_big = format!("1{}", "0".repeat(39));
        assert!(parse_amount(&too_big).is_err());
    }

    #[test]
    fn a_send_request_defaults_its_optional_fields() {
        let req: SendRequest =
            serde_json::from_str(r#"{"chain":"ethereum","to":"0x0","amount":"1"}"#).unwrap();
        assert_eq!(req.chain, ChainId::Ethereum);
        assert_eq!(req.fee_rate, None);
        assert_eq!(req.token, None);
        // The chain name is required and must be one we know.
        assert!(serde_json::from_str::<SendRequest>(r#"{"to":"0x0","amount":"1"}"#).is_err());
    }

    #[test]
    fn a_chain_view_serializes_amounts_as_strings() {
        let view = ChainView {
            chain: ChainId::Ethereum,
            available: true,
            address: Some("0x9858EfFD232B4033E47d90003D41EC34EcaEda94".into()),
            unit: "wei".into(),
            confirmed: amount_string(10u128.pow(18)),
            pending: amount_string(0),
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["chain"], "ethereum");
        assert_eq!(json["confirmed"], "1000000000000000000");
        assert!(json["confirmed"].is_string());
    }
}
