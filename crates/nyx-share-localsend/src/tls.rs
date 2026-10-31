//! TLS as LocalSend uses it: self-signed certificates on both sides, trusted by
//! fingerprint. A sender pins the receiver's fingerprint from discovery; a
//! receiver learns the sender's from its client certificate.

use crate::identity::{Identity, fingerprint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};
use std::sync::Arc;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn algorithms() -> WebPkiSupportedAlgorithms {
    rustls::crypto::ring::default_provider().signature_verification_algorithms
}

/// Accepts exactly the certificate with this fingerprint. The handshake
/// signature still proves the peer holds its private key.
#[derive(Debug)]
struct Pinned {
    expected: String,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if fingerprint(cert) == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &algorithms())
    }

    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

/// Asks every client for a certificate and takes any; its fingerprint is
/// checked against what the client claims, per request.
#[derive(Debug)]
struct AnyClient;

impl ClientCertVerifier for AnyClient {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &algorithms())
    }

    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

/// For talking to the peer with `fingerprint`, presenting our certificate.
pub fn client(identity: &Identity, fingerprint: &str) -> Result<Arc<rustls::ClientConfig>, Error> {
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { expected: fingerprint.to_ascii_uppercase() }))
        .with_client_auth_cert(vec![identity.cert.clone()], identity.key())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

pub fn server(identity: &Identity) -> Result<Arc<rustls::ServerConfig>, Error> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(Arc::new(AnyClient))
        .with_single_cert(vec![identity.cert.clone()], identity.key())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The fingerprint of the certificate a client presented, if any.
pub fn peer_fingerprint(conn: &rustls::ServerConnection) -> Option<String> {
    conn.peer_certificates().and_then(|c| c.first()).map(|c| fingerprint(c))
}
