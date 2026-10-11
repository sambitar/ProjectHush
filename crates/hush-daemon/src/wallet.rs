//! The wallet: one self-custody Bitcoin wallet per vault.
//!
//! Shape mirrors [`crate::torrents::TorrentsManager`]: the durable state is a
//! vault sidecar read and written through the account worker, loaded lazily on
//! first use and persisted only when it changed. The BDK wallet itself is held
//! in memory while the vault is unlocked and rebuilt from the phrase plus the
//! stored change set on demand.
//!
//! Two rules drive the network side:
//!
//! - **The wallet is never less private than the app.** When the relay is on
//!   Tor, the wallet is on Tor and the mode is locked on; the user may only
//!   choose clearnet when the relay is on clearnet.
//! - **All chain I/O is off the async runtime.** `electrum-client` and BDK are
//!   blocking, so every call that can touch the network runs in
//!   `spawn_blocking`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};

use hush_wallet::bitcoin::{network_from_str, BitcoinConfig, BitcoinWallet};
use hush_wallet::net::NetworkMode;
use hush_wallet::seed::parse_mnemonic;
use hush_wallet::store::{decode, BitcoinState, WalletState};

use crate::account_worker::{AccountWorker, WorkResult};

/// The vault sidecar file name. Re-exported so `account_worker` and the routes
/// all name the same file.
pub const STATE_FILE: &str = hush_wallet::STATE_FILE;

/// Default fee rate, in sat/vB, when the UI does not pass one. Low but not
/// below the common minimum relay fee; the UI sends its own when it has one.
const DEFAULT_FEE_RATE: u64 = 2;

/// The UI-facing wallet state. Never carries the phrase or the change set.
#[derive(Serialize)]
pub struct PublicState {
    pub exists: bool,
    pub network: String,
    /// The mode in effect: Tor whenever the relay is on Tor.
    pub mode: NetworkMode,
    /// True when the relay pins the wallet to Tor, so the toggle is locked on.
    pub mode_locked: bool,
    pub label: Option<String>,
    /// The first receive address, for a quick "receive" display.
    pub address: Option<String>,
    pub confirmed_sat: u64,
    pub pending_sat: u64,
    pub created_at: i64,
}

/// Returned once, at creation, so the user can write the phrase down. It is
/// never returned again by any endpoint.
#[derive(Serialize)]
pub struct CreateResult {
    pub state: PublicState,
    pub mnemonic: String,
}

#[derive(Serialize)]
pub struct AddressResult {
    pub address: String,
}

#[derive(Serialize)]
pub struct SendResult {
    pub txid: String,
    pub fee_sat: u64,
}

#[derive(Deserialize)]
pub struct CreateRequest {
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default)]
    pub mode: NetworkMode,
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    pub mnemonic: String,
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default)]
    pub mode: NetworkMode,
}

#[derive(Deserialize)]
pub struct ModeRequest {
    pub mode: NetworkMode,
}

#[derive(Deserialize)]
pub struct SendRequest {
    pub to: String,
    /// Satoshi.
    pub amount: u64,
    #[serde(default)]
    pub fee_rate: Option<u64>,
}

fn default_network() -> String {
    "mainnet".to_string()
}

/// The effective mode: Tor wins whenever the relay is on Tor.
fn effective_mode(chosen: NetworkMode, relay_tor: bool) -> NetworkMode {
    if relay_tor {
        NetworkMode::Tor
    } else {
        chosen
    }
}

/// Build a BDK wallet from stored state. Pure CPU, no I/O.
fn build_wallet(
    state: &WalletState,
    relay_tor: bool,
    socks: Option<&str>,
    electrum_override: Option<&str>,
) -> Result<Option<BitcoinWallet>, String> {
    if !state.has_wallet() {
        return Ok(None);
    }
    let phrase = state.mnemonic().map_err(|e| e.to_string())?;
    let mnemonic = parse_mnemonic(phrase).map_err(|e| e.to_string())?;
    let network = network_from_str(&state.network).map_err(|e| e.to_string())?;
    let electrum_url = electrum_override
        .map(str::to_string)
        .unwrap_or_else(|| BitcoinConfig::default_electrum_url(network).to_string());
    let config = BitcoinConfig {
        network,
        electrum_url,
        mode: effective_mode(state.mode, relay_tor),
        socks: socks.map(str::to_string),
    };
    match state.bitcoin.as_ref().and_then(|b| b.changeset.as_ref()) {
        Some(value) => {
            let changeset = BitcoinWallet::changeset_from_value(value).map_err(|e| e.to_string())?;
            BitcoinWallet::load(config, &mnemonic, Some(changeset))
                .map(Some)
                .map_err(|e| e.to_string())
        }
        None => BitcoinWallet::create(config, &mnemonic)
            .map(Some)
            .map_err(|e| e.to_string()),
    }
}

pub struct WalletManager {
    worker: AccountWorker,
    /// Kept so the manager can rebuild a wallet on a mode change without a
    /// round trip to the worker.
    #[allow(dead_code)]
    vault_dir: PathBuf,
    state: tokio::sync::Mutex<WalletState>,
    /// The unlocked BDK wallet. `None` while locked or before one exists.
    /// `StdMutex` (not tokio) because every use is inside `spawn_blocking`.
    btc: Arc<StdMutex<Option<BitcoinWallet>>>,
    loaded: AtomicBool,
    dirty: AtomicBool,
    /// Whether the relay pins the wallet to Tor.
    relay_tor: AtomicBool,
    /// The shell's Tor SOCKS proxy, when the relay is on Tor.
    socks: tokio::sync::Mutex<Option<String>>,
    /// Electrum endpoint override (`HUSH_WALLET_ELECTRUM`).
    electrum_override: Option<String>,
}

impl WalletManager {
    pub fn new(worker: AccountWorker, vault_dir: &std::path::Path) -> Self {
        Self {
            worker,
            vault_dir: vault_dir.to_path_buf(),
            state: tokio::sync::Mutex::new(WalletState::default()),
            btc: Arc::new(StdMutex::new(None)),
            loaded: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            relay_tor: AtomicBool::new(false),
            socks: tokio::sync::Mutex::new(None),
            electrum_override: std::env::var("HUSH_WALLET_ELECTRUM")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }

    /// Load the sidecar once. `Err("locked")` while the vault is locked, which
    /// the routes translate to a 401.
    async fn ensure_loaded(&self) -> Result<(), String> {
        if self.loaded.load(Ordering::SeqCst) {
            return Ok(());
        }
        match self.worker.wallet_state_get().await {
            WorkResult::ReadsState(raw) => {
                let parsed = decode(raw.as_deref()).map_err(|e| e.to_string())?;
                *self.state.lock().await = parsed;
                self.loaded.store(true, Ordering::SeqCst);
                self.rebuild_btc().await?;
                Ok(())
            }
            WorkResult::Err(m) if m == "locked" => Err("locked".into()),
            WorkResult::Err(m) => Err(m),
            _ => Err("the wallet state could not be read".into()),
        }
    }

    /// Persist the state if it changed. A locked vault clears `loaded` so the
    /// next unlock reloads from disk.
    async fn persist(&self) -> Result<(), String> {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let bytes = self.state.lock().await.encode().map_err(|e| e.to_string())?;
        match self.worker.wallet_state_put(bytes).await {
            WorkResult::Ok => Ok(()),
            WorkResult::Err(m) if m == "locked" => {
                self.loaded.store(false, Ordering::SeqCst);
                Err("locked".into())
            }
            WorkResult::Err(m) => Err(m),
            _ => Err("the wallet state could not be saved".into()),
        }
    }

    /// Rebuild the in-memory BDK wallet from the current state and mode.
    async fn rebuild_btc(&self) -> Result<(), String> {
        let state = self.state.lock().await.clone();
        let relay_tor = self.relay_tor.load(Ordering::SeqCst);
        let socks = self.socks.lock().await.clone();
        let electrum = self.electrum_override.clone();
        let btc = self.btc.clone();
        let built = tokio::task::spawn_blocking(move || {
            build_wallet(&state, relay_tor, socks.as_deref(), electrum.as_deref())
        })
        .await
        .map_err(|e| e.to_string())??;
        *btc.lock().unwrap() = built;
        Ok(())
    }

    /// Copy the live change set back into the durable state and mark it dirty.
    async fn capture_changeset(&self) -> Result<(), String> {
        let btc = self.btc.clone();
        let value = tokio::task::spawn_blocking(move || {
            let guard = btc.lock().unwrap();
            match guard.as_ref() {
                Some(w) => w.changeset().map(Some).map_err(|e| e.to_string()),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        if let Some(value) = value {
            let mut state = self.state.lock().await;
            state.bitcoin = Some(BitcoinState {
                changeset: Some(value),
            });
            drop(state);
            self.dirty.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Apply the relay's Tor state to the wallet. Called whenever the relay
    /// switch changes, so the wallet follows it (and is locked on).
    pub async fn apply_relay(&self, relay_tor: bool, socks: Option<String>) {
        self.relay_tor.store(relay_tor, Ordering::SeqCst);
        *self.socks.lock().await = socks;
        if self.loaded.load(Ordering::SeqCst) {
            // A failure here is not fatal: the next operation rebuilds.
            let _ = self.rebuild_btc().await;
        }
    }

    pub async fn public_state(&self, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let state = self.state.lock().await.clone();
        let (address, confirmed_sat, pending_sat) = {
            let guard = self.btc.lock().unwrap();
            match guard.as_ref() {
                Some(w) => {
                    let (c, p) = w.balance();
                    (w.current_receive_address(), c, p)
                }
                None => (None, 0, 0),
            }
        };
        Ok(PublicState {
            exists: state.has_wallet(),
            network: state.network.clone(),
            mode: effective_mode(state.mode, relay_tor),
            mode_locked: relay_tor,
            label: state.label.clone(),
            address,
            confirmed_sat,
            pending_sat,
            created_at: state.created_at,
        })
    }

    /// Generate a new phrase and store it. The phrase is returned exactly once.
    pub async fn create(
        &self,
        network: &str,
        mode: NetworkMode,
        relay_tor: bool,
    ) -> Result<CreateResult, String> {
        self.ensure_loaded().await?;
        // Validate before generating, so a bad network name does not leave a
        // half-created wallet behind.
        network_from_str(network).map_err(|e| e.to_string())?;
        let phrase = hush_wallet::seed::generate_mnemonic().map_err(|e| e.to_string())?;
        {
            let mut state = self.state.lock().await;
            if state.has_wallet() {
                return Err("a wallet already exists in this vault".into());
            }
            *state = WalletState::new(phrase.clone(), network, mode);
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.rebuild_btc().await?;
        self.capture_changeset().await?;
        self.persist().await?;
        let state = self.public_state(relay_tor).await?;
        Ok(CreateResult {
            state,
            mnemonic: phrase,
        })
    }

    /// Restore from an existing phrase. Refused when a wallet already exists.
    pub async fn restore(
        &self,
        phrase: &str,
        network: &str,
        mode: NetworkMode,
        relay_tor: bool,
    ) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let mnemonic = parse_mnemonic(phrase).map_err(|e| e.to_string())?;
        network_from_str(network).map_err(|e| e.to_string())?;
        {
            let mut state = self.state.lock().await;
            if state.has_wallet() {
                return Err("a wallet already exists in this vault".into());
            }
            *state = WalletState::new(mnemonic.to_string(), network, mode);
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.rebuild_btc().await?;
        self.capture_changeset().await?;
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    /// Hand out a fresh receive address.
    pub async fn receive_address(&self) -> Result<String, String> {
        self.ensure_loaded().await?;
        let btc = self.btc.clone();
        let address = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut guard = btc.lock().unwrap();
            let w = guard.as_mut().ok_or_else(|| "no wallet".to_string())?;
            w.receive_address().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.capture_changeset().await?;
        self.persist().await?;
        Ok(address)
    }

    /// Full-scan the chain and refresh the balance.
    pub async fn sync(&self, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let btc = self.btc.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mut guard = btc.lock().unwrap();
            let w = guard.as_mut().ok_or_else(|| "no wallet".to_string())?;
            w.sync().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.capture_changeset().await?;
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    /// Build, sign, and broadcast a payment.
    pub async fn send(
        &self,
        to: String,
        amount: u64,
        fee_rate: Option<u64>,
    ) -> Result<SendResult, String> {
        self.ensure_loaded().await?;
        let btc = self.btc.clone();
        let (txid, fee_sat) = tokio::task::spawn_blocking(move || -> Result<(String, u64), String> {
            let mut guard = btc.lock().unwrap();
            let w = guard.as_mut().ok_or_else(|| "no wallet".to_string())?;
            let rate = fee_rate.unwrap_or(DEFAULT_FEE_RATE);
            let (tx, fee) = w.build_send(&to, amount, rate).map_err(|e| e.to_string())?;
            let txid = w.broadcast(&tx).map_err(|e| e.to_string())?;
            Ok((txid, fee))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(SendResult { txid, fee_sat })
    }

    /// Set the wallet's network mode, honouring the relay lock.
    pub async fn set_mode(
        &self,
        mode: NetworkMode,
        relay_tor: bool,
    ) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        if relay_tor && mode != NetworkMode::Tor {
            return Err(
                "Tor is on for the relay, so the wallet stays on Tor for its own privacy".into(),
            );
        }
        {
            let mut state = self.state.lock().await;
            state.mode = mode;
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.rebuild_btc().await?;
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    /// Forget the wallet. The sealed sidecar is overwritten with an empty
    /// state, so the phrase is gone from disk.
    pub async fn wipe(&self) -> Result<(), String> {
        self.ensure_loaded().await?;
        {
            let mut state = self.state.lock().await;
            *state = WalletState::default();
        }
        *self.btc.lock().unwrap() = None;
        self.dirty.store(true, Ordering::SeqCst);
        self.persist().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_lock_forces_tor_and_only_tor() {
        // Relay on Tor: both choices resolve to Tor.
        assert_eq!(effective_mode(NetworkMode::Clearnet, true), NetworkMode::Tor);
        assert_eq!(effective_mode(NetworkMode::Tor, true), NetworkMode::Tor);
        // Relay on clearnet: the user's choice stands.
        assert_eq!(
            effective_mode(NetworkMode::Clearnet, false),
            NetworkMode::Clearnet
        );
        assert_eq!(effective_mode(NetworkMode::Tor, false), NetworkMode::Tor);
    }

    #[test]
    fn building_without_a_wallet_yields_nothing() {
        let state = WalletState::default();
        let built = build_wallet(&state, false, None, None).unwrap();
        assert!(built.is_none());
    }
}
