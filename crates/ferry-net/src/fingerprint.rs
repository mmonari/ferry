//! A peer's identity: SHA-256 of its certificate's SubjectPublicKeyInfo.

use rustls::pki_types::CertificateDer;
use rustls::server::ParsedCertificate;
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// SHA-256 of a peer certificate's SubjectPublicKeyInfo (CONTEXT "Fingerprint").
///
/// Hashing the SPKI rather than the whole certificate keeps the fingerprint stable when the
/// certificate is re-issued from the same key.
///
/// - `Display` prints 64 lowercase hex digits; [`FromStr`] parses that form back.
/// - [`Fingerprint::short`] is the first 16 hex digits, the form shown in peer lists and carried
///   in the mDNS `id` TXT record.
/// - `Ord` is the byte order, which is also the order of the hex strings. The connection manager
///   uses it to break a tie between simultaneous dials.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    /// Fingerprint of a DER certificate: SHA-256 of its SubjectPublicKeyInfo.
    pub fn of_cert(cert: &CertificateDer<'_>) -> Result<Self, rustls::Error> {
        let parsed = ParsedCertificate::try_from(cert)?;
        Ok(Self::of_spki(parsed.subject_public_key_info().as_ref()))
    }

    /// Fingerprint of a DER SubjectPublicKeyInfo.
    pub fn of_spki(spki_der: &[u8]) -> Self {
        let digest = Sha256::digest(spki_der);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// The first 16 hex digits (64 bits): the short form for lists and mDNS.
    pub fn short(&self) -> String {
        hex(&self.0[..8])
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

/// The text was not a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a fingerprint: expected 64 hex digits (optionally prefixed `SHA256:`, grouped by spaces or colons)"
)]
pub struct ParseFingerprintError;

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    /// Accepts 64 hex digits in either case. An optional `SHA256:` prefix and `:`, ` ` or `-`
    /// separators are ignored, so text pasted from the GUI or another tool parses too.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let s = s
            .strip_prefix("SHA256:")
            .or_else(|| s.strip_prefix("sha256:"))
            .unwrap_or(s);
        let mut out = [0u8; 32];
        let mut n = 0usize;
        for c in s.chars() {
            if matches!(c, ':' | ' ' | '-') {
                continue;
            }
            let v = c.to_digit(16).ok_or(ParseFingerprintError)? as u8;
            if n >= 64 {
                return Err(ParseFingerprintError);
            }
            out[n / 2] |= if n.is_multiple_of(2) { v << 4 } else { v };
            n += 1;
        }
        if n == 64 {
            Ok(Self(out))
        } else {
            Err(ParseFingerprintError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_round_trips_and_short_is_a_prefix() {
        let fp = Fingerprint::of_spki(b"spki");
        let text = fp.to_string();
        assert_eq!(text.len(), 64);
        assert!(text.starts_with(&fp.short()));
        assert_eq!(fp.short().len(), 16);
        assert_eq!(text.parse::<Fingerprint>(), Ok(fp));
    }

    #[test]
    fn parse_accepts_prefix_case_and_separators() {
        let fp = Fingerprint::of_spki(b"x");
        let upper = fp.to_string().to_uppercase();
        let grouped: Vec<String> = upper
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        let pasted = format!("SHA256:{}", grouped.join(" "));
        assert_eq!(pasted.parse::<Fingerprint>(), Ok(fp));
    }

    #[test]
    fn parse_rejects_wrong_length_and_non_hex() {
        assert!("abcd".parse::<Fingerprint>().is_err());
        assert!("g".repeat(64).parse::<Fingerprint>().is_err());
        assert!("a".repeat(66).parse::<Fingerprint>().is_err());
    }

    #[test]
    fn order_matches_hex_order() {
        let a = Fingerprint([0x01; 32]);
        let b = Fingerprint([0xfe; 32]);
        assert!(a < b);
        assert!(a.to_string() < b.to_string());
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `trusted.toml` and `layout.toml` hand their fingerprint text to this parser.
        #[test]
        fn fingerprint_parse_never_panics(s in "\\PC{0,200}") {
            let _ = s.parse::<Fingerprint>();
        }

        #[test]
        fn a_fingerprint_parses_back_from_its_text(bytes in any::<[u8; 32]>()) {
            let fp = Fingerprint(bytes);
            prop_assert_eq!(fp.to_string().parse::<Fingerprint>(), Ok(fp));
            prop_assert_eq!(fp.to_string().to_uppercase().parse::<Fingerprint>(), Ok(fp));
        }
    }
}
