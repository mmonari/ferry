//! `config.toml` sections that the machines read. ferryd
//! parses the file; these are the validated values.

use serde::{Deserialize, Serialize};

use crate::chord::{EscapeChord, GoChord};
use crate::time::LockBudget;

/// `[crossing]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrossingConfig {
    /// 0–200; 0 crosses on contact.
    pub edge_resistance_px: u16,
    /// Time-based switch delay, 0 = off.
    pub edge_dwell_ms: u16,
    /// 0–200 at each end of every link.
    pub corner_dead_zone_px: u16,
    /// The return-direction drag block.
    pub block_while_button_held: bool,
    /// Parsed with [`EscapeChord::parse`] by ferryd.
    pub escape_chord: String,
    /// Parsed with [`GoChord::parse`] by ferryd: these modifiers and an arrow key move control
    /// to the machine on that side.
    pub go_chord: String,
    /// Send the `edge` events that make a frontend light the edge of a crossing.
    pub edge_glow: bool,
}

impl Default for CrossingConfig {
    fn default() -> Self {
        Self {
            edge_resistance_px: 0,
            edge_dwell_ms: 0,
            corner_dead_zone_px: 8,
            block_while_button_held: true,
            escape_chord: "Super+Ctrl+Escape".into(),
            go_chord: "Super+Ctrl+Alt".into(),
            edge_glow: true,
        }
    }
}

/// What [`crate::source::SourceMachine`] runs with: `[crossing]` validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConfig {
    pub edge_resistance_px: u16,
    pub edge_dwell_ms: u16,
    pub corner_dead_zone_px: u16,
    pub block_while_button_held: bool,
    pub escape_chord: EscapeChord,
    pub go_chord: GoChord,
    /// The repeat filter holds a key's release for up to 1 ms, waiting for a
    /// synthesized re-press, only once the key has been down this long.
    /// Ctl sets it from the source's
    /// `input:repeat_delay` − 50 ms (desktop: 600 → 550). 0 holds every
    /// release: safe, at most 1 ms on any release.
    pub repeat_hold_after_ms: u32,
    /// `[safety] lock_check_budget_ms`, clamped. It sets the active lock
    /// poll and the Pending timeout too ([`LockBudget`]).
    pub lock_check_budget: LockBudget,
}

impl Default for SourceConfig {
    fn default() -> Self {
        let c = CrossingConfig::default();
        Self {
            edge_resistance_px: c.edge_resistance_px,
            edge_dwell_ms: c.edge_dwell_ms,
            corner_dead_zone_px: c.corner_dead_zone_px,
            block_while_button_held: c.block_while_button_held,
            escape_chord: EscapeChord::default(),
            go_chord: GoChord::default(),
            repeat_hold_after_ms: 0,
            lock_check_budget: LockBudget::DEFAULT,
        }
    }
}

/// `~/.local/state/ferry/layout.toml`: the revisioned
/// [`Layout`](crate::geom::Layout) as `[revision]` + `[[origin]]`, plus the
/// last-seen monitors per peer as `[[known]]`, which are not part of the
/// revisioned value, and this peer's monitor memory as `[anchor]` +
/// `[[anchor.monitor]]` (M3 design 4.1), which is not either. ferryd reads and
/// writes the file (atomically); this is only its shape. Peers are 64-hex
/// fingerprints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LayoutFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<RevisionEntry>,
    #[serde(default, rename = "origin", skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<OriginEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known: Vec<KnownEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<AnchorFile>,
}

/// `[anchor]`: the anchor shift (`x`, `y`) and the monitors remembered, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorFile {
    pub x: i32,
    pub y: i32,
    #[serde(default, rename = "monitor", skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<AnchorEntry>,
}

/// `[[anchor.monitor]]`: where a monitor sat in the stable frame, and whether it was there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorEntry {
    pub key: String,
    /// The connector it was last read on; files written before F-1 have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    pub x: i32,
    pub y: i32,
    pub present: bool,
}

impl AnchorFile {
    /// The file form of a memory; `None` for an empty one, so a peer that never moved leaves no
    /// `[anchor]` table behind.
    pub fn of(anchors: &crate::anchor::Anchors) -> Option<Self> {
        (!anchors.is_empty()).then(|| AnchorFile {
            x: anchors.shift.0,
            y: anchors.shift.1,
            monitors: anchors
                .monitors
                .iter()
                .map(|r| AnchorEntry {
                    key: r.key.clone(),
                    connector: r.connector.clone(),
                    x: r.x,
                    y: r.y,
                    present: r.present,
                })
                .collect(),
        })
    }

    /// The memory this table holds, if every position and key is one `observe` could have
    /// written.
    pub fn anchors(&self) -> Result<crate::anchor::Anchors, crate::anchor::AnchorsError> {
        let anchors = crate::anchor::Anchors {
            shift: (self.x, self.y),
            monitors: self
                .monitors
                .iter()
                .map(|e| crate::anchor::Remembered {
                    key: e.key.clone(),
                    connector: e.connector.clone(),
                    x: e.x,
                    y: e.y,
                    present: e.present,
                })
                .collect(),
        };
        anchors.check()?;
        Ok(anchors)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionEntry {
    pub counter: u64,
    pub peer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginEntry {
    pub peer: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownEntry {
    pub peer: String,
    /// `[[known.monitor]]`; `monitors`, as ferryd wrote it before M1's wiring, still reads.
    #[serde(default, rename = "monitor", alias = "monitors")]
    pub monitors: Vec<crate::geom::MonitorInfo>,
}

impl LayoutFile {
    pub fn new(layout: &crate::geom::Layout, known: &[crate::geom::PeerMonitors]) -> Self {
        LayoutFile {
            revision: Some(RevisionEntry {
                counter: layout.rev.counter,
                peer: layout.rev.peer.to_string(),
            }),
            origins: layout
                .origins
                .iter()
                .map(|o| OriginEntry {
                    peer: o.peer.to_string(),
                    x: o.x,
                    y: o.y,
                })
                .collect(),
            known: known
                .iter()
                .map(|k| KnownEntry {
                    peer: k.peer.to_string(),
                    monitors: k.monitors.clone(),
                })
                .collect(),
            anchor: None,
        }
    }

    /// The same file with this peer's monitor memory in it.
    #[must_use]
    pub fn with_anchors(mut self, anchors: &crate::anchor::Anchors) -> Self {
        self.anchor = AnchorFile::of(anchors);
        self
    }

    /// The revisioned value; a file without `[revision]` is `(0, me)`.
    pub fn layout(
        &self,
        me: crate::PeerId,
    ) -> Result<crate::geom::Layout, crate::peer::FingerprintError> {
        use crate::PeerId;
        use crate::geom::{Layout, Origin, Revision};
        let rev = match &self.revision {
            Some(r) => Revision {
                counter: r.counter,
                peer: PeerId::parse(&r.peer)?,
            },
            None => Revision {
                counter: 0,
                peer: me,
            },
        };
        let mut layout = Layout {
            rev,
            origins: Vec::new(),
        };
        for o in &self.origins {
            layout.set_origin(Origin {
                peer: PeerId::parse(&o.peer)?,
                x: o.x,
                y: o.y,
            });
        }
        Ok(layout)
    }

    pub fn known(&self) -> Result<Vec<crate::geom::PeerMonitors>, crate::peer::FingerprintError> {
        self.known
            .iter()
            .map(|k| {
                Ok(crate::geom::PeerMonitors {
                    peer: crate::PeerId::parse(&k.peer)?,
                    monitors: k.monitors.clone(),
                })
            })
            .collect()
    }
}
