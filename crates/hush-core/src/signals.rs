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
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

impl SignalBody {
    /// The wire kind, for the audit log and diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ping { .. } => "ping",
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
            serde_json::from_str(r#"{"kind":"torrent_offer","magnet":"magnet:?xt=urn:btih:x"}"#)
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
    }

    #[test]
    fn a_ping_note_over_the_cap_is_refused_locally() {
        let body = SignalBody::Ping {
            note: Some("x".repeat(MAX_PING_NOTE_BYTES + 1)),
        };
        assert!(matches!(body.validate(), Err(Error::SignalTooLarge(_))));
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
