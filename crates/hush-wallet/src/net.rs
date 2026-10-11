//! Network routing for the wallet: clearnet or Tor.
//!
//! The wallet follows the app's privacy posture. When Hush's relay path is on
//! Tor, the wallet is on Tor too and the toggle is locked on — the wallet must
//! never be *less* private than the rest of the app. When the relay is on
//! clearnet, the user may choose.
//!
//! Tor routing is a SOCKS5 proxy the desktop shell already runs for the relay.
//! The daemon hands the wallet the same `socks5h://host:port` string it hands
//! the relay client, and this module turns it into the `Socks5Config` an
//! electrum client expects.

use serde::{Deserialize, Serialize};

use crate::error::{Result, WalletError};

/// How the wallet reaches its chain data source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// Direct connection. The chain-data provider sees the user's IP.
    Clearnet,
    /// Everything routed through the shell's Tor SOCKS proxy.
    Tor,
}

impl NetworkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkMode::Clearnet => "clearnet",
            NetworkMode::Tor => "tor",
        }
    }
}

impl Default for NetworkMode {
    fn default() -> Self {
        // Fail private: an unset mode is Tor, not clearnet.
        NetworkMode::Tor
    }
}

/// Parse a `socks5[h]://host:port` proxy string into the `host:port` form an
/// electrum `Socks5Config` wants.
///
/// Accepts the same shapes the daemon already validates for the relay
/// (`hush_core::validate_socks_proxy`), so the two never disagree about what a
/// usable proxy is.
pub fn socks_addr_from_proxy(proxy: &str) -> Result<String> {
    let trimmed = proxy.trim();
    let rest = trimmed
        .strip_prefix("socks5h://")
        .or_else(|| trimmed.strip_prefix("socks5://"))
        .ok_or_else(|| {
            WalletError::Network(format!("not a SOCKS5 proxy address: {proxy}"))
        })?;
    // Reject anything with a path or query: a proxy address is just host:port.
    if rest.is_empty() || rest.contains('/') || rest.contains('?') || rest.contains('@') {
        return Err(WalletError::Network(format!(
            "not a usable SOCKS5 proxy address: {proxy}"
        )));
    }
    let (host, port) = rest.rsplit_once(':').ok_or_else(|| {
        WalletError::Network(format!("SOCKS5 proxy has no port: {proxy}"))
    })?;
    if host.is_empty() || port.parse::<u16>().is_err() {
        return Err(WalletError::Network(format!(
            "SOCKS5 proxy host or port is malformed: {proxy}"
        )));
    }
    Ok(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_socks_urls_the_daemon_hands_over() {
        assert_eq!(
            socks_addr_from_proxy("socks5h://127.0.0.1:9050").unwrap(),
            "127.0.0.1:9050"
        );
        assert_eq!(
            socks_addr_from_proxy("socks5://127.0.0.1:9150").unwrap(),
            "127.0.0.1:9150"
        );
        // Surrounding whitespace is tolerated; a bad port is not.
        assert_eq!(
            socks_addr_from_proxy("  socks5h://127.0.0.1:9050  ").unwrap(),
            "127.0.0.1:9050"
        );
        assert!(socks_addr_from_proxy("socks5h://127.0.0.1:notaport").is_err());
        assert!(socks_addr_from_proxy("socks5h://127.0.0.1").is_err());
        assert!(socks_addr_from_proxy("http://127.0.0.1:9050").is_err());
        assert!(socks_addr_from_proxy("127.0.0.1:9050").is_err());
    }

    #[test]
    fn the_default_mode_fails_private() {
        assert_eq!(NetworkMode::default(), NetworkMode::Tor);
    }
}
