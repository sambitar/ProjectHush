//! The wallet: one self-custody wallet per vault, across two chains.
//!
//! Shape mirrors [`crate::torrents::TorrentsManager`]: the durable state is a
//! vault sidecar read and written through the account worker, loaded lazily on
//! first use and persisted only when it changed. One BIP39 phrase backs both
//! chains, so a wallet is created, restored, and wiped as a whole.
//!
//! - Bitcoin keeps a BDK wallet in memory, rebuilt from the phrase plus the
//!   stored change set, and holds its balance locally after a scan.
//! - Ethereum keeps a provider and one BIP44 account. There is no local chain
//!   data, so its balance is whatever the last sync read, cached here.
//!
//! Two rules drive the network side:
//!
//! - **The wallet is never less private than the app.** When the relay is on
//!   Tor, the wallet is on Tor and the mode is locked on; the user may only
//!   choose clearnet when the relay is on clearnet.
//! - **All Bitcoin chain I/O is off the async runtime.** `electrum-client` and
//!   BDK are blocking, so every call that can touch the network runs in
//!   `spawn_blocking`. The Ethereum side is async already and is awaited
//!   directly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};

use hush_wallet::bitcoin::{network_from_str, BitcoinConfig, BitcoinWallet};
use hush_wallet::chain::{amount_string, ChainId, ChainView, TokenView};
use hush_wallet::net::NetworkMode;
use hush_wallet::seed::parse_mnemonic;
use hush_wallet::store::{decode, BitcoinState, EthereumState, TokenEntry, WalletState};

use crate::account_worker::{AccountWorker, WorkResult};

/// The vault sidecar file name. Re-exported so `account_worker` and the routes
/// all name the same file.
pub const STATE_FILE: &str = hush_wallet::STATE_FILE;

/// Default fee rate, in sat/vB, when the UI does not pass one. Low but not
/// below the common minimum relay fee; the UI sends its own when it has one.
const DEFAULT_FEE_RATE: u64 = 2;

/// How many ERC-20 tokens one wallet may track. A cap keeps the per-sync call
/// count, and the state file, from growing without bound.
const MAX_TOKENS: usize = 50;

/// The UI-facing wallet state. Never carries the phrase or the change set.
#[derive(Serialize)]
pub struct PublicState {
    pub exists: bool,
    /// The Bitcoin network name (`"mainnet"`, `"testnet"`, …).
    pub network: String,
    /// The mode in effect: Tor whenever the relay is on Tor.
    pub mode: NetworkMode,
    /// True when the relay pins the wallet to Tor, so the toggle is locked on.
    pub mode_locked: bool,
    pub label: Option<String>,
    pub created_at: i64,
    /// One entry per chain, always in the same order: Bitcoin, then Ethereum.
    pub chains: Vec<ChainView>,
    /// Tracked ERC-20 tokens, with the balances from the last sync.
    pub tokens: Vec<TokenView>,
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
    pub chain: ChainId,
    pub address: String,
}

#[derive(Serialize)]
pub struct SendResult {
    pub chain: ChainId,
    pub txid: String,
    /// Bitcoin only: the fee the signed transaction pays, in satoshi.
    /// Ethereum's fee is not known until the transaction is mined.
    pub fee_sat: Option<u64>,
}

#[derive(Deserialize)]
pub struct CreateRequest {
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default = "default_network")]
    pub ethereum_network: String,
    #[serde(default)]
    pub mode: NetworkMode,
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    pub mnemonic: String,
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default = "default_network")]
    pub ethereum_network: String,
    #[serde(default)]
    pub mode: NetworkMode,
}

#[derive(Deserialize)]
pub struct ModeRequest {
    pub mode: NetworkMode,
}

/// The shell reporting the SOCKS proxy its Tor is listening on, for the wallet.
/// `null` means Tor is not running, which is not an error: the wallet keeps its
/// mode and refuses to reach the chain until a proxy is reported.
#[derive(Deserialize)]
pub struct ProxyRequest {
    #[serde(default)]
    pub socks: Option<String>,
}

#[derive(Deserialize)]
pub struct ChainRequest {
    /// Which chain to act on. Absent means Bitcoin, the chain that existed
    /// before the wallet was multi-chain.
    #[serde(default = "default_chain")]
    pub chain: ChainId,
}

#[derive(Deserialize)]
pub struct TokenRequest {
    /// The ERC-20 contract address.
    pub address: String,
}

fn default_network() -> String {
    "mainnet".to_string()
}

fn default_chain() -> ChainId {
    ChainId::Bitcoin
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

/// Build the Ethereum account from stored state. Pure CPU, no I/O.
#[cfg(feature = "ethereum")]
fn build_eth(
    state: &WalletState,
    relay_tor: bool,
    socks: Option<&str>,
    rpc_override: Option<&str>,
) -> Result<Option<hush_wallet::ethereum::EthereumWallet>, String> {
    if !state.has_wallet() {
        return Ok(None);
    }
    let phrase = state.mnemonic().map_err(|e| e.to_string())?;
    let mnemonic = parse_mnemonic(phrase).map_err(|e| e.to_string())?;
    let mut config = hush_wallet::ethereum::EthereumConfig::for_network(
        &state.ethereum.network,
        effective_mode(state.mode, relay_tor),
        socks.map(str::to_string),
        state.ethereum.account_index,
    )
    .map_err(|e| e.to_string())?;
    if let Some(url) = rpc_override {
        config.rpc_url = url.to_string();
    }
    hush_wallet::ethereum::EthereumWallet::create(&config, &mnemonic)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Without the `ethereum` feature there is no adapter to build. Kept so the
/// rest of the manager reads the same either way.
#[cfg(not(feature = "ethereum"))]
fn build_eth(
    _state: &WalletState,
    _relay_tor: bool,
    _socks: Option<&str>,
    _rpc_override: Option<&str>,
) -> Result<Option<()>, String> {
    Ok(None)
}

/// Validate an Ethereum network name before it is stored.
#[cfg(feature = "ethereum")]
fn validate_eth_network(name: &str) -> Result<(), String> {
    hush_wallet::ethereum::network(name)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Without the adapter there is no endpoint to point at a network, so the name
/// is stored as given and nothing reads it.
#[cfg(not(feature = "ethereum"))]
fn validate_eth_network(_name: &str) -> Result<(), String> {
    Ok(())
}

/// What the last Ethereum sync read: the native balance and each token's.
///
/// Nothing here is persisted. A balance is a fact about the chain, not about
/// this wallet, and storing a stale one in the vault would only make the UI
/// show money that may not be there.
#[derive(Default)]
struct EthSnapshot {
    /// Native balance in wei.
    wei: u128,
    /// Per-token balances, by checksummed contract address.
    tokens: Vec<(String, u128)>,
}

impl EthSnapshot {
    fn token(&self, address: &str) -> u128 {
        self.tokens
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(address))
            .map(|(_, b)| *b)
            .unwrap_or(0)
    }

    fn set_token(&mut self, address: &str, balance: u128) {
        match self
            .tokens
            .iter_mut()
            .find(|(a, _)| a.eq_ignore_ascii_case(address))
        {
            Some(slot) => slot.1 = balance,
            None => self.tokens.push((address.to_string(), balance)),
        }
    }

    fn forget_token(&mut self, address: &str) {
        self.tokens.retain(|(a, _)| !a.eq_ignore_ascii_case(address));
    }

    fn forget_all(&mut self) {
        self.wei = 0;
        self.tokens.clear();
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
    /// Why the Bitcoin wallet is not available, when it is not.
    btc_error: StdMutex<Option<String>>,
    /// The unlocked Ethereum account. Cloned out of the lock for each call:
    /// the clone shares the provider, and holding a lock across an await would
    /// serialise every RPC call behind the slowest one.
    eth: StdMutex<Option<EthHandle>>,
    /// Why the Ethereum account is not available, when it is not.
    eth_error: StdMutex<Option<String>>,
    /// The last Ethereum balances read, for rendering without a network call.
    eth_balances: StdMutex<EthSnapshot>,
    loaded: AtomicBool,
    dirty: AtomicBool,
    /// Whether the relay pins the wallet to Tor.
    relay_tor: AtomicBool,
    /// The shell's Tor SOCKS proxy, when the relay is on Tor.
    socks: tokio::sync::Mutex<Option<String>>,
    /// Electrum endpoint override (`HUSH_WALLET_ELECTRUM`).
    electrum_override: Option<String>,
    /// Ethereum RPC endpoint override (`HUSH_WALLET_ETH_RPC`).
    eth_rpc_override: Option<String>,
}

/// The Ethereum account, or nothing when this build has no adapter.
#[cfg(feature = "ethereum")]
type EthHandle = hush_wallet::ethereum::EthereumWallet;
#[cfg(not(feature = "ethereum"))]
type EthHandle = ();

impl WalletManager {
    pub fn new(worker: AccountWorker, vault_dir: &std::path::Path) -> Self {
        Self {
            worker,
            vault_dir: vault_dir.to_path_buf(),
            state: tokio::sync::Mutex::new(WalletState::default()),
            btc: Arc::new(StdMutex::new(None)),
            btc_error: StdMutex::new(None),
            eth: StdMutex::new(None),
            eth_error: StdMutex::new(None),
            eth_balances: StdMutex::new(EthSnapshot::default()),
            loaded: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            relay_tor: AtomicBool::new(false),
            socks: tokio::sync::Mutex::new(None),
            electrum_override: std::env::var("HUSH_WALLET_ELECTRUM")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            eth_rpc_override: std::env::var("HUSH_WALLET_ETH_RPC")
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
                self.rebuild().await?;
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

    /// Rebuild both in-memory wallets from the current state and mode.
    async fn rebuild(&self) -> Result<(), String> {
        self.rebuild_btc().await?;
        // An Ethereum failure is recorded and surfaced by the operations that
        // need it; it must not stop the Bitcoin wallet from loading.
        let _ = self.rebuild_eth().await;
        Ok(())
    }

    /// Rebuild the Bitcoin wallet.
    ///
    /// Fails closed, like the Ethereum side: a wallet that cannot be built for
    /// the current mode is dropped rather than left in memory on the previous
    /// one. A relay switch to Tor must never leave a clearnet wallet reachable.
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
        .map_err(|e| e.to_string())?;
        match built {
            Ok(wallet) => {
                *btc.lock().unwrap() = wallet;
                *self.btc_error.lock().unwrap() = None;
                Ok(())
            }
            Err(e) => {
                *btc.lock().unwrap() = None;
                *self.btc_error.lock().unwrap() = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Rebuild the Ethereum account. Synchronous: opening a provider is a
    /// client construction, not a request.
    ///
    /// Fails closed. A wallet that cannot be built for the current mode is
    /// dropped rather than left in memory on the previous one: a relay switch
    /// to Tor must never leave a clearnet provider reachable.
    async fn rebuild_eth(&self) -> Result<(), String> {
        let state = self.state.lock().await.clone();
        let relay_tor = self.relay_tor.load(Ordering::SeqCst);
        let socks = self.socks.lock().await.clone();
        match build_eth(&state, relay_tor, socks.as_deref(), self.eth_rpc_override.as_deref()) {
            Ok(wallet) => {
                *self.eth.lock().unwrap() = wallet;
                *self.eth_error.lock().unwrap() = None;
                Ok(())
            }
            Err(e) => {
                *self.eth.lock().unwrap() = None;
                *self.eth_error.lock().unwrap() = Some(e.clone());
                Err(e)
            }
        }
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
            let _ = self.rebuild().await;
        }
    }

    /// A clone of the Ethereum account, if there is one. Cheap: the provider
    /// is shared behind an `Arc`.
    #[cfg(feature = "ethereum")]
    fn eth_handle(&self) -> Option<EthHandle> {
        self.eth.lock().unwrap().clone()
    }

    /// The Ethereum receive address, read without touching the network.
    #[cfg(feature = "ethereum")]
    fn eth_address(&self) -> Option<String> {
        self.eth.lock().unwrap().as_ref().map(|w| w.address())
    }

    #[cfg(not(feature = "ethereum"))]
    fn eth_address(&self) -> Option<String> {
        None
    }

    pub async fn public_state(&self, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let state = self.state.lock().await.clone();

        let (btc_address, btc_confirmed, btc_pending) = {
            let guard = self.btc.lock().unwrap();
            match guard.as_ref() {
                Some(w) => {
                    let (c, p) = w.balance();
                    (w.current_receive_address(), c, p)
                }
                None => (None, 0, 0),
            }
        };

        let eth_address = self.eth_address();
        let (eth_wei, tokens) = {
            let snapshot = self.eth_balances.lock().unwrap();
            let tokens = state
                .ethereum
                .tokens
                .iter()
                .map(|t| TokenView {
                    address: t.address.clone(),
                    symbol: t.symbol.clone(),
                    decimals: t.decimals,
                    balance: amount_string(snapshot.token(&t.address)),
                })
                .collect::<Vec<_>>();
            (snapshot.wei, tokens)
        };

        Ok(PublicState {
            exists: state.has_wallet(),
            network: state.network.clone(),
            mode: effective_mode(state.mode, relay_tor),
            mode_locked: relay_tor,
            label: state.label.clone(),
            created_at: state.created_at,
            chains: vec![
                ChainView {
                    chain: ChainId::Bitcoin,
                    available: true,
                    address: btc_address,
                    unit: "sat".into(),
                    confirmed: amount_string(btc_confirmed as u128),
                    pending: amount_string(btc_pending as u128),
                },
                ChainView {
                    chain: ChainId::Ethereum,
                    available: cfg!(feature = "ethereum"),
                    address: eth_address,
                    unit: "wei".into(),
                    confirmed: amount_string(eth_wei),
                    pending: amount_string(0),
                },
            ],
            tokens,
        })
    }

    /// Generate a new phrase and store it. The phrase is returned exactly once.
    pub async fn create(
        &self,
        network: &str,
        ethereum_network: &str,
        mode: NetworkMode,
        relay_tor: bool,
    ) -> Result<CreateResult, String> {
        self.ensure_loaded().await?;
        // Validate before generating, so a bad network name does not leave a
        // half-created wallet behind.
        network_from_str(network).map_err(|e| e.to_string())?;
        validate_eth_network(ethereum_network)?;
        let phrase = hush_wallet::seed::generate_mnemonic().map_err(|e| e.to_string())?;
        {
            let mut state = self.state.lock().await;
            if state.has_wallet() {
                return Err("a wallet already exists in this vault".into());
            }
            *state = WalletState::new(phrase.clone(), network, mode);
            state.ethereum = EthereumState {
                network: ethereum_network.to_string(),
                ..EthereumState::default()
            };
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.rebuild().await?;
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
        ethereum_network: &str,
        mode: NetworkMode,
        relay_tor: bool,
    ) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let mnemonic = parse_mnemonic(phrase).map_err(|e| e.to_string())?;
        network_from_str(network).map_err(|e| e.to_string())?;
        validate_eth_network(ethereum_network)?;
        {
            let mut state = self.state.lock().await;
            if state.has_wallet() {
                return Err("a wallet already exists in this vault".into());
            }
            *state = WalletState::new(mnemonic.to_string(), network, mode);
            state.ethereum = EthereumState {
                network: ethereum_network.to_string(),
                ..EthereumState::default()
            };
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.rebuild().await?;
        self.capture_changeset().await?;
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    /// Hand out the address to receive on.
    ///
    /// Bitcoin rotates: each call reveals a new one. Ethereum has a single
    /// account, so this returns that account's address and advances nothing.
    pub async fn receive_address(&self, chain: ChainId) -> Result<AddressResult, String> {
        self.ensure_loaded().await?;
        match chain {
            ChainId::Bitcoin => {
                if self.btc.lock().unwrap().is_none() {
                    return Err(self.btc_unavailable());
                }
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
                Ok(AddressResult {
                    chain,
                    address,
                })
            }
            ChainId::Ethereum => {
                let address = self
                    .eth_handle()
                    .map(|w| w.address())
                    .ok_or_else(|| self.eth_unavailable())?;
                Ok(AddressResult { chain, address })
            }
        }
    }

    /// Why the Bitcoin side has no wallet, in the terms of the last failure.
    fn btc_unavailable(&self) -> String {
        self.btc_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "no wallet".to_string())
    }

    /// Why the Ethereum side has no account, said in the terms of the build.
    #[cfg(feature = "ethereum")]
    fn eth_unavailable(&self) -> String {
        self.eth_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "no wallet".to_string())
    }

    #[cfg(not(feature = "ethereum"))]
    fn eth_unavailable(&self) -> String {
        "this build of Hush has no Ethereum support".to_string()
    }

    /// Refresh a chain's balances and return the new state.
    pub async fn sync(&self, chain: ChainId, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        match chain {
            ChainId::Bitcoin => {
                if self.btc.lock().unwrap().is_none() {
                    return Err(self.btc_unavailable());
                }
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
            }
            ChainId::Ethereum => self.sync_eth().await?,
        }
        self.public_state(relay_tor).await
    }

    /// Read the native balance and every tracked token's balance.
    #[cfg(feature = "ethereum")]
    async fn sync_eth(&self) -> Result<(), String> {
        let eth = self.eth_handle().ok_or_else(|| self.eth_unavailable())?;
        // The endpoint has to be the chain the wallet is set to, or the
        // numbers that follow are about a different network.
        eth.verify_chain().await.map_err(|e| e.to_string())?;
        let wei = eth.balance().await.map_err(|e| e.to_string())?;
        // Read the token list before the first await, so no lock is held
        // across a request.
        let tracked = {
            let state = self.state.lock().await;
            state.ethereum.tokens.clone()
        };
        let mut snapshot = EthSnapshot {
            wei,
            tokens: Vec::new(),
        };
        for token in tracked {
            let balance = eth
                .token_balance(&token.address, None)
                .await
                .map_err(|e| format!("{}: {e}", token.symbol))?;
            snapshot.set_token(&token.address, balance);
        }
        *self.eth_balances.lock().unwrap() = snapshot;
        Ok(())
    }

    #[cfg(not(feature = "ethereum"))]
    async fn sync_eth(&self) -> Result<(), String> {
        Err(self.eth_unavailable())
    }

    /// Build, sign, and broadcast a payment on either chain.
    pub async fn send(&self, request: hush_wallet::chain::SendRequest) -> Result<SendResult, String> {
        self.ensure_loaded().await?;
        let amount = hush_wallet::chain::parse_amount(&request.amount).map_err(|e| e.to_string())?;
        match request.chain {
            ChainId::Bitcoin => {
                // Refusing here keeps a u128 that would truncate out of the
                // signing path.
                let sat = u64::try_from(amount)
                    .map_err(|_| "that amount is larger than this wallet can send".to_string())?;
                self.send_btc(request, sat).await
            }
            ChainId::Ethereum => self.send_eth(request, amount).await,
        }
    }

    async fn send_btc(
        &self,
        request: hush_wallet::chain::SendRequest,
        amount_sat: u64,
    ) -> Result<SendResult, String> {
        if request.token.is_some() {
            return Err("Bitcoin has no tokens to send; drop the token field".into());
        }
        if self.btc.lock().unwrap().is_none() {
            return Err(self.btc_unavailable());
        }
        let to = request.to;
        let fee_rate = request.fee_rate;
        let btc = self.btc.clone();
        let (txid, fee_sat) = tokio::task::spawn_blocking(move || -> Result<(String, u64), String> {
            let mut guard = btc.lock().unwrap();
            let w = guard.as_mut().ok_or_else(|| "no wallet".to_string())?;
            let rate = fee_rate.unwrap_or(DEFAULT_FEE_RATE);
            let (tx, fee) = w.build_send(&to, amount_sat, rate).map_err(|e| e.to_string())?;
            let txid = w.broadcast(&tx).map_err(|e| e.to_string())?;
            Ok((txid, fee))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(SendResult {
            chain: ChainId::Bitcoin,
            txid,
            fee_sat: Some(fee_sat),
        })
    }

    #[cfg(feature = "ethereum")]
    async fn send_eth(
        &self,
        request: hush_wallet::chain::SendRequest,
        amount: u128,
    ) -> Result<SendResult, String> {
        let eth = self.eth_handle().ok_or_else(|| self.eth_unavailable())?;
        let txid = match request.token.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(token) => eth
                .send_token(token, &request.to, amount)
                .await
                .map_err(|e| e.to_string())?,
            None => eth
                .send(&request.to, amount)
                .await
                .map_err(|e| e.to_string())?,
        };
        Ok(SendResult {
            chain: ChainId::Ethereum,
            txid,
            fee_sat: None,
        })
    }

    #[cfg(not(feature = "ethereum"))]
    async fn send_eth(
        &self,
        _request: hush_wallet::chain::SendRequest,
        _amount: u128,
    ) -> Result<SendResult, String> {
        Err(self.eth_unavailable())
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
        self.rebuild().await?;
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    /// Start tracking an ERC-20 token, reading its symbol and decimals from the
    /// contract rather than trusting anything the user typed.
    #[cfg(feature = "ethereum")]
    pub async fn add_token(&self, address: &str, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let canonical =
            hush_wallet::ethereum::checksummed_address(address).map_err(|e| e.to_string())?;
        {
            let state = self.state.lock().await;
            if state
                .ethereum
                .tokens
                .iter()
                .any(|t| t.address.eq_ignore_ascii_case(&canonical))
            {
                return Err("that token is already in the wallet".into());
            }
            if state.ethereum.tokens.len() >= MAX_TOKENS {
                return Err(format!(
                    "this wallet already tracks {MAX_TOKENS} tokens, which is the limit"
                ));
            }
        }
        let eth = self.eth_handle().ok_or_else(|| self.eth_unavailable())?;
        let (symbol, decimals) = eth
            .token_metadata(&canonical)
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut state = self.state.lock().await;
            state.ethereum.tokens.push(TokenEntry {
                address: canonical.clone(),
                symbol,
                decimals,
            });
        }
        self.dirty.store(true, Ordering::SeqCst);
        self.persist().await?;
        // Read its balance straight away, so the row shows a number instead of
        // a zero that looks like an empty account.
        if let Ok(balance) = eth.token_balance(&canonical, None).await {
            self.eth_balances
                .lock()
                .unwrap()
                .set_token(&canonical, balance);
        }
        self.public_state(relay_tor).await
    }

    #[cfg(not(feature = "ethereum"))]
    pub async fn add_token(&self, _address: &str, _relay_tor: bool) -> Result<PublicState, String> {
        Err(self.eth_unavailable())
    }

    /// Stop tracking a token. Nothing on chain is touched.
    #[cfg(feature = "ethereum")]
    pub async fn remove_token(&self, address: &str, relay_tor: bool) -> Result<PublicState, String> {
        self.ensure_loaded().await?;
        let canonical =
            hush_wallet::ethereum::checksummed_address(address).map_err(|e| e.to_string())?;
        let removed = {
            let mut state = self.state.lock().await;
            let before = state.ethereum.tokens.len();
            state
                .ethereum
                .tokens
                .retain(|t| !t.address.eq_ignore_ascii_case(&canonical));
            before != state.ethereum.tokens.len()
        };
        if !removed {
            return Err("that token is not in the wallet".into());
        }
        self.eth_balances.lock().unwrap().forget_token(&canonical);
        self.dirty.store(true, Ordering::SeqCst);
        self.persist().await?;
        self.public_state(relay_tor).await
    }

    #[cfg(not(feature = "ethereum"))]
    pub async fn remove_token(
        &self,
        _address: &str,
        _relay_tor: bool,
    ) -> Result<PublicState, String> {
        Err(self.eth_unavailable())
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
        *self.eth.lock().unwrap() = None;
        self.eth_balances.lock().unwrap().forget_all();
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
        assert!(build_wallet(&state, false, None, None).unwrap().is_none());
        assert!(build_eth(&state, false, None, None).unwrap().is_none());
    }

    #[test]
    fn a_tor_relay_builds_the_wallet_on_tor_and_refuses_a_missing_proxy() {
        let state = WalletState::new(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
            "mainnet",
            NetworkMode::Clearnet,
        );
        // Relay on Tor with no proxy: the wallet cannot be built, rather than
        // quietly falling back to a direct connection.
        assert!(build_wallet(&state, true, None, None).is_err());
        assert!(build_eth(&state, true, None, None).is_err());
        // With the proxy it builds, in Tor mode.
        let proxy = Some("socks5h://127.0.0.1:9050");
        let btc = build_wallet(&state, true, proxy, None).unwrap().unwrap();
        assert_eq!(btc.address_at(0), "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
    }

    #[cfg(feature = "ethereum")]
    #[test]
    fn the_ethereum_account_is_the_same_one_the_phrase_derives() {
        let state = WalletState::new(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
            "mainnet",
            NetworkMode::Clearnet,
        );
        let eth = build_eth(&state, false, None, None).unwrap().unwrap();
        assert_eq!(eth.address(), "0x9858EfFD232B4033E47d90003D41EC34EcaEda94");
        // Tor mode with the shell's proxy is accepted.
        let eth = build_eth(&state, true, Some("socks5h://127.0.0.1:9050"), None)
            .unwrap()
            .unwrap();
        assert_eq!(eth.chain_id(), 1);
    }

    #[cfg(feature = "ethereum")]
    #[test]
    fn an_unknown_ethereum_network_is_refused_rather_than_defaulted() {
        // A real phrase, so a failure can only come from the network name. The
        // earlier version of this test passed "x" as the phrase and so asserted
        // nothing about the network at all.
        let mut state = WalletState::new(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
            "mainnet",
            NetworkMode::Clearnet,
        );
        state.ethereum.network = "sepoliaa".into();
        assert!(build_eth(&state, false, None, None).is_err());
        assert!(validate_eth_network("sepoliaa").is_err());
        state.ethereum.network = "sepolia".into();
        let eth = build_eth(&state, false, None, None).unwrap().unwrap();
        assert_eq!(eth.chain_id(), 11_155_111);
        assert!(validate_eth_network("sepolia").is_ok());
    }

    #[test]
    fn the_snapshot_tracks_each_token_once_and_forgets_on_removal() {
        let mut snapshot = EthSnapshot::default();
        snapshot.set_token("0xAbC", 5);
        snapshot.set_token("0xabc", 7);
        assert_eq!(snapshot.tokens.len(), 1, "a token is tracked once");
        assert_eq!(snapshot.token("0xABC"), 7, "the later read wins");
        assert_eq!(snapshot.token("0xdef"), 0, "an unknown token reads zero");
        snapshot.forget_token("0xAbC");
        assert_eq!(snapshot.token("0xabc"), 0);
        snapshot.wei = 10;
        snapshot.forget_all();
        assert_eq!(snapshot.wei, 0);
    }

    #[test]
    fn a_chain_request_defaults_to_bitcoin() {
        let req: ChainRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req.chain, ChainId::Bitcoin);
        let req: ChainRequest = serde_json::from_str(r#"{"chain":"ethereum"}"#).unwrap();
        assert_eq!(req.chain, ChainId::Ethereum);
    }
}
