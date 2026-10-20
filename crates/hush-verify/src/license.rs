//! The license format, and the sign/verify pair that must not drift apart.
//!
//! A license is a signed statement that a vault is entitled to the first-party
//! relay and official updates. It is bound to the vault's `account_id` — the
//! same hex identity key the relay already sees on `/k/{account_id}` — so one
//! subscription cannot be shared between vaults.
//!
//! Signing and verification live in the same module on purpose. The wire format
//! is [`License::canonical_bytes`], and if the minter and the checker ever
//! disagreed about it every license would fail to verify. Keeping them one
//! function apart is what makes that impossible rather than merely unlikely.
//!
//! # What this deliberately does not do
//!
//! Verification is **offline and stateless**. There is no revocation list and no
//! call to the license service, which means a cancelled subscription keeps
//! working until `not_after`. That is the accepted price of not phoning home:
//! the only enforcement available before expiry is refusing to renew, and the
//! relay therefore stores no list of payers.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use thiserror::Error;

/// The text prefix on every encoded license. Versioned so a future format can
/// be told apart from this one rather than mis-parsed.
pub const PREFIX: &str = "hush-lic-v1";

/// A signed statement of entitlement, bound to one vault.
///
/// `issued_at` and `not_after` are Unix seconds. `not_after` is inclusive: a
/// license is valid at the instant it expires and invalid one second later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    /// The vault's account id — hex, as the relay already addresses it.
    pub account_id: String,
    /// The plan the license was issued under, e.g. `hush-monthly`.
    pub plan: String,
    /// Unix seconds the license was minted.
    pub issued_at: u64,
    /// Unix seconds the license stops being valid, inclusive.
    pub not_after: u64,
    /// An opaque identifier for this issuance, for support and for telling two
    /// licenses for the same vault apart.
    pub serial: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("license is malformed")]
    Malformed,
    #[error("license has expired")]
    Expired,
    #[error("license is bound to a different account")]
    WrongAccount,
    #[error("license signature does not verify")]
    BadSignature,
}

impl License {
    /// The exact bytes that are signed and verified.
    ///
    /// **This layout is the wire format.** Field order, the little-endian
    /// integers and the `u32` length prefixes are all load-bearing: changing any
    /// of them invalidates every license already in the wild. Add a version
    /// prefix and a new `PREFIX` instead of editing this.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        push_str(&mut out, &self.account_id);
        push_str(&mut out, &self.plan);
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        out.extend_from_slice(&self.not_after.to_le_bytes());
        push_str(&mut out, &self.serial);
        out
    }

    /// Whether this license entitles `account_id` at `now`.
    ///
    /// Checks binding and expiry, but **not** the signature — callers that
    /// accepted the license from anywhere but their own storage must call
    /// [`verify_license`] or [`check_encoded_license`] first. Binding is checked
    /// before expiry so a license lifted from another vault reports the reason
    /// that matters.
    pub fn check_valid_at(&self, now: u64, account_id: &str) -> Result<(), VerifyError> {
        if self.account_id != account_id {
            return Err(VerifyError::WrongAccount);
        }
        if now > self.not_after {
            return Err(VerifyError::Expired);
        }
        Ok(())
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Sign a license with the license service's private key.
///
/// Only the minting service should call this; the key never ships in the app.
pub fn sign_license(license: &License, signing_key: &SigningKey) -> Vec<u8> {
    signing_key.sign(&license.canonical_bytes()).to_bytes().to_vec()
}

/// Check a signature over a license against the license public key.
///
/// Returns `Err(BadSignature)` for a signature that does not verify, a key that
/// is not a valid curve point, and a signature of the wrong length alike — a
/// license that cannot be checked is a license that is not verified.
pub fn verify_license(
    license: &License,
    signature: &[u8],
    pubkey: &[u8; 32],
) -> Result<(), VerifyError> {
    let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| VerifyError::BadSignature)?;
    let sig = Signature::from_slice(signature).map_err(|_| VerifyError::BadSignature)?;
    vk.verify(&license.canonical_bytes(), &sig)
        .map_err(|_| VerifyError::BadSignature)
}

/// Encode a license and its signature as one pasteable string.
pub fn encode_license(license: &License, signature: &[u8]) -> String {
    format!(
        "{PREFIX}:{}:{}",
        B64.encode(license.canonical_bytes()),
        B64.encode(signature)
    )
}

/// Decode a string produced by [`encode_license`].
///
/// Decoding is not verification: it proves the string is well-formed, not that
/// anyone signed it.
pub fn decode_license(text: &str) -> Result<(License, Vec<u8>), VerifyError> {
    let rest = text
        .trim()
        .strip_prefix(PREFIX)
        .and_then(|r| r.strip_prefix(':'))
        .ok_or(VerifyError::Malformed)?;
    let (payload_b64, sig_b64) = rest.split_once(':').ok_or(VerifyError::Malformed)?;
    let payload = B64.decode(payload_b64).map_err(|_| VerifyError::Malformed)?;
    let signature = B64.decode(sig_b64).map_err(|_| VerifyError::Malformed)?;
    let license = parse_canonical(&payload)?;
    Ok((license, signature))
}

/// The whole check in one call: decode, verify the signature, then confirm the
/// license entitles `account_id` at `now`.
///
/// This is what the relay and the daemon should call, so that neither of them
/// can forget one of the three steps.
pub fn check_encoded_license(
    text: &str,
    pubkey: &[u8; 32],
    account_id: &str,
    now: u64,
) -> Result<License, VerifyError> {
    let (license, signature) = decode_license(text)?;
    verify_license(&license, &signature, pubkey)?;
    license.check_valid_at(now, account_id)?;
    Ok(license)
}

fn parse_canonical(payload: &[u8]) -> Result<License, VerifyError> {
    let mut cursor = 0usize;
    let account_id = read_str(payload, &mut cursor)?;
    let plan = read_str(payload, &mut cursor)?;
    let issued_at = read_u64(payload, &mut cursor)?;
    let not_after = read_u64(payload, &mut cursor)?;
    let serial = read_str(payload, &mut cursor)?;
    // Trailing bytes mean the payload is not the thing that was signed, so
    // refuse rather than ignore them.
    if cursor != payload.len() {
        return Err(VerifyError::Malformed);
    }
    Ok(License {
        account_id,
        plan,
        issued_at,
        not_after,
        serial,
    })
}

fn read_u32(payload: &[u8], cursor: &mut usize) -> Result<u32, VerifyError> {
    let end = cursor.checked_add(4).ok_or(VerifyError::Malformed)?;
    let bytes = payload.get(*cursor..end).ok_or(VerifyError::Malformed)?;
    *cursor = end;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| VerifyError::Malformed)?,
    ))
}

fn read_u64(payload: &[u8], cursor: &mut usize) -> Result<u64, VerifyError> {
    let end = cursor.checked_add(8).ok_or(VerifyError::Malformed)?;
    let bytes = payload.get(*cursor..end).ok_or(VerifyError::Malformed)?;
    *cursor = end;
    Ok(u64::from_le_bytes(
        bytes.try_into().map_err(|_| VerifyError::Malformed)?,
    ))
}

fn read_str(payload: &[u8], cursor: &mut usize) -> Result<String, VerifyError> {
    let len = read_u32(payload, cursor)? as usize;
    let end = cursor.checked_add(len).ok_or(VerifyError::Malformed)?;
    let bytes = payload.get(*cursor..end).ok_or(VerifyError::Malformed)?;
    *cursor = end;
    String::from_utf8(bytes.to_vec()).map_err(|_| VerifyError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> License {
        License {
            account_id: "ab12".into(),
            plan: "hush-monthly".into(),
            issued_at: 1_700_000_000,
            not_after: 1_700_086_400,
            serial: "lic_0001".into(),
        }
    }

    /// A fixed seed, so a failure is reproducible. Signing keys are only ever
    /// minted from a CSPRNG in the real service.
    fn keypair() -> (SigningKey, [u8; 32]) {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        (signing, pubkey)
    }

    #[test]
    fn round_trips_through_the_text_form() {
        let (lic, sig) = (sample(), vec![7u8; 64]);
        let encoded = encode_license(&lic, &sig);
        let (back, sig_back) = decode_license(&encoded).expect("decodes");
        assert_eq!(back, lic);
        assert_eq!(sig_back, sig);
    }

    #[test]
    fn canonical_bytes_are_stable() {
        assert_eq!(sample().canonical_bytes(), sample().canonical_bytes());
    }

    #[test]
    fn rejects_an_expired_license() {
        let err = sample().check_valid_at(1_700_086_401, "ab12").unwrap_err();
        assert_eq!(err, VerifyError::Expired);
    }

    #[test]
    fn accepts_a_license_at_the_instant_it_expires() {
        sample().check_valid_at(1_700_086_400, "ab12").unwrap();
    }

    #[test]
    fn rejects_a_license_bound_to_another_account() {
        let err = sample().check_valid_at(1_700_000_001, "ffff").unwrap_err();
        assert_eq!(err, VerifyError::WrongAccount);
    }

    #[test]
    fn a_valid_signature_verifies() {
        let (signing, pubkey) = keypair();
        let lic = sample();
        let sig = sign_license(&lic, &signing);
        verify_license(&lic, &sig, &pubkey).expect("verifies");
    }

    #[test]
    fn a_signature_over_a_mutated_payload_is_refused() {
        let (signing, pubkey) = keypair();
        let lic = sample();
        let sig = sign_license(&lic, &signing);
        let mut tampered = lic.clone();
        tampered.not_after = u64::MAX;
        assert_eq!(
            verify_license(&tampered, &sig, &pubkey).unwrap_err(),
            VerifyError::BadSignature
        );
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let (signing, _) = keypair();
        let lic = sample();
        let sig = sign_license(&lic, &signing);
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes();
        assert_eq!(
            verify_license(&lic, &sig, &other).unwrap_err(),
            VerifyError::BadSignature
        );
    }

    #[test]
    fn a_short_signature_is_refused_rather_than_panicking() {
        let (_, pubkey) = keypair();
        assert_eq!(
            verify_license(&sample(), &[1, 2, 3], &pubkey).unwrap_err(),
            VerifyError::BadSignature
        );
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        for bad in ["", "nope", "hush-lic-v1:", "hush-lic-v1:!!!:!!!", "hush-lic-v2:a:b"] {
            assert_eq!(decode_license(bad).unwrap_err(), VerifyError::Malformed, "{bad:?}");
        }
    }

    #[test]
    fn trailing_bytes_in_the_payload_are_refused() {
        let lic = sample();
        let mut payload = lic.canonical_bytes();
        payload.push(0);
        let encoded = format!("{PREFIX}:{}:{}", B64.encode(&payload), B64.encode([0u8; 64]));
        assert_eq!(decode_license(&encoded).unwrap_err(), VerifyError::Malformed);
    }

    #[test]
    fn the_whole_check_passes_for_a_good_license() {
        let (signing, pubkey) = keypair();
        let lic = sample();
        let encoded = encode_license(&lic, &sign_license(&lic, &signing));
        let back = check_encoded_license(&encoded, &pubkey, "ab12", 1_700_000_001).expect("entitled");
        assert_eq!(back, lic);
    }

    #[test]
    fn the_whole_check_refuses_a_license_for_another_account() {
        let (signing, pubkey) = keypair();
        let lic = sample();
        let encoded = encode_license(&lic, &sign_license(&lic, &signing));
        assert_eq!(
            check_encoded_license(&encoded, &pubkey, "ffff", 1_700_000_001).unwrap_err(),
            VerifyError::WrongAccount
        );
    }

    #[test]
    fn the_whole_check_refuses_a_resigned_license_from_an_unknown_key() {
        let (_, pubkey) = keypair();
        let attacker = SigningKey::from_bytes(&[3u8; 32]);
        let lic = sample();
        let encoded = encode_license(&lic, &sign_license(&lic, &attacker));
        assert_eq!(
            check_encoded_license(&encoded, &pubkey, "ab12", 1_700_000_001).unwrap_err(),
            VerifyError::BadSignature
        );
    }
}
