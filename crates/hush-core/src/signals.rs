//! Trustee signals: the non-chat sideband between two contacts.
//!
//! A signal is a typed, store-and-forward payload that is never rendered as a
//! chat line or a mail message. It rides the same encrypted 1:1 session as
//! everything else, so the relay sees only ciphertext, timing and size — not the
//! kind or the body.
//!
//! The layer deliberately does not know what any particular signal *means*. It
//! owns the envelope, the per-contact policy, expiry, duplicate suppression,
//! acknowledgements and the audit log; the daemon (or a future feature) owns the
//! body. A new signal kind is a new [`SignalBody`] variant, not a new transport.
//!
//! Three rules keep the sideband from becoming a covert channel:
//!
//! - **Consent:** a contact is either `Allow`ed or `Deny`ed signals. The default
//!   is `Allow`, because a trustee relationship is already a trusted
//!   relationship; the menu can narrow it at any time.
//! - **Visibility:** every accepted signal is recorded in a bounded audit log,
//!   so "not in the chat stream" never means "not inspectable".
//! - **No remote control:** the envelope carries data, never a command. A kind
//!   that changes state (a download, a payment) must ask the user by its own
//!   rules; the signal layer only delivers it.

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// How many received signals one vault remembers for audit and deduplication.
///
/// Bounded on purpose: the log is a diagnostic and a replay guard, not a second
/// message store. A signal older than this many newer ones may be accepted
/// again, which is fine — the session already rejects replayed ciphertext.
pub const MAX_SIGNAL_LOG: usize = 256;

/// The largest encoded signal this build will send.
///
/// Well below the relay's 1 MiB blob cap. The cap is here so one feature cannot
/// accidentally turn the sideband into a file transfer; large payloads belong on
/// the file or torrent path.
pub const MAX_SIGNAL_BYTES: usize = 16 * 1024;

/// The largest note a reference `Ping` may carry.
pub const MAX_PING_NOTE_BYTES: usize = 4 * 1024;

/// The longest chain address a `WalletAddress` signal may carry.
///
/// A Bitcoin bech32 or an Ethereum hex address is well under this; the cap is
/// here so a crafted body cannot park a payload on a contact record.
pub const MAX_ADDRESS_BYTES: usize = 128;

/// The longest token symbol a `Payment` signal may carry.
pub const MAX_SYMBOL_BYTES: usize = 16;

/// An Ethereum transaction hash as this build writes and accepts it: `0x` and
/// 64 hex digits.
pub const TXID_HEX_LEN: usize = 64;

/// The largest base-unit amount a `Payment` signal may carry.
///
/// `u128::MAX` is 39 digits; anything longer cannot be an amount on a chain
/// whose base unit is a whole number.
pub const MAX_AMOUNT_DIGITS: usize = 39;

/// The largest torrent display name a `TorrentOffer` may carry.
pub const MAX_TORRENT_NAME_BYTES: usize = 512;

/// The largest offer id accepted from a peer (UUIDs fit comfortably).
pub const MAX_TORRENT_OFFER_ID_BYTES: usize = 64;

/// The shortest info hash accepted in a `TorrentOffer` (v1 base32 is 32 chars).
pub const MIN_TORRENT_HASH_BYTES: usize = 32;

/// The longest info hash accepted in a `TorrentOffer` (v2 hex is 64 chars).
pub const MAX_TORRENT_HASH_BYTES: usize = 64;

/// Whether a contact's signals are accepted and sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalPolicy {
    /// Signals are on. This is the default for a new trustee.
    Allow,
    /// Signals are off in both directions. Inbound signals are dropped without
    /// an acknowledgement, so a denied relationship produces no extra traffic.
    Deny,
}

impl Default for SignalPolicy {
    fn default() -> Self {
        Self::Allow
    }
}

impl SignalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn allows(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl<'de> Deserialize<'de> for SignalPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // An unknown value in a stored contact is read as `Allow`, never as a
        // deserialization failure: one bad field must not lock the whole vault.
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "deny" => Self::Deny,
            _ => Self::Allow,
        })
    }
}

/// Whether a trustee relationship may carry payments.
///
/// Off by default: money is opt-in on both sides. When allowed, wallet-address
/// signals are accepted and the chat offers a payment action; when denied,
/// addresses are neither sent nor stored and no payment route will run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentPolicy {
    Allow,
    Deny,
}

impl Default for PaymentPolicy {
    fn default() -> Self {
        Self::Deny
    }
}

impl PaymentPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn allows(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl<'de> Deserialize<'de> for PaymentPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "allow" => Self::Allow,
            _ => Self::Deny,
        })
    }
}

/// Whether a trustee relationship may carry torrent offers.
///
/// Off by default on both sides: a torrent offer makes the other vault fetch
/// potentially gigabytes from the torrent network, so it is opt-in exactly like
/// payments. Both sides must allow it before an offer is sent or accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TorrentPolicy {
    Allow,
    Deny,
}

impl Default for TorrentPolicy {
    fn default() -> Self {
        Self::Deny
    }
}

impl TorrentPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn allows(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl<'de> Deserialize<'de> for TorrentPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "allow" => Self::Allow,
            _ => Self::Deny,
        })
    }
}

/// The status carried by a [`crate::payload::MessagePayload::SignalAck`].
///
/// `Delivered` is the normal successful ack; the rest explain why a signal was
/// not handed to a feature. Unknown statuses from a newer peer are read as
/// [`SignalStatus::Unknown`] rather than rejecting the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalStatus {
    /// The receiver accepted the signal (it may still choose not to act on it).
    Delivered,
    /// The receiver's build does not know this signal kind.
    Unsupported,
    /// The signal's `expires_ms` had already passed on arrival.
    Expired,
    /// The receiver has signals disabled for the sender.
    Denied,
    /// The receiver tried and failed to apply it.
    Failed,
    /// A status this build does not know.
    Unknown,
}

impl SignalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Unsupported => "unsupported",
            Self::Expired => "expired",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl<'de> Deserialize<'de> for SignalStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "delivered" => Self::Delivered,
            "unsupported" => Self::Unsupported,
            "expired" => Self::Expired,
            "denied" => Self::Denied,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        })
    }
}

/// One kind of signal.
///
/// Internally tagged on the wire (`{"kind":"ping",...}`) so a future kind is a
/// new variant and an older build reads it as [`SignalBody::Unknown`] instead of
/// treating the whole message as corrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalBody {
    /// A silent attention ping: the reference kind, and the smallest proof that
    /// the sideband works end to end.
    Ping {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The sender's receive addresses, so a trustee can pay them without an
    /// out-of-band address swap.
    ///
    /// Durable: the latest announcement replaces the previous one and lives on
    /// the contact record until a new one arrives. A receiver that already has
    /// an address should warn before paying a changed one — the send is
    /// irreversible and the change is exactly what a compromised contact would
    /// use.
    WalletAddress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bitcoin: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ethereum: Option<String>,
    },
    /// A claim that a token transfer was sent to this contact.
    ///
    /// Not applied by the core: the daemon verifies the transaction against the
    /// chain before it writes a "received" line into the chat. The signal is a
    /// pointer to evidence, never the evidence itself.
    Payment {
        /// The Ethereum transaction hash, `0x` and 64 hex digits.
        txid: String,
        /// The transferred amount in the token's smallest unit, decimal.
        amount: String,
        /// The token symbol, e.g. `USDC`.
        symbol: String,
    },
    /// The sender's payment choice for this relationship.
    ///
    /// Payments are a two-sided agreement, so a change has to cross the
    /// sideband: turning them off clears the peer's stored address and blocks
    /// its payment route, not only the sender's. `allow` is the peer's own
    /// switch, never a request to change ours.
    PaymentConsent { allow: bool },
    /// An offer to download one torrent from the sender's library.
    ///
    /// A pointer, never the content: the recipient's own TorBox account fetches
    /// it. Nothing starts automatically; the recipient confirms, and a response
    /// tells the sender what they chose.
    TorrentOffer {
        offer_id: String,
        name: String,
        /// The BitTorrent info hash, hex or base32, lowercased by the sender.
        hash: String,
        /// The torrent's total size in bytes, or 0 when the sender does not know.
        #[serde(default)]
        size: u64,
    },
    /// The recipient's answer to a [`SignalBody::TorrentOffer`].
    TorrentResponse { offer_id: String, accepted: bool },
    /// The sender's torrent-sharing choice for this relationship.
    TorrentConsent { allow: bool },
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

impl SignalBody {
    /// The wire kind, for the audit log and diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ping { .. } => "ping",
            Self::WalletAddress { .. } => "wallet_address",
            Self::Payment { .. } => "payment",
            Self::PaymentConsent { .. } => "payment_consent",
            Self::TorrentOffer { .. } => "torrent_offer",
            Self::TorrentResponse { .. } => "torrent_response",
            Self::TorrentConsent { .. } => "torrent_consent",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this build can interpret the body.
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Validate a locally constructed body before it is sent.
    ///
    /// `Unknown` is never valid locally: it exists only so decoding can land
    /// somewhere honest. Each future kind adds its own checks here.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Ping { note } => {
                let len = note.as_ref().map(|n| n.len()).unwrap_or(0);
                if len > MAX_PING_NOTE_BYTES {
                    return Err(Error::SignalTooLarge(MAX_PING_NOTE_BYTES));
                }
                Ok(())
            }
            Self::WalletAddress { bitcoin, ethereum } => {
                for address in [bitcoin.as_deref(), ethereum.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
                        return Err(Error::InvalidSignal);
                    }
                }
                Ok(())
            }
            Self::Payment {
                txid,
                amount,
                symbol,
            } => {
                let rest = txid.strip_prefix("0x").unwrap_or("");
                if rest.len() != TXID_HEX_LEN || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::InvalidSignal);
                }
                if amount.is_empty()
                    || amount.len() > MAX_AMOUNT_DIGITS
                    || !amount.bytes().all(|b| b.is_ascii_digit())
                    || amount.parse::<u128>().map(|v| v == 0).unwrap_or(true)
                {
                    return Err(Error::InvalidSignal);
                }
                if symbol.is_empty()
                    || symbol.len() > MAX_SYMBOL_BYTES
                    || !symbol.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    return Err(Error::InvalidSignal);
                }
                Ok(())
            }
            Self::PaymentConsent { .. } => Ok(()),
            Self::TorrentOffer {
                offer_id,
                name,
                hash,
                size: _,
            } => {
                if !valid_offer_id(offer_id) {
                    return Err(Error::InvalidSignal);
                }
                if name.trim().is_empty()
                    || name.len() > MAX_TORRENT_NAME_BYTES
                    || name.chars().any(char::is_control)
                {
                    return Err(Error::InvalidSignal);
                }
                if hash.len() < MIN_TORRENT_HASH_BYTES
                    || hash.len() > MAX_TORRENT_HASH_BYTES
                    || !hash.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    return Err(Error::InvalidSignal);
                }
                Ok(())
            }
            Self::TorrentResponse { offer_id, .. } => {
                if !valid_offer_id(offer_id) {
                    return Err(Error::InvalidSignal);
                }
                Ok(())
            }
            Self::TorrentConsent { .. } => Ok(()),
            Self::Unknown => Err(Error::InvalidSignal),
        }
    }
}

/// One accepted signal, kept for audit and duplicate suppression.
///
/// The body is deliberately not stored: the log answers "who sent what kind,
/// when", which is what a user needs to inspect a sideband, without turning the
/// vault into a second archive of potentially sensitive payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub id: String,
    pub from_account: String,
    /// The sender's nickname as it was known when the signal arrived.
    pub from_nick: String,
    /// `body.kind()` at arrival, or `"unknown"`.
    pub kind: String,
    pub received_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    /// Whether this build understood the kind.
    pub supported: bool,
}

/// A signal delivered to the caller by `Account::recv`.
///
/// Kept out of `InboxResult::messages` for the same reason as papers and voice
/// payloads: a signal is not a chat message and must never render as one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReceivedSignal {
    pub from_account: String,
    pub from_nick: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    pub body: SignalBody,
}

/// An acknowledgement of a signal this vault sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReceivedSignalAck {
    pub from_account: String,
    pub from_nick: String,
    pub id: String,
    pub status: SignalStatus,
}

/// An offer id is a UUID-shaped, filesystem-safe token. The bounds keep a
/// crafted body from parking a long string in the vault sidecar.
fn valid_offer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TORRENT_OFFER_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Trim the audit log to [`MAX_SIGNAL_LOG`], dropping the oldest entries.
pub(crate) fn prune_signal_log(log: &mut Vec<SignalRecord>) {
    if log.len() > MAX_SIGNAL_LOG {
        let drop = log.len() - MAX_SIGNAL_LOG;
        log.drain(0..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ping_round_trips_and_names_its_kind() {
        let body = SignalBody::Ping {
            note: Some("look at this".into()),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["kind"], "ping");
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), body);
        assert_eq!(body.kind(), "ping");
        assert!(body.is_supported());
    }

    #[test]
    fn an_unknown_kind_decodes_to_unknown_instead_of_failing() {
        let body: SignalBody =
            serde_json::from_str(r#"{"kind":"something_new","note":"not a known kind"}"#)
                .unwrap();
        assert_eq!(body, SignalBody::Unknown);
        assert!(!body.is_supported());
        assert_eq!(body.kind(), "unknown");
        assert!(body.validate().is_err(), "unknown is never sent locally");
    }

    #[test]
    fn policy_and_status_read_unknown_values_without_failing() {
        assert_eq!(
            serde_json::from_str::<SignalPolicy>(r#""allow""#).unwrap(),
            SignalPolicy::Allow
        );
        assert_eq!(
            serde_json::from_str::<SignalPolicy>(r#""deny""#).unwrap(),
            SignalPolicy::Deny
        );
        // A value from a newer build must not fail the whole contact/vault.
        assert_eq!(
            serde_json::from_str::<SignalPolicy>(r#""ask""#).unwrap(),
            SignalPolicy::Allow
        );
        assert_eq!(
            serde_json::from_str::<SignalStatus>(r#""delivered""#).unwrap(),
            SignalStatus::Delivered
        );
        assert_eq!(
            serde_json::from_str::<SignalStatus>(r#""something_new""#).unwrap(),
            SignalStatus::Unknown
        );
        assert_eq!(
            serde_json::from_str::<TorrentPolicy>(r#""allow""#).unwrap(),
            TorrentPolicy::Allow
        );
        // Torrent sharing is opt-in: an unknown value is a denial, not a grant.
        assert_eq!(
            serde_json::from_str::<TorrentPolicy>(r#""something_new""#).unwrap(),
            TorrentPolicy::Deny
        );
    }

    #[test]
    fn a_ping_note_over_the_cap_is_refused_locally() {
        let body = SignalBody::Ping {
            note: Some("x".repeat(MAX_PING_NOTE_BYTES + 1)),
        };
        assert!(matches!(body.validate(), Err(Error::SignalTooLarge(_))));
    }

    #[test]
    fn a_wallet_address_round_trips_and_is_capped() {
        let body = SignalBody::WalletAddress {
            bitcoin: Some("bc1qexample".into()),
            ethereum: Some("0x9858EfFD232B4033E47d90003D41EC34EcaEda94".into()),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["kind"], "wallet_address");
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), body);
        assert_eq!(body.kind(), "wallet_address");
        body.validate().unwrap();

        let too_long = SignalBody::WalletAddress {
            bitcoin: Some("x".repeat(MAX_ADDRESS_BYTES + 1)),
            ethereum: None,
        };
        assert!(matches!(too_long.validate(), Err(Error::InvalidSignal)));

        let empty = SignalBody::WalletAddress {
            bitcoin: Some(String::new()),
            ethereum: None,
        };
        assert!(matches!(empty.validate(), Err(Error::InvalidSignal)));
    }

    #[test]
    fn a_payment_round_trips_and_refuses_a_forged_body() {
        let txid = format!("0x{}", "ab".repeat(32));
        let body = SignalBody::Payment {
            txid: txid.clone(),
            amount: "1000000".into(),
            symbol: "USDC".into(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["kind"], "payment");
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), body);
        assert_eq!(body.kind(), "payment");
        body.validate().unwrap();

        for bad in [
            SignalBody::Payment {
                txid: "0x1234".into(),
                amount: "1".into(),
                symbol: "USDC".into(),
            },
            SignalBody::Payment {
                txid: txid.clone(),
                amount: "0".into(),
                symbol: "USDC".into(),
            },
            SignalBody::Payment {
                txid: txid.clone(),
                amount: "1.5".into(),
                symbol: "USDC".into(),
            },
            SignalBody::Payment {
                txid,
                amount: "1".into(),
                symbol: "US DC".into(),
            },
        ] {
            assert!(matches!(bad.validate(), Err(Error::InvalidSignal)));
        }
    }

    #[test]
    fn a_payment_consent_round_trips() {
        let body = SignalBody::PaymentConsent { allow: false };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["kind"], "payment_consent");
        assert_eq!(json["allow"], false);
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), body);
        assert_eq!(body.kind(), "payment_consent");
        body.validate().unwrap();
    }

    #[test]
    fn a_torrent_offer_round_trips_and_refuses_a_forged_body() {
        let hash = "ab".repeat(20);
        let body = SignalBody::TorrentOffer {
            offer_id: "2f1c6a50-6d7d-4b7d-9d8b-8a0f1f3c2e11".into(),
            name: "A film".into(),
            hash: hash.clone(),
            size: 4_000_000_000,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["kind"], "torrent_offer");
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), body);
        assert_eq!(body.kind(), "torrent_offer");
        body.validate().unwrap();

        for bad in [
            SignalBody::TorrentOffer {
                offer_id: String::new(),
                name: "A film".into(),
                hash: hash.clone(),
                size: 1,
            },
            SignalBody::TorrentOffer {
                offer_id: "offer".into(),
                name: "  ".into(),
                hash: hash.clone(),
                size: 1,
            },
            SignalBody::TorrentOffer {
                offer_id: "offer".into(),
                name: "A film".into(),
                hash: "not-a-hash".into(),
                size: 1,
            },
            SignalBody::TorrentOffer {
                offer_id: "offer".into(),
                name: "A film".into(),
                hash: "a&b".repeat(20),
                size: 1,
            },
        ] {
            assert!(matches!(bad.validate(), Err(Error::InvalidSignal)));
        }
    }

    #[test]
    fn a_torrent_response_and_consent_round_trip() {
        let response = SignalBody::TorrentResponse {
            offer_id: "offer-1".into(),
            accepted: true,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["kind"], "torrent_response");
        assert_eq!(
            serde_json::from_value::<SignalBody>(json).unwrap(),
            response
        );
        response.validate().unwrap();

        let consent = SignalBody::TorrentConsent { allow: false };
        let json = serde_json::to_value(&consent).unwrap();
        assert_eq!(json["kind"], "torrent_consent");
        assert_eq!(json["allow"], false);
        assert_eq!(serde_json::from_value::<SignalBody>(json).unwrap(), consent);
        consent.validate().unwrap();
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let mut log: Vec<SignalRecord> = (0..MAX_SIGNAL_LOG + 5)
            .map(|i| SignalRecord {
                id: i.to_string(),
                from_account: "a".into(),
                from_nick: "alice".into(),
                kind: "ping".into(),
                received_ms: i as u64,
                expires_ms: None,
                supported: true,
            })
            .collect();
        prune_signal_log(&mut log);
        assert_eq!(log.len(), MAX_SIGNAL_LOG);
        assert_eq!(log.first().unwrap().id, "5");
    }
}
