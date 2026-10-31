//! This device's LocalSend identity: a self-signed certificate whose SHA-256
//! fingerprint is how peers recognise it. Made like LocalSend's own (RSA-2048,
//! `CN=LocalSend User`) and kept for good: a new one would look like a new device.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Vec<u8>,
    /// Uppercase hex SHA-256 of the certificate.
    pub fingerprint: String,
}

impl Identity {
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }

    /// Load the identity from `dir`, creating it the first time.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let (cert_path, key_path) = (dir.join("cert.der"), dir.join("key.der"));
        if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            return Ok(Self::from_parts(cert, key));
        }
        let (cert, key) = generate().map_err(io::Error::other)?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let write = |path: &Path, bytes: &[u8]| -> io::Result<()> {
            let tmp = path.with_extension("tmp");
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            io::Write::write_all(&mut f, bytes)?;
            f.sync_all()?;
            std::fs::rename(tmp, path)
        };
        write(&key_path, &key)?;
        write(&cert_path, &cert)?;
        Ok(Self::from_parts(cert, key))
    }

    fn from_parts(cert: Vec<u8>, key: Vec<u8>) -> Self {
        let fingerprint = fingerprint(&cert);
        Self { cert: CertificateDer::from(cert), key, fingerprint }
    }
}

fn generate() -> Result<(Vec<u8>, Vec<u8>), String> {
    use rsa::pkcs8::EncodePrivateKey;
    let private = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).map_err(|e| e.to_string())?;
    let pkcs8 = private.to_pkcs8_der().map_err(|e| e.to_string())?;
    let key_pair = rcgen::KeyPair::try_from(pkcs8.as_bytes()).map_err(|e| e.to_string())?;
    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, "LocalSend User");
    let cert = params.self_signed(&key_pair).map_err(|e| e.to_string())?;
    Ok((cert.der().to_vec(), pkcs8.as_bytes().to_vec()))
}

/// LocalSend's fingerprint of a certificate: uppercase hex SHA-256 of its DER.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_localsends_fingerprint_format() {
        // The certificate and fingerprint from LocalSend's own server_cert_verifier tests.
        let pem = "MIIDGTCCAgGgAwIBAgIBATANBgkqhkiG9w0BAQsFADBQMRcwFQYDVQQDEw5Mb2NhbFNlbmQgVXNlcjEJMAcGA1UEChMAMQkwBwYDVQQLEwAxCTAHBgNVBAcTADEJMAcGA1UECBMAMQkwBwYDVQQGEwAwHhcNMjUwMjA5MDAwMzE0WhcNMzUwMjA3MDAwMzE0WjBQMRcwFQYDVQQDEw5Mb2NhbFNlbmQgVXNlcjEJMAcGA1UEChMAMQkwBwYDVQQLEwAxCTAHBgNVBAcTADEJMAcGA1UECBMAMQkwBwYDVQQGEwAwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCL24MxhGfrdJm0Q8ZGiBkZ27ldcEChB4w7rSbJyiKeosoNbJl2kyj5dZjfBhWGgDLGDMM5w+Mh/5SrWgTL/QrhbB+lsrxILLznWqBiR8wJP0P2YW9fBahQskJQcUXt/3jsCsMTWea4rWc3HZGh03bAkJfLM+PDSOfTpvAZ6DQSp9QLzC9bgVNnq3W0SvOZGpF0xRa4InCyTUgxsNsV4+GIrmN5w4EbRFVVYu7D5OS5fxNSCukiS0fb6oQzUp0vIAycvvWHHbAy8T6UMoUor2nfvNcryiaOX5WBMLyhyMZ5gMOyXjdm1bT1XSlvtXPYUzxvsGAzTqS8mXjw8h7mm5htAgMBAAEwDQYJKoZIhvcNAQELBQADggEBABZ+I7D6wkeSrsi1NBLP2zoZ5oGh+INNcGTravfOQHs4Fbas/CysaUYjsD3fmaDh4MxgWEqAmWnnBiojfpGX2SGuFqRBKyT9DgitBt0L7Ezg1k3hbfSiFW4hXWp75grVO8xfML7ZcWMlhKrOsOMUGiy1qs3qsyJ3w7B2Tz78HhXGO5ddjyPmZarhixKO92UpEvKGxjO0E/3UUNUzxKTAAgFfhKpuwHUgIijM/EppZtA8OcShfEztiV0xKfcPVx4d6dqRt/NMElK1Ivw2vUuxTymphZkkFOzht9m73/kyKaeFp8IjVRus1zGVD8IVpIdPMyz01WJyS7M0fWaHXKWo+Bo=";
        let der = base64(pem);
        assert_eq!(super::fingerprint(&der), "4BADDE53A7F7CDEEED93189FD898E02BF6B4806CA4C05DE0ACE08319B86552FA");
    }

    fn base64(s: &str) -> Vec<u8> {
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in s.bytes().filter(|&c| c != b'=') {
            acc = (acc << 6) | table.iter().position(|&t| t == c).unwrap() as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    #[test]
    fn creates_once_and_keeps_it() {
        let dir = std::env::temp_dir().join(format!("nyx-share-identity-{}", std::process::id()));
        let a = super::Identity::load_or_create(&dir).unwrap();
        let b = super::Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 64);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("key.der")).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
