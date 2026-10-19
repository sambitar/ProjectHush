//! Places: parent-signed vouch hops so peers can badge a contact as
//! "place of …" without sharing a vault.
//!
//! Settled design: [`docs/plans/2026-10-18-android-linking.md`](D1–D17).
//! A hop is signed with the minting Place's libsignal identity key (XEd25519),
//! the same primitive queue grants use. Chains are capped at
//! [`MAX_PLACE_CHAIN_HOPS`].

use libsignal_protocol::IdentityKeyPair;
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::signal::protocol_name_from_identity;

/// Longest vouch chain that still earns a peer badge (desktop → phone → tablet).
pub const MAX_PLACE_CHAIN_HOPS: usize = 2;

const VOUCH_PREFIX: &str = "hush-place-vouch-v1";

/// One adoption claim: `minter` adopted `place`, optionally with a short label.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaceHop {
    /// Account id of the minting Place (`hex(identity key)`).
    pub minter_account_id: String,
    /// Account id of the adopted Place.
    pub place_account_id: String,
    /// Suggested nickname fragment ("phone", "laptop"); may be empty.
    pub label: String,
    pub signed_unix: u64,
}

/// A hop plus the minter's signature over [`place_vouch_signing_bytes`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedHop {
    pub hop: PlaceHop,
    pub signature: Vec<u8>,
}

/// Ordered hops from an earlier ancestor toward the place being introduced.
///
/// Continuity: `hops[i].place_account_id == hops[i+1].minter_account_id`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VouchChain {
    pub hops: Vec<SignedHop>,
}

/// Canonical bytes covered by a place-vouch signature.
pub fn place_vouch_signing_bytes(hop: &PlaceHop) -> Vec<u8> {
    format!(
        "{VOUCH_PREFIX}\n{}\n{}\n{}\n{}",
        hop.minter_account_id, hop.place_account_id, hop.label, hop.signed_unix
    )
    .into_bytes()
}

impl VouchChain {
    /// Sign a new hop with `minter` and append it.
    ///
    /// `minter`'s protocol name must equal `hop.minter_account_id`. The chain
    /// must stay within [`MAX_PLACE_CHAIN_HOPS`], and if non-empty the previous
    /// hop's place must equal this hop's minter.
    pub fn sign_hop(&mut self, minter: &IdentityKeyPair, hop: PlaceHop) -> Result<(), Error> {
        let minter_id = protocol_name_from_identity(minter);
        if hop.minter_account_id != minter_id {
            return Err(Error::InvalidPlace);
        }
        if self.hops.len() >= MAX_PLACE_CHAIN_HOPS {
            return Err(Error::PlaceChainTooLong);
        }
        if let Some(prev) = self.hops.last() {
            if prev.hop.place_account_id != hop.minter_account_id {
                return Err(Error::InvalidPlace);
            }
        }
        let mut csprng = OsRng.unwrap_err();
        let signature = minter
            .private_key()
            .calculate_signature(&place_vouch_signing_bytes(&hop), &mut csprng)
            .map(|sig| sig.into_vec())
            .map_err(|_| Error::Corrupt)?;
        self.hops.push(SignedHop { hop, signature });
        Ok(())
    }

    /// Verify every signature and chain continuity. Rejects empty and over-long chains.
    pub fn verify(&self) -> Result<(), Error> {
        if self.hops.is_empty() {
            return Err(Error::InvalidPlace);
        }
        if self.hops.len() > MAX_PLACE_CHAIN_HOPS {
            return Err(Error::PlaceChainTooLong);
        }
        for (i, signed) in self.hops.iter().enumerate() {
            if i > 0 {
                let prev = &self.hops[i - 1].hop;
                if prev.place_account_id != signed.hop.minter_account_id {
                    return Err(Error::InvalidPlace);
                }
            }
            if !verify_hop_signature(signed) {
                return Err(Error::InvalidPlace);
            }
        }
        Ok(())
    }

    /// Earliest hop whose minter is in `trusted`. Caller must `verify` first.
    ///
    /// That minter is the badge anchor ("place of …").
    pub fn trusted_anchor<'a>(&self, trusted: &[&'a str]) -> Option<&'a str> {
        for signed in &self.hops {
            if let Some(id) = trusted
                .iter()
                .copied()
                .find(|t| *t == signed.hop.minter_account_id.as_str())
            {
                return Some(id);
            }
        }
        None
    }

    /// Account id of the leaf place (last hop's place).
    pub fn leaf_account_id(&self) -> Option<&str> {
        self.hops.last().map(|h| h.hop.place_account_id.as_str())
    }
}

fn verify_hop_signature(signed: &SignedHop) -> bool {
    let Ok(key_bytes) = hex::decode(&signed.hop.minter_account_id) else {
        return false;
    };
    let Ok(key) = libsignal_protocol::IdentityKey::try_from(key_bytes.as_slice()) else {
        return false;
    };
    key.public_key()
        .verify_signature(&place_vouch_signing_bytes(&signed.hop), &signed.signature)
}

/// Unix seconds for hop timestamps.
pub fn place_now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsignal_protocol::IdentityKeyPair;
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn keypair() -> IdentityKeyPair {
        let mut csprng = OsRng.unwrap_err();
        IdentityKeyPair::generate(&mut csprng)
    }

    fn hop(minter: &IdentityKeyPair, place: &str, label: &str) -> PlaceHop {
        PlaceHop {
            minter_account_id: protocol_name_from_identity(minter),
            place_account_id: place.to_string(),
            label: label.to_string(),
            signed_unix: 1_700_000_000,
        }
    }

    #[test]
    fn signed_hop_verifies_and_detects_tampering() {
        let desktop = keypair();
        let phone_id = "aa".repeat(33);
        let mut chain = VouchChain::default();
        chain
            .sign_hop(&desktop, hop(&desktop, &phone_id, "phone"))
            .unwrap();
        chain.verify().unwrap();

        let mut tampered = chain.clone();
        tampered.hops[0].hop.label = "tablet".into();
        assert!(tampered.verify().is_err());
    }

    #[test]
    fn chain_longer_than_cap_is_rejected_on_sign() {
        let a = keypair();
        let b = keypair();
        let c = keypair();
        let d_id = "bb".repeat(33);
        let mut chain = VouchChain::default();
        chain
            .sign_hop(&a, hop(&a, &protocol_name_from_identity(&b), "phone"))
            .unwrap();
        chain
            .sign_hop(&b, hop(&b, &protocol_name_from_identity(&c), "tablet"))
            .unwrap();
        let err = chain
            .sign_hop(&c, hop(&c, &d_id, "watch"))
            .unwrap_err();
        assert!(matches!(err, Error::PlaceChainTooLong));
    }

    #[test]
    fn discontinuous_chain_fails_verify() {
        let a = keypair();
        let b = keypair();
        let stranger = keypair();
        let leaf = "cc".repeat(33);
        let mut chain = VouchChain::default();
        chain
            .sign_hop(&a, hop(&a, &protocol_name_from_identity(&b), "phone"))
            .unwrap();
        // Manually append a hop that does not continue from b.
        let mut rogue = VouchChain::default();
        rogue
            .sign_hop(&stranger, hop(&stranger, &leaf, "x"))
            .unwrap();
        chain.hops.push(rogue.hops.pop().unwrap());
        assert!(chain.verify().is_err());
    }

    #[test]
    fn trusted_anchor_is_the_earliest_trusted_minter() {
        let desktop = keypair();
        let phone = keypair();
        let tablet_id = "dd".repeat(33);
        let mut chain = VouchChain::default();
        chain
            .sign_hop(
                &desktop,
                hop(&desktop, &protocol_name_from_identity(&phone), "phone"),
            )
            .unwrap();
        chain
            .sign_hop(&phone, hop(&phone, &tablet_id, "tablet"))
            .unwrap();
        chain.verify().unwrap();

        let desktop_id = protocol_name_from_identity(&desktop);
        let phone_id = protocol_name_from_identity(&phone);
        assert_eq!(
            chain.trusted_anchor(&[desktop_id.as_str(), phone_id.as_str()]),
            Some(desktop_id.as_str())
        );
        assert_eq!(
            chain.trusted_anchor(&[phone_id.as_str()]),
            Some(phone_id.as_str())
        );
        assert_eq!(chain.trusted_anchor(&["nobody"]), None);
    }

    #[test]
    fn wrong_minter_key_rejected_on_sign() {
        let desktop = keypair();
        let other = keypair();
        let phone_id = "ee".repeat(33);
        let mut chain = VouchChain::default();
        let err = chain
            .sign_hop(&other, hop(&desktop, &phone_id, "phone"))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidPlace));
    }
}
