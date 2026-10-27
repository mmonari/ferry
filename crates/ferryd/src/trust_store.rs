//! `~/.local/share/ferry/trusted.toml`: the pinned peers.
//!
//! ```toml
//! [[peer]]
//! fingerprint = "9c2e…"   # 64 lowercase hex
//! name = "laptop"
//! addr = "192.0.2.11"  # optional; this peer dials it
//! paired = "sas"          # "sas" (ferry pair) or "manual" (ferry trust); absent: manual
//! paired_at = "2026-10-18T09:12:03Z"   # optional, RFC 3339 UTC
//! last_seen = "2026-10-18T09:40:11Z"   # optional: the last connection that was up
//! ```
//!
//! `addr` follows the peer: each connection that comes up
//! stores the address that worked, except that a host name the user gave is kept.
//!
//! Mode 0600, written atomically. A corrupted file stops ferryd with a message that names it;
//! ferryd never replaces a file it cannot read.

use std::path::Path;
use std::time::SystemTime;

use ferry_core::api::Paired;
use ferry_net::{Fingerprint, TrustSet};
use serde::{Deserialize, Serialize};

use crate::fsutil::{StoreError, mode_of, read_optional, write_atomic};

pub const MODE: u32 = 0o600;

/// One pinned peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub fingerprint: Fingerprint,
    pub name: String,
    /// `host[:port]`, as the user gave it, or the address a connection last came up on.
    pub addr: Option<String>,
    pub paired: Paired,
    /// RFC 3339 UTC.
    pub paired_at: Option<String>,
    /// RFC 3339 UTC: the last time a connection to the peer was up.
    pub last_seen: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "peer")]
    peers: Vec<PinRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PinRecord {
    fingerprint: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paired: Option<Paired>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paired_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_seen: Option<String>,
}

/// What `upsert` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Updated,
    /// The fingerprint was already pinned with exactly this name and address.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error("the name {name:?} already belongs to peer {fingerprint}")]
    NameTaken { name: String, fingerprint: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustStore {
    pins: Vec<Pin>,
}

/// `peer-` plus the first eight hex digits: the name of a peer that was pinned without one.
pub fn default_name(fp: &Fingerprint) -> String {
    ferry_core::PeerId(fp.0).default_name()
}

impl TrustStore {
    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    pub fn get(&self, fp: &Fingerprint) -> Option<&Pin> {
        self.pins.iter().find(|p| p.fingerprint == *fp)
    }

    pub fn fingerprints(&self) -> Vec<Fingerprint> {
        self.pins.iter().map(|p| p.fingerprint).collect()
    }

    pub fn trust_set(&self) -> TrustSet {
        self.fingerprints().into_iter().collect()
    }

    /// Pins `fp`, or updates the existing pin: a given `name` or `addr` replaces the stored one,
    /// an omitted one keeps it (idempotent `ferry trust`). Pure; the caller saves.
    pub fn upsert(
        &mut self,
        fp: Fingerprint,
        name: Option<&str>,
        addr: Option<&str>,
    ) -> Result<Upsert, TrustError> {
        if let Some(name) = name
            && let Some(other) = self
                .pins
                .iter()
                .find(|p| p.name == name && p.fingerprint != fp)
        {
            return Err(TrustError::NameTaken {
                name: name.to_string(),
                fingerprint: other.fingerprint.to_string(),
            });
        }
        match self.pins.iter_mut().find(|p| p.fingerprint == fp) {
            Some(pin) => {
                let before = pin.clone();
                if let Some(name) = name {
                    pin.name = name.to_string();
                }
                if let Some(addr) = addr {
                    pin.addr = Some(addr.to_string());
                }
                Ok(if *pin == before {
                    Upsert::Unchanged
                } else {
                    Upsert::Updated
                })
            }
            None => {
                self.pins.push(Pin {
                    fingerprint: fp,
                    name: name.map_or_else(|| default_name(&fp), str::to_string),
                    addr: addr.map(str::to_string),
                    paired: Paired::Manual,
                    paired_at: None,
                    last_seen: None,
                });
                Ok(Upsert::Added)
            }
        }
    }

    /// `name`, or `name-2`, `name-3`, … : the first that no other peer has (design m1-rest
    /// §2.11). A name that is not valid falls back to `peer-<8 hex>`.
    pub fn free_name(&self, name: &str, fp: &Fingerprint) -> String {
        let base = if crate::config::validate_peer_name(name).is_ok() {
            name.to_string()
        } else {
            default_name(fp)
        };
        let taken = |n: &str| {
            self.pins
                .iter()
                .any(|p| p.name == n && p.fingerprint != *fp)
        };
        if !taken(&base) {
            return base;
        }
        (2u32..)
            .map(|i| {
                let suffix = format!("-{i}");
                // stay within the 64-character limit
                let keep: String = base.chars().take(64 - suffix.chars().count()).collect();
                format!("{keep}{suffix}")
            })
            .find(|n| !taken(n))
            .expect("some suffix is free")
    }

    /// Pins `fp` after SAS pairing, or refreshes its pin: a different key is a different peer,
    /// so re-pairing a pinned fingerprint only updates its name, address and `paired_at`.
    /// Returns the name it is pinned under. Pure; the caller saves.
    pub fn pin_paired(
        &mut self,
        fp: Fingerprint,
        name: &str,
        addr: Option<&str>,
        now: &str,
    ) -> String {
        let name = self.free_name(name, &fp);
        match self.pins.iter_mut().find(|p| p.fingerprint == fp) {
            Some(pin) => {
                pin.name = name.clone();
                if let Some(a) = addr {
                    pin.addr = Some(a.to_string());
                }
                pin.paired = Paired::Sas;
                pin.paired_at = Some(now.to_string());
                pin.last_seen = Some(now.to_string());
            }
            None => self.pins.push(Pin {
                fingerprint: fp,
                name: name.clone(),
                addr: addr.map(str::to_string),
                paired: Paired::Sas,
                paired_at: Some(now.to_string()),
                last_seen: Some(now.to_string()),
            }),
        }
        name
    }

    /// Records `paired_at` on a pin that has none (`ferry trust` of a new fingerprint).
    pub fn set_paired_at(&mut self, fp: &Fingerprint, now: &str) {
        if let Some(pin) = self.pins.iter_mut().find(|p| p.fingerprint == *fp)
            && pin.paired_at.is_none()
        {
            pin.paired_at = Some(now.to_string());
        }
    }

    /// Unpins `fp`. Returns the pin, if there was one.
    pub fn remove(&mut self, fp: &Fingerprint) -> Option<Pin> {
        let i = self.pins.iter().position(|p| p.fingerprint == *fp)?;
        Some(self.pins.remove(i))
    }

    /// A connection to `fp` came up: `last_seen` is now, and `addr` becomes
    /// `remote` when the peer dialled in from somewhere else. A host name the user gave is kept;
    /// an address this side dials and reached stays as it is. Returns whether the stored
    /// address changed.
    pub fn seen(
        &mut self,
        fp: &Fingerprint,
        remote: Option<std::net::SocketAddr>,
        now: &str,
    ) -> bool {
        let Some(pin) = self.pins.iter_mut().find(|p| p.fingerprint == *fp) else {
            return false;
        };
        pin.last_seen = Some(now.to_string());
        let Some(remote) = remote else {
            return false;
        };
        let keep_name = pin.addr.as_deref().is_some_and(|a| {
            crate::netaddr::parse_host_port(a, ferry_net::DEFAULT_PORT)
                .is_ok_and(|hp| hp.literal().is_none())
        });
        let same = pin.addr.as_deref().is_some_and(|a| {
            crate::netaddr::parse_host_port(a, ferry_net::DEFAULT_PORT)
                .ok()
                .and_then(|hp| hp.literal())
                .is_some_and(|sa| sa == remote)
        });
        if keep_name || same {
            return false;
        }
        pin.addr = Some(remote.to_string());
        true
    }

    pub fn load(path: &Path) -> Result<Self, StoreError> {
        let Some(text) = read_optional(path)? else {
            return Ok(Self::default());
        };
        // A file another user could read or write is not a trust anchor. Tighten it and say so.
        if let Ok(mode) = mode_of(path)
            && mode & 0o077 != 0
        {
            tracing::warn!(path = %path.display(), mode = format!("{mode:04o}"), "trusted.toml is accessible by group or others; setting 0600");
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(MODE));
        }
        Self::parse(&text, path)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self, StoreError> {
        let file: File =
            toml::from_str(text).map_err(|e| StoreError::corrupt(path, e.message()))?;
        let mut store = Self::default();
        for (i, rec) in file.peers.iter().enumerate() {
            let fp: Fingerprint = rec.fingerprint.parse().map_err(|_| {
                StoreError::corrupt(
                    path,
                    format!("[[peer]] #{} has an invalid fingerprint", i + 1),
                )
            })?;
            if store.get(&fp).is_some() {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[peer]] #{} repeats fingerprint {fp}", i + 1),
                ));
            }
            if crate::config::validate_peer_name(&rec.name).is_err() {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[peer]] #{} has an invalid name {:?}", i + 1, rec.name),
                ));
            }
            if let Some(other) = store.pins.iter().find(|p| p.name == rec.name) {
                return Err(StoreError::corrupt(
                    path,
                    format!(
                        "[[peer]] #{} reuses the name {:?} of {}",
                        i + 1,
                        rec.name,
                        other.fingerprint
                    ),
                ));
            }
            store.pins.push(Pin {
                fingerprint: fp,
                name: rec.name.clone(),
                addr: rec.addr.clone(),
                paired: rec.paired.unwrap_or(Paired::Manual),
                paired_at: rec.paired_at.clone(),
                last_seen: rec.last_seen.clone(),
            });
        }
        Ok(store)
    }

    pub fn to_toml(&self) -> String {
        let file = File {
            peers: self
                .pins
                .iter()
                .map(|p| PinRecord {
                    fingerprint: p.fingerprint.to_string(),
                    name: p.name.clone(),
                    addr: p.addr.clone(),
                    paired: Some(p.paired),
                    paired_at: p.paired_at.clone(),
                    last_seen: p.last_seen.clone(),
                })
                .collect(),
        };
        toml::to_string(&file).expect("pins serialise")
    }

    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        write_atomic(path, self.to_toml().as_bytes(), MODE)
    }
}

/// `t` as RFC 3339 UTC with seconds: `2026-10-18T09:12:03Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil from days (H. Hinnant's algorithm), proleptic Gregorian
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// [`rfc3339`] of now.
pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_utc_seconds() {
        use std::time::Duration;
        let at = |s| rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        // 2026-10-18T09:12:03Z
        assert_eq!(at(1_792_314_723), "2026-10-18T09:12:03Z");
        assert_eq!(at(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn pairing_pins_names_and_addresses() {
        let mut s = TrustStore::default();
        s.upsert(fp(1), Some("laptop"), None).unwrap();
        // a second machine called laptop gets laptop-2
        assert_eq!(
            s.pin_paired(fp(2), "laptop", Some("10.0.0.2:4243"), "T1"),
            "laptop-2"
        );
        let p = s.get(&fp(2)).unwrap();
        assert_eq!(
            (p.paired, p.paired_at.as_deref(), p.last_seen.as_deref()),
            (Paired::Sas, Some("T1"), Some("T1"))
        );
        // re-pairing a pinned key keeps it and updates name, address and time
        assert_eq!(
            s.pin_paired(fp(1), "laptop", Some("10.0.0.1"), "T2"),
            "laptop"
        );
        let p = s.get(&fp(1)).unwrap();
        assert_eq!(
            (p.paired, p.addr.as_deref()),
            (Paired::Sas, Some("10.0.0.1"))
        );
        assert_eq!(s.pins().len(), 2);
        // a name that is not a valid peer name falls back to peer-<hex>
        assert_eq!(s.free_name("bad name", &fp(3)), "peer-03030303");
        assert_eq!(s.remove(&fp(2)).unwrap().name, "laptop-2");
        assert!(s.remove(&fp(2)).is_none());
    }

    #[test]
    fn a_connection_keeps_the_address_up_to_date() {
        let mut s = TrustStore::default();
        s.upsert(fp(1), Some("a"), None).unwrap();
        s.upsert(fp(2), Some("b"), Some("laptop.lan")).unwrap();
        s.upsert(fp(3), Some("c"), Some("10.0.0.3")).unwrap();
        let remote = |ip: &str| Some(format!("{ip}:4243").parse().unwrap());
        // an inbound connection teaches a pin without an address where to dial
        assert!(s.seen(&fp(1), remote("10.0.0.1"), "T"));
        assert_eq!(
            s.get(&fp(1)).unwrap().addr.as_deref(),
            Some("10.0.0.1:4243")
        );
        assert_eq!(s.get(&fp(1)).unwrap().last_seen.as_deref(), Some("T"));
        // a host name the user gave stays
        assert!(!s.seen(&fp(2), remote("10.0.0.2"), "T"));
        assert_eq!(s.get(&fp(2)).unwrap().addr.as_deref(), Some("laptop.lan"));
        // the same address (default port) is no change; a new one replaces it
        assert!(!s.seen(&fp(3), remote("10.0.0.3"), "T"));
        assert!(s.seen(&fp(3), remote("10.0.0.33"), "T"));
        assert_eq!(
            s.get(&fp(3)).unwrap().addr.as_deref(),
            Some("10.0.0.33:4243")
        );
        // a dialled connection only refreshes last_seen
        assert!(!s.seen(&fp(3), None, "T2"));
        assert_eq!(s.get(&fp(3)).unwrap().last_seen.as_deref(), Some("T2"));
        // the new fields survive the file
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.toml");
        s.save(&path).unwrap();
        assert_eq!(TrustStore::load(&path).unwrap(), s);
    }

    #[test]
    fn an_older_file_reads_as_manual_pins() {
        let text = format!("[[peer]]\nfingerprint = \"{}\"\nname = \"a\"\n", fp(1));
        let s = TrustStore::parse(&text, Path::new("t")).unwrap();
        let p = s.get(&fp(1)).unwrap();
        assert_eq!((p.paired, p.paired_at.as_deref()), (Paired::Manual, None));
    }

    fn fp(n: u8) -> Fingerprint {
        Fingerprint([n; 32])
    }

    #[test]
    fn upsert_adds_updates_and_is_idempotent() {
        let mut s = TrustStore::default();
        assert_eq!(
            s.upsert(fp(1), Some("a"), Some("10.0.0.1")),
            Ok(Upsert::Added)
        );
        assert_eq!(
            s.upsert(fp(1), Some("a"), Some("10.0.0.1")),
            Ok(Upsert::Unchanged)
        );
        // omitted fields keep the stored value
        assert_eq!(s.upsert(fp(1), None, None), Ok(Upsert::Unchanged));
        assert_eq!(s.upsert(fp(1), Some("b"), None), Ok(Upsert::Updated));
        assert_eq!(s.get(&fp(1)).unwrap().name, "b");
        assert_eq!(s.get(&fp(1)).unwrap().addr.as_deref(), Some("10.0.0.1"));
        assert_eq!(
            s.upsert(fp(1), None, Some("10.0.0.2:5000")),
            Ok(Upsert::Updated)
        );
        assert_eq!(s.pins().len(), 1);
    }

    #[test]
    fn an_unnamed_pin_gets_a_default_name() {
        let mut s = TrustStore::default();
        s.upsert(fp(0xab), None, None).unwrap();
        assert_eq!(s.pins()[0].name, "peer-abababab");
    }

    #[test]
    fn a_name_belongs_to_one_peer() {
        let mut s = TrustStore::default();
        s.upsert(fp(1), Some("a"), None).unwrap();
        assert!(matches!(
            s.upsert(fp(2), Some("a"), None),
            Err(TrustError::NameTaken { .. })
        ));
        assert_eq!(s.pins().len(), 1);
    }

    #[test]
    fn round_trips_through_the_file_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/trusted.toml");
        let mut s = TrustStore::default();
        s.upsert(fp(1), Some("laptop"), Some("192.0.2.11")).unwrap();
        s.upsert(fp(2), Some("lap"), None).unwrap();
        s.save(&path).unwrap();
        assert_eq!(mode_of(&path).unwrap(), 0o600);
        assert_eq!(TrustStore::load(&path).unwrap(), s);
        insta::assert_snapshot!(
            std::fs::read_to_string(&path)
                .unwrap()
                .replace(&fp(1).to_string(), "<fp1>")
                .replace(&fp(2).to_string(), "<fp2>")
        );
    }

    #[test]
    fn a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            TrustStore::load(&dir.path().join("trusted.toml"))
                .unwrap()
                .pins()
                .is_empty()
        );
    }

    #[test]
    fn a_loose_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.toml");
        let mut s = TrustStore::default();
        s.upsert(fp(1), Some("a"), None).unwrap();
        s.save(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        TrustStore::load(&path).unwrap();
        assert_eq!(mode_of(&path).unwrap(), 0o600);
    }

    #[test]
    fn corrupted_files_are_a_clear_error_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.toml");
        let good = fp(1).to_string();
        let cases = [
            ("[[peer\n".to_string(), "corrupted"),
            ("[[peer]]\nname = \"a\"\n".to_string(), "fingerprint"),
            (
                "[[peer]]\nfingerprint = \"nothex\"\nname = \"a\"\n".to_string(),
                "invalid fingerprint",
            ),
            (
                format!(
                    "[[peer]]\nfingerprint = \"{good}\"\nname = \"a\"\n[[peer]]\nfingerprint = \"{good}\"\nname = \"b\"\n"
                ),
                "repeats",
            ),
            (
                format!("[[peer]]\nfingerprint = \"{good}\"\nname = \"a b\"\n"),
                "invalid name",
            ),
        ];
        for (text, needle) in cases {
            std::fs::write(&path, &text).unwrap();
            let e = TrustStore::load(&path).unwrap_err().to_string();
            assert!(e.contains(path.to_str().unwrap()), "{e}");
            assert!(e.contains(needle), "{needle}: {e}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn the_trust_set_holds_every_pin() {
        let mut s = TrustStore::default();
        s.upsert(fp(1), None, None).unwrap();
        s.upsert(fp(2), None, None).unwrap();
        let set = s.trust_set();
        assert!(set.contains(&fp(1)) && set.contains(&fp(2)) && !set.contains(&fp(3)));
    }

    use proptest::prelude::*;

    /// A valid file with a few characters replaced and maybe a cut tail: the damage a disk, an
    /// editor or a half-finished write does.
    fn mangled(valid: String) -> impl Strategy<Value = String> {
        let n = valid.chars().count().max(1);
        (
            proptest::collection::vec((0..n, any::<char>()), 0..6),
            proptest::option::of(0..=n),
        )
            .prop_map(move |(edits, cut)| {
                let mut chars: Vec<char> = valid.chars().collect();
                for (i, c) in edits {
                    if let Some(slot) = chars.get_mut(i) {
                        *slot = c;
                    }
                }
                if let Some(cut) = cut {
                    chars.truncate(cut);
                }
                chars.into_iter().collect()
            })
    }

    /// Pins with distinct fingerprints and distinct names, as `parse` demands.
    fn arb_store() -> impl Strategy<Value = TrustStore> {
        let text = || proptest::option::of("[a-z0-9:.\\[\\]-]{0,24}");
        proptest::collection::btree_map(
            any::<[u8; 32]>(),
            (
                "[a-zA-Z0-9._-]{1,20}",
                text(),
                proptest::sample::select(vec![Paired::Manual, Paired::Sas]),
                text(),
                text(),
            ),
            0..6,
        )
        .prop_map(|m| TrustStore {
            pins: m
                .into_iter()
                .enumerate()
                .map(
                    |(i, (fp, (name, addr, paired, paired_at, last_seen)))| Pin {
                        fingerprint: Fingerprint(fp),
                        name: format!("{name}-{i}"),
                        addr,
                        paired,
                        paired_at,
                        last_seen,
                    },
                )
                .collect(),
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        /// `trusted.toml` is the trust anchor: what `to_toml` wrote, `parse` reads back.
        #[test]
        fn a_trust_store_parses_back_from_its_toml(s in arb_store()) {
            prop_assert_eq!(TrustStore::parse(&s.to_toml(), Path::new("/x")).unwrap(), s);
        }

        /// A damaged file is `Ok` or a `Corrupt` error; when `Ok`, the checks `parse` promises
        /// hold: distinct fingerprints and distinct, valid names.
        #[test]
        fn trust_store_parse_never_panics_and_keeps_its_invariants(
            raw in "\\PC{0,300}",
            damaged in arb_store().prop_flat_map(|s| mangled(s.to_toml())),
        ) {
            for text in [raw, damaged] {
                if let Ok(s) = TrustStore::parse(&text, Path::new("/x")) {
                    let mut names: Vec<&str> = s.pins().iter().map(|p| p.name.as_str()).collect();
                    names.sort_unstable();
                    names.dedup();
                    prop_assert_eq!(names.len(), s.pins().len());
                    let mut fps = s.fingerprints();
                    fps.sort_unstable();
                    fps.dedup();
                    prop_assert_eq!(fps.len(), s.pins().len());
                    prop_assert!(s.pins().iter().all(|p| crate::config::validate_peer_name(&p.name).is_ok()));
                }
            }
        }
    }
}
