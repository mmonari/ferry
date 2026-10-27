//! Monitor memory and the anchor shift (M3 design 4.1).
//!
//! Hyprland re-lays out a peer's monitors on a hot-plug: with `auto` positions, unplugging the
//! left monitor moves the right one to x = 0. The layout puts a monitor at `rect + origin`, so
//! without help the monitor that stayed would jump on the plane although nobody edited anything.
//! ADR 0002 says a local monitor change never needs a layout edit, so the group is not moved by a
//! revision. Its owner translates it instead: [`Anchors::observe`] turns each reading of the own
//! monitors into the same monitors plus one **anchor shift**, and a monitor sits on the plane at
//! `rect + shift + origin`. The shift travels in [`MonitorInfo::shift`].
//!
//! The memory is per monitor, keyed by [`monitor_keys`] (Hyprland description, else connector
//! name) and matched on the connector first, so identical monitors stay apart. It records where each monitor sits in the *stable frame* `rect + shift`, whether it was
//! present in the last reading, and the current shift. It is persisted in `layout.toml`, so a
//! monitor that returns after a restart finds its place too.
//!
//! Sans-IO: the daemon calls [`Anchors::observe`] and saves the result.

use crate::geom::{MonitorInfo, WireOffset, coord_in_range, validate_monitor_text};
use crate::wire::caps;

/// Keys remembered at most; the oldest absent ones go first. A peer reports at most
/// [`caps::MONITORS`] monitors, so a full memory is the present ones plus the last unplugged.
pub const MAX_REMEMBERED: usize = 32;

/// What `observe` remembers of one monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remembered {
    /// [`monitor_keys`].
    pub key: String,
    /// The connector it was last read on (`DP-1`), which tells identical monitors apart.
    /// `None` for an entry from a file written before the connector was remembered.
    pub connector: Option<String>,
    /// Where the monitor's top-left sat in the stable frame, `rect + shift`.
    pub x: i32,
    pub y: i32,
    /// It was in the last reading.
    pub present: bool,
}

/// The memory of one peer's own monitors, and the shift it works out from it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Anchors {
    /// The shift of the last reading.
    pub shift: (i32, i32),
    /// Oldest first: a monitor read again goes to the end.
    pub monitors: Vec<Remembered>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnchorsError {
    #[error("the anchor shift is outside the coordinate cap")]
    ShiftRange,
    #[error("a remembered monitor {0:?} is outside the coordinate cap")]
    Range(String),
    #[error("a remembered monitor has a key that is not allowed: {0}")]
    Key(String),
    #[error("the monitor {0:?} is remembered twice")]
    Duplicate(String),
    #[error("more than {MAX_REMEMBERED} monitors are remembered")]
    TooMany,
}

/// How each monitor of a reading is recognised across hot-plugs: its description, else its
/// connector name. A key that an earlier monitor of the same reading already has gets `#2`, `#3`,
/// in reading order. The same panel on another connector keeps its key; the base is cut so that
/// every key is a valid monitor text (`geom::validate_monitor_text`).
pub fn monitor_keys(reading: &[MonitorInfo]) -> Vec<String> {
    let mut seen: Vec<(String, usize)> = Vec::new();
    reading
        .iter()
        .map(|m| {
            let base = if m.description.is_empty() {
                &m.name
            } else {
                &m.description
            };
            let base = cut(base, caps::MONITOR_TEXT - 8);
            match seen.iter_mut().find(|(b, _)| *b == base) {
                Some((_, n)) => {
                    *n += 1;
                    format!("{base}#{n}")
                }
                None => {
                    seen.push((base.to_string(), 1));
                    base.to_string()
                }
            }
        })
        .collect()
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Anchors {
    /// Nothing remembered and no shift: what a peer that never saw a monitor has.
    pub fn is_empty(&self) -> bool {
        self.shift == (0, 0) && self.monitors.is_empty()
    }

    /// Checks a memory that came from a file: every position and key is one `observe` could have
    /// written.
    pub fn check(&self) -> Result<(), AnchorsError> {
        if !(coord_in_range(self.shift.0) && coord_in_range(self.shift.1)) {
            return Err(AnchorsError::ShiftRange);
        }
        if self.monitors.len() > MAX_REMEMBERED {
            return Err(AnchorsError::TooMany);
        }
        // a stable-frame position is a monitor position plus a shift: twice the cap at most
        let frame_cap = caps::COORD * 2;
        for (i, r) in self.monitors.iter().enumerate() {
            if let Err(why) = validate_monitor_text(&r.key) {
                return Err(AnchorsError::Key(why));
            }
            if let Some(Err(why)) = r.connector.as_deref().map(validate_monitor_text) {
                return Err(AnchorsError::Key(why));
            }
            if r.x.unsigned_abs() > frame_cap || r.y.unsigned_abs() > frame_cap {
                return Err(AnchorsError::Range(r.key.clone()));
            }
            if self.monitors[..i].iter().any(|o| o.key == r.key) {
                return Err(AnchorsError::Duplicate(r.key.clone()));
            }
        }
        Ok(())
    }

    /// Reads the own monitors as Hyprland reports them (`reading`, the shift of each ignored) and
    /// returns them with the anchor shift every peer must add.
    ///
    /// 1. Each monitor with a remembered entry proposes a shift: the remembered position minus
    ///    the position it has now.
    /// 2. Monitors that were present in the last reading (they stayed) decide. If none stayed,
    ///    the ones that were absent (they returned) decide. With neither, the shift is kept.
    /// 3. Within the deciding group: a monitor that Hyprland did not move proposes the current
    ///    shift, and then the frame did not move, so the shift is kept. Otherwise every monitor
    ///    moved, and the most common proposal wins; a tie goes to a proposal of zero (no shift),
    ///    else to the first monitor of the reading.
    /// 4. Every monitor of the reading is remembered at `rect + shift`. The absent ones keep
    ///    their entry, which is how a returning monitor finds its place.
    ///
    /// A shift past the coordinate cap is not sent: the memory is dropped and the shift is zero.
    /// The result is the same for the same memory and reading, and reading the same monitors a
    /// second time changes nothing.
    pub fn observe(&mut self, reading: &[MonitorInfo]) -> Vec<MonitorInfo> {
        let keys = monitor_keys(reading);
        let matched = self.match_entries(reading, &keys);
        let mut stayed: Vec<((i32, i32), usize)> = Vec::new();
        let mut returned: Vec<((i32, i32), usize)> = Vec::new();
        for (i, (m, entry)) in reading.iter().zip(&matched).enumerate() {
            let Some(r) = entry.map(|e| &self.monitors[e]) else {
                continue;
            };
            let proposal = (r.x.saturating_sub(m.rect.x), r.y.saturating_sub(m.rect.y));
            if r.present {
                stayed.push((proposal, i));
            } else {
                returned.push((proposal, i));
            }
        }
        let deciding = if stayed.is_empty() { returned } else { stayed };
        let mut shift = most_common(&deciding, self.shift).unwrap_or(self.shift);
        let mut matched = matched;
        if !(coord_in_range(shift.0) && coord_in_range(shift.1)) {
            self.monitors.clear();
            matched.iter_mut().for_each(|e| *e = None);
            shift = (0, 0);
        }
        self.shift = shift;

        // A new monitor's key: its reading-order key, unless an entry (kept, or matched by another
        // monitor of this reading) already holds it.
        let mut held: Vec<String> = self.monitors.iter().map(|r| r.key.clone()).collect();
        let keys: Vec<String> = keys
            .into_iter()
            .zip(&matched)
            .map(|(key, e)| match e {
                Some(_) => key,
                None => {
                    let key = free_key(&held, key);
                    held.push(key.clone());
                    key
                }
            })
            .collect();

        // Take the matched entries out, so that every monitor read goes to the end (newest).
        let mut taken: Vec<Option<Remembered>> = vec![None; reading.len()];
        let mut by_entry: Vec<(usize, usize)> = matched
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.map(|e| (e, i)))
            .collect();
        by_entry.sort_unstable_by_key(|&(e, _)| std::cmp::Reverse(e));
        for (e, i) in by_entry {
            taken[i] = Some(self.monitors.remove(e));
        }
        for r in &mut self.monitors {
            r.present = false;
        }
        for ((m, key), entry) in reading.iter().zip(keys).zip(taken) {
            if !m.position_in_range() {
                continue;
            }
            let mut entry = entry.unwrap_or(Remembered {
                key,
                connector: None,
                x: 0,
                y: 0,
                present: true,
            });
            entry.connector = Some(m.name.clone()).filter(|c| validate_monitor_text(c).is_ok());
            entry.x = m.rect.x.saturating_add(shift.0);
            entry.y = m.rect.y.saturating_add(shift.1);
            entry.present = true;
            self.monitors.push(entry);
        }
        while self.monitors.len() > MAX_REMEMBERED {
            match self.monitors.iter().position(|r| !r.present) {
                Some(i) => {
                    self.monitors.remove(i);
                }
                None => break,
            }
        }

        reading
            .iter()
            .map(|m| MonitorInfo {
                shift: WireOffset::of(shift),
                ..m.clone()
            })
            .collect()
    }
    /// The remembered entry of each monitor of the reading (an index into `monitors`), each entry
    /// used at most once. An entry with the same description and the same connector comes first:
    /// that is what tells identical monitors apart when one of them goes. Then, for the
    /// monitors left, the entry with the same key ([`monitor_keys`], reading order), which is how
    /// a panel on another connector, or an entry written before connectors were remembered, is
    /// found. A monitor outside the coordinate cap matches nothing.
    fn match_entries(&self, reading: &[MonitorInfo], keys: &[String]) -> Vec<Option<usize>> {
        let mut matched: Vec<Option<usize>> = vec![None; reading.len()];
        let mut used = vec![false; self.monitors.len()];
        let usable = |m: &MonitorInfo| m.position_in_range();
        for (i, (m, key)) in reading.iter().zip(keys).enumerate() {
            if !usable(m) {
                continue;
            }
            let base = base_key(key);
            let found = self.monitors.iter().enumerate().position(|(e, r)| {
                !used[e]
                    && r.connector.as_deref() == Some(m.name.as_str())
                    && base_key(&r.key) == base
            });
            if let Some(e) = found {
                used[e] = true;
                matched[i] = Some(e);
            }
        }
        for (i, (m, key)) in reading.iter().zip(keys).enumerate() {
            if !usable(m) || matched[i].is_some() {
                continue;
            }
            let found = self
                .monitors
                .iter()
                .enumerate()
                .position(|(e, r)| !used[e] && r.key == *key);
            if let Some(e) = found {
                used[e] = true;
                matched[i] = Some(e);
            }
        }
        matched
    }
}

/// `key`, or the first `key`'s base with `#2`, `#3`, ... that `held` does not hold: an entry keeps
/// its key while it lives, so a new twin may find its reading-order key taken.
fn free_key(held: &[String], key: String) -> String {
    let taken = |k: &str| held.iter().any(|h| h == k);
    if !taken(&key) {
        return key;
    }
    let base = base_key(&key);
    (2..)
        .map(|n| format!("{base}#{n}"))
        .find(|k| !taken(k))
        .unwrap_or(key)
}

/// A key without the `#n` that [`monitor_keys`] numbers twins with.
fn base_key(key: &str) -> &str {
    match key.rsplit_once('#') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => key,
    }
}

/// The proposal to follow, or `None` for none: `current` when any monitor proposes it (that
/// monitor did not move, so the frame did not), else the most common proposal. A tie goes to a
/// proposal of zero, the frame Hyprland itself reports, else to the proposal whose monitor
/// comes first in the reading.
fn most_common(proposals: &[((i32, i32), usize)], current: (i32, i32)) -> Option<(i32, i32)> {
    let mut groups: Vec<((i32, i32), usize, usize)> = Vec::new();
    for &(shift, order) in proposals {
        match groups.iter_mut().find(|g| g.0 == shift) {
            Some(g) => g.1 += 1,
            None => groups.push((shift, 1, order)),
        }
    }
    groups
        .into_iter()
        .max_by_key(|&(shift, count, first)| {
            (
                shift == current,
                count,
                shift == (0, 0),
                std::cmp::Reverse(first),
            )
        })
        .map(|(shift, _, _)| shift)
}

#[cfg(test)]
mod tests;
