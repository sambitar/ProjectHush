//! TLS configuration for the IRC client.
//!
//! The rustls provider is named explicitly (`ring`) rather than left to the
//! process default: other parts of the workspace also pull in rustls, and with
//! more than one provider compiled in, `ClientConfig::builder()` panics at
//! runtime. Naming the provider makes this module independent of that.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

/// Whether a host is a Tor onion service. Onion hosts default to plaintext
/// (Tor already encrypts end to end); if TLS is forced on one, its certificate
/// cannot be publicly trusted, so the onion name itself is the authentication.
pub fn is_onion(host: &str) -> bool {
    host.to_ascii_lowercase().ends_with(".onion")
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Roots for ordinary hosts: the platform trust store, falling back to the
/// bundled Mozilla set when the platform store cannot be read.
fn roots() -> RootCertStore {
    let mut store = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = store.add(cert);
    }
    if store.is_empty() {
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    store
}

/// The normal client config: platform-trusted roots, full verification.
pub fn client_config() -> Arc<ClientConfig> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("safe default protocol versions")
        .with_root_certificates(roots())
        .with_no_client_auth();
    Arc::new(config)
}

/// The onion-only config: the certificate is accepted without chain
/// verification, because a `.onion` address has no CA and its authenticity is
/// provided by Tor's own end-to-end connection to the service.
pub fn onion_config() -> Arc<ClientConfig> {
    let provider = provider();
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("safe default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    Arc::new(config)
}

/// Accepts any certificate. Signature checks still run, so the handshake is
/// still cryptographically sound; only the (unavailable) chain of trust is
/// skipped. Used solely for `.onion` hosts.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_onion_hosts() {
        assert!(is_onion("abcdefghijklmnop.onion"));
        assert!(is_onion("Example.ONION"));
        assert!(!is_onion("irc.libera.chat"));
    }

    #[test]
    fn builds_both_configs() {
        // Exercises the provider/roots path; a panic here would be a runtime
        // failure on the first connection.
        let _ = client_config();
        let _ = onion_config();
    }
}
