//! Live reads against real JSON-RPC endpoints.
//!
//! Every other test in this crate is offline: it proves derivation, calldata,
//! and validation, but never that a request leaves the process. These are the
//! opposite — the only tests that touch a real chain — so they are `#[ignore]`d
//! and have to be asked for:
//!
//! ```text
//! cargo test -p hush-wallet --features ethereum --test live_ethereum -- --ignored --nocapture
//! ```
//!
//! The Tor checks additionally need a SOCKS proxy to point at:
//!
//! ```text
//! HUSH_LIVE_SOCKS=socks5h://127.0.0.1:9050 cargo test -p hush-wallet \
//!   --features ethereum --test live_ethereum -- --ignored --nocapture
//! ```
//!
//! Nothing here sends value. `send()` and `send_token()` stay unproven until
//! they are run against a funded testnet account, which no unattended check can
//! do — a test that spends real value is not a test that can run on a whim.

// The module under test is behind the `ethereum` feature, so this file has to
// be too: without the gate, a plain `cargo test -p hush-wallet` fails to compile
// the test target instead of skipping it.
#![cfg(feature = "ethereum")]

use hush_wallet::ethereum::{
    EthereumConfig, EthereumWallet, DEFAULT_RPC_MAINNET, DEFAULT_RPC_SEPOLIA,
};
use hush_wallet::seed::parse_mnemonic;
use hush_wallet::NetworkMode;

/// The published all-`abandon` vector, the same phrase the offline tests use.
/// Its first Ethereum account is a fixed, widely known address, so a live read
/// is attributable to this phrase and nothing else.
const VECTOR: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const VECTOR_FIRST_ACCOUNT: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";

/// USDC on mainnet. A real contract whose `symbol()` and `decimals()` only
/// decode if the hand-rolled ABI binding in `ethereum.rs` is right.
const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

fn wallet(network: &str, mode: NetworkMode, socks: Option<String>) -> EthereumWallet {
    let mnemonic = parse_mnemonic(VECTOR).expect("the vector phrase parses");
    let config =
        EthereumConfig::for_network(network, mode, socks, 0).expect("the network name is valid");
    let wallet = EthereumWallet::create(&config, &mnemonic).expect("the account is derivable");
    // Every live check is about this one account, so prove it is the derived
    // one before asking the network anything.
    assert_eq!(wallet.address(), VECTOR_FIRST_ACCOUNT);
    wallet
}

/// The shell's Tor SOCKS address, when a live Tor check is being run.
fn socks_from_env() -> Option<String> {
    std::env::var("HUSH_LIVE_SOCKS")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint"]
async fn mainnet_confirms_the_chain_id_it_was_built_for() {
    let wallet = wallet("mainnet", NetworkMode::Clearnet, None);
    assert_eq!(wallet.chain_id(), 1);
    wallet
        .verify_chain()
        .await
        .expect("the mainnet endpoint reports chain 1");
    println!("verified {} on chain {}", wallet.address(), wallet.chain_id());
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint"]
async fn a_mainnet_balance_reads_back_over_clearnet() {
    let wallet = wallet("mainnet", NetworkMode::Clearnet, None);
    let wei = wallet.balance().await.expect("the balance is readable");
    println!("{} holds {} wei", wallet.address(), wei);
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint"]
async fn a_real_erc20_contract_answers_metadata_and_balance() {
    let wallet = wallet("mainnet", NetworkMode::Clearnet, None);
    let (symbol, decimals) = wallet
        .token_metadata(USDC)
        .await
        .expect("USDC answers symbol() and decimals()");
    assert_eq!(symbol, "USDC", "symbol() must decode as USDC");
    assert_eq!(decimals, 6, "USDC has six decimals");
    let balance = wallet
        .token_balance(USDC, None)
        .await
        .expect("balanceOf() is readable");
    println!("USDC: {symbol} ({decimals} decimals), balance {balance}");
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint"]
async fn an_endpoint_on_another_chain_is_refused() {
    // Mainnet's chain id, Sepolia's endpoint. The adapter must notice rather
    // than read Sepolia's balance as if it were mainnet's.
    let mnemonic = parse_mnemonic(VECTOR).expect("the vector phrase parses");
    let config = EthereumConfig {
        chain_id: 1,
        rpc_url: DEFAULT_RPC_SEPOLIA.to_string(),
        mode: NetworkMode::Clearnet,
        socks: None,
        account_index: 0,
    };
    let wallet = EthereumWallet::create(&config, &mnemonic).expect("the account is derivable");
    let err = wallet
        .verify_chain()
        .await
        .expect_err("a Sepolia endpoint must not pass as mainnet");
    println!("refused as expected: {err}");
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint and HUSH_LIVE_SOCKS"]
async fn mainnet_answers_through_the_shells_tor_proxy() {
    let socks = match socks_from_env() {
        Some(socks) => socks,
        None => {
            eprintln!("skipped: set HUSH_LIVE_SOCKS to a SOCKS proxy to run the Tor check");
            return;
        }
    };
    let wallet = wallet("mainnet", NetworkMode::Tor, Some(socks.clone()));
    wallet
        .verify_chain()
        .await
        .unwrap_or_else(|e| panic!("chain id must be readable through {socks}: {e}"));
    let wei = wallet
        .balance()
        .await
        .unwrap_or_else(|e| panic!("balance must be readable through {socks}: {e}"));
    println!("through {socks}: {} holds {wei} wei", wallet.address());
}

#[tokio::test]
#[ignore = "needs a live JSON-RPC endpoint and HUSH_LIVE_SOCKS"]
async fn the_tor_path_refuses_a_proxy_that_is_not_socks() {
    // A plain-HTTP proxy would silently send the request in the clear if the
    // adapter accepted it, so it must not.
    let config = EthereumConfig {
        chain_id: 1,
        rpc_url: DEFAULT_RPC_MAINNET.to_string(),
        mode: NetworkMode::Tor,
        socks: Some("http://127.0.0.1:9050".to_string()),
        account_index: 0,
    };
    let mnemonic = parse_mnemonic(VECTOR).expect("the vector phrase parses");
    assert!(
        EthereumWallet::create(&config, &mnemonic).is_err(),
        "an http:// proxy is not a SOCKS route and must be refused"
    );
}
