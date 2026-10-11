//! The Ethereum adapter: one BIP44 account per wallet, driven by JSON-RPC over
//! HTTP, optionally routed through Tor.
//!
//! Unlike Bitcoin there is no local chain data to keep: an Ethereum account is
//! a key and a balance, and the balance only exists on a node. So this adapter
//! holds no change set and writes nothing to the vault beyond the phrase. The
//! account is derived on demand from the same phrase the Bitcoin wallet uses,
//! at the interoperable path `m/44'/60'/0'/0/{index}`.
//!
//! Privacy note: an RPC endpoint sees every address a wallet asks about, and
//! the sender's IP unless it is routed through Tor. That is the same trade the
//! Bitcoin side documents for electrum — a hosted endpoint is trusted not to
//! lie about the chain, and Tor hides the network path but not the linkage. The
//! wallet never talks to an endpoint that is not one of the two defaults below
//! unless the user points it at their own.
//!
//! Everything here is async (alloy's HTTP transport is), so the daemon awaits
//! these calls directly rather than pushing them onto a blocking thread.

use std::str::FromStr;

use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::error::{Result, WalletError};
use crate::net::{socks_addr_from_proxy, NetworkMode};
use crate::seed;

/// Default RPC endpoints. Overridable per-config so a user can point at their
/// own node, or at a different provider entirely.
pub const DEFAULT_RPC_MAINNET: &str = "https://ethereum-rpc.publicnode.com";
pub const DEFAULT_RPC_SEPOLIA: &str = "https://ethereum-sepolia-rpc.publicnode.com";

// The ERC-20 calls this wallet makes. Declared here rather than pulled from a
// contract crate: four calls are the whole surface, and a hand-rolled ABI
// binding would be the thing that breaks silently.
sol! {
    function balanceOf(address owner) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function decimals() external view returns (uint8);
    function symbol() external view returns (string);
}

/// A named Ethereum network: its canonical name, chain id, and default
/// endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthereumNetwork {
    pub name: &'static str,
    pub chain_id: u64,
    pub rpc_url: &'static str,
}

/// Look up a named network, refusing anything unrecognised.
///
/// A typo must not silently land on mainnet: a wallet pointed at the wrong
/// chain reads the wrong balance and can spend real value by accident.
pub fn network(name: &str) -> Result<EthereumNetwork> {
    match name.trim().to_ascii_lowercase().as_str() {
        "mainnet" | "ethereum" => Ok(EthereumNetwork {
            name: "mainnet",
            chain_id: 1,
            rpc_url: DEFAULT_RPC_MAINNET,
        }),
        "sepolia" | "testnet" => Ok(EthereumNetwork {
            name: "sepolia",
            chain_id: 11_155_111,
            rpc_url: DEFAULT_RPC_SEPOLIA,
        }),
        other => Err(WalletError::Network(format!(
            "unknown Ethereum network {other:?}"
        ))),
    }
}

/// Everything the adapter needs to reach a chain and sign for it.
#[derive(Clone, Debug)]
pub struct EthereumConfig {
    /// The chain this endpoint is expected to be. Checked on sync, so a wallet
    /// pointed at the wrong network says so instead of spending there.
    pub chain_id: u64,
    pub rpc_url: String,
    pub mode: NetworkMode,
    /// The shell's Tor SOCKS proxy (`socks5h://host:port`), required in Tor
    /// mode and ignored in clearnet mode.
    pub socks: Option<String>,
    /// Which BIP44 account (`m/44'/60'/0'/0/{index}`) to use.
    pub account_index: u32,
}

impl EthereumConfig {
    /// A config for a named network, with its default endpoint.
    pub fn for_network(
        network_name: &str,
        mode: NetworkMode,
        socks: Option<String>,
        account_index: u32,
    ) -> Result<Self> {
        let net = network(network_name)?;
        Ok(Self {
            chain_id: net.chain_id,
            rpc_url: net.rpc_url.to_string(),
            mode,
            socks,
            account_index,
        })
    }

    /// The HTTP client, routed through the shell's Tor proxy in Tor mode.
    ///
    /// A missing proxy in Tor mode is an error, never a silent fallback to a
    /// direct connection: the wallet must not be less private than the app.
    fn http_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder();
        if self.mode == NetworkMode::Tor {
            let proxy = self.socks.as_deref().ok_or_else(|| {
                WalletError::Network(
                    "Tor mode needs the shell's SOCKS proxy, and none was given".into(),
                )
            })?;
            let addr = socks_addr_from_proxy(proxy)?;
            builder = builder.proxy(
                reqwest::Proxy::all(format!("socks5h://{addr}"))
                    .map_err(|e| WalletError::Network(e.to_string()))?,
            );
        }
        builder.build().map_err(|e| WalletError::Network(e.to_string()))
    }
}

/// Parse a 0x address, or say why it is not one.
pub fn parse_address(value: &str) -> Result<Address> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(WalletError::Address("the address is empty".into()));
    }
    Address::from_str(trimmed).map_err(|e| WalletError::Address(e.to_string()))
}

/// The checksummed form of an address, for storing and displaying.
///
/// Kept here so the daemon never has to name an alloy type: a token is stored
/// in its canonical spelling, which is what makes a duplicate check reliable.
pub fn checksummed_address(value: &str) -> Result<String> {
    Ok(parse_address(value)?.to_checksum(None))
}

/// A loaded Ethereum account plus the provider that reaches its chain.
///
/// Cloneable, and cheap to clone: the provider is behind an `Arc`, so a clone
/// shares the one connection pool and nonce cache. That is what lets the
/// daemon keep the wallet in a plain mutex and still await its calls.
#[derive(Clone)]
pub struct EthereumWallet {
    /// Type-erased so the daemon can hold it without naming the provider's
    /// filler stack, which is five types deep.
    provider: DynProvider<Ethereum>,
    address: Address,
    chain_id: u64,
}

impl EthereumWallet {
    /// Derive the account from the phrase and open a provider for it.
    pub fn create(config: &EthereumConfig, mnemonic: &bdk_wallet::keys::bip39::Mnemonic) -> Result<Self> {
        let secret = seed::ethereum_secret(mnemonic, config.account_index)?;
        let signer = PrivateKeySigner::from_slice(&secret)
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        let address = signer.address();
        let client = config.http_client()?;
        let url: reqwest::Url = config.rpc_url.parse().map_err(|_| {
            WalletError::Network(format!("not a usable RPC URL: {}", config.rpc_url))
        })?;
        // The wallet layer is what makes `send_transaction` sign: the fillers
        // add the nonce, the fees, and the chain id, then the signer turns the
        // request into a signed envelope. Nothing is signed anywhere else.
        let provider = ProviderBuilder::new()
            .wallet(signer)
            .connect_reqwest(client, url)
            .erased();
        Ok(Self {
            provider,
            address,
            chain_id: config.chain_id,
        })
    }

    /// The account address, EIP-55 checksummed.
    ///
    /// One account per wallet: unlike Bitcoin there is no address rotation, so
    /// this is the same address for the life of the wallet.
    pub fn address(&self) -> String {
        self.address.to_checksum(None)
    }

    /// The chain this adapter is configured for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Refuse to act if the endpoint is serving a different chain than the one
    /// the wallet is configured for. A balance read on the wrong chain is a
    /// wrong number; a send on the wrong chain is money in the wrong place.
    pub async fn verify_chain(&self) -> Result<()> {
        let reported = self
            .provider
            .get_chain_id()
            .await
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        if reported != self.chain_id {
            return Err(WalletError::Ethereum(format!(
                "the RPC endpoint is on chain {reported}, but this wallet is set to chain {}",
                self.chain_id
            )));
        }
        Ok(())
    }

    /// The native balance in wei.
    ///
    /// Ethereum has no local view of the chain, so there is nothing to "sync"
    /// beyond asking for this number; the daemon calls it on demand.
    pub async fn balance(&self) -> Result<u128> {
        let wei = self
            .provider
            .get_balance(self.address)
            .await
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        Ok(to_u128(wei))
    }

    /// Send native value. Returns the transaction hash, without waiting for a
    /// confirmation: a mined receipt can take minutes, and the hash is what the
    /// user needs to look the payment up.
    pub async fn send(&self, to: &str, amount_wei: u128) -> Result<String> {
        let to = parse_address(to)?;
        if amount_wei == 0 {
            return Err(WalletError::Amount(
                "the amount must be greater than zero".into(),
            ));
        }
        let tx = TransactionRequest::default()
            .to(to)
            .value(U256::from(amount_wei));
        self.broadcast(tx).await
    }

    /// The balance of an ERC-20 token, in the token's smallest unit.
    pub async fn token_balance(&self, token: &str, owner: Option<&str>) -> Result<u128> {
        let token = parse_address(token)?;
        let owner = match owner {
            Some(a) => parse_address(a)?,
            None => self.address,
        };
        let call = balanceOfCall { owner };
        let reply = self.call(token, call.abi_encode()).await?;
        let value = balanceOfCall::abi_decode_returns(&reply)
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        Ok(to_u128(value))
    }

    /// A token's symbol and decimals, read from the contract itself.
    ///
    /// Fetched once when the user adds the token, then stored, so the UI never
    /// has to trust a number the user typed for how many decimals a token has.
    pub async fn token_metadata(&self, token: &str) -> Result<(String, u8)> {
        let token = parse_address(token)?;
        let symbol = self
            .token_symbol(token)
            .await
            .unwrap_or_else(|_| "TOKEN".to_string());
        let decimals = self.token_decimals(token).await?;
        Ok((symbol, decimals))
    }

    async fn token_symbol(&self, token: Address) -> Result<String> {
        let reply = self.call(token, symbolCall {}.abi_encode()).await?;
        let symbol = symbolCall::abi_decode_returns(&reply)
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        let trimmed: String = symbol.chars().filter(|c| !c.is_control()).take(16).collect();
        Ok(if trimmed.trim().is_empty() {
            "TOKEN".to_string()
        } else {
            trimmed.trim().to_string()
        })
    }

    async fn token_decimals(&self, token: Address) -> Result<u8> {
        let reply = self.call(token, decimalsCall {}.abi_encode()).await?;
        let decimals = decimalsCall::abi_decode_returns(&reply)
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        if decimals > 36 {
            // A contract claiming more decimals than there are wei in an ether
            // is not a token worth displaying.
            return Err(WalletError::Ethereum(format!(
                "the token reports {decimals} decimals, which is not a token this wallet can display"
            )));
        }
        Ok(decimals)
    }

    /// Send ERC-20 tokens. Returns the transaction hash.
    pub async fn send_token(&self, token: &str, to: &str, amount: u128) -> Result<String> {
        let token = parse_address(token)?;
        let to = parse_address(to)?;
        if amount == 0 {
            return Err(WalletError::Amount(
                "the amount must be greater than zero".into(),
            ));
        }
        let call = transferCall {
            to,
            amount: U256::from(amount),
        };
        let tx = TransactionRequest::default()
            .to(token)
            .input(call.abi_encode().into());
        self.broadcast(tx).await
    }

    /// A read-only call, as a call to the token contract.
    async fn call(&self, to: Address, data: Vec<u8>) -> Result<Vec<u8>> {
        let request = TransactionRequest::default().to(to).input(data.into());
        let reply = self
            .provider
            .call(request)
            .await
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        Ok(reply.to_vec())
    }

    /// Hand a prepared request to the provider, which fills and signs it, and
    /// return the hash it broadcast.
    async fn broadcast(&self, tx: TransactionRequest) -> Result<String> {
        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .map_err(|e| WalletError::Ethereum(e.to_string()))?;
        Ok(pending.into_inner().tx_hash().to_string())
    }
}

/// A U256 as u128, saturating rather than wrapping. Balances are far below the
/// u128 ceiling (the entire ether supply is 1.2e26 wei, and u128 holds 3.4e38),
/// so saturation is unreachable in practice and the wrong answer is not
/// reachable at all.
fn to_u128(value: U256) -> u128 {
    if value > U256::from(u128::MAX) {
        u128::MAX
    } else {
        value.to::<u128>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::parse_mnemonic;

    /// The canonical all-`abandon` vector, the same one the Bitcoin tests use.
    /// Its first BIP44 Ethereum account is a fixed, widely published address:
    /// if derivation or checksumming is wrong, this fails.
    const VECTOR: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const VECTOR_FIRST_ACCOUNT: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";

    fn config(mode: NetworkMode) -> EthereumConfig {
        EthereumConfig::for_network("mainnet", mode, None, 0).unwrap()
    }

    #[test]
    fn derives_the_known_ethereum_account_for_the_vector() {
        let mnemonic = parse_mnemonic(VECTOR).unwrap();
        let wallet = EthereumWallet::create(&config(NetworkMode::Clearnet), &mnemonic).unwrap();
        assert_eq!(wallet.address(), VECTOR_FIRST_ACCOUNT);
        // The address is fixed for the life of the wallet: no rotation.
        assert_eq!(wallet.address(), wallet.address());
    }

    #[test]
    fn a_second_account_is_a_different_address() {
        let mnemonic = parse_mnemonic(VECTOR).unwrap();
        let first = EthereumWallet::create(&config(NetworkMode::Clearnet), &mnemonic).unwrap();
        let mut cfg = config(NetworkMode::Clearnet);
        cfg.account_index = 1;
        let second = EthereumWallet::create(&cfg, &mnemonic).unwrap();
        assert_ne!(first.address(), second.address());
    }

    #[test]
    fn the_address_is_checksummed_not_lowercase() {
        let mnemonic = parse_mnemonic(VECTOR).unwrap();
        let wallet = EthereumWallet::create(&config(NetworkMode::Clearnet), &mnemonic).unwrap();
        let address = wallet.address();
        assert!(address.starts_with("0x"));
        assert_eq!(address.len(), 42);
        assert_ne!(
            address,
            address.to_lowercase(),
            "an EIP-55 address has mixed case; a lowercase one would compare wrong against every other wallet"
        );
    }

    #[test]
    fn tor_mode_without_a_proxy_refuses_to_build_a_client() {
        assert!(matches!(
            config(NetworkMode::Tor).http_client(),
            Err(WalletError::Network(_))
        ));
        // The same config in clearnet mode is fine.
        assert!(config(NetworkMode::Clearnet).http_client().is_ok());
    }

    #[test]
    fn a_tor_client_uses_the_given_proxy() {
        let mut cfg = config(NetworkMode::Tor);
        cfg.socks = Some("socks5h://127.0.0.1:9050".into());
        assert!(cfg.http_client().is_ok());
        // A proxy address that is not a SOCKS address is refused, not ignored.
        cfg.socks = Some("http://127.0.0.1:9050".into());
        assert!(matches!(cfg.http_client(), Err(WalletError::Network(_))));
    }

    #[test]
    fn a_bad_rpc_url_is_refused_before_any_request() {
        let mnemonic = parse_mnemonic(VECTOR).unwrap();
        let mut cfg = config(NetworkMode::Clearnet);
        cfg.rpc_url = "not a url".into();
        assert!(matches!(
            EthereumWallet::create(&cfg, &mnemonic),
            Err(WalletError::Network(_))
        ));
    }

    #[test]
    fn addresses_parse_with_or_without_the_checksum_case() {
        assert_eq!(
            parse_address(VECTOR_FIRST_ACCOUNT).unwrap(),
            parse_address(&VECTOR_FIRST_ACCOUNT.to_lowercase()).unwrap()
        );
        // The canonical spelling is the checksummed one, whatever case the
        // user pasted, so a stored token address compares reliably.
        assert_eq!(
            checksummed_address(&VECTOR_FIRST_ACCOUNT.to_lowercase()).unwrap(),
            VECTOR_FIRST_ACCOUNT
        );
        assert_eq!(
            checksummed_address(VECTOR_FIRST_ACCOUNT).unwrap(),
            VECTOR_FIRST_ACCOUNT
        );
        for bad in ["", "0x", "0x1234", "not an address", "0xzzzz"] {
            assert!(
                matches!(parse_address(bad), Err(WalletError::Address(_))),
                "{bad:?} must not parse as an address"
            );
            assert!(checksummed_address(bad).is_err());
        }
    }

    #[test]
    fn erc20_calldata_matches_the_published_selectors() {
        // The four selectors are fixed by the ERC-20 standard; a mismatch here
        // would mean every token call goes to the wrong function.
        assert_eq!(balanceOfCall::SELECTOR, [0x70, 0xa0, 0x82, 0x31]);
        assert_eq!(transferCall::SELECTOR, [0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(decimalsCall::SELECTOR, [0x31, 0x3c, 0xe5, 0x67]);
        assert_eq!(symbolCall::SELECTOR, [0x95, 0xd8, 0x9b, 0x41]);
        // `transfer` argument order is (to, amount): swapping them would send
        // the wrong amount to the wrong place.
        let encoded = transferCall {
            to: parse_address(VECTOR_FIRST_ACCOUNT).unwrap(),
            amount: U256::from(1u64),
        }
        .abi_encode();
        let hex = hex::encode(&encoded);
        let one_as_a_word = format!("{}{}", "0".repeat(63), "1");
        assert!(hex.starts_with("a9059cbb"));
        assert!(hex.contains("9858effd232b4033e47d90003d41ec34ecaeda94"));
        assert!(hex.ends_with(&one_as_a_word));
    }

    #[test]
    fn the_default_endpoints_and_chain_ids_match_their_networks() {
        let mainnet = network("mainnet").unwrap();
        assert_eq!(mainnet.chain_id, 1);
        assert_eq!(mainnet.name, "mainnet");
        let sepolia = network("SEPOLIA").unwrap();
        assert_eq!(sepolia.chain_id, 11_155_111);
        assert_eq!(sepolia.name, "sepolia");
        assert_ne!(mainnet.rpc_url, sepolia.rpc_url);
        assert!(mainnet.rpc_url.starts_with("https://"));
        // An unknown name is refused rather than quietly read as mainnet.
        assert!(matches!(network("sepoliaa"), Err(WalletError::Network(_))));
        assert!(matches!(network(""), Err(WalletError::Network(_))));
        // And a config built from a name carries that network's chain id.
        let cfg =
            EthereumConfig::for_network("sepolia", NetworkMode::Clearnet, None, 0).unwrap();
        assert_eq!(cfg.chain_id, 11_155_111);
        assert_eq!(cfg.rpc_url, DEFAULT_RPC_SEPOLIA);
    }

    #[test]
    fn a_balance_wider_than_u128_saturates_instead_of_wrapping() {
        assert_eq!(to_u128(U256::from(0u64)), 0);
        assert_eq!(to_u128(U256::from(u128::MAX)), u128::MAX);
        assert_eq!(to_u128(U256::MAX), u128::MAX);
    }
}
