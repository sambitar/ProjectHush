//! The device's Libera client certificate: the `EXTERNAL` half of SASL.
//!
//! Libera's Tor onion refuses password authentication outright. It accepts only
//! a public-key mechanism, and `EXTERNAL` means a client certificate whose
//! SHA-512 fingerprint the account has authorized. Their guide is the reference:
//! <https://libera.chat/guides/certfp>
//!
//! The certificate is password-equivalent — Libera says so outright — so the
//! private key is never written to a plain file. It is sealed in a vault
//! sidecar (the daemon holds the vault key) and lives in memory only while a
//! connection is being set up. The fingerprint is public, and is the one thing
//! the user has to hand to NickServ.
//!
//! Nothing here registers anything with Libera: `CERT ADD` runs over a live,
//! already-identified connection (see [`authorize_line`]), because adding a
//! fingerprint to an account requires being logged in as that account.

use base64::Engine;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use crate::daemon_client::DaemonClient;

/// The vault sidecar holding this device's certificate. One per vault: a second
/// certificate would be a second credential to authorize, not a better one.
pub const VAULT_SECRET: &str = "irc-client-cert.json";

/// A certificate loaded and ready for a TLS handshake.
pub struct ClientIdentity {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    /// Lowercase hex SHA-512 of the DER certificate: exactly what
    /// `NickServ CERT ADD` takes, and what `WHOIS` prints as the 276 numeric.
    pub fingerprint_sha512: String,
}

/// Never prints the key. This type ends up inside `ServerConfig`, which is
/// `Debug`, and connection configs reach logs and error strings.
impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("fingerprint_sha512", &self.fingerprint_sha512)
            .finish_non_exhaustive()
    }
}

/// The sidecar's JSON. DER blobs in base64 rather than PEM, because the key is
/// only ever read back by this module and PEM would add a parser for nothing.
#[derive(Serialize, Deserialize)]
struct StoredCert {
    cert_der_base64: String,
    key_der_base64: String,
}

/// Mint a self-signed certificate for this device.
///
/// ECDSA P-256 rather than the Ed25519 their guide suggests: both are accepted
/// (Libera's own fallback for clients without Ed25519 is RSA), and P-256 is the
/// algorithm every TLS stack in the path already speaks, so a handshake cannot
/// fail over signature negotiation. The subject is a label, not an identity —
/// the fingerprint is what the account matches on.
pub fn generate() -> Result<ClientIdentity, String> {
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|e| format!("could not generate a certificate key: {e}"))?;
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Hush IRC client");
    // No explicit validity window: rcgen's default (1975-4096) never expires,
    // and Libera no longer checks expiry on client certificates.
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("could not sign the certificate: {e}"))?;

    let cert_der = cert.der().to_vec();
    Ok(ClientIdentity {
        fingerprint_sha512: fingerprint_sha512(&cert_der),
        chain: vec![CertificateDer::from(cert_der)],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())),
    })
}

/// The SHA-512 fingerprint of a DER certificate, lowercase hex.
///
/// Libera prints the same value in `WHOIS` (the 276 numeric) and takes it as an
/// argument to `NickServ CERT ADD`, so this is the string the user compares.
pub fn fingerprint_sha512(cert_der: &[u8]) -> String {
    let digest = Sha512::digest(cert_der);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Serialize a certificate for the vault sidecar.
pub fn encode(identity: &ClientIdentity) -> Result<String, String> {
    let cert_der = identity
        .chain
        .first()
        .ok_or_else(|| "the certificate has no DER body".to_string())?;
    let key_der = identity.key.secret_der();
    serde_json::to_string(&StoredCert {
        cert_der_base64: base64::engine::general_purpose::STANDARD.encode(cert_der.as_ref()),
        key_der_base64: base64::engine::general_purpose::STANDARD.encode(key_der),
    })
    .map_err(|e| format!("could not encode the certificate: {e}"))
}

/// Read back a certificate written by [`encode`].
pub fn decode(json: &str) -> Result<ClientIdentity, String> {
    let stored: StoredCert =
        serde_json::from_str(json).map_err(|e| format!("the stored certificate is unreadable: {e}"))?;
    let cert_der = base64::engine::general_purpose::STANDARD
        .decode(&stored.cert_der_base64)
        .map_err(|e| format!("the stored certificate body is not base64: {e}"))?;
    let key_der = base64::engine::general_purpose::STANDARD
        .decode(&stored.key_der_base64)
        .map_err(|e| format!("the stored certificate key is not base64: {e}"))?;
    if cert_der.is_empty() || key_der.is_empty() {
        return Err("the stored certificate is empty".into());
    }
    Ok(ClientIdentity {
        fingerprint_sha512: fingerprint_sha512(&cert_der),
        chain: vec![CertificateDer::from(cert_der)],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
    })
}

/// The line that authorizes this certificate on the account, sent over a live
/// connection the user has already identified with. Atheme accepts the
/// fingerprint as an argument, which is why this works from a connection
/// authenticated by password rather than by the certificate itself.
pub fn authorize_line(fingerprint: &str) -> String {
    format!("PRIVMSG NickServ :CERT ADD {fingerprint}")
}

/// The daemon's sealed copy of this device's certificate, if one was minted.
pub async fn load(client: &DaemonClient) -> Result<Option<ClientIdentity>, String> {
    let reply = client
        .request(
            "GET",
            &format!("/api/vault/secret/{VAULT_SECRET}"),
            None,
        )
        .await?;
    if reply.status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "the daemon would not read the certificate (HTTP {})",
            reply.status
        ));
    }
    let value: SecretValue = serde_json::from_str(&reply.body)
        .map_err(|e| format!("the daemon's certificate reply is unreadable: {e}"))?;
    let json = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&value.value_base64)
            .map_err(|e| format!("the daemon's certificate value is not base64: {e}"))?,
    )
    .map_err(|e| format!("the stored certificate is not UTF-8: {e}"))?;
    decode(&json).map(Some)
}

/// Seal a certificate into the vault, replacing any previous one.
pub async fn store(client: &DaemonClient, identity: &ClientIdentity) -> Result<(), String> {
    let json = encode(identity)?;
    let body = serde_json::json!({
        "value_base64": base64::engine::general_purpose::STANDARD.encode(json.as_bytes()),
    })
    .to_string();
    let reply = client
        .request(
            "PUT",
            &format!("/api/vault/secret/{VAULT_SECRET}"),
            Some(body),
        )
        .await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "the daemon would not seal the certificate (HTTP {})",
            reply.status
        ));
    }
    Ok(())
}

/// Forget this device's certificate. The account keeps its authorized
/// fingerprint until the user removes it with `NickServ CERT DEL`, which only
/// matters if the key is being retired rather than replaced.
pub async fn forget(client: &DaemonClient) -> Result<(), String> {
    let reply = client
        .request(
            "DELETE",
            &format!("/api/vault/secret/{VAULT_SECRET}"),
            None,
        )
        .await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "the daemon would not forget the certificate (HTTP {})",
            reply.status
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SecretValue {
    value_base64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_certificate_has_the_fingerprint_form_liberas_commands_want() {
        let identity = generate().expect("a certificate is minted");
        // 64 bytes of SHA-512, lowercase hex, no separators: what `WHOIS`
        // prints and what `CERT ADD` takes.
        assert_eq!(identity.fingerprint_sha512.len(), 128);
        assert!(
            identity
                .fingerprint_sha512
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "the fingerprint must be lowercase hex: {}",
            identity.fingerprint_sha512
        );
    }

    #[test]
    fn the_fingerprint_is_the_sha512_of_the_certificate_body() {
        let identity = generate().expect("a certificate is minted");
        let cert_der = identity.chain[0].as_ref();
        let expected = fingerprint_sha512(cert_der);
        assert_eq!(identity.fingerprint_sha512, expected);
        // And a different body gives a different fingerprint, so this is not
        // accidentally hashing a constant.
        assert_ne!(expected, fingerprint_sha512(b"not a certificate"));
    }

    #[test]
    fn a_sealed_certificate_survives_the_round_trip_with_its_key() {
        let identity = generate().expect("a certificate is minted");
        let json = encode(&identity).expect("it encodes");
        let restored = decode(&json).expect("it decodes");
        assert_eq!(restored.fingerprint_sha512, identity.fingerprint_sha512);
        assert_eq!(
            restored.key.secret_der(),
            identity.key.secret_der(),
            "the private key must come back byte for byte"
        );
        assert_eq!(restored.chain[0].as_ref(), identity.chain[0].as_ref());
    }

    #[test]
    fn a_corrupt_sidecar_is_an_error_not_a_silent_empty_certificate() {
        assert!(decode("not json").is_err());
        assert!(decode("{}").is_err());
        assert!(decode(r#"{"cert_der_base64":"","key_der_base64":""}"#).is_err());
        assert!(decode(r#"{"cert_der_base64":"!!!","key_der_base64":"AAAA"}"#).is_err());
    }

    #[test]
    fn the_authorize_line_names_nickserv_and_carries_the_fingerprint() {
        let line = authorize_line("abc123");
        assert_eq!(line, "PRIVMSG NickServ :CERT ADD abc123");
    }

    #[test]
    fn debug_prints_the_fingerprint_and_never_the_key() {
        let identity = generate().expect("a certificate is minted");
        let key_der = identity.key.secret_der().to_vec();
        let rendered = format!("{identity:?}");
        assert!(rendered.contains(&identity.fingerprint_sha512));
        // The DER would not appear as literal text either way, but the base64 of
        // it is what a careless `Debug` derive would print.
        let key_b64 = base64::engine::general_purpose::STANDARD.encode(&key_der);
        assert!(!rendered.contains(&key_b64), "the key must not be printable");
        assert!(!rendered.contains(&format!("{key_der:?}")));
    }
}
