//! The messages between the threads. Capacities are the §9 table's.
//!
//! | Channel | Type | Capacity |
//! |---|---|---|
//! | to `ctl` (from `net`, `capture`, the monitor watcher, `Daemon`) | tokio mpsc of [`CtlMsg`] | [`CTL_CAP`] |
//! | `ctl` → `net` | tokio mpsc of [`NetCmd`] | [`NET_CAP`] |
//! | `capture` → the portal driver (a task on `ctl`) | tokio mpsc of [`DriverMsg`] | [`DRIVER_CAP`] |
//! | `ctl`, `net` → `capture` | calloop sync channel of [`CaptureCmd`] | [`CAPTURE_CAP`] |
//! | `ctl`'s hooks → the notify task (a task on `ctl`) | tokio mpsc of `notify::Cmd` | `notify::QUEUE_CAP` |
//! | `net` → `inject` | ferry-inject's own `InputCmd` channel | 1024 |
//! | `net` → the device owner (create, drop, sync) | ferry-inject's `Owner` requests | `OWNER_CAPACITY` (64) |
//!
//! The notify queue is full only while a notification daemon hangs: a full queue drops the toast
//! and counts it (`toast_queue_full`), and `ctl` never waits for it.
//!
//! Every send from `capture` and `net` is a `try_send`: neither thread ever waits for another.
//! A message whose loss would leave capture on or a key held is kept and retried instead of
//! dropped when a queue is full (RG-CONC-02 as amended). The portal commands have a queue of
//! their own, which only the capture thread fills, so nothing `net` sends can crowd out a
//! `Release`. `ctl` never awaits `net` from its main loop either.

use std::os::fd::OwnedFd;

use ferry_core::PeerId;
use ferry_core::api::LinkState;
use ferry_core::geom::{Layout, MonitorInfo};
use ferry_core::source::SourceEvent;
use ferry_core::wire::Control;
use ferry_net::{ConnectError, Connection, Fingerprint};
use tokio::sync::oneshot;

use crate::inject::Started;
use crate::pairing::{PairNote, PairReply};
#[cfg(doc)]
use crate::portal_driver::DriverMsg;

pub const CTL_CAP: usize = 64;
pub const NET_CAP: usize = 64;
pub const CAPTURE_CAP: usize = 64;
/// The portal driver's queue (B-06: bounded; full means a stuck driver, see `capture_thread`).
pub const DRIVER_CAP: usize = 64;

/// Into the `ctl` thread.
#[derive(Debug)]
pub enum CtlMsg {
    /// `net` saw a peer's link change (connected, refused, offline) or refreshed its RTT.
    Link(LinkUpdate),
    /// An inbound connection did not open: this side refused it (for an unknown key the
    /// fingerprint is named, FR-PAIR-05), or its dialer gave up first, which is not a refusal.
    InboundFailed(ConnectError),
    /// The own monitors changed (Hyprland socket2).
    OwnMonitors(Vec<MonitorInfo>),
    /// Hyprland reloaded its config: read `input:repeat_delay` again.
    HyprConfigReloaded,
    /// `net`: both `Hello`s are in and compatible (design §2.2 step 4).
    PeerUp {
        peer: Fingerprint,
        monitors: Vec<MonitorInfo>,
    },
    /// `net`: a peer's `Monitors`.
    PeerMonitors {
        peer: Fingerprint,
        monitors: Vec<MonitorInfo>,
    },
    /// `net`: a peer pushed its layout.
    PeerLayout { peer: Fingerprint, layout: Layout },
    /// `capture`: the portal driver's queue is full while a portal command waits: release
    /// capture over Hyprland IPC now, whatever the driver is doing.
    IpcRelease,
    /// `net`: the TargetMachine's takeover poll; read Hyprland's cursor and answer with
    /// [`NetCmd::CursorPolled`] .
    CheckCursor,
    /// `capture`: the SourceMachine watches a parked barrier; read Hyprland's cursor
    /// and answer with `SourceEvent::CursorPolled`.
    SourceCheckCursor,
    /// `capture`: a key crossing. Warp the cursor to `x, y` (local logical), then move
    /// it by `dx, dy` with a pointer-only virtual device, so the edge's barrier activates.
    SourceGo { x: f64, y: f64, dx: f64, dy: f64 },
    /// `net`: how the pairing goes. ctl tells the socket connection that
    /// started it, and writes the pin on `PairNote::Pinned`.
    Pairing(PairNote),
    /// `net`: a connection to a pinned peer is up (`up`), or one that was up ended. ctl keeps
    /// `last_seen` and the address that worked. `dialed` is false for an inbound
    /// connection, whose remote is the peer's bound port.
    PeerSeen {
        peer: Fingerprint,
        remote: std::net::SocketAddr,
        dialed: bool,
        up: bool,
    },
    /// `net`: the peer closed its connection with `CloseCode::UNPAIRED`, so it unpaired this
    /// machine. ctl removes the pin and shows the toast. Sent before the
    /// `Offline` link update that follows the close.
    Revoked { peer: Fingerprint },
    /// A toast to hand to the `notice` subscribers. From any thread.
    Notice(ferry_core::api::Notice),
    /// A request that frontends open a view (the `ui` topic). From any thread.
    Ui(ferry_core::api::UiView),
    /// `capture` or `net`: a crossing went out or came home over one of this peer's edges. ctl
    /// finds the edge and publishes it on the `edge` topic.
    Crossed(Crossed),
    /// Start-up steps 4 and 5: the IPC release, the portal session, the dials.
    Begin,
    /// Stop: run the §9 shutdown order.
    Shutdown,
}

impl CtlMsg {
    /// The message cannot change what any snapshot shows, so ctl need not let the event hub look
    /// after it. These come at the pace of a session (the cursor polls, 25 a second), not of a
    /// change, and the hub would build its snapshots for nothing.
    pub fn is_quiet(&self) -> bool {
        matches!(
            self,
            Self::CheckCursor | Self::SourceCheckCursor | Self::IpcRelease
        )
    }
}

/// A crossing over an edge of this peer, as a machine saw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Crossed {
    /// The SourceMachine went `Remote` over `link` (`home` false), or came back (`home` true).
    Source {
        link: ferry_core::geom::EdgeLink,
        home: bool,
    },
    /// The TargetMachine was entered by `peer` at `entry` (local logical), or that session
    /// ended with `Home`.
    Target {
        peer: ferry_core::PeerId,
        entry: (f64, f64),
        home: bool,
    },
}

/// The state of one pinned peer's link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkUpdate {
    pub peer: Fingerprint,
    pub state: LinkState,
    /// The remote address of the live connection.
    pub remote: Option<String>,
    pub rtt_us: Option<u32>,
    /// The peer's protocol version from its `Hello` (`"1.0"`), when it changed.
    pub proto: Option<String>,
    /// Why it is offline or refused.
    pub detail: Option<String>,
}

/// `ctl` → `net`.
pub enum NetCmd {
    /// Start-up step 3: the injection backend is up; feed `TargetEvent::Start`, then begin
    /// accepting connections.
    Start(Started),
    /// Replace the pinned fingerprints (`Endpoint::set_trust`). `ack` fires once the swap is
    /// live, so `trust` answers only after the next handshake would honour it.
    SetTrust {
        fps: Vec<Fingerprint>,
        ack: oneshot::Sender<()>,
    },
    /// Keep dialling `peer` at `addr` (`host[:port]`) until connected.
    Dial { peer: Fingerprint, addr: String },
    /// The own monitors: sent in every `Hello`, and as `Monitors` to every up peer.
    OwnMonitors(Vec<MonitorInfo>),
    /// One frame to one up peer.
    Send { peer: Fingerprint, msg: Control },
    /// One frame to every up peer.
    Broadcast(Control),
    /// `ferry release`: `ForceRelease` to the TargetMachine.
    ForceRelease,
    /// Shutdown step 1 for the target side: `Shutdown` to the TargetMachine.
    ShutdownMachine { ack: oneshot::Sender<()> },
    /// The answer to [`CtlMsg::CheckCursor`]: `None` when the read failed.
    CursorPolled(Option<(f64, f64)>),
    /// Lock to this machine changed: tell every up peer that lists `peer-state`.
    SetLocked(bool),
    /// `config.set clipboard.*`: sharing on or off, and the largest item, at once.
    ClipConfig { enabled: bool, max_bytes: u64 },
    /// This peer's source crossed into `peer`: it is the clipboard partner now, and gets the
    /// offer it missed, if any.
    ClipEntered(Fingerprint),
    /// Shutdown, after the portal `Release`: ferry-inject `sync()`, `close_graceful(100 ms)` on
    /// every connection, close the endpoint, stop.
    Shutdown { ack: oneshot::Sender<()> },
    /// `pair.accept`: open the pairing window until `until`.
    PairAccept {
        until: std::time::Instant,
        reply: oneshot::Sender<PairReply>,
    },
    /// `pair.start`: dial `addr` on `ferry-pair/1`; the reply comes once the peer accepted the
    /// connection, or with why not.
    PairStart {
        addr: String,
        until: std::time::Instant,
        reply: oneshot::Sender<PairReply>,
    },
    /// `pair.answer`: the local user's answer.
    PairAnswer {
        ok: bool,
        reply: oneshot::Sender<PairReply>,
    },
    /// `pair.cancel`, Ctrl+C, or the socket connection that started the pairing closed.
    PairCancel { reply: oneshot::Sender<PairReply> },
}

impl std::fmt::Debug for NetCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(s) => f.debug_tuple("Start").field(s).finish(),
            Self::SetTrust { fps, .. } => f.debug_struct("SetTrust").field("fps", fps).finish(),
            Self::Dial { peer, addr } => f
                .debug_struct("Dial")
                .field("peer", peer)
                .field("addr", addr)
                .finish(),
            Self::OwnMonitors(m) => f.debug_tuple("OwnMonitors").field(&m.len()).finish(),
            Self::Send { peer, msg } => f
                .debug_struct("Send")
                .field("peer", peer)
                .field("variant", &msg.variant())
                .finish(),
            Self::Broadcast(msg) => f.debug_tuple("Broadcast").field(&msg.variant()).finish(),
            Self::ForceRelease => f.write_str("ForceRelease"),
            Self::ShutdownMachine { .. } => f.write_str("ShutdownMachine"),
            Self::CursorPolled(p) => f.debug_tuple("CursorPolled").field(p).finish(),
            Self::SetLocked(on) => f.debug_tuple("SetLocked").field(on).finish(),
            Self::ClipConfig { enabled, max_bytes } => f
                .debug_struct("ClipConfig")
                .field("enabled", enabled)
                .field("max_bytes", max_bytes)
                .finish(),
            Self::ClipEntered(p) => f.debug_tuple("ClipEntered").field(p).finish(),
            Self::Shutdown { .. } => f.write_str("Shutdown"),
            Self::PairAccept { .. } => f.write_str("PairAccept"),
            Self::PairStart { .. } => f.write_str("PairStart"),
            Self::PairAnswer { ok, .. } => f.debug_struct("PairAnswer").field("ok", ok).finish(),
            Self::PairCancel { .. } => f.write_str("PairCancel"),
        }
    }
}

/// `ctl` and `net` → `capture`.
#[derive(Debug)]
pub enum CaptureCmd {
    /// One event for the SourceMachine.
    Event(SourceEvent),
    /// The connection handle for `peer` (before its `PeerUp`), or `None` after its `PeerDown`.
    Conn {
        peer: PeerId,
        conn: Option<Connection>,
    },
    /// A new portal session's EIS socket. The old EIS source goes; its end, reported late,
    /// carries the old generation and is ignored.
    Eis { generation: u64, fd: OwnedFd },
    /// `Shutdown` to the SourceMachine, then leave the loop.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The messages that come at the pace of a session must not make the event hub look; the ones
    /// that change a link, a layout or a pairing must.
    #[test]
    fn only_the_polls_are_quiet() {
        assert!(CtlMsg::CheckCursor.is_quiet());
        assert!(CtlMsg::SourceCheckCursor.is_quiet());
        assert!(CtlMsg::IpcRelease.is_quiet());
        let link = CtlMsg::Link(LinkUpdate {
            peer: Fingerprint([1; 32]),
            state: LinkState::Connected,
            remote: None,
            rtt_us: None,
            proto: None,
            detail: None,
        });
        assert!(!link.is_quiet());
        assert!(!CtlMsg::Begin.is_quiet());
        assert!(!CtlMsg::OwnMonitors(Vec::new()).is_quiet());
        assert!(!CtlMsg::HyprConfigReloaded.is_quiet());
        assert!(!CtlMsg::Pairing(PairNote::Connecting).is_quiet());
    }
}
