//! Trusting the server by its certificate's SHA-256 fingerprint, so a self-signed certificate
//! works without installing any CA.

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Normalize a fingerprint as printed by `openssl x509 -fingerprint -sha256` or as plain hex.
pub fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let value = s.rsplit_once('=').map_or(s, |(_, v)| v);
    let hex: String = value.chars().filter(|c| *c != ':' && !c.is_whitespace()).collect();
    hex::decode(hex).ok()?.try_into().ok()
}

pub fn fingerprint(cert: &[u8]) -> [u8; 32] {
    Sha256::digest(cert).into()
}

#[derive(Debug)]
struct PinnedVerifier {
    pin: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // The pin replaces CA, name and expiry checks: only this exact certificate is accepted.
        if fingerprint(end_entity) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "server certificate fingerprint {} doesn't match the pinned one",
                hex::encode(fingerprint(end_entity))
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A TLS client config that accepts only the certificate with this fingerprint.
pub fn pinned_config(pin: [u8; 32]) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { pin, provider }))
        .with_no_client_auth()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssl_and_plain_fingerprints() {
        let plain = "ab".repeat(32);
        let openssl = format!("sha256 Fingerprint={}", vec!["AB"; 32].join(":"));
        assert_eq!(parse_fingerprint(&plain), Some([0xab; 32]));
        assert_eq!(parse_fingerprint(&openssl), Some([0xab; 32]));
        assert_eq!(parse_fingerprint("abcd"), None);
    }
}
