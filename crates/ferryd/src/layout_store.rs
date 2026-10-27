//! `~/.local/state/ferry/layout.toml`: the shared layout value (`[revision]` and
//! `[[origin]]`) and, beside it, the last-seen monitors of each peer (`[[known]]`) and this peer's
//! monitor memory (`[anchor]`).
//!
//! `[[known]]` and `[anchor]` are **not** part of the revisioned value: each peer owns its own
//! geometry. `[[known]]` only remembers what a peer last reported so that `layout
//! place` works while that peer is offline. `[anchor]` is what keeps this peer's group in place
//! when Hyprland moves its monitors (M3 design 4.1).
//!
//! ```toml
//! [revision]
//! counter = 3
//! peer = "4b1f…"
//!
//! [[origin]]
//! peer = "4b1f…"
//! x = 0
//! y = 0
//!
//! [[known]]
//! peer = "9c2e…"
//! [[known.monitor]]
//! name = "eDP-1"
//! …
//!
//! [anchor]
//! x = 1920
//! y = 0
//! [[anchor.monitor]]
//! key = "Dell Inc. DELL U2415 7MT0167B1ZUL"
//! …
//! ```
//!
//! The file's shape is `ferry_core::config::LayoutFile`. Mode 0644, written
//! atomically.

use std::path::Path;

use ferry_core::PeerId;
use ferry_core::anchor::Anchors;
use ferry_core::config::{AnchorFile, KnownEntry, LayoutFile, OriginEntry, RevisionEntry};
use ferry_core::geom::{Layout, MonitorInfo, Origin, PeerMonitors, Revision, coord_in_range};
use ferry_core::wire::caps;

use crate::fsutil::{StoreError, read_optional, write_atomic};

pub const MODE: u32 = 0o644;

/// `PeerId` as 64 lowercase hex digits (its `Display`).
pub fn peer_hex(id: PeerId) -> String {
    id.to_string()
}

/// Inverse of [`peer_hex`]. Goes through ferry-net's parser, which is implemented.
pub fn parse_peer(s: &str) -> Option<PeerId> {
    s.parse::<ferry_net::Fingerprint>()
        .ok()
        .map(|f| PeerId(f.0))
}

pub fn peer_id(fp: ferry_net::Fingerprint) -> PeerId {
    PeerId(fp.0)
}

/// The layout value, the last-seen monitors and this peer's monitor memory.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutStore {
    pub layout: Layout,
    pub known: Vec<PeerMonitors>,
    /// What keeps this peer's group where it was when Hyprland moves its monitors.
    pub anchors: Anchors,
}

impl LayoutStore {
    /// No file yet: revision `(0, me)` and nothing placed.
    pub fn empty(me: PeerId) -> Self {
        Self {
            layout: Layout {
                rev: Revision {
                    counter: 0,
                    peer: me,
                },
                origins: Vec::new(),
            },
            known: Vec::new(),
            anchors: Anchors::default(),
        }
    }

    pub fn origin_of(&self, peer: PeerId) -> Option<(i32, i32)> {
        self.layout
            .origins
            .iter()
            .find(|o| o.peer == peer)
            .map(|o| (o.x, o.y))
    }

    pub fn known_monitors(&self, peer: PeerId) -> Option<&[MonitorInfo]> {
        self.known
            .iter()
            .find(|k| k.peer == peer)
            .map(|k| k.monitors.as_slice())
    }

    /// Records what `peer` last reported (a `Hello` or `Monitors`).
    pub fn set_known(&mut self, peer: PeerId, monitors: Vec<MonitorInfo>) {
        match self.known.iter_mut().find(|k| k.peer == peer) {
            Some(k) => k.monitors = monitors,
            None => self.known.push(PeerMonitors { peer, monitors }),
        }
    }

    /// The layout push: keep the higher revision. Returns true when
    /// `received` replaced the current value. `[[known]]` is not touched.
    pub fn merge(&mut self, received: Layout) -> bool {
        self.layout.merge(received)
    }

    /// The layout after setting `origin`, at revision `Revision::next(highest seen, me)`.
    /// The current revision is the highest seen: a pushed layout is kept only
    /// when it is higher. `me` gets origin (0, 0) the first time anything is placed (design
    /// §7.3). Pure; the caller saves.
    pub fn with_origin(&self, me: PeerId, origin: Origin) -> Layout {
        self.with_origins(me, &[origin])
    }

    /// [`with_origin`](Self::with_origin) for several groups: still one revision (`layout.set`).
    pub fn with_origins(&self, me: PeerId, origins: &[Origin]) -> Layout {
        self.layout.with_origins(me, origins)
    }

    /// Takes one reading of this peer's own monitors (as Hyprland reports them) and
    /// returns them with the anchor shift added, and whether the memory changed and needs saving.
    /// The layout and its revision are not touched.
    pub fn observe_own_monitors(&mut self, reading: &[MonitorInfo]) -> (Vec<MonitorInfo>, bool) {
        let before = self.anchors.clone();
        let monitors = self.anchors.observe(reading);
        (monitors, self.anchors != before)
    }

    pub fn load(path: &Path, me: PeerId) -> Result<Self, StoreError> {
        match read_optional(path)? {
            Some(text) => Self::parse(&text, path, me),
            None => Ok(Self::empty(me)),
        }
    }

    pub fn parse(text: &str, path: &Path, me: PeerId) -> Result<Self, StoreError> {
        let file: LayoutFile =
            toml::from_str(text).map_err(|e| StoreError::corrupt(path, e.message()))?;
        let peer = |s: &str, what: &str| {
            parse_peer(s).ok_or_else(|| {
                StoreError::corrupt(path, format!("{what} has an invalid peer {s:?}"))
            })
        };
        let rev = match &file.revision {
            Some(r) => Revision {
                counter: r.counter,
                peer: peer(&r.peer, "[revision]")?,
            },
            None => Revision {
                counter: 0,
                peer: me,
            },
        };
        let mut origins: Vec<Origin> = Vec::new();
        for o in &file.origins {
            let id = peer(&o.peer, "[[origin]]")?;
            if origins.iter().any(|x| x.peer == id) {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[origin]] repeats peer {id}"),
                ));
            }
            // The wire caps (A-03): a value past them would overflow `geom`.
            if !(coord_in_range(o.x) && coord_in_range(o.y)) {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[origin]] of peer {id} is out of range"),
                ));
            }
            origins.push(Origin {
                peer: id,
                x: o.x,
                y: o.y,
            });
        }
        let mut known: Vec<PeerMonitors> = Vec::new();
        for k in file.known {
            let id = peer(&k.peer, "[[known]]")?;
            if known.iter().any(|x| x.peer == id) {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[known]] repeats peer {id}"),
                ));
            }
            if k.monitors.len() > caps::MONITORS || !k.monitors.iter().all(MonitorInfo::in_range) {
                return Err(StoreError::corrupt(
                    path,
                    format!("[[known]] monitors of peer {id} are out of range"),
                ));
            }
            known.push(PeerMonitors {
                peer: id,
                monitors: k.monitors,
            });
        }
        let anchors = match &file.anchor {
            Some(table) => table
                .anchors()
                .map_err(|e| StoreError::corrupt(path, format!("[anchor]: {e}")))?,
            None => Anchors::default(),
        };
        Ok(Self {
            layout: Layout { rev, origins },
            known,
            anchors,
        })
    }

    pub fn to_toml(&self) -> String {
        let file = LayoutFile {
            revision: Some(RevisionEntry {
                counter: self.layout.rev.counter,
                peer: peer_hex(self.layout.rev.peer),
            }),
            origins: self
                .layout
                .origins
                .iter()
                .map(|o| OriginEntry {
                    peer: peer_hex(o.peer),
                    x: o.x,
                    y: o.y,
                })
                .collect(),
            known: self
                .known
                .iter()
                .map(|k| KnownEntry {
                    peer: peer_hex(k.peer),
                    monitors: k.monitors.clone(),
                })
                .collect(),
            anchor: AnchorFile::of(&self.anchors),
        };
        toml::to_string(&file).expect("the layout serialises")
    }

    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        write_atomic(path, self.to_toml().as_bytes(), MODE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::mode_of;
    use ferry_core::anchor::Remembered;
    use ferry_core::geom::WireRect;

    fn id(n: u8) -> PeerId {
        PeerId([n; 32])
    }

    fn mon(name: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            description: format!("{name} desc"),
            rect: WireRect { x, y, w, h },
            scale: 1.0,
            transform: 0,
            shift: None,
        }
    }

    #[test]
    fn empty_is_revision_zero_owned_by_me() {
        let s = LayoutStore::empty(id(1));
        assert_eq!(
            s.layout.rev,
            Revision {
                counter: 0,
                peer: id(1)
            }
        );
        assert!(s.layout.origins.is_empty() && s.known.is_empty());
    }

    #[test]
    fn with_origin_bumps_the_revision_and_defaults_my_origin() {
        let s = LayoutStore::empty(id(1));
        let l = s.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 277,
                y: 1080,
            },
        );
        assert_eq!(
            l.rev,
            Revision {
                counter: 1,
                peer: id(1)
            }
        );
        assert_eq!(
            l.origins,
            vec![
                Origin {
                    peer: id(1),
                    x: 0,
                    y: 0
                },
                Origin {
                    peer: id(2),
                    x: 277,
                    y: 1080
                }
            ]
        );
        // placing again replaces that peer's origin and keeps mine
        let s2 = LayoutStore {
            layout: l,
            known: vec![],
            anchors: Anchors::default(),
        };
        let l2 = s2.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 0,
                y: 1080,
            },
        );
        assert_eq!(l2.rev.counter, 2);
        assert_eq!(l2.origins.len(), 2);
        assert_eq!(s2.origin_of(id(2)), Some((277, 1080)));
    }

    #[test]
    fn with_origin_keeps_an_origin_already_set_for_me() {
        let mut s = LayoutStore::empty(id(1));
        s.layout.origins.push(Origin {
            peer: id(1),
            x: -5,
            y: 7,
        });
        let l = s.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 0,
                y: 0,
            },
        );
        assert!(l.origins.contains(&Origin {
            peer: id(1),
            x: -5,
            y: 7
        }));
    }

    #[test]
    fn round_trips_with_mode_0644() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/layout.toml");
        let mut s = LayoutStore::empty(id(1));
        s.layout = s.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 277,
                y: 1080,
            },
        );
        s.set_known(id(2), vec![mon("eDP-1", 0, 0, 1366, 768)]);
        s.save(&path).unwrap();
        assert_eq!(mode_of(&path).unwrap(), 0o644);
        assert_eq!(LayoutStore::load(&path, id(1)).unwrap(), s);
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace(&peer_hex(id(1)), "<me>")
            .replace(&peer_hex(id(2)), "<laptop>");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn a_pushed_layout_is_kept_only_when_higher() {
        let mut s = LayoutStore::empty(id(1));
        s.layout = s.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 277,
                y: 1080,
            },
        );
        s.set_known(id(2), vec![mon("eDP-1", 0, 0, 1366, 768)]);
        let ours = s.layout.clone();
        // lower: (0, peer 2) < (1, peer 1)
        assert!(!s.merge(Layout::empty(id(2))));
        // equal counter, lower peer id: still lower
        let mut equal_lower = ours.clone();
        equal_lower.rev.peer = id(0);
        assert!(!s.merge(equal_lower));
        assert_eq!(s.layout, ours);
        // equal counter, higher peer id: higher
        let mut higher = ours.clone();
        higher.rev.peer = id(9);
        higher.origins[1].x = 0;
        assert!(s.merge(higher.clone()));
        assert_eq!(s.layout, higher);
        // [[known]] is not part of the value
        assert_eq!(s.known.len(), 1);
        // the next local edit goes past the highest seen
        let next = s.with_origin(
            id(1),
            Origin {
                peer: id(2),
                x: 5,
                y: 1080,
            },
        );
        assert_eq!(
            next.rev,
            Revision {
                counter: 2,
                peer: id(1)
            }
        );
    }

    #[test]
    fn known_is_per_peer_and_replaced_in_place() {
        let mut s = LayoutStore::empty(id(1));
        s.set_known(id(2), vec![mon("a", 0, 0, 10, 10)]);
        s.set_known(id(3), vec![mon("b", 0, 0, 10, 10)]);
        s.set_known(id(2), vec![mon("c", 0, 0, 20, 20)]);
        assert_eq!(s.known.len(), 2);
        assert_eq!(s.known_monitors(id(2)).unwrap()[0].name, "c");
        assert!(s.known_monitors(id(9)).is_none());
    }

    #[test]
    fn the_older_known_monitors_key_still_reads() {
        let text = format!(
            "[[known]]\npeer = \"{}\"\n[[known.monitors]]\nname = \"eDP-1\"\ndescription = \"\"\n\
             scale = 1.0\ntransform = 0\n[known.monitors.rect]\nx = 0\ny = 0\nw = 1366\nh = 768\n",
            peer_hex(id(2))
        );
        let s = LayoutStore::parse(&text, Path::new("/x/layout.toml"), id(1)).unwrap();
        assert_eq!(s.known_monitors(id(2)).unwrap()[0].rect.w, 1366);
        assert!(s.to_toml().contains("[[known.monitor]]"));
    }

    #[test]
    fn a_file_without_the_anchor_table_has_no_memory_and_writes_none() {
        let text = format!("[[known]]\npeer = \"{}\"\n", peer_hex(id(2)));
        let s = LayoutStore::parse(&text, Path::new("/x/layout.toml"), id(1)).unwrap();
        assert!(s.anchors.is_empty());
        assert!(!s.to_toml().contains("anchor"));
    }

    #[test]
    fn the_monitor_memory_is_saved_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.toml");
        let mut s = LayoutStore::empty(id(1));
        let desk = vec![
            mon("HDMI-A-1", 0, 0, 1920, 1080),
            mon("DVI-D-1", 1920, 0, 1920, 1080),
        ];
        let (framed, changed) = s.observe_own_monitors(&desk);
        assert!(
            changed && framed == desk,
            "the first reading is remembered, nothing shifts"
        );
        // HDMI-A-1 unplugged, DVI-D-1 moved to 0 by Hyprland
        let (framed, changed) = s.observe_own_monitors(&[mon("DVI-D-1", 0, 0, 1920, 1080)]);
        assert!(changed);
        assert_eq!(framed[0].shift_xy(), (1920, 0));
        s.save(&path).unwrap();
        let back = LayoutStore::load(&path, id(1)).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.anchors.shift, (1920, 0));
        assert_eq!(
            back.anchors.monitors.len(),
            2,
            "HDMI-A-1 is remembered while it is away"
        );
        // after a restart the same reading changes nothing, and HDMI-A-1 still finds its place
        let mut back = back;
        let (framed, changed) = back.observe_own_monitors(&[mon("DVI-D-1", 0, 0, 1920, 1080)]);
        assert!(!changed, "reading the same monitors again is not a change");
        assert_eq!(framed[0].shift_xy(), (1920, 0));
        let (framed, _) = back.observe_own_monitors(&desk);
        assert!(framed.iter().all(|m| m.shift.is_none()), "{framed:?}");
        // the layout itself was not touched: no revision for a monitor change
        assert_eq!(back.layout, LayoutStore::empty(id(1)).layout);
    }

    #[test]
    fn a_batch_of_origins_is_one_revision_here_too() {
        let s = LayoutStore::empty(id(1));
        let l = s.with_origins(
            id(1),
            &[
                Origin {
                    peer: id(2),
                    x: 10,
                    y: 20,
                },
                Origin {
                    peer: id(3),
                    x: 30,
                    y: 40,
                },
            ],
        );
        assert_eq!(
            l.rev,
            Revision {
                counter: 1,
                peer: id(1)
            }
        );
        assert_eq!(l.origin(id(2)), Some((10, 20)));
        assert_eq!(l.origin(id(3)), Some((30, 40)));
        assert_eq!(l.origin(id(1)), Some((0, 0)));
    }

    #[test]
    fn a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = LayoutStore::load(&dir.path().join("layout.toml"), id(1)).unwrap();
        assert_eq!(s, LayoutStore::empty(id(1)));
    }

    #[test]
    fn corrupted_files_are_a_clear_error_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.toml");
        let good = peer_hex(id(2));
        let cases = [
            ("[revision\n".to_string(), "corrupted"),
            ("[revision]\ncounter = 1\n".to_string(), "peer"),
            (
                "[revision]\ncounter = 1\npeer = \"zz\"\n".to_string(),
                "invalid peer",
            ),
            (
                format!(
                    "[[origin]]\npeer = \"{good}\"\nx = 0\ny = 0\n[[origin]]\npeer = \"{good}\"\nx = 1\ny = 1\n"
                ),
                "repeats",
            ),
            (
                format!("[[origin]]\npeer = \"{good}\"\nx = \"a\"\ny = 0\n"),
                "x",
            ),
            (
                format!("[[origin]]\npeer = \"{good}\"\nx = -2147483648\ny = 0\n"),
                "out of range",
            ),
            (
                format!(
                    "[[known]]\npeer = \"{good}\"\n[[known.monitor]]\nname = \"eDP-1\"\n\
                     description = \"\"\nscale = 1.0\ntransform = 0\n[known.monitor.rect]\n\
                     x = 0\ny = 0\nw = 4294967295\nh = 768\n"
                ),
                "out of range",
            ),
            ("[anchor]\nx = 99999999\ny = 0\n".to_string(), "[anchor]"),
            (
                "[anchor]\nx = 0\ny = 0\n[[anchor.monitor]]\nkey = \"a\"\nx = 0\ny = 0\npresent = true\n\
                 [[anchor.monitor]]\nkey = \"a\"\nx = 1\ny = 1\npresent = false\n"
                    .to_string(),
                "remembered twice",
            ),
            (
                "[anchor]\nx = 0\ny = 0\n[[anchor.monitor]]\nkey = \"a\\nb\"\nx = 0\ny = 0\npresent = true\n"
                    .to_string(),
                "key that is not allowed",
            ),
        ];
        for (text, needle) in cases {
            std::fs::write(&path, &text).unwrap();
            let e = LayoutStore::load(&path, id(1)).unwrap_err().to_string();
            assert!(e.contains(path.to_str().unwrap()), "{e}");
            assert!(e.contains(needle), "{needle}: {e}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
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

    fn arb_peer() -> impl Strategy<Value = PeerId> {
        any::<[u8; 32]>().prop_map(PeerId)
    }

    fn arb_monitor() -> impl Strategy<Value = MonitorInfo> {
        (
            "[A-Za-z0-9-]{1,12}",
            -4000i32..4000,
            -4000i32..4000,
            1u32..8000,
            1u32..8000,
        )
            .prop_map(|(name, x, y, w, h)| mon(&name, x, y, w, h))
    }

    /// A memory `observe` could have written, or at least one `check` accepts.
    fn arb_anchors() -> impl Strategy<Value = Anchors> {
        let cap = 1i32 << 24;
        (
            (-cap..=cap, -cap..=cap),
            proptest::collection::btree_map(
                "[A-Za-z0-9 ._-]{1,24}",
                (
                    -2 * cap..=2 * cap,
                    -2 * cap..=2 * cap,
                    any::<bool>(),
                    proptest::option::of("[A-Z]{1,4}-[0-9]"),
                ),
                0..=8,
            ),
        )
            .prop_map(|(shift, monitors)| Anchors {
                shift,
                monitors: monitors
                    .into_iter()
                    .map(|(key, (x, y, present, connector))| Remembered {
                        key,
                        connector,
                        x,
                        y,
                        present,
                    })
                    .collect(),
            })
    }

    fn arb_store() -> impl Strategy<Value = LayoutStore> {
        (
            arb_peer(),
            any::<u64>(),
            proptest::collection::btree_map(
                any::<[u8; 32]>(),
                (-4000i32..4000, -4000i32..4000),
                0..5,
            ),
            proptest::collection::btree_map(
                any::<[u8; 32]>(),
                proptest::collection::vec(arb_monitor(), 0..3),
                0..4,
            ),
            arb_anchors(),
        )
            .prop_map(|(peer, counter, origins, known, anchors)| LayoutStore {
                layout: Layout {
                    rev: Revision { counter, peer },
                    origins: origins
                        .into_iter()
                        .map(|(p, (x, y))| Origin {
                            peer: PeerId(p),
                            x,
                            y,
                        })
                        .collect(),
                },
                known: known
                    .into_iter()
                    .map(|(p, monitors)| PeerMonitors {
                        peer: PeerId(p),
                        monitors,
                    })
                    .collect(),
                anchors,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// `layout.toml` round-trips through `to_toml` and `parse`.
        #[test]
        fn a_layout_store_parses_back_from_its_toml(s in arb_store()) {
            prop_assert_eq!(
                LayoutStore::parse(&s.to_toml(), Path::new("/x"), id(1)).unwrap(),
                s
            );
        }

        /// A damaged `layout.toml` is `Ok` or an error. An `Ok` has every origin inside the wire
        /// caps (A-03) and no peer twice.
        #[test]
        fn layout_store_parse_never_panics_and_keeps_its_invariants(
            raw in "\\PC{0,300}",
            damaged in arb_store().prop_flat_map(|s| mangled(s.to_toml())),
        ) {
            for text in [raw, damaged] {
                if let Ok(s) = LayoutStore::parse(&text, Path::new("/x"), id(1)) {
                    prop_assert!(s.layout.origins.iter().all(|o| coord_in_range(o.x) && coord_in_range(o.y)));
                    let mut ps: Vec<PeerId> = s.layout.origins.iter().map(|o| o.peer).collect();
                    ps.sort_unstable();
                    ps.dedup();
                    prop_assert_eq!(ps.len(), s.layout.origins.len());
                }
            }
        }
    }
}
