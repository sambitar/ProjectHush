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

use crate::irc::cert::ClientIdentity;

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

/// The client config for one host: platform roots for a normal host, the
/// relaxed onion verifier for a `.onion`, and the device's certificate offered
/// when there is one.
///
/// `identity` is what makes SASL `EXTERNAL` possible — the certificate has to
/// be presented in the handshake, before any IRC line is exchanged, which is
/// why it is threaded through here rather than into the SASL exchange.
pub fn config_for(
    host: &str,
    identity: Option<&ClientIdentity>,
) -> Result<Arc<ClientConfig>, String> {
    if is_onion(host) {
        onion_config(identity)
    } else {
        client_config(identity)
    }
}

/// The normal client config: platform-trusted roots, full verification.
pub fn client_config(identity: Option<&ClientIdentity>) -> Result<Arc<ClientConfig>, String> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("safe default protocol versions")
        .with_root_certificates(roots());
    Ok(Arc::new(finish(builder, identity)?))
}

/// The onion-only config: the certificate is accepted without chain
/// verification, because a `.onion` address has no CA and its authenticity is
/// provided by Tor's own end-to-end connection to the service.
pub fn onion_config(identity: Option<&ClientIdentity>) -> Result<Arc<ClientConfig>, String> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("safe default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)));
    Ok(Arc::new(finish(builder, identity)?))
}

/// Attach the device's certificate, or none.
///
/// A failure here means the sealed sidecar holds something rustls will not
/// accept as a client certificate, which is worth surfacing as a connect error
/// rather than panicking the connection task.
fn finish(
    builder: rustls::ConfigBuilder<ClientConfig, rustls::client::WantsClientCert>,
    identity: Option<&ClientIdentity>,
) -> Result<ClientConfig, String> {
    match identity {
        Some(identity) => builder
            .with_client_auth_cert(identity.chain.clone(), identity.key.clone_key())
            .map_err(|e| format!("this device's certificate cannot be used: {e}")),
        None => Ok(builder.with_no_client_auth()),
    }
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
        let _ = client_config(None).expect("a plain client config");
        let _ = onion_config(None).expect("a plain onion config");
    }

    /// The certificate is minted here, sealed by the daemon, and read back
    /// before a connection — so rustls has to accept it as a client certificate.
    /// If it did not, SASL EXTERNAL could never work and the failure would only
    /// show up on the onion, where it is hardest to read.
    #[test]
    fn a_connection_with_an_identity_offers_the_certificate_and_one_without_does_not() {
        let identity = crate::irc::cert::generate().expect("a certificate is minted");
        for host in ["irc.libera.chat", crate::irc::LIBERA_ONION_HOST] {
            let with = config_for(host, Some(&identity))
                .unwrap_or_else(|e| panic!("{host} must accept the device certificate: {e}"));
            assert!(
                with.client_auth_cert_resolver.has_certs(),
                "{host}: the handshake must present the certificate, or EXTERNAL has nothing to match"
            );

            let without = config_for(host, None).expect("a config without a certificate");
            assert!(
                !without.client_auth_cert_resolver.has_certs(),
                "{host}: no certificate was asked for, so none may be sent"
            );
        }
    }
}
