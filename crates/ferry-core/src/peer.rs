//! Peer identity: the fingerprint.

use std::fmt;

/// A peer's identity: SHA-256 of its certificate's `SubjectPublicKeyInfo`
/// DER (CONTEXT: "the SHA-256 hash of a peer's public key"). Stable across a
/// re-issued certificate with the same key. Ordered bytewise; the lower id
/// wins a simultaneous dial and breaks revision ties.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub [u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintError {
    #[error("a fingerprint is 64 hex digits (separators ':' and ' ' allowed), got {0} digits")]
    Length(usize),
    #[error("not a hex digit: {0:?}")]
    Digit(char),
}

/// A peer name is shown in the UI and typed on the command line, and `:` separates
/// `<peer>:<monitor>` in layout anchors. A name that fails this never reaches a terminal or a
/// log: a peer's `Hello.name` is replaced by [`PeerId::default_name`] .
pub fn validate_peer_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a peer name cannot be empty".into());
    }
    if name.chars().count() > 64 {
        return Err("a peer name is at most 64 characters".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_control() || c.is_whitespace() || *c == ':')
    {
        return Err(format!(
            "a peer name cannot contain {c:?} (no whitespace, control characters or ':')"
        ));
    }
    Ok(())
}

impl PeerId {
    /// `peer-<first 8 hex digits>`: the name of a peer that has no valid one.
    pub fn default_name(&self) -> String {
        let [a, b, c, d, ..] = self.0;
        format!("peer-{a:02x}{b:02x}{c:02x}{d:02x}")
    }

    /// Hash a `SubjectPublicKeyInfo` DER. ferry-net extracts the SPKI from
    /// the certificate and calls this, so both crates agree on identity.
    pub fn of_spki(spki_der: &[u8]) -> Self {
        use sha2::Digest;
        PeerId(sha2::Sha256::digest(spki_der).into())
    }

    /// Parse 64 hex digits; `:`, ` ` and `-` separators are ignored, case
    /// does not matter, surrounding whitespace is trimmed and an optional
    /// `SHA256:` (or `sha256:`) prefix is dropped (what `ferry id` prints,
    /// pasted into `ferry trust`). It accepts exactly what ferry-net's
    /// `Fingerprint::from_str` accepts.
    pub fn parse(s: &str) -> Result<Self, FingerprintError> {
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
            let v = c.to_digit(16).ok_or(FingerprintError::Digit(c))? as u8;
            if let Some(b) = out.get_mut(n / 2) {
                *b = (*b << 4) | v;
            }
            n += 1;
        }
        if n != 64 {
            return Err(FingerprintError::Length(n));
        }
        Ok(PeerId(out))
    }

    /// Short form for the UI: the first 8 bytes as 4 groups of 4 hex digits,
    /// joined by `:` (one shell word, and [`PeerId::parse`] skips the `:`).
    pub fn short(&self) -> String {
        let mut s = String::with_capacity(19);
        for (i, pair) in self.0[..8].chunks(2).enumerate() {
            if i > 0 {
                s.push(':');
            }
            for b in pair {
                s.push_str(&format!("{b:02x}"));
            }
        }
        s
    }
}

/// Full form: 64 lowercase hex digits.
impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spki_hash_is_sha256() {
        // SHA-256("abc"), FIPS 180-2 appendix B.1.
        let id = PeerId::of_spki(b"abc");
        assert_eq!(
            id.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(id.short(), "ba78:16bf:8f01:cfea");
    }

    #[test]
    fn parse_round_trips_and_accepts_separators() {
        let id = PeerId::of_spki(b"ferry");
        let full = id.to_string();
        assert_eq!(PeerId::parse(&full), Ok(id));
        assert_eq!(PeerId::parse(&full.to_uppercase()), Ok(id));
        let grouped: Vec<String> = full
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        assert_eq!(PeerId::parse(&grouped.join(":")), Ok(id));
        assert_eq!(PeerId::parse(&grouped.join(" ")), Ok(id));
        assert_eq!(PeerId::parse(&grouped.join("-")), Ok(id));
    }

    /// ferry-net's `Fingerprint::from_str` (crates/ferry-net/src/fingerprint.rs
    /// at 71b374c), copied as a fixture so the two parsers can be compared
    /// without a dependency on ferry-net.
    fn ferry_net_from_str(s: &str) -> Option<[u8; 32]> {
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
            let v = c.to_digit(16)? as u8;
            if n >= 64 {
                return None;
            }
            out[n / 2] |= if n.is_multiple_of(2) { v << 4 } else { v };
            n += 1;
        }
        (n == 64).then_some(out)
    }

    #[test]
    fn parse_agrees_with_ferry_net() {
        let full = PeerId::of_spki(b"ferry").to_string();
        let inputs = [
            full.clone(),
            full.to_uppercase(),
            format!("SHA256:{full}"),
            format!("sha256:{full}"),
            format!("Sha256:{full}"),
            format!("  {full}\n"),
            format!("\t{full}"),
            full.as_bytes()
                .chunks(2)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect::<Vec<_>>()
                .join(":"),
            full.as_bytes()
                .chunks(4)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect::<Vec<_>>()
                .join(" "),
            format!("{full}0"),
            full[..63].to_string(),
            format!("{}g", &full[..63]),
            format!("SHA256: {full}"),
            format!("{full}:"),
            "SHA256:".into(),
            String::new(),
            "é".repeat(64),
        ];
        for i in &inputs {
            assert_eq!(
                PeerId::parse(i).ok().map(|p| p.0),
                ferry_net_from_str(i),
                "{i:?}"
            );
        }
        // Display is 64 lowercase hex and parses back.
        assert_eq!(full.len(), 64);
        assert!(
            full.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4096))]

        #[test]
        fn parse_agrees_with_ferry_net_on_any_input(
            s in "(SHA256:|sha256:| )?[0-9a-fA-F: \\-]{0,80}|.{0,80}",
        ) {
            proptest::prop_assert_eq!(PeerId::parse(&s).ok().map(|p| p.0), ferry_net_from_str(&s));
        }
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(PeerId::parse("abcd"), Err(FingerprintError::Length(4)));
        assert_eq!(
            PeerId::parse(&"0".repeat(66)),
            Err(FingerprintError::Length(66))
        );
        let mut bad = "0".repeat(63);
        bad.push('g');
        assert_eq!(PeerId::parse(&bad), Err(FingerprintError::Digit('g')));
    }
}
