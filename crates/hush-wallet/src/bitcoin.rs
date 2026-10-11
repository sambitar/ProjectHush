//! The Bitcoin adapter: a BDK descriptor wallet synced through an electrum
//! server, optionally routed through Tor.
//!
//! Everything that touches the network is synchronous (electrum-client is a
//! blocking client), so the daemon runs these calls inside `spawn_blocking`.
//!
//! Privacy note: electrum servers learn which scripts a wallet asks about, so
//! a server can link the addresses of one wallet together. Running over Tor
//! hides the *IP*, not that linkage. Compact block filters (BIP157/158) would
//! remove the server from the trust path entirely, but there is no mature
//! embeddable crate for them yet; this is the documented trade, not an
//! oversight. See the DEEPSEEKPROBLEMS log.

use std::convert::Infallible;
use std::str::FromStr;

use bdk_chain::Merge;
use bdk_electrum::BdkElectrumClient;
use bdk_electrum::electrum_client::{Client, ConfigBuilder, Socks5Config};
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::{Address, Amount, FeeRate, Network, Transaction};
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::template::Bip84;
use bdk_wallet::{
    ChangeSet, KeychainKind, PersistedWallet, SignOptions, Wallet, WalletPersister,
};

use crate::error::{Result, WalletError};
use crate::net::{socks_addr_from_proxy, NetworkMode};

/// Default electrum endpoints. Overridable per-config so a user can point at
/// their own server.
pub const DEFAULT_ELECTRUM_MAINNET: &str = "ssl://electrum.blockstream.info:50002";
pub const DEFAULT_ELECTRUM_TESTNET: &str = "ssl://electrum.blockstream.info:60002";

/// How far ahead of the last used address to scan on a full scan. 20 is the
/// conventional BIP44 gap limit.
const STOP_GAP: usize = 20;
/// Electrum servers cap the size of a batch request; 50 is safely under the
/// common 100-item limit.
const BATCH_SIZE: usize = 50;

/// Everything the adapter needs to build a client and a wallet.
#[derive(Clone, Debug)]
pub struct BitcoinConfig {
    pub network: Network,
    pub electrum_url: String,
    pub mode: NetworkMode,
    /// The shell's Tor SOCKS proxy (`socks5h://host:port`), required in Tor
    /// mode and ignored in clearnet mode.
    pub socks: Option<String>,
}

impl BitcoinConfig {
    /// The default endpoint for a network.
    pub fn default_electrum_url(network: Network) -> &'static str {
        match network {
            Network::Bitcoin => DEFAULT_ELECTRUM_MAINNET,
            _ => DEFAULT_ELECTRUM_TESTNET,
        }
    }

    fn make_client(&self) -> Result<BdkElectrumClient<Client>> {
        let mut builder = ConfigBuilder::new();
        if self.mode == NetworkMode::Tor {
            let proxy = self.socks.as_deref().ok_or_else(|| {
                WalletError::Network(
                    "Tor mode needs the shell's SOCKS proxy, and none was given".into(),
                )
            })?;
            let addr = socks_addr_from_proxy(proxy)?;
            builder = builder.socks5(Some(Socks5Config {
                addr,
                credentials: None,
            }));
        }
        let config = builder.build();
        let raw = Client::from_config(&self.electrum_url, config)
            .map_err(|e| WalletError::Electrum(e.to_string()))?;
        Ok(BdkElectrumClient::new(raw))
    }
}

/// Parse the network name stored in the wallet state.
pub fn network_from_str(s: &str) -> Result<Network> {
    match s {
        "mainnet" | "bitcoin" => Ok(Network::Bitcoin),
        "testnet" | "test" => Ok(Network::Testnet),
        "regtest" => Ok(Network::Regtest),
        "signet" => Ok(Network::Signet),
        other => Err(WalletError::Network(format!("unknown network {other:?}"))),
    }
}

/// An in-memory [`WalletPersister`].
///
/// BDK hands the persister only the *staged* changes, which must be merged into
/// the accumulated set. The daemon takes that set out with [`changeset`] and
/// seals it in the vault; it never reaches disk in the clear.
///
/// [`changeset`]: BitcoinWallet::changeset
#[derive(Default)]
struct MemPersister {
    changeset: ChangeSet,
}

impl WalletPersister for MemPersister {
    type Error = Infallible;

    fn initialize(persister: &mut Self) -> std::result::Result<ChangeSet, Self::Error> {
        Ok(persister.changeset.clone())
    }

    fn persist(persister: &mut Self, changeset: &ChangeSet) -> std::result::Result<(), Self::Error> {
        persister.changeset.merge(changeset.clone());
        Ok(())
    }
}

/// A loaded Bitcoin wallet plus the config it needs to reach the chain.
pub struct BitcoinWallet {
    wallet: PersistedWallet<MemPersister>,
    persister: MemPersister,
    config: BitcoinConfig,
}

impl BitcoinWallet {
    /// Build a brand-new wallet from a phrase.
    ///
    /// One receive address is revealed up front so the UI has something to show
    /// immediately and the first "new address" the user asks for is genuinely
    /// new rather than a repeat of the one already on screen.
    pub fn create(config: BitcoinConfig, mnemonic: &Mnemonic) -> Result<Self> {
        let (external, internal) = descriptors(mnemonic, config.network)?;
        let params = Wallet::create(external, internal).network(config.network);
        let mut persister = MemPersister::default();
        let mut wallet = PersistedWallet::create(&mut persister, params)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        wallet.reveal_next_address(KeychainKind::External);
        wallet
            .persist(&mut persister)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        Ok(Self {
            wallet,
            persister,
            config,
        })
    }

    /// Reload a wallet from a previously exported change set.
    pub fn load(
        config: BitcoinConfig,
        mnemonic: &Mnemonic,
        changeset: Option<ChangeSet>,
    ) -> Result<Self> {
        let (external, internal) = descriptors(mnemonic, config.network)?;
        let params = Wallet::load()
            .descriptor(KeychainKind::External, Some(external))
            .descriptor(KeychainKind::Internal, Some(internal))
            .check_network(config.network);
        let mut persister = MemPersister {
            changeset: changeset.unwrap_or_default(),
        };
        let wallet = PersistedWallet::load(&mut persister, params)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?
            .ok_or(WalletError::NoWallet)?;
        Ok(Self {
            wallet,
            persister,
            config,
        })
    }

    /// A fresh receive address, advancing the keychain so no address is reused.
    pub fn receive_address(&mut self) -> Result<String> {
        let info = self.wallet.reveal_next_address(KeychainKind::External);
        // Revealing an address stages a change; keep it so the next restart
        // does not hand out the same address again.
        self.wallet
            .persist(&mut self.persister)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        Ok(info.address.to_string())
    }

    /// The address at a specific receive index, without advancing anything.
    pub fn address_at(&self, index: u32) -> String {
        self.wallet
            .peek_address(KeychainKind::External, index)
            .address
            .to_string()
    }

    /// The receive address the UI should show: the most recently revealed one.
    ///
    /// Read-only, so it is safe to call on every state poll. `None` only before
    /// any address has been revealed, which cannot happen for a wallet built by
    /// [`create`](Self::create).
    pub fn current_receive_address(&self) -> Option<String> {
        self.wallet
            .derivation_index(KeychainKind::External)
            .map(|index| self.address_at(index))
    }

    /// Whether any output the wallet knows about — spent or unspent — pays the
    /// address currently on screen, which means it has been used and should no
    /// longer be handed out.
    fn current_receive_address_used(&self) -> bool {
        let Some(index) = self.wallet.derivation_index(KeychainKind::External) else {
            return false;
        };
        let spk = self
            .wallet
            .peek_address(KeychainKind::External, index)
            .address
            .script_pubkey();
        self.wallet
            .list_output()
            .any(|output| output.txout.script_pubkey == spk)
    }

    /// Full-scan the keychain against the electrum server and fold the result
    /// into the wallet.
    pub fn sync(&mut self) -> Result<()> {
        let client = self.config.make_client()?;
        let request = self.wallet.start_full_scan().build();
        let response = client
            .full_scan(request, STOP_GAP, BATCH_SIZE, true)
            .map_err(|e| WalletError::Electrum(e.to_string()))?;
        self.wallet
            .apply_update(response)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        self.wallet
            .persist(&mut self.persister)
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        // Once a payment has landed on the address on screen, move on: showing
        // it again would invite a second payer to reuse it.
        if self.current_receive_address_used() {
            self.receive_address()?;
        }
        Ok(())
    }

    /// `(confirmed, pending)` in satoshi.
    pub fn balance(&self) -> (u64, u64) {
        let b = self.wallet.balance();
        (b.confirmed.to_sat(), b.trusted_pending.to_sat())
    }

    /// Build, sign, and return a transaction ready to broadcast, with the fee
    /// in satoshi. Returned rather than broadcast so the caller can show the
    /// fee and destination first.
    ///
    /// `Wallet::sign` is deprecated in BDK 3.2 in favour of
    /// `sign_with_signers`, which wants a caller-owned `SignersContainer`. The
    /// wallet already owns the right signers for a single-key descriptor, so
    /// this uses the deprecated path rather than rebuild that container for no
    /// behavioural gain. Revisit if BDK removes it.
    #[allow(deprecated)]
    pub fn build_send(
        &mut self,
        to: &str,
        amount_sat: u64,
        fee_rate: u64,
    ) -> Result<(Transaction, u64)> {
        if amount_sat == 0 {
            return Err(WalletError::Amount(
                "the amount must be greater than zero".into(),
            ));
        }
        let address = Address::from_str(to)
            .map_err(|e| WalletError::Address(e.to_string()))?
            .require_network(self.config.network)
            .map_err(|e| WalletError::Address(e.to_string()))?;
        let rate = FeeRate::from_sat_per_vb(fee_rate)
            .ok_or_else(|| WalletError::Amount("that fee rate is out of range".into()))?;

        let mut builder = self.wallet.build_tx();
        builder.add_recipient(address.script_pubkey(), Amount::from_sat(amount_sat));
        builder.fee_rate(rate);
        let mut psbt = builder
            .finish()
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        let signed = self
            .wallet
            .sign(&mut psbt, SignOptions::default())
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        if !signed {
            return Err(WalletError::Bitcoin(
                "the wallet could not sign this transaction".into(),
            ));
        }
        let fee_sat = psbt.fee().map(|a| a.to_sat()).unwrap_or(0);
        let tx = psbt
            .extract_tx()
            .map_err(|e| WalletError::Bitcoin(e.to_string()))?;
        Ok((tx, fee_sat))
    }

    /// Broadcast a signed transaction. Returns the txid.
    pub fn broadcast(&self, tx: &Transaction) -> Result<String> {
        let client = self.config.make_client()?;
        let txid = client
            .transaction_broadcast(tx)
            .map_err(|e| WalletError::Electrum(e.to_string()))?;
        Ok(txid.to_string())
    }

    /// The accumulated BDK change set, as JSON, for the vault sidecar.
    pub fn changeset(&self) -> Result<serde_json::Value> {
        serde_json::to_value(&self.persister.changeset)
            .map_err(|e| WalletError::State(e.to_string()))
    }

    /// Decode a change set exported by [`changeset`].
    ///
    /// [`changeset`]: BitcoinWallet::changeset
    pub fn changeset_from_value(value: &serde_json::Value) -> Result<ChangeSet> {
        serde_json::from_value(value.clone()).map_err(|e| WalletError::State(e.to_string()))
    }
}

/// The BIP84 receive and change descriptors for a phrase.
fn descriptors(mnemonic: &Mnemonic, network: Network) -> Result<(Bip84<Xpriv>, Bip84<Xpriv>)> {
    let xprv = crate::seed::master_xprv(mnemonic, network)?;
    Ok((
        Bip84(xprv.clone(), KeychainKind::External),
        Bip84(xprv, KeychainKind::Internal),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::parse_mnemonic;

    /// The canonical all-`abandon` vector. Its first BIP84 mainnet address is a
    /// fixed, well-known value: if derivation is wrong, this fails.
    const VECTOR: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const VECTOR_FIRST_ADDRESS: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

    fn config(network: Network) -> BitcoinConfig {
        BitcoinConfig {
            network,
            electrum_url: BitcoinConfig::default_electrum_url(network).to_string(),
            mode: NetworkMode::Clearnet,
            socks: None,
        }
    }

    #[test]
    fn derives_the_known_bip84_address_for_the_vector() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let wallet = BitcoinWallet::create(config(Network::Bitcoin), &m).unwrap();
        assert_eq!(wallet.address_at(0), VECTOR_FIRST_ADDRESS);
    }

    #[test]
    fn a_new_wallet_shows_one_receive_address_and_advances_past_it() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let mut wallet = BitcoinWallet::create(config(Network::Bitcoin), &m).unwrap();
        // Creation reveals exactly one address, and that is what the UI shows.
        assert_eq!(
            wallet.current_receive_address().as_deref(),
            Some(VECTOR_FIRST_ADDRESS)
        );
        // Asking for a new one advances, so the user never sees a repeat.
        let next = wallet.receive_address().unwrap();
        assert_ne!(next, VECTOR_FIRST_ADDRESS);
        assert_eq!(wallet.current_receive_address().as_deref(), Some(next.as_str()));
    }

    #[test]
    fn receive_addresses_advance_and_never_repeat() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let mut wallet = BitcoinWallet::create(config(Network::Bitcoin), &m).unwrap();
        let a0 = wallet.receive_address().unwrap();
        let a1 = wallet.receive_address().unwrap();
        assert_eq!(a0, wallet.address_at(1));
        assert_ne!(a0, a1, "a second receive address must not repeat the first");
    }

    #[test]
    fn changeset_roundtrips_through_json() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let mut wallet = BitcoinWallet::create(config(Network::Bitcoin), &m).unwrap();
        let _ = wallet.receive_address().unwrap();
        let shown = wallet.current_receive_address().unwrap();
        let value = wallet.changeset().unwrap();
        let back = BitcoinWallet::changeset_from_value(&value).unwrap();
        // Reloading from the exported change set must reproduce the wallet,
        // including the revealed index, so the UI shows the same address again.
        let reloaded = BitcoinWallet::load(config(Network::Bitcoin), &m, Some(back)).unwrap();
        assert_eq!(reloaded.address_at(0), VECTOR_FIRST_ADDRESS);
        assert_eq!(reloaded.current_receive_address().as_deref(), Some(shown.as_str()));
    }

    #[test]
    fn a_zero_amount_is_refused_before_any_network_use() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let mut wallet = BitcoinWallet::create(config(Network::Bitcoin), &m).unwrap();
        let err = wallet
            .build_send(VECTOR_FIRST_ADDRESS, 0, 1)
            .expect_err("zero must be refused");
        assert!(matches!(err, WalletError::Amount(_)));
    }

    #[test]
    fn a_wrong_network_address_is_refused() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let mut wallet = BitcoinWallet::create(config(Network::Testnet), &m).unwrap();
        // A mainnet address on a testnet wallet must be refused before signing.
        let err = wallet
            .build_send(VECTOR_FIRST_ADDRESS, 1000, 1)
            .expect_err("wrong network must be refused");
        assert!(matches!(err, WalletError::Address(_)));
    }

    #[test]
    fn tor_mode_without_a_proxy_refuses_to_build_a_client() {
        let cfg = BitcoinConfig {
            network: Network::Bitcoin,
            electrum_url: BitcoinConfig::default_electrum_url(Network::Bitcoin).to_string(),
            mode: NetworkMode::Tor,
            socks: None,
        };
        assert!(matches!(cfg.make_client(), Err(WalletError::Network(_))));
    }
}
