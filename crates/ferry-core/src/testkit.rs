//! Shared fixtures for the unit tests: the first-light geometry (design note
//! §7.2) and helpers to place the netbook on any side.

use crate::geom::{
    self, Align, Anchor, Layout, MonitorId, MonitorInfo, Origin, PeerMonitors, Rel, Topology,
    WireRect,
};
use crate::peer::PeerId;

pub fn desk() -> PeerId {
    PeerId([0xd0; 32])
}

pub fn netbook() -> PeerId {
    PeerId([0x0b; 32])
}

pub fn third() -> PeerId {
    PeerId([0x3c; 32])
}

pub fn mon(name: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
    MonitorInfo {
        name: name.into(),
        description: format!("test {name}"),
        rect: WireRect { x, y, w, h },
        scale: 1.0,
        transform: 0,
        shift: None,
    }
}

/// HDMI-A-1 at (0,0) and DVI-D-1 at (1920,0), both 1920×1080.
pub fn desk_monitors() -> PeerMonitors {
    PeerMonitors {
        peer: desk(),
        monitors: vec![
            mon("HDMI-A-1", 0, 0, 1920, 1080),
            mon("DVI-D-1", 1920, 0, 1920, 1080),
        ],
    }
}

/// eDP-1, 1366×768.
pub fn netbook_monitors() -> PeerMonitors {
    PeerMonitors {
        peer: netbook(),
        monitors: vec![mon("eDP-1", 0, 0, 1366, 768)],
    }
}

pub fn hdmi() -> MonitorId {
    MonitorId {
        peer: desk(),
        index: 0,
    }
}

pub fn edp() -> MonitorId {
    MonitorId {
        peer: netbook(),
        index: 0,
    }
}

pub fn dvi() -> MonitorId {
    MonitorId {
        peer: desk(),
        index: 1,
    }
}

/// The desktop's layout with the netbook placed `rel` HDMI-A-1, centred
/// (right of DVI-D-1 for `RightOf`, since DVI-D-1 is right of HDMI-A-1).
pub fn layout_with_netbook(rel: Rel) -> Layout {
    let mut layout = Layout::empty(desk());
    layout.set_origin(Origin {
        peer: desk(),
        x: 0,
        y: 0,
    });
    let monitors = [desk_monitors(), netbook_monitors()];
    let o = geom::place(
        &layout,
        &monitors,
        netbook(),
        rel,
        Anchor::Monitor(if rel == Rel::RightOf { dvi() } else { hdmi() }),
        Align::Center,
    )
    .expect("placement");
    layout.set_origin(o);
    layout.rev.counter += 1;
    layout
}

/// The desktop's topology with the netbook placed by [`layout_with_netbook`].
pub fn topo_with_netbook(rel: Rel) -> Topology {
    geom::topology(
        desk(),
        &layout_with_netbook(rel),
        &[desk_monitors(), netbook_monitors()],
        &[netbook()],
    )
}

/// First light: the netbook below HDMI-A-1, centred.
pub fn first_light() -> Topology {
    topo_with_netbook(Rel::Below)
}
