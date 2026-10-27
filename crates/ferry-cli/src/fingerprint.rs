//! Fingerprints on the command line: 64 lowercase hex digits, however they were pasted.

/// Why a pasted fingerprint was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintError {
    #[error("a fingerprint is 64 hex digits, but this has {0}")]
    Length(usize),
    #[error("not a hex digit: {0:?}")]
    Digit(char),
}

/// Turns what `ferry id` printed, or what a GUI or `ssh-keygen`-style tool showed, into the 64
/// lowercase hex digits ferryd stores.
///
/// Accepted: either case, `:` `-` and whitespace between digits, and a leading `SHA256:`.
pub fn normalize(input: &str) -> Result<String, FingerprintError> {
    let s = input.trim();
    let s = match s.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("sha256:") => &s[7..],
        _ => s,
    };
    let mut out = String::with_capacity(64);
    for c in s.chars() {
        if c == ':' || c == '-' || c.is_whitespace() {
            continue;
        }
        if !c.is_ascii_hexdigit() {
            return Err(FingerprintError::Digit(c));
        }
        out.push(c.to_ascii_lowercase());
    }
    if out.len() == 64 {
        Ok(out)
    } else {
        Err(FingerprintError::Length(out.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "4b1f0a9c2e7d3b5a6c8e1f0a9c2e7d3b5a6c8e1f0a9c2e7d3b5a6c8e1f0a9c2e";

    #[test]
    fn the_canonical_form_is_unchanged() {
        assert_eq!(normalize(FP).unwrap(), FP);
    }

    #[test]
    fn pasted_forms_normalise() {
        let upper = FP.to_uppercase();
        let colons: Vec<_> = FP
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        let groups: Vec<_> = FP
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        for form in [
            upper.clone(),
            colons.join(":"),
            colons.join(":").to_uppercase(),
            groups.join(" "),
            groups.join("-"),
            format!("SHA256:{FP}"),
            format!("sha256:{upper}"),
            format!("SHA256:{}", groups.join(" ")),
            format!("  {FP}\n"),
            format!("{}\t{}", &FP[..32], &FP[32..]),
        ] {
            assert_eq!(normalize(&form).as_deref(), Ok(FP), "{form:?}");
        }
    }

    #[test]
    fn wrong_lengths_are_counted() {
        assert_eq!(normalize(""), Err(FingerprintError::Length(0)));
        assert_eq!(normalize("abcd"), Err(FingerprintError::Length(4)));
        assert_eq!(normalize(&FP[..63]), Err(FingerprintError::Length(63)));
        assert_eq!(
            normalize(&format!("{FP}0")),
            Err(FingerprintError::Length(65))
        );
    }

    #[test]
    fn non_hex_names_the_character() {
        assert_eq!(
            normalize(&format!("g{}", &FP[1..])),
            Err(FingerprintError::Digit('g'))
        );
        assert_eq!(normalize("SHA256 :abc"), Err(FingerprintError::Digit('S')));
        // a multi-byte character right at the prefix boundary must not panic
        assert_eq!(normalize("ééééé"), Err(FingerprintError::Digit('é')));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Whatever the user pastes: `Ok` with 64 lowercase hex digits, or an `Err`.
        #[test]
        fn normalize_never_panics_and_only_returns_canonical_form(s in "\\PC{0,200}") {
            if let Ok(fp) = normalize(&s) {
                prop_assert_eq!(fp.len(), 64);
                prop_assert!(fp.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            }
        }

        /// Every accepted form of one fingerprint gives the same 64 digits.
        #[test]
        fn every_pasted_form_normalises_to_the_same_digits(
            bytes in any::<[u8; 32]>(),
            sep in proptest::sample::select(vec![":", "-", " ", "\t", ""]),
            group in 1usize..=8,
            upper in any::<bool>(),
            prefix in any::<bool>(),
        ) {
            let canonical: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let grouped: Vec<&str> = canonical
                .as_bytes()
                .chunks(group * 2)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect();
            let mut pasted = grouped.join(sep);
            if prefix {
                pasted = format!("SHA256:{pasted}");
            }
            if upper {
                pasted = pasted.to_uppercase();
            }
            prop_assert_eq!(normalize(&format!("  {pasted}\n")), Ok(canonical));
        }
    }
}
