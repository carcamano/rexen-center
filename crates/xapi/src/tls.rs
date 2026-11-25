//! Trust-on-first-use (TOFU) certificate pinning.
//!
//! XCP-ng hosts ship self-signed certificates, so instead of CA validation the
//! client pins the SHA-256 fingerprint of each host's end-entity certificate,
//! after the user confirmed it once. The handshake signature is still verified
//! against the pinned certificate, so a different key cannot impersonate it.
//!
//! A rejected certificate never lets the request through: the verifier records
//! the fingerprint it saw so the caller can show it and retry once trusted.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

/// SHA-256 fingerprint of a DER certificate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    /// Fingerprint of a DER-encoded certificate.
    pub fn of_der(der: &[u8]) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Self(out)
    }
}

impl fmt::Display for Fingerprint {
    /// Colon-separated upper-case hex, e.g. `AB:CD:…`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// Pinned certificates, keyed by `host:port` ([`crate::Endpoint::authority`]).
///
/// Cheap to clone; clones share the same set. Phase F1 keeps it in memory for
/// the session; persistence arrives with the settings work in F2.
#[derive(Clone, Default)]
pub struct TrustStore {
    inner: Arc<Mutex<HashMap<String, Fingerprint>>>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trusts `fingerprint` for `authority`, replacing any previous pin.
    pub fn trust(&self, authority: &str, fingerprint: Fingerprint) {
        self.lock().insert(authority.to_owned(), fingerprint);
    }

    /// The pinned fingerprint for `authority`, if any.
    pub fn get(&self, authority: &str) -> Option<Fingerprint> {
        self.lock().get(authority).copied()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Fingerprint>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// rustls verifier that accepts exactly the certificate pinned for one host.
#[derive(Debug)]
pub(crate) struct PinnedVerifier {
    pinned: Option<Fingerprint>,
    /// Fingerprint of the last certificate this verifier rejected.
    rejected: Mutex<Option<Fingerprint>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedVerifier {
    pub(crate) fn new(pinned: Option<Fingerprint>, provider: &CryptoProvider) -> Self {
        Self {
            pinned,
            rejected: Mutex::new(None),
            algorithms: provider.signature_verification_algorithms,
        }
    }

    /// Takes the fingerprint of the last rejected certificate.
    pub(crate) fn take_rejected(&self) -> Option<Fingerprint> {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let seen = Fingerprint::of_der(end_entity.as_ref());
        if self.pinned == Some(seen) {
            Ok(ServerCertVerified::assertion())
        } else {
            *self.rejected.lock().unwrap_or_else(PoisonError::into_inner) = Some(seen);
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_formats_as_colon_hex() {
        let fp = Fingerprint::of_der(b"abc");
        let s = fp.to_string();
        // SHA-256("abc") = ba7816bf…
        assert!(s.starts_with("BA:78:16:BF:"), "{s}");
        assert_eq!(s.len(), 32 * 3 - 1);
    }

    #[test]
    fn trust_store_is_shared_between_clones() {
        let a = TrustStore::new();
        let b = a.clone();
        let fp = Fingerprint([7; 32]);
        a.trust("h:443", fp);
        assert_eq!(b.get("h:443"), Some(fp));
        assert_eq!(b.get("other:443"), None);
    }
}
