//! Layout geometry: monitors on one logical plane, edge links derived as
//! touching segments, placement, and barriers.
//!
//! Coordinates are logical pixels (after scale). A peer's own
//! Hyprland coordinates are *local*; the shared plane is local + the peer's
//! origin. The source's cursor model works on the plane; everything sent to a
//! target is in that target's local coordinates; everything sent to the
//! portal is in the source's local coordinates.

use serde::{Deserialize, Serialize};

use crate::peer::PeerId;

/// A point in logical pixels (plane or local, as the context says).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A half-open rectangle `[x, x+w) × [y, y+h)` in integer logical pixels.
/// Hyprland positions are integers; sizes are rounded the way `GetZones`
/// reports them, so links and barriers agree with the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= f64::from(self.x)
            && p.x < f64::from(self.x) + f64::from(self.w)
            && p.y >= f64::from(self.y)
            && p.y < f64::from(self.y) + f64::from(self.h)
    }

    /// The point inside `self` nearest to `p` (pixel-inclusive: at most
    /// `x+w-1`, `y+h-1`).
    pub fn clamp(&self, p: Point) -> Point {
        let (x0, y0) = (f64::from(self.x), f64::from(self.y));
        let x1 = (x0 + f64::from(self.w) - 1.0).max(x0);
        let y1 = (y0 + f64::from(self.h) - 1.0).max(y0);
        Point {
            x: p.x.clamp(x0, x1),
            y: p.y.clamp(y0, y1),
        }
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }

    pub fn overlaps(&self, other: &Rect) -> bool {
        self.w > 0
            && self.h > 0
            && other.w > 0
            && other.h > 0
            && self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// The bounding box of `rects`; `None` when empty.
    pub fn bounding(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
        let mut it = rects.into_iter();
        let first = it.next()?;
        let (mut x0, mut y0) = (first.x, first.y);
        let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
        for r in it {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.x + r.w);
            y1 = y1.max(r.y + r.h);
        }
        Some(Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// A monitor's logical size from Hyprland's pixel mode, scale and
/// `wl_output` transform: `round(px / scale)`, with w and h swapped for the
/// transforms 1, 3, 5 and 7. It matches `GetZones` at the
/// scales S12 tested; for non-integral results it is unverified, so `ctl`
/// checks each barrier against `GetZones`.
pub fn logical_size(width_px: u32, height_px: u32, scale: f64, transform: u8) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // `as` saturates: a NaN or huge value cannot wrap.
    let w = (f64::from(width_px) / scale).round() as u32;
    let h = (f64::from(height_px) / scale).round() as u32;
    if transform % 2 == 1 { (h, w) } else { (w, h) }
}

/// The point `inset` px inside `r` from its `side`, at `p`'s position
/// otherwise (clamped into `r`). Release positions use 2 px: the
/// insets keep the next motion from re-activating at once.
pub fn inside(r: &Rect, side: Side, p: Point, inset: f64) -> Point {
    let mut q = r.clamp(p);
    let (x0, y0) = (f64::from(r.x), f64::from(r.y));
    let (x1, y1) = (x0 + f64::from(r.w) - 1.0, y0 + f64::from(r.h) - 1.0);
    match side {
        Side::Left => q.x = q.x.max((x0 + inset).min(x1)),
        Side::Right => q.x = q.x.min((x1 - inset).max(x0)),
        Side::Top => q.y = q.y.max((y0 + inset).min(y1)),
        Side::Bottom => q.y = q.y.min((y1 - inset).max(y0)),
    }
    q
}

/// A monitor side. Generic over all four directions: every rule below is
/// written once against `Side` and its helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

    /// The side that touches this one across a link.
    pub fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }

    /// Left and right edges run along y; top and bottom along x.
    pub fn is_vertical(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    /// The plane coordinate of the edge line of `r` on this side:
    /// `x` (left), `x+w` (right), `y` (top), `y+h` (bottom).
    pub fn line(self, r: &Rect) -> i32 {
        match self {
            Side::Left => r.x,
            Side::Right => r.x + r.w,
            Side::Top => r.y,
            Side::Bottom => r.y + r.h,
        }
    }

    /// `r`'s extent along this side, `[lo, hi)`.
    pub fn span(self, r: &Rect) -> (i32, i32) {
        if self.is_vertical() {
            (r.y, r.y + r.h)
        } else {
            (r.x, r.x + r.w)
        }
    }

    /// The coordinate of `p` across this side (x for vertical sides, else y).
    pub fn across(self, p: Point) -> f64 {
        if self.is_vertical() { p.x } else { p.y }
    }

    /// The point at `along` on this side's axis and `across` on the other.
    pub fn point(self, along: f64, across: f64) -> Point {
        if self.is_vertical() {
            Point {
                x: across,
                y: along,
            }
        } else {
            Point {
                x: along,
                y: across,
            }
        }
    }

    /// Has `p` gone past this side of `r` (outside the half-open rect)?
    pub fn beyond(self, r: &Rect, p: Point) -> bool {
        let line = f64::from(self.line(r));
        match self {
            Side::Left | Side::Top => self.across(p) < line,
            Side::Right | Side::Bottom => self.across(p) >= line,
        }
    }

    /// The coordinate of `p` along this side (y for vertical sides, else x).
    pub fn along(self, p: Point) -> f64 {
        if self.is_vertical() { p.y } else { p.x }
    }

    /// The outward component of a relative motion (positive = through the
    /// edge, away from the monitor).
    pub fn outward(self, dx: f64, dy: f64) -> f64 {
        match self {
            Side::Left => -dx,
            Side::Right => dx,
            Side::Top => -dy,
            Side::Bottom => dy,
        }
    }
}

/// Whether a peer monitor position or a layout origin is inside
/// [`crate::wire::caps::COORD`].
pub fn coord_in_range(v: i32) -> bool {
    v.unsigned_abs() <= crate::wire::caps::COORD
}

/// A monitor name, or description, from a peer or from Hyprland. It is printed in `ferry layout
/// show`, in `status` and in logs, and `<peer>:<monitor>` anchors name it, so a control
/// character (a cursor escape, a newline) must not get in. The length is the wire cap.
pub fn validate_monitor_text(text: &str) -> Result<(), String> {
    if text.len() > crate::wire::caps::MONITOR_TEXT {
        return Err(format!(
            "a monitor name is at most {} bytes",
            crate::wire::caps::MONITOR_TEXT
        ));
    }
    match text.chars().find(|c| c.is_control()) {
        Some(c) => Err(format!("a monitor name cannot contain {c:?}")),
        None => Ok(()),
    }
}

impl MonitorInfo {
    /// Whether the name and description pass [`validate_monitor_text`].
    pub fn text_is_clean(&self) -> bool {
        validate_monitor_text(&self.name).is_ok()
            && validate_monitor_text(&self.description).is_ok()
    }

    /// Whether the position is inside [`crate::wire::caps::COORD`].
    pub fn position_in_range(&self) -> bool {
        coord_in_range(self.rect.x) && coord_in_range(self.rect.y)
    }

    /// Whether the size is inside [`crate::wire::caps::MONITOR_SIZE`].
    pub fn size_in_range(&self) -> bool {
        let cap = crate::wire::caps::MONITOR_SIZE;
        self.rect.w <= cap && self.rect.h <= cap
    }

    /// Whether the anchor shift (if any) is inside [`crate::wire::caps::COORD`].
    pub fn shift_in_range(&self) -> bool {
        self.shift
            .is_none_or(|s| coord_in_range(s.x) && coord_in_range(s.y))
    }

    /// The position, the size and the shift: what the wire decoder and the layout file accept
    /// from a peer.
    pub fn in_range(&self) -> bool {
        self.position_in_range() && self.size_in_range() && self.shift_in_range()
    }

    /// The anchor shift this report carries; (0, 0) when it carries none.
    pub fn shift_xy(&self) -> (i32, i32) {
        self.shift.map_or((0, 0), |s| (s.x, s.y))
    }

    /// The local logical rectangle as a [`Rect`] (sizes saturate at `i32::MAX`).
    pub fn local_rect(&self) -> Rect {
        Rect {
            x: self.rect.x,
            y: self.rect.y,
            w: i32::try_from(self.rect.w).unwrap_or(i32::MAX),
            h: i32::try_from(self.rect.h).unwrap_or(i32::MAX),
        }
    }
}

/// One monitor as its own peer reports it (Hyprland IPC, local logical
/// coordinates). Also the wire type inside `Hello` and `Monitors`.
///
/// Encoded as a CBOR array. Field 5, the anchor shift, is left out when it is `None`, so a
/// report without one is the five elements it always was (wire evolution rules, M3 design 4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, minicbor::Decode)]
pub struct MonitorInfo {
    /// Connector, e.g. `HDMI-A-1`.
    #[n(0)]
    pub name: String,
    /// Hyprland `description` (make, model, serial): the stable identity
    /// for FR-LAYOUT-03 (see [`crate::anchor`]).
    #[n(1)]
    pub description: String,
    /// Local logical rectangle.
    #[n(2)]
    pub rect: WireRect,
    #[n(3)]
    pub scale: f32,
    /// `wl_output` transform, 0–7.
    #[n(4)]
    pub transform: u8,
    /// How far this peer moved its whole group to keep its monitors in place across a hot-plug
    /// (M3 design 4.1): the plane position of the monitor is `rect + shift +
    /// origin`. Every monitor of one report carries the same value. `None` is (0, 0).
    #[n(5)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<WireOffset>,
}

impl<C> minicbor::Encode<C> for MonitorInfo {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(if self.shift.is_some() { 6 } else { 5 })?;
        e.encode_with(&self.name, ctx)?;
        e.encode_with(&self.description, ctx)?;
        e.encode_with(self.rect, ctx)?;
        e.encode_with(self.scale, ctx)?;
        e.encode_with(self.transform, ctx)?;
        if let Some(shift) = self.shift {
            e.encode_with(shift, ctx)?;
        }
        Ok(())
    }
}

/// A translation in logical pixels as it travels in CBOR: `[x, y]`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, minicbor::Encode, minicbor::Decode,
)]
pub struct WireOffset {
    #[n(0)]
    pub x: i32,
    #[n(1)]
    pub y: i32,
}

impl WireOffset {
    /// The wire form of a shift: `None` for (0, 0), which is never sent.
    pub fn of(shift: (i32, i32)) -> Option<WireOffset> {
        (shift != (0, 0)).then_some(WireOffset {
            x: shift.0,
            y: shift.1,
        })
    }
}

/// [`Rect`] as it travels in CBOR: `[x, y, w, h]`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, minicbor::Encode, minicbor::Decode,
)]
pub struct WireRect {
    #[n(0)]
    pub x: i32,
    #[n(1)]
    pub y: i32,
    #[n(2)]
    pub w: u32,
    #[n(3)]
    pub h: u32,
}

/// Names one monitor of one peer: the index into that peer's monitor list
/// as last reported (connector order from Hyprland). Valid until that peer's
/// next `Monitors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId {
    pub peer: PeerId,
    pub index: u8,
}

/// A monitor on the shared plane.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedMonitor {
    pub id: MonitorId,
    pub name: String,
    /// Plane rectangle = local rectangle + the peer's origin.
    pub rect: Rect,
}

/// A layout revision `(counter, peer)`, compared lexicographically; the
/// higher one wins everywhere. M1 creates them; M2 syncs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision {
    pub counter: u64,
    pub peer: PeerId,
}

impl Revision {
    /// The revision of a new local edit: the highest counter
    /// seen plus one, stamped with this peer.
    pub fn next(highest_seen: Revision, me: PeerId) -> Revision {
        Revision {
            counter: highest_seen.counter.saturating_add(1),
            peer: me,
        }
    }
}

/// Where one peer's monitor group sits on the plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub peer: PeerId,
    pub x: i32,
    pub y: i32,
}

/// The shared layout value: per-peer origins under one revision. This is
/// exactly what M2 syncs (FR-LAYOUT-07/08); monitors are not part of it,
/// because each peer owns its own geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rev: Revision,
    pub origins: Vec<Origin>,
}

impl Layout {
    /// A layout with no origins, at revision `(0, me)`.
    pub fn empty(me: PeerId) -> Layout {
        Layout {
            rev: Revision {
                counter: 0,
                peer: me,
            },
            origins: Vec::new(),
        }
    }

    pub fn origin(&self, peer: PeerId) -> Option<(i32, i32)> {
        self.origins
            .iter()
            .find(|o| o.peer == peer)
            .map(|o| (o.x, o.y))
    }

    /// Set (or add) `peer`'s origin. The caller bumps the revision.
    pub fn set_origin(&mut self, origin: Origin) {
        match self.origins.iter_mut().find(|o| o.peer == origin.peer) {
            Some(o) => *o = origin,
            None => self.origins.push(origin),
        }
    }

    /// The layout after setting every origin in `moves`, as **one** edit: the revision is
    /// `Revision::next(self.rev, me)`, whatever the number of peers moved. A moved
    /// peer's entry goes to the end of the list; the others keep their place and value. `me` gets
    /// the origin (0, 0) the first time anything is placed (M1 design 7.3). Pure; the caller
    /// checks the result and saves it.
    pub fn with_origins(&self, me: PeerId, moves: &[Origin]) -> Layout {
        let mut origins: Vec<Origin> = self
            .origins
            .iter()
            .copied()
            .filter(|o| moves.iter().all(|m| m.peer != o.peer))
            .collect();
        for m in moves {
            // a peer named twice keeps its last origin
            origins.retain(|o| o.peer != m.peer);
            origins.push(*m);
        }
        if !origins.iter().any(|o| o.peer == me) {
            origins.insert(
                0,
                Origin {
                    peer: me,
                    x: 0,
                    y: 0,
                },
            );
        }
        Layout {
            rev: Revision::next(self.rev, me),
            origins,
        }
    }

    /// The layout without `peer` (FR-LAYOUT-13: an unpaired peer leaves the layout), at
    /// revision `Revision::next(self.rev, me)`. `None` when `peer` has no origin, or is `me`:
    /// nothing to remove, so no revision is spent. Pure; the caller saves.
    pub fn without(&self, me: PeerId, peer: PeerId) -> Option<Layout> {
        if peer == me || self.origin(peer).is_none() {
            return None;
        }
        Some(Layout {
            rev: Revision::next(self.rev, me),
            origins: self
                .origins
                .iter()
                .copied()
                .filter(|o| o.peer != peer)
                .collect(),
        })
    }

    /// The layout push: keep the higher revision
    /// (counter, then peer). Returns true when `received` replaced `self`.
    /// An equal revision keeps the current value.
    pub fn merge(&mut self, received: Layout) -> bool {
        if received.rev > self.rev {
            *self = received;
            true
        } else {
            false
        }
    }

    /// The value as wire `Layout` #10 carries it.
    pub fn to_wire(&self) -> crate::wire::Control {
        use crate::wire::control::{WireOrigin, WireRevision};
        crate::wire::Control::Layout {
            rev: WireRevision {
                counter: self.rev.counter,
                peer: self.rev.peer.0,
            },
            origins: self
                .origins
                .iter()
                .map(|o| WireOrigin {
                    peer: o.peer.0,
                    x: o.x,
                    y: o.y,
                })
                .collect(),
        }
    }

    /// The value from a decoded wire `Layout` #10; `None` for any other
    /// message.
    pub fn from_wire(msg: &crate::wire::Control) -> Option<Layout> {
        match msg {
            crate::wire::Control::Layout { rev, origins } => Some(Layout {
                rev: Revision {
                    counter: rev.counter,
                    peer: PeerId(rev.peer),
                },
                origins: origins
                    .iter()
                    .map(|o| Origin {
                        peer: PeerId(o.peer),
                        x: o.x,
                        y: o.y,
                    })
                    .collect(),
            }),
            _ => None,
        }
    }
}

/// One peer's monitors in local coordinates, as last reported.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerMonitors {
    pub peer: PeerId,
    pub monitors: Vec<MonitorInfo>,
}

impl PeerMonitors {
    /// The group's anchor shift: that of its first monitor, (0, 0) without one.
    pub fn shift(&self) -> (i32, i32) {
        self.monitors.first().map_or((0, 0), MonitorInfo::shift_xy)
    }

    /// Where the group's local (0, 0) sits on the plane when the layout puts its origin at
    /// `origin`: the origin plus the anchor shift. Both are inside [`crate::wire::caps::COORD`],
    /// so the sum fits in `i32`.
    pub fn plane_offset(&self, origin: (i32, i32)) -> (i32, i32) {
        let (sx, sy) = self.shift();
        (origin.0.saturating_add(sx), origin.1.saturating_add(sy))
    }
}

/// A directed edge link: the part of `from`'s `side` that touches `to`'s
/// opposite side. Every touching segment yields two links, one each way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeLink {
    pub from: MonitorId,
    pub side: Side,
    pub to: MonitorId,
    /// Plane coordinate of the shared edge line.
    pub line: i32,
    /// Extent along the line, `[lo, hi)`, non-empty.
    pub span: (i32, i32),
}

/// Everything the source machine needs about geometry: this peer's id, every
/// placed monitor of every trusted, known peer, and the derived links.
#[derive(Debug, Clone, PartialEq)]
pub struct Topology {
    pub me: PeerId,
    /// Where this peer's local (0, 0) sits on the plane, to convert plane ↔ local: its layout
    /// origin plus its anchor shift.
    pub my_origin: (i32, i32),
    pub monitors: Vec<PlacedMonitor>,
    pub links: Vec<EdgeLink>,
    /// Where the local (0, 0) of every peer in `monitors` (`me` included) sits on the plane (the
    /// layout origin plus the anchor shift), to convert plane positions to a target's local
    /// coordinates.
    pub origins: Vec<Origin>,
}

impl Topology {
    pub fn monitor(&self, id: MonitorId) -> Option<&PlacedMonitor> {
        self.monitors.iter().find(|m| m.id == id)
    }

    pub fn origin(&self, peer: PeerId) -> Option<(i32, i32)> {
        if peer == self.me {
            return Some(self.my_origin);
        }
        self.origins
            .iter()
            .find(|o| o.peer == peer)
            .map(|o| (o.x, o.y))
    }

    /// Plane → this peer's local coordinates (what the portal takes).
    pub fn to_local(&self, p: Point) -> Point {
        Point {
            x: p.x - f64::from(self.my_origin.0),
            y: p.y - f64::from(self.my_origin.1),
        }
    }

    /// This peer's local coordinates (the `Activated` x,y) → plane.
    pub fn to_plane(&self, p: Point) -> Point {
        Point {
            x: p.x + f64::from(self.my_origin.0),
            y: p.y + f64::from(self.my_origin.1),
        }
    }

    /// Plane → `peer`'s local coordinates (what a target injects).
    pub fn to_peer_local(&self, peer: PeerId, p: Point) -> Option<Point> {
        let (ox, oy) = self.origin(peer)?;
        Some(Point {
            x: p.x - f64::from(ox),
            y: p.y - f64::from(oy),
        })
    }
}

/// Place every peer that has an origin and known monitors, then derive the
/// links between monitors of different peers. Only peers in `trusted` (and
/// `me`) take part, so an untrusted peer's edges are walls.
pub fn topology(
    me: PeerId,
    layout: &Layout,
    monitors: &[PeerMonitors],
    trusted: &[PeerId],
) -> Topology {
    let mut placed = Vec::new();
    let mut origins = Vec::new();
    for pm in monitors {
        if pm.peer != me && !trusted.contains(&pm.peer) {
            continue;
        }
        if origins.iter().any(|o: &Origin| o.peer == pm.peer) {
            continue;
        }
        let Some(origin) = layout.origin(pm.peer) else {
            continue;
        };
        // The layout origin plus the peer's anchor shift: where its local (0, 0) sits.
        let (ox, oy) = pm.plane_offset(origin);
        origins.push(Origin {
            peer: pm.peer,
            x: ox,
            y: oy,
        });
        for (i, m) in pm.monitors.iter().enumerate() {
            let Ok(index) = u8::try_from(i) else { break };
            placed.push(PlacedMonitor {
                id: MonitorId {
                    peer: pm.peer,
                    index,
                },
                name: m.name.clone(),
                rect: m.local_rect().translate(ox, oy),
            });
        }
    }
    let links = derive_links(&placed);
    let my_origin = origins
        .iter()
        .find(|o| o.peer == me)
        .map(|o| (o.x, o.y))
        .or_else(|| layout.origin(me))
        .unwrap_or((0, 0));
    Topology {
        me,
        my_origin,
        monitors: placed,
        links,
        origins,
    }
}

/// Links are the touching segments between monitors of different peers, in
/// both directions. Monitors of the same peer never link.
pub fn derive_links(monitors: &[PlacedMonitor]) -> Vec<EdgeLink> {
    let mut links = Vec::new();
    for (i, a) in monitors.iter().enumerate() {
        for b in &monitors[i + 1..] {
            if a.id.peer == b.id.peer {
                continue;
            }
            for side in Side::ALL {
                if side.line(&a.rect) != side.opposite().line(&b.rect) {
                    continue;
                }
                let (alo, ahi) = side.span(&a.rect);
                let (blo, bhi) = side.span(&b.rect);
                let span = (alo.max(blo), ahi.min(bhi));
                if span.0 >= span.1 {
                    continue;
                }
                let line = side.line(&a.rect);
                links.push(EdgeLink {
                    from: a.id,
                    side,
                    to: b.id,
                    line,
                    span,
                });
                links.push(EdgeLink {
                    from: b.id,
                    side: side.opposite(),
                    to: a.id,
                    line,
                    span,
                });
            }
        }
    }
    links
}

/// `ferry layout place <peer> --below|--above|--left-of|--right-of <anchor>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rel {
    Below,
    Above,
    LeftOf,
    RightOf,
}

impl Rel {
    /// The side of the anchor the placed group goes against.
    pub fn side(self) -> Side {
        match self {
            Rel::Below => Side::Bottom,
            Rel::Above => Side::Top,
            Rel::LeftOf => Side::Left,
            Rel::RightOf => Side::Right,
        }
    }
}

/// `--align start|center|end|<px>`: how the placed group lines up with the
/// anchor along the shared edge. `Offset(px)` puts the group's start `px`
/// after the anchor's start (negative allowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Offset(i32),
}

/// What the placed group is put next to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The bounding box of that peer's whole group.
    Peer(PeerId),
    /// One monitor of a peer.
    Monitor(MonitorId),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceError {
    #[error("cannot place a peer relative to itself")]
    SelfAnchor,
    #[error("monitors of {0} are not known yet (the peer has never connected)")]
    UnknownMonitors(PeerId),
    #[error("anchor peer {0} is not placed")]
    AnchorUnplaced(PeerId),
    #[error("the placement overlaps {0}")]
    Overlap(PeerId),
    /// The origin would fall outside [`crate::wire::caps::COORD`], which
    /// every peer refuses (and a saved layout would no longer load).
    #[error("the placement is too far from the rest of the layout")]
    OutOfRange,
}

/// Compute the new origin of `peer` so its group's bounding box sits on the
/// `rel` side of `anchor`, aligned by `align`, touching it. Fails on overlap
/// with any other placed group. Pure: the caller bumps the
/// revision and persists.
pub fn place(
    layout: &Layout,
    monitors: &[PeerMonitors],
    peer: PeerId,
    rel: Rel,
    anchor: Anchor,
    align: Align,
) -> Result<Origin, PlaceError> {
    let anchor_peer = match anchor {
        Anchor::Peer(p) => p,
        Anchor::Monitor(m) => m.peer,
    };
    if anchor_peer == peer {
        return Err(PlaceError::SelfAnchor);
    }
    let local_rects = |p: PeerId| -> Option<Vec<Rect>> {
        let pm = monitors.iter().find(|m| m.peer == p)?;
        if pm.monitors.is_empty() {
            return None;
        }
        Some(pm.monitors.iter().map(MonitorInfo::local_rect).collect())
    };
    // Each group's anchor shift (M3 design 4.1): a group sits at origin + shift.
    let shift_of = |p: PeerId| -> (i32, i32) {
        monitors
            .iter()
            .find(|m| m.peer == p)
            .map_or((0, 0), PeerMonitors::shift)
    };
    let group = local_rects(peer).ok_or(PlaceError::UnknownMonitors(peer))?;
    let (ax, ay) = layout
        .origin(anchor_peer)
        .ok_or(PlaceError::AnchorUnplaced(anchor_peer))?;
    let (asx, asy) = shift_of(anchor_peer);
    let anchor_rects = local_rects(anchor_peer).ok_or(PlaceError::UnknownMonitors(anchor_peer))?;
    let anchor_box = match anchor {
        Anchor::Peer(_) => Rect::bounding(anchor_rects.iter().copied()),
        Anchor::Monitor(m) => anchor_rects.get(usize::from(m.index)).copied(),
    }
    .ok_or(PlaceError::UnknownMonitors(anchor_peer))?
    .translate(ax.saturating_add(asx), ay.saturating_add(asy));
    let gbox = Rect::bounding(group.iter().copied()).ok_or(PlaceError::UnknownMonitors(peer))?;
    let (gsx, gsy) = shift_of(peer);

    // Across: the group's box touches the anchor's `side` line.
    let side = rel.side();
    let line = side.line(&anchor_box);
    let (g_across_len, g_along_len) = if side.is_vertical() {
        (gbox.w, gbox.h)
    } else {
        (gbox.h, gbox.w)
    };
    let across = match side {
        Side::Right | Side::Bottom => line,
        Side::Left | Side::Top => line - g_across_len,
    };
    // Along: the alignment against the anchor's span.
    let (lo, hi) = side.span(&anchor_box);
    let along = match align {
        Align::Start => lo,
        Align::Center => lo + (hi - lo - g_along_len).div_euclid(2),
        Align::End => hi - g_along_len,
        // Clamped so the sums below cannot overflow; the range check
        // after them refuses a result past the cap.
        Align::Offset(px) => {
            let c = i32::try_from(crate::wire::caps::COORD).unwrap_or(i32::MAX);
            lo + px.clamp(-c, c)
        }
    };
    let (bx, by) = if side.is_vertical() {
        (across, along)
    } else {
        (along, across)
    };
    // The group's box lands at (bx, by) on the plane: its local (0, 0) sits at the origin plus the
    // shift, so the layout origin is what is left after the box position and the shift.
    let origin = Origin {
        peer,
        x: bx.saturating_sub(gbox.x).saturating_sub(gsx),
        y: by.saturating_sub(gbox.y).saturating_sub(gsy),
    };
    if !(coord_in_range(origin.x) && coord_in_range(origin.y)) {
        return Err(PlaceError::OutOfRange);
    }

    // No monitor of the placed group may overlap a monitor of
    // any other placed group.
    let mut proposed = layout.clone();
    proposed.set_origin(origin);
    match first_overlap(&proposed, monitors, &[peer]) {
        Some((_, other)) => Err(PlaceError::Overlap(other)),
        None => Ok(origin),
    }
}

/// The first pair of placed groups that overlap, among the pairs that have at
/// least one peer in `moved`: `(the moved peer, the other)`. Two groups overlap when a monitor of
/// one overlaps a monitor of the other; a group sits at its origin plus its anchor shift. A peer
/// that is not placed, or whose monitors are unknown, overlaps nothing. The order is the order of
/// `moved`, then of the layout's origins, so the answer is the same on every peer.
pub fn first_overlap(
    layout: &Layout,
    monitors: &[PeerMonitors],
    moved: &[PeerId],
) -> Option<(PeerId, PeerId)> {
    let placed = |p: PeerId| -> Option<Vec<Rect>> {
        let origin = layout.origin(p)?;
        let pm = monitors.iter().find(|m| m.peer == p)?;
        let (ox, oy) = pm.plane_offset(origin);
        Some(
            pm.monitors
                .iter()
                .map(|m| m.local_rect().translate(ox, oy))
                .collect(),
        )
    };
    for &a in moved {
        let Some(mine) = placed(a) else { continue };
        for other in &layout.origins {
            if other.peer == a {
                continue;
            }
            let Some(theirs) = placed(other.peer) else {
                continue;
            };
            if mine.iter().any(|r| theirs.iter().any(|t| r.overlaps(t))) {
                return Some((a, other.peer));
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    /// An origin past [`crate::wire::caps::COORD`], which every peer refuses.
    #[error("the origin of {0} is too far from the rest of the layout")]
    OutOfRange(PeerId),
    /// The layout would hold more peers than a `Layout` frame carries
    /// ([`crate::wire::caps::LAYOUT_PEERS`]); the others would drop it.
    #[error("a layout holds at most {} peers", crate::wire::caps::LAYOUT_PEERS)]
    TooManyPeers,
    #[error("{moved} overlaps {other}")]
    Overlap { moved: PeerId, other: PeerId },
}

/// `layout.set` (-07): move any number of groups in **one** revision.
/// `me` stamps the revision. Peers that are not named keep their origins. Pure: the caller
/// persists and pushes the result.
///
/// - `Ok(None)`: every named origin already holds, so there is nothing to edit and no revision to
///   spend.
/// - `Ok(Some(layout))`: the layout after the move, at the revision after the current one.
/// - `Err`: the layout is left as it was. An overlap is looked for between each moved group and
///   every other placed group, both at their new positions.
pub fn set_origins(
    layout: &Layout,
    monitors: &[PeerMonitors],
    me: PeerId,
    moves: &[Origin],
) -> Result<Option<Layout>, SetError> {
    for o in moves {
        if !(coord_in_range(o.x) && coord_in_range(o.y)) {
            return Err(SetError::OutOfRange(o.peer));
        }
    }
    let unchanged = layout.origin(me).is_some()
        && moves
            .iter()
            .all(|o| layout.origin(o.peer) == Some((o.x, o.y)));
    if unchanged {
        return Ok(None);
    }
    let next = layout.with_origins(me, moves);
    if next.origins.len() > crate::wire::caps::LAYOUT_PEERS {
        return Err(SetError::TooManyPeers);
    }
    let moved: Vec<PeerId> = moves.iter().map(|o| o.peer).collect();
    if let Some((moved, other)) = first_overlap(&next, monitors, &moved) {
        return Err(SetError::Overlap { moved, other });
    }
    Ok(Some(next))
}

/// One `SetPointerBarriers` entry, in this peer's local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Barrier {
    /// 1-based, stable for a given topology: the portal echoes it in
    /// `Activated`.
    pub id: u32,
    pub monitor: MonitorId,
    pub side: Side,
    /// `(x1, y1, x2, y2)` as Hyprland 0.56.2 `isBarrierValidAgainstMonitor`
    /// accepts: the whole edge (S02 `edge.rs` `barrier`).
    pub pos: (i32, i32, i32, i32),
}

/// The barriers to arm: one whole-edge barrier per side of a home monitor
/// that carries at least one link to a peer in `up` (Hyprland arms only
/// whole edges; ferry enforces the link span after activation, ADR 0002).
pub fn barriers(topo: &Topology, up: &[PeerId]) -> Vec<Barrier> {
    let mut out = Vec::new();
    for m in topo.monitors.iter().filter(|m| m.id.peer == topo.me) {
        for side in Side::ALL {
            let live = topo
                .links
                .iter()
                .any(|l| l.from == m.id && l.side == side && up.contains(&l.to.peer));
            if !live {
                continue;
            }
            let r = m.rect.translate(-topo.my_origin.0, -topo.my_origin.1);
            let pos = match side {
                Side::Left => (r.x, r.y, r.x, r.y + r.h - 1),
                Side::Right => (r.x + r.w, r.y, r.x + r.w, r.y + r.h - 1),
                Side::Top => (r.x, r.y, r.x + r.w - 1, r.y),
                Side::Bottom => (r.x, r.y + r.h, r.x + r.w - 1, r.y + r.h),
            };
            out.push(Barrier {
                id: u32::try_from(out.len() + 1).unwrap_or(u32::MAX),
                monitor: m.id,
                side,
                pos,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
