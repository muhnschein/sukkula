//! F-LS2 and F-LS3: the TLS configurations.
//!
//! LocalSend peers carry self-signed certificates, so there is no authority
//! to chain to: a peer *is* the SHA-256 fingerprint of its certificate.
//!
//! - As a server we require a client certificate, as LocalSend does in HTTPS
//!   mode, and accept any certificate whose self-signature and validity
//!   period check out (the upstream crate's own check). The fingerprint of
//!   that certificate is what a registering peer's claimed fingerprint must
//!   equal.
//! - As a client we pin the certificate to the fingerprint the peer
//!   announced (F-LS3), during the handshake, so a peer that does not hold
//!   the announced certificate never receives a byte of the request. The
//!   verifier records a mismatch so the transfer can end with
//!   [`crate::api::ErrorCode::PeerMismatch`] rather than a generic error.
//!
//! Both sides use rustls with the ring provider, passed explicitly: the
//! process-wide default provider is never installed, because it is shared
//! with everything else in the process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use localsend::crypto::cert;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig,
    SignatureScheme,
};

use super::identity::Identity;
use crate::api::{ErrorCode, ErrorInfo};

/// The only application protocol spoken. HTTP/2 is not offered: LocalSend
/// does not use it, and every protocol not spoken is surface not exposed.
const ALPN_HTTP1: &[u8] = b"http/1.1";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn config_error(_: Error) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, "TLS configuration failed")
}

/// Checks that `key` is the private half of `cert`.
pub(super) fn check_pair(
    cert: &CertificateDer<'static>,
    key: &PrivateKeyDer<'static>,
) -> Result<(), &'static str> {
    let mismatch = "key does not match the certificate";
    let pair =
        CertifiedKey::from_der(vec![cert.clone()], key.clone_key(), &provider()).map_err(|e| {
            match e {
                Error::InconsistentKeys(_) => mismatch,
                _ => "key unusable",
            }
        })?;
    // `from_der` lets a pair whose match cannot be told through; ours must.
    pair.keys_match().map_err(|_| mismatch)
}

/// The server side: our certificate, peers' certificates required.
pub(super) fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>, ErrorInfo> {
    let provider = provider();
    let verifier = Arc::new(AnySelfSigned {
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(config_error)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.chain(), identity.key())
        .map_err(config_error)?;
    config.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
    Ok(Arc::new(config))
}

/// The client side, presenting our certificate and checking the peer's with
/// `verifier`.
pub(super) fn client_config(
    identity: &Identity,
    verifier: Arc<PinnedServer>,
) -> Result<Arc<ClientConfig>, ErrorInfo> {
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(config_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(identity.chain(), identity.key())
        .map_err(config_error)?;
    config.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
    // Peers are addressed by IP; there is no name to indicate.
    config.enable_sni = false;
    Ok(Arc::new(config))
}

/// Accepts any client certificate that is a valid self-signed certificate,
/// as LocalSend does; identity comes from its fingerprint, checked later.
#[derive(Debug)]
struct AnySelfSigned {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for AnySelfSigned {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        cert::verify_cert_from_der(end_entity.as_ref(), None)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Checks a server's certificate: valid and self-signed, and -- when
/// `expected` is set -- carrying exactly that fingerprint (F-LS3).
///
/// `expected: None` is trust on first use, only for asking a known address
/// who is there; the fingerprint seen is recorded and becomes the pin for
/// everything sent to that peer afterwards.
#[derive(Debug)]
pub(super) struct PinnedServer {
    expected: Option<String>,
    algorithms: WebPkiSupportedAlgorithms,
    mismatch: AtomicBool,
    seen: Mutex<Option<String>>,
}

impl PinnedServer {
    /// A verifier for one connection. `expected` is compared uppercase.
    pub(super) fn new(expected: Option<&str>) -> Arc<PinnedServer> {
        Arc::new(PinnedServer {
            expected: expected.map(str::to_ascii_uppercase),
            algorithms: provider().signature_verification_algorithms,
            mismatch: AtomicBool::new(false),
            seen: Mutex::new(None),
        })
    }

    /// Whether the handshake failed because the certificate was not the
    /// pinned one.
    pub(super) fn mismatched(&self) -> bool {
        self.mismatch.load(Ordering::Acquire)
    }

    /// The fingerprint of the certificate the server presented, once the
    /// handshake got that far.
    pub(super) fn seen(&self) -> Option<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // The name is ignored on purpose: peers are addressed by IP and
        // their certificates carry no matching name. The fingerprint is the
        // identity.
        cert::verify_cert_from_der(end_entity.as_ref(), None)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))?;
        let actual = cert::fingerprint_from_cert_der(end_entity.as_ref());
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(actual.clone());
        if let Some(expected) = &self.expected
            && *expected != actual
        {
            self.mismatch.store(true, Ordering::Release);
            return Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::pem::PemObject;

    fn der(pem: &str) -> CertificateDer<'static> {
        CertificateDer::from_pem_slice(pem.as_bytes()).unwrap()
    }

    fn check(verifier: &PinnedServer, cert: &CertificateDer<'_>) -> Result<(), Error> {
        verifier
            .verify_server_cert(
                cert,
                &[],
                &ServerName::try_from("192.168.1.2").unwrap(),
                &[],
                UnixTime::now(),
            )
            .map(|_| ())
    }

    #[test]
    fn the_pinned_certificate_passes_and_any_other_is_refused() {
        let [a, b] = crate::localsend::identity::tests::test_pairs();

        let pinned = PinnedServer::new(Some(&a.fingerprint.to_ascii_lowercase()));
        assert!(check(&pinned, &der(&a.certificate_pem)).is_ok());
        assert!(!pinned.mismatched());

        let pinned = PinnedServer::new(Some(&a.fingerprint));
        assert!(check(&pinned, &der(&b.certificate_pem)).is_err());
        assert!(pinned.mismatched());
        assert_eq!(pinned.seen(), Some(b.fingerprint.clone()));

        let unpinned = PinnedServer::new(None);
        assert!(check(&unpinned, &der(&b.certificate_pem)).is_ok());
        assert_eq!(unpinned.seen(), Some(b.fingerprint.clone()));
    }

    #[test]
    fn garbage_certificates_are_refused_without_counting_as_a_mismatch() {
        let pinned = PinnedServer::new(Some(&"0".repeat(64)));
        let junk = CertificateDer::from(vec![0x30, 0x03, 0x02, 0x01, 0x00]);
        assert!(check(&pinned, &junk).is_err());
        assert!(!pinned.mismatched());
        let client = AnySelfSigned {
            algorithms: provider().signature_verification_algorithms,
        };
        assert!(
            client
                .verify_client_cert(&junk, &[], UnixTime::now())
                .is_err()
        );
    }
}
