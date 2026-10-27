//! This peer's key pair and self-signed certificate.

use crate::Fingerprint;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519, PublicKeyData};
use rustls::crypto::ring::sign::any_eddsa_type;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

const KEY_FILE: &str = "key.der";
const CERT_FILE: &str = "cert.der";
/// Subject alternative name in the certificate. Nothing checks it: peers are pinned by
/// fingerprint, never by name.
const CERT_NAME: &str = "ferry";
/// Largest key file read. An Ed25519 PKCS#8 key is 48 or 83 bytes; the cap keeps the read
/// buffer a single allocation, so no reallocation leaves an unwiped copy behind.
const MAX_KEY_FILE: usize = 4096;

/// Loading or creating the identity failed.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Ferryd refuses to start on a group- or world-accessible key or key directory.
    #[error(
        "{path} has mode {mode:04o}; it must not be accessible by group or others (chmod go-rwx)"
    )]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("{path}: not a usable Ed25519 PKCS#8 key: {reason}")]
    BadKey { path: PathBuf, reason: String },
    #[error("certificate generation failed: {0}")]
    Cert(String),
}

/// This peer's TLS identity: an Ed25519 key and a self-signed certificate.
///
/// The key is never logged, and `Debug` prints the fingerprint only.
///
/// **Key material in memory.** The PKCS#8 bytes exist only while the identity is loaded or
/// created: every buffer ferry-net owns for them is wiped on drop ([`zeroize`]). What remains
/// for the life of the value is the parsed signing key inside rustls (ring's Ed25519 key
/// pair), built once and shared by every handshake, which neither ring nor rustls wipes, and the
/// stateless-reset key derived from it (ring's HMAC key, also not wiped).
pub struct Identity {
    cert: CertificateDer<'static>,
    /// The QUIC stateless-reset key, derived from the private key ([`derive`]).
    reset_key: Arc<ring::hmac::Key>,
    /// The key of the connection-ID generator's check, derived the same way.
    cid_key: u64,
    /// The certificate chain and the parsed signing key, shared by every handshake. Holding
    /// them parsed means no handshake copies the key's DER again.
    resolver: Arc<SingleCertAndKey>,
    fingerprint: Fingerprint,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Loads `dir/key.der` and `dir/cert.der`, creating them on first use.
    ///
    /// - `dir` is created with mode 0700; both files are written 0600, atomically.
    /// - An existing `dir` or key that group or others can access is refused.
    /// - The key is the identity. A missing certificate, or one whose public key does not match
    ///   the key, is re-issued from the key; the fingerprint does not change.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        ensure_private_dir(dir)?;
        let key_path = dir.join(KEY_FILE);
        let cert_path = dir.join(CERT_FILE);

        // `Zeroizing` wipes each secret buffer when it goes out of scope, on every path.
        let (key, der): (Zeroizing<KeyPair>, Zeroizing<Vec<u8>>) = match read_secret(&key_path) {
            Ok(der) => {
                check_private(&key_path)?;
                let key = parse_key(&der).map_err(|reason| IdentityError::BadKey {
                    path: key_path.clone(),
                    reason,
                })?;
                (key, der)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = Zeroizing::new(
                    KeyPair::generate_for(&PKCS_ED25519)
                        .map_err(|e| IdentityError::Cert(e.to_string()))?,
                );
                let der = Zeroizing::new(key.serialize_der());
                write_private(&key_path, &der)?;
                (key, der)
            }
            Err(source) => {
                return Err(IdentityError::Io {
                    path: key_path,
                    source,
                });
            }
        };

        let spki = key.subject_public_key_info();
        let cert = match fs::read(&cert_path) {
            Ok(der) => {
                let cert = CertificateDer::from(der);
                match Fingerprint::of_cert(&cert) {
                    Ok(fp) if fp == Fingerprint::of_spki(&spki) => Some(cert),
                    _ => None, // unparsable or issued for another key: re-issue below
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(IdentityError::Io {
                    path: cert_path,
                    source,
                });
            }
        };
        let cert = match cert {
            Some(cert) => cert,
            None => {
                let params = CertificateParams::new(vec![CERT_NAME.to_string()])
                    .map_err(|e| IdentityError::Cert(e.to_string()))?;
                let cert = params
                    .self_signed(&*key)
                    .map_err(|e| IdentityError::Cert(e.to_string()))?;
                let der = cert.der().clone();
                write_private(&cert_path, &der)?;
                der
            }
        };

        Self::assemble(cert, &der)
    }

    /// Creates a throwaway identity that is never written to disk (tests, probes).
    pub fn ephemeral() -> Result<Self, IdentityError> {
        let key = Zeroizing::new(
            KeyPair::generate_for(&PKCS_ED25519).map_err(|e| IdentityError::Cert(e.to_string()))?,
        );
        let cert = CertificateParams::new(vec![CERT_NAME.to_string()])
            .and_then(|p| p.self_signed(&*key))
            .map_err(|e| IdentityError::Cert(e.to_string()))?;
        let der = Zeroizing::new(key.serialize_der());
        Self::assemble(cert.der().clone(), &der)
    }

    /// Parses the key once into rustls' signing key, checks that it belongs to `cert`, and
    /// computes the fingerprint. `key_der` is the caller's wiped-on-drop buffer; nothing here
    /// keeps a copy of it.
    fn assemble(cert: CertificateDer<'static>, key_der: &[u8]) -> Result<Self, IdentityError> {
        // borrows `key_der`: no copy is made
        let signing = any_eddsa_type(&PrivatePkcs8KeyDer::from(key_der))
            .map_err(|e| IdentityError::Cert(e.to_string()))?;
        let certified = CertifiedKey::new(vec![cert.clone()], signing);
        certified
            .keys_match()
            .map_err(|e| IdentityError::Cert(e.to_string()))?;
        let fingerprint =
            Fingerprint::of_cert(&cert).map_err(|e| IdentityError::Cert(e.to_string()))?;
        let (reset_key, cid_key) = derive(key_der);
        Ok(Self {
            cert,
            reset_key: Arc::new(reset_key),
            cid_key,
            resolver: Arc::new(SingleCertAndKey::from(certified)),
            fingerprint,
        })
    }

    /// The key quinn signs stateless-reset tokens with, the same for every
    /// run of this identity. A restarted peer can then reset the connections its previous run
    /// had, and a dialer drops its dead connection at the first packet after the restart instead
    /// of at its idle timeout ("Restart seen late"). Never logged.
    pub(crate) fn reset_key(&self) -> Arc<dyn quinn::crypto::HmacKey> {
        self.reset_key.clone()
    }

    /// The key of quinn's `HashedConnectionIdGenerator`, which recognises the connection IDs it
    /// issued. A restarted endpoint only resets a packet whose connection ID passes that check,
    /// so the key must survive the restart too.
    pub(crate) fn cid_key(&self) -> u64 {
        self.cid_key
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// The DER certificate, for pinning out of band (`ferry id` / `ferry trust`).
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    /// The certificate and signing key, for rustls' `with_cert_resolver` /
    /// `with_client_cert_resolver`. Shared, so a handshake copies no key bytes.
    pub(crate) fn resolver(&self) -> Arc<SingleCertAndKey> {
        self.resolver.clone()
    }
}

/// HKDF label of the stateless-reset key.
const RESET_KEY_LABEL: &[u8] = b"ferry stateless reset v1";
/// HKDF label of the connection-ID check key.
const CID_KEY_LABEL: &[u8] = b"ferry connection id v1";

/// Eight bytes of HKDF output.
struct U64Len;

impl ring::hkdf::KeyType for U64Len {
    fn len(&self) -> usize {
        8
    }
}

/// Two keys that stay the same for every run of this identity, from
/// `HKDF-SHA256(ikm = the Ed25519 seed, info = label)`:
///
/// - the stateless-reset key (`"ferry stateless reset v1"`), an HMAC-SHA256 key, the shape
///   quinn's own random key has (RFC 9000 §10.3.2: a static key, and a reset token is an HMAC
///   of the connection ID);
/// - the key of the connection-ID check (`"ferry connection id v1"`), so a restarted endpoint
///   recognises its previous run's connection IDs and answers them with a reset.
///
/// The labels separate them from each other and from the signing key; HKDF's output reveals
/// nothing about the seed. The seed is the 32-byte `CurvePrivateKey` inside the PKCS#8 document
/// (`04 22 04 20 …`), so the v1 and v2 encodings of one key give the same keys. A document
/// without one (never an Ed25519 key) falls back to the whole document as the input key material.
fn derive(pkcs8: &[u8]) -> (ring::hmac::Key, u64) {
    let seed = pkcs8
        .windows(4)
        .position(|w| w == [0x04, 0x22, 0x04, 0x20])
        .and_then(|i| pkcs8.get(i + 4..i + 36))
        .unwrap_or(pkcs8);
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &[]).extract(seed);
    let reset = ring::hmac::Key::from(
        prk.expand(&[RESET_KEY_LABEL], ring::hmac::HMAC_SHA256)
            .expect("32 bytes is a valid HKDF-SHA256 output length"),
    );
    let mut cid = [0u8; 8];
    prk.expand(&[CID_KEY_LABEL], U64Len)
        .and_then(|okm| okm.fill(&mut cid))
        .expect("8 bytes is a valid HKDF-SHA256 output length");
    (reset, u64::from_le_bytes(cid))
}

/// Reads a secret file into a buffer that is wiped on drop.
///
/// The buffer is allocated once at [`MAX_KEY_FILE`] + 1 and never grows, so no reallocation
/// leaves an unwiped copy behind (unlike `fs::read`, which may regrow for a file that grew
/// after `stat`). A longer file is refused.
fn read_secret(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut buf = Zeroizing::new(Vec::with_capacity(MAX_KEY_FILE + 1));
    File::open(path)?
        .take(MAX_KEY_FILE as u64 + 1)
        .read_to_end(&mut buf)?;
    if buf.len() > MAX_KEY_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("longer than {MAX_KEY_FILE} bytes"),
        ));
    }
    Ok(buf)
}

/// Parses an Ed25519 PKCS#8 key. rcgen copies the bytes into the `KeyPair` once, and its
/// `Zeroize` impl wipes that copy when the `Zeroizing` wrapper drops.
fn parse_key(der: &[u8]) -> Result<Zeroizing<KeyPair>, String> {
    KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(der), &PKCS_ED25519)
        .map(Zeroizing::new)
        .map_err(|e| e.to_string())
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> IdentityError + '_ {
    move |source| IdentityError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn ensure_private_dir(dir: &Path) -> Result<(), IdentityError> {
    match fs::metadata(dir) {
        Ok(_) => check_private(dir),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent).map_err(io_err(parent))?;
            }
            DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(io_err(dir))?;
            // the umask can only remove bits, never add group/other access to 0700
            check_private(dir)
        }
        Err(source) => Err(IdentityError::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

fn check_private(path: &Path) -> Result<(), IdentityError> {
    let mode = fs::metadata(path)
        .map_err(io_err(path))?
        .permissions()
        .mode()
        & 0o7777;
    if mode & 0o077 != 0 {
        return Err(IdentityError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// Writes `data` to `path` with mode 0600 through a temporary file and a rename, so a crash
/// leaves either the old file or the new one, never half a key.
fn write_private(path: &Path, data: &[u8]) -> Result<(), IdentityError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io_err(&tmp))?;
    f.write_all(data).map_err(io_err(&tmp))?;
    f.sync_all().map_err(io_err(&tmp))?;
    drop(f);
    fs::rename(&tmp, path).map_err(io_err(path))?;
    if let Some(parent) = path.parent()
        && let Ok(d) = fs::File::open(parent)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private scratch directory that goes away when the test ends, whether it passes or not.
    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ferry-net-id-")
            .tempdir()
            .unwrap()
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_with_private_modes_and_reloads_the_same_identity() {
        let tmp = scratch();
        let base = tmp.path();
        let dir = base.join("share/ferry");
        let a = Identity::load_or_create(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(KEY_FILE)), 0o600);
        assert_eq!(mode(&dir.join(CERT_FILE)), 0o600);
        assert!(!dir.join("key.der.tmp").exists());
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn reissued_cert_keeps_the_fingerprint() {
        let tmp = scratch();
        let base = tmp.path();
        let a = Identity::load_or_create(&base.join("id")).unwrap();
        fs::remove_file(base.join("id").join(CERT_FILE)).unwrap();
        let b = Identity::load_or_create(&base.join("id")).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn refuses_group_readable_key_and_dir() {
        let tmp = scratch();
        let base = tmp.path();
        let dir = base.join("id");
        Identity::load_or_create(&dir).unwrap();
        let key = dir.join(KEY_FILE);
        fs::set_permissions(&key, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            Identity::load_or_create(&dir),
            Err(IdentityError::InsecurePermissions { .. })
        ));
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            Identity::load_or_create(&dir),
            Err(IdentityError::InsecurePermissions { .. })
        ));
    }

    #[test]
    fn secret_reads_never_regrow_and_oversized_files_are_refused() {
        let tmp = scratch();
        let base = tmp.path();
        let small = base.join("small");
        fs::write(&small, [7u8; 83]).unwrap();
        let buf = read_secret(&small).unwrap();
        assert_eq!(&buf[..], &[7u8; 83][..]);
        // one allocation for the whole read: a regrow would leave an unwiped copy behind
        assert_eq!(buf.capacity(), MAX_KEY_FILE + 1);

        let big = base.join("big");
        fs::write(&big, vec![1u8; MAX_KEY_FILE + 1]).unwrap();
        assert_eq!(
            read_secret(&big).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::write(&big, vec![1u8; MAX_KEY_FILE]).unwrap();
        assert!(read_secret(&big).is_ok());
    }

    #[test]
    fn a_key_that_is_not_ed25519_pkcs8_is_refused() {
        let tmp = scratch();
        let base = tmp.path();
        let dir = base.join("id");
        Identity::load_or_create(&dir).unwrap();
        let key = dir.join(KEY_FILE);

        let ecdsa = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        fs::write(&key, ecdsa.serialize_der()).unwrap();
        for bytes in [fs::read(&key).unwrap(), b"not a key".to_vec(), Vec::new()] {
            fs::write(&key, bytes).unwrap();
            fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                Identity::load_or_create(&dir),
                Err(IdentityError::BadKey { .. })
            ));
        }
    }

    #[test]
    fn debug_never_prints_key_material() {
        let id = Identity::ephemeral().unwrap();
        let text = format!("{id:?}");
        assert!(text.contains(&id.fingerprint().short()), "{text}");
        assert!(!text.contains("key"), "{text}");
    }

    /// The reset key is the identity's: the same key file gives the same key, another key
    /// another one. Checked through the tokens it signs, as quinn uses it.
    #[test]
    fn the_reset_key_follows_the_identity() {
        let token = |id: &Identity| {
            let mut out = [0u8; 32];
            id.reset_key().sign(b"connection id", &mut out);
            out
        };
        let tmp = scratch();
        let base = tmp.path();
        let a = Identity::load_or_create(&base.join("id")).unwrap();
        let again = Identity::load_or_create(&base.join("id")).unwrap();
        assert_eq!(token(&a), token(&again));
        assert_eq!(a.cid_key(), again.cid_key());
        let other = Identity::ephemeral().unwrap();
        assert_ne!(token(&a), token(&other));
        assert_ne!(a.cid_key(), other.cid_key());
        // the seed is found in rcgen's PKCS#8 document, so the derivation does not
        // depend on the encoding around it
        let der = fs::read(base.join("id").join(KEY_FILE)).unwrap();
        assert!(der.windows(4).any(|w| w == [0x04, 0x22, 0x04, 0x20]));
    }

    #[test]
    fn new_key_means_new_fingerprint() {
        let a = Identity::ephemeral().unwrap();
        let b = Identity::ephemeral().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// `key.der` and `cert.der` are files on disk: raw bytes, and a valid pair with bytes
        /// flipped or cut, load to an identity or an error, never a panic. When it loads, the
        /// fingerprint is the key's, whatever state the certificate was in.
        #[test]
        fn load_or_create_never_panics_on_damaged_files(
            key_raw in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..200)),
            cert_raw in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..200)),
            key_flips in proptest::collection::vec((any::<usize>(), 1u8..=255), 0..4),
            cert_flips in proptest::collection::vec((any::<usize>(), 1u8..=255), 0..4),
            cert_cut in proptest::option::of(any::<usize>()),
        ) {
            let tmp = scratch();
            let dir = tmp.path().join("id");
            let good = Identity::load_or_create(&dir).unwrap();
            let damage = |mut b: Vec<u8>, flips: &[(usize, u8)], cut: Option<usize>| {
                for &(i, x) in flips {
                    let n = b.len();
                    b[i % n] ^= x;
                }
                if let Some(c) = cut {
                    b.truncate(c % (b.len() + 1));
                }
                b
            };
            let key = key_raw.unwrap_or_else(|| damage(fs::read(dir.join(KEY_FILE)).unwrap(), &key_flips, None));
            let cert = cert_raw.unwrap_or_else(|| damage(fs::read(dir.join(CERT_FILE)).unwrap(), &cert_flips, cert_cut));
            let key_intact = key == fs::read(dir.join(KEY_FILE)).unwrap();
            for (name, bytes) in [(KEY_FILE, key), (CERT_FILE, cert)] {
                let p = dir.join(name);
                fs::write(&p, bytes).unwrap();
                fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            }
            if let Ok(loaded) = Identity::load_or_create(&dir)
                && key_intact
            {
                prop_assert_eq!(loaded.fingerprint(), good.fingerprint());
            }
        }
    }
}
