//! F-LS2 and S9: this device's TLS identity.
//!
//! LocalSend peers know each other by the SHA-256 fingerprint of a
//! self-signed certificate. Ours is generated on first use with the upstream
//! crate's own generator (RSA-2048, like every LocalSend app) and kept in the
//! app data directory through `sukkula_core::store`, which writes `0600` in a
//! `0700` directory and refuses to read a secret anyone else could read.
//!
//! A certificate or key that cannot be used -- missing half, exposed,
//! malformed, expired, or not a pair -- is replaced by a fresh one. That
//! changes our fingerprint, which peers treat as a new device; it never
//! makes us use a key someone else may have read. The reason is logged, the
//! key never is: [`Identity`] has no `Debug` that could print it.

use std::fmt;

use localsend::crypto::cert;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sukkula_core::store::{Store, StoreError};

use crate::api::{ErrorCode, ErrorInfo};

/// The certificate, PEM.
const CERT_FILE: &str = "localsend-cert.pem";

/// The private key, PEM (PKCS#8). A secret.
const KEY_FILE: &str = "localsend-key.pem";

/// Largest PEM file read. An RSA-2048 key is about 1.7 KiB, its certificate
/// about 1.1 KiB.
const MAX_PEM_BYTES: usize = 16 * 1024;

/// Our certificate and key, parsed and checked to belong together.
pub(super) struct Identity {
    /// The certificate, DER.
    cert: CertificateDer<'static>,
    /// The private key, DER. Never logged, never leaves the process except
    /// as a TLS signature.
    key: PrivateKeyDer<'static>,
    /// Uppercase hex SHA-256 of `cert`, the form LocalSend announces.
    fingerprint: String,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // S9: the key is not part of any rendering of this type.
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Our fingerprint, uppercase hex.
    pub(super) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The certificate chain to present: just ours.
    pub(super) fn chain(&self) -> Vec<CertificateDer<'static>> {
        vec![self.cert.clone()]
    }

    /// The private key, for rustls.
    pub(super) fn key(&self) -> PrivateKeyDer<'static> {
        self.key.clone_key()
    }

    fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Identity, &'static str> {
        let cert = CertificateDer::from_pem_slice(cert_pem).map_err(|_| "malformed certificate")?;
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| "malformed key")?;
        // Self-signature and validity period, as peers will check them.
        cert::verify_cert_from_der(cert.as_ref(), None)
            .map_err(|_| "certificate invalid or expired")?;
        // rustls compares the key's public half with the certificate's; a
        // mismatched pair would fail every handshake.
        super::tls::check_pair(&cert, &key)?;
        let fingerprint = cert::fingerprint_from_cert_der(cert.as_ref());
        Ok(Identity {
            cert,
            key,
            fingerprint,
        })
    }
}

/// Loads the identity, or makes and stores a new one. Blocking: file I/O and,
/// on first run, RSA key generation. Run it off the async threads.
///
/// # Errors
///
/// The key could not be generated or stored.
pub(super) fn load_or_create(store: &Store) -> Result<Identity, ErrorInfo> {
    match load(store) {
        Ok(Some(identity)) => return Ok(identity),
        Ok(None) => tracing::debug!("no LocalSend identity yet; generating one"),
        // The reason names the file and what is wrong with it, never its
        // contents (S9).
        Err(reason) => {
            tracing::warn!(reason, "LocalSend identity unusable; generating a new one");
        }
    }
    let generated = cert::generate_self_signed()
        .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "could not generate a TLS key"))?;
    let stored = ErrorInfo::new(ErrorCode::Storage, "could not store the TLS key");
    // The key first: a crash between the two writes leaves a pair that does
    // not match, which the next start replaces.
    store
        .write(KEY_FILE, generated.private_key_pem.as_bytes())
        .map_err(|_| stored.clone())?;
    store
        .write(CERT_FILE, generated.certificate_pem.as_bytes())
        .map_err(|_| stored)?;
    Identity::from_pem(
        generated.certificate_pem.as_bytes(),
        generated.private_key_pem.as_bytes(),
    )
    .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "the generated TLS key is unusable"))
}

/// `Ok(None)`: nothing stored yet. `Err`: something stored, but unusable.
fn load(store: &Store) -> Result<Option<Identity>, &'static str> {
    let key = store.read_secret(KEY_FILE, MAX_PEM_BYTES).map_err(why)?;
    let cert = store.read(CERT_FILE, MAX_PEM_BYTES).map_err(why)?;
    match (cert, key) {
        (None, None) => Ok(None),
        (Some(cert), Some(key)) => Identity::from_pem(&cert, &key).map(Some),
        _ => Err("only one of certificate and key is present"),
    }
}

fn why(e: StoreError) -> &'static str {
    match e {
        StoreError::Exposed(_) => "the key is readable by other users",
        StoreError::NotRegular(_) => "not a regular file owned by this user",
        StoreError::TooLarge(_) => "file too large",
        StoreError::Directory(_) => "data directory unusable",
        StoreError::BadName | StoreError::Malformed(..) | StoreError::Io(_) => "unreadable",
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn generated_once_then_reused_with_private_modes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        let first = load_or_create(&store).unwrap();
        assert_eq!(first.fingerprint().len(), 64);
        assert_eq!(mode(&store.dir().join(KEY_FILE)), 0o600);
        assert_eq!(mode(&store.dir().join(CERT_FILE)), 0o600);
        let second = load_or_create(&store).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        // S9: nothing that renders the identity shows the key.
        let shown = format!("{first:?}");
        assert!(!shown.contains("PRIVATE"), "{shown}");
        assert!(shown.contains(first.fingerprint()));
    }

    /// Two pre-made pairs, shared by the tests of this module and `tls`:
    /// RSA key generation takes seconds in a debug build.
    pub(crate) fn test_pairs() -> &'static [cert::SelfSignedCert; 2] {
        static PAIRS: std::sync::OnceLock<[cert::SelfSignedCert; 2]> = std::sync::OnceLock::new();
        PAIRS.get_or_init(|| {
            let other = std::thread::spawn(|| cert::generate_self_signed().unwrap());
            let one = cert::generate_self_signed().unwrap();
            [one, other.join().unwrap()]
        })
    }

    /// Pre-made pair `i` as an identity.
    pub(crate) fn pair(i: usize) -> Identity {
        let p = &test_pairs()[i];
        Identity::from_pem(p.certificate_pem.as_bytes(), p.private_key_pem.as_bytes()).unwrap()
    }

    /// Writes pair `cert_of`'s certificate and pair `key_of`'s key.
    pub(crate) fn seed(store: &Store, cert_of: usize, key_of: usize) {
        let pairs = test_pairs();
        store
            .write(CERT_FILE, pairs[cert_of].certificate_pem.as_bytes())
            .unwrap();
        store
            .write(KEY_FILE, pairs[key_of].private_key_pem.as_bytes())
            .unwrap();
    }

    #[test]
    fn a_stored_pair_is_used_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        seed(&store, 0, 0);
        let loaded = load_or_create(&store).unwrap();
        assert_eq!(loaded.fingerprint(), test_pairs()[0].fingerprint);
    }

    #[test]
    fn an_exposed_key_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        seed(&store, 0, 0);
        std::fs::set_permissions(
            dir.path().join(KEY_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(
            load(&store).unwrap_err(),
            "the key is readable by other users"
        );
        let replaced = load_or_create(&store).unwrap();
        assert_ne!(replaced.fingerprint(), test_pairs()[0].fingerprint);
        assert_eq!(mode(&dir.path().join(KEY_FILE)), 0o600);
    }

    #[test]
    fn broken_mismatched_or_half_pairs_are_unusable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        seed(&store, 0, 0);
        store.write(CERT_FILE, b"not a certificate").unwrap();
        assert_eq!(load(&store).unwrap_err(), "malformed certificate");

        // A valid certificate with somebody else's key.
        seed(&store, 0, 1);
        assert_eq!(
            load(&store).unwrap_err(),
            "key does not match the certificate"
        );

        seed(&store, 0, 0);
        std::fs::remove_file(dir.path().join(CERT_FILE)).unwrap();
        assert_eq!(
            load(&store).unwrap_err(),
            "only one of certificate and key is present"
        );

        // A symlink where the key should be is not followed.
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", dir.path().join(KEY_FILE)).unwrap();
        assert_eq!(
            load(&store).unwrap_err(),
            "not a regular file owned by this user"
        );
    }
}
