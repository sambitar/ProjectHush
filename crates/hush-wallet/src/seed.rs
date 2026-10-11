//! BIP39 mnemonics and the per-chain keys derived from them.
//!
//! One phrase is the whole wallet. It is generated here, validated here, and
//! turned into a Bitcoin master key (BIP32) or an Ethereum key. The phrase is
//! never written anywhere by this module: the daemon seals it with the vault
//! master key before it touches disk.
//!
//! Derivation paths are the interoperable defaults so the phrase restores in
//! any other wallet: BIP84 (`m/84'/0'/0'`) for Bitcoin native segwit, BIP44
//! (`m/44'/60'/0'/0/i`) for Ethereum.

use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::{Network, NetworkKind};
use bdk_wallet::keys::bip39::{Language, Mnemonic, WordCount};
use bdk_wallet::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk_wallet::miniscript::Segwitv0;

use crate::error::{Result, WalletError};

/// The word count of a freshly generated phrase. 12 words is 128 bits of
/// entropy, the BIP39 default and what most wallets show.
pub const DEFAULT_WORDS: usize = 12;

/// A fresh, English, 12-word phrase.
///
/// Generation goes through BDK's `GeneratableKey` rather than `bip39`'s own
/// `generate_in`, because BDK does not enable bip39's `rand` feature — and
/// adding it just for this call would be a second source of randomness for a
/// job BDK already does.
pub fn generate_mnemonic() -> Result<String> {
    let generated: GeneratedKey<Mnemonic, Segwitv0> =
        Mnemonic::generate((WordCount::Words12, Language::English))
            .map_err(|e| WalletError::Mnemonic(format!("{e:?}")))?;
    Ok(generated.into_key().to_string())
}

/// Parse and checksum-verify a phrase the user typed. Accepts any BIP39 word
/// count and only English, which is what this app generates.
pub fn parse_mnemonic(phrase: &str) -> Result<Mnemonic> {
    let normalized = normalize_phrase(phrase);
    Mnemonic::parse_in(Language::English, normalized.as_str())
        .map_err(|e| WalletError::Mnemonic(e.to_string()))
}

/// Collapse whitespace and lowercase, so a phrase pasted with stray spaces or
/// capitals still parses.
pub fn normalize_phrase(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The BIP32 master key for the phrase, on the given network's key kind.
///
/// The phrase itself encodes no network, so the same phrase yields the same
/// key material on every chain; only the serialization prefix differs.
pub fn master_xprv(mnemonic: &Mnemonic, network: Network) -> Result<Xpriv> {
    let kind = match network {
        Network::Bitcoin => NetworkKind::Main,
        _ => NetworkKind::Test,
    };
    let extended: ExtendedKey = mnemonic
        .clone()
        .into_extended_key()
        .map_err(|e| WalletError::Mnemonic(e.to_string()))?;
    extended
        .into_xprv(kind)
        .ok_or_else(|| WalletError::Mnemonic("phrase did not yield a private key".into()))
}

/// Derive a raw 32-byte secp256k1 secret for an Ethereum account at
/// `m/44'/60'/0'/0/{index}`.
///
/// The secret is returned as bytes rather than an `alloy` type so this module
/// carries no Ethereum dependency; the `ethereum` adapter wraps it. The bytes
/// are the same a `PrivateKeySigner` would hold, so the account is the standard
/// Ethereum account for that phrase.
pub fn ethereum_secret(mnemonic: &Mnemonic, index: u32) -> Result<[u8; 32]> {
    use std::str::FromStr;

    let path = bdk_wallet::bitcoin::bip32::DerivationPath::from_str(&format!(
        "m/44'/60'/0'/0/{index}"
    ))
    .map_err(|e| WalletError::Mnemonic(e.to_string()))?;
    let master = master_xprv(mnemonic, Network::Bitcoin)?;
    let secp = bdk_wallet::bitcoin::secp256k1::Secp256k1::new();
    let child = master
        .derive_priv(&secp, &path)
        .map_err(|e| WalletError::Mnemonic(e.to_string()))?;
    Ok(child.private_key.secret_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical all-`abandon` BIP39 vector, used by every wallet's test
    /// suite. Derivation must be reproducible from the phrase alone.
    const VECTOR: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn generates_a_parseable_twelve_word_phrase() {
        let phrase = generate_mnemonic().unwrap();
        assert_eq!(phrase.split_whitespace().count(), DEFAULT_WORDS);
        let parsed = parse_mnemonic(&phrase).unwrap();
        assert_eq!(parsed.to_string(), phrase);
    }

    #[test]
    fn parsing_is_stable_and_rejects_a_bad_checksum() {
        let parsed = parse_mnemonic(VECTOR).unwrap();
        assert_eq!(parsed.to_string(), VECTOR);

        // Uppercase and stray whitespace are normalized away.
        let messy = format!("  {}  ", VECTOR.to_uppercase());
        assert_eq!(parse_mnemonic(&messy).unwrap().to_string(), VECTOR);

        // A swapped word breaks the checksum.
        let broken = VECTOR.replace("about", "abandon");
        assert!(parse_mnemonic(&broken).is_err());
    }

    #[test]
    fn master_key_is_deterministic_for_a_phrase() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let a = master_xprv(&m, Network::Bitcoin).unwrap();
        let b = master_xprv(&m, Network::Bitcoin).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn ethereum_secret_is_32_bytes_and_index_dependent() {
        let m = parse_mnemonic(VECTOR).unwrap();
        let s0 = ethereum_secret(&m, 0).unwrap();
        let s1 = ethereum_secret(&m, 1).unwrap();
        assert_eq!(s0.len(), 32);
        assert_ne!(s0, s1, "different accounts must not share a key");
    }
}
