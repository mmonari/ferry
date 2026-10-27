//! The source side of a crossing: the cursor model, edge
//! enforcement, the held set, the repeat filter, the escape chord, the
//! liveness watchdog and the lock gate, as one pure machine.
//!
//! The capture thread owns it. EIS events, portal signals (via `ctl`), lock
//! checks, peer events and acknowledgements come in as [`SourceEvent`]s;
//! datagrams, control frames and portal commands go out as
//! [`SourceAction`]s.
//!
//! **Capture off on purpose**. While another peer
//! drives this one ([`SourceEvent::Controlled`]) or this peer is locked to
//! itself ([`SourceEvent::LockedHere`]), the barrier list is empty, so the gate
//! takes its "Empty: `Disable` → Idle" row. Lock to this machine never ends a
//! crossing and never blocks a release: locked during a crossing, the gate
//! changes only once the pointer is home. A target that is locked to itself
//! ([`SourceEvent::PeerLocked`]) turns its outgoing links into walls in the
//! Remote model, home and hop alike; entering it stays allowed.

use crate::actions::Actions;
use crate::config::SourceConfig;
use crate::geom::{self, Barrier, EdgeLink, MonitorId, Point, Rect, Side, Topology};
use crate::held::{Edge, HeldSet};
use crate::peer::PeerId;
use crate::repeat::RepeatFilter;
use crate::time::{
    ARM_TIMEOUT, BARRIER_RETRY_DELAY, BARRIER_RETRY_MAX, LOCK_POLL_IDLE, LOCK_POLL_LOCKED, Micros,
    PARK_MAX, PARK_POLL, REFUSE_LATE_ACTIVATION, REZONE_SETTLE, SNAPSHOT_PERIOD, SOURCE_LIVENESS,
};
use crate::wire::{Control, LeaveReason, Motion, Pos};

mod pace;
pub use pace::{MotionPacer, Pace};

/// The result of a fresh read of Hyprland's monitors (`solitaryBlockedBy`
/// containing `LOCK` on any monitor means locked; ADR 0017).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCheck {
    Unlocked,
    Locked,
    /// The IPC read failed or took longer than 50 ms. Treated as locked for
    /// an activation; ignored by the poll (the previous state stands).
    Unknown,
}

/// One axis event from EIS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollIn {
    /// 0 vertical, 1 horizontal (wire codes).
    pub axis: u8,
    /// 0 wheel, 1 finger, 2 continuous (wire codes).
    pub source: u8,
    pub value: f32,
    pub value120: Option<i32>,
    pub stop: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceEvent {
    // --- EIS, on the capture thread ---------------------------------------
    /// `start_emulating`: the activation edge.
    StartEmulating,
    /// `stop_emulating`: capture ended on the compositor side.
    StopEmulating,
    Motion {
        dx: f32,
        dy: f32,
    },
    Key {
        code: u16,
        pressed: bool,
        /// EIS event time, for the repeat filter.
        t_eis: Micros,
    },
    Button {
        code: u16,
        pressed: bool,
    },
    Scroll(ScrollIn),
    /// The EIS read batch is drained (flushes the repeat filter).
    BatchEnd,
    /// The EIS keyboard was (re-)added with this keymap text (ADR 0014:
    /// rebuilt on every `DeviceAdded`).
    Keymap {
        text: String,
    },

    // --- portal signals, forwarded by ctl --------------------------------
    /// `Activated`, with the lock check ctl made before forwarding it.
    /// `x, y` are local logical and may lie one motion step past the edge.
    Activated {
        activation_id: u32,
        barrier_id: u32,
        x: f64,
        y: f64,
        lock: LockCheck,
    },
    Deactivated {
        activation_id: u32,
    },
    Disabled,
    ZonesChanged,
    /// Result of a [`PortalCmd::SetBarriers`].
    BarriersSet {
        failed: Vec<u32>,
    },

    // --- system, from ctl ------------------------------------------------
    /// Result of a [`SourceAction::CheckLock`].
    LockPolled(LockCheck),
    /// logind `PrepareForSleep(start)`.
    PrepareForSleep {
        start: bool,
    },
    Topology(Topology),
    Config(SourceConfig),
    /// `ferry release`.
    ForceRelease,
    /// ferryd is exiting (SIGTERM, SIGINT, panic hook).
    Shutdown,

    // --- net ---------------------------------------------------------------
    /// Both `Hello`s exchanged and compatible.
    PeerUp(PeerId),
    PeerDown(PeerId),
    /// A Ping or Pong arrived from this peer: clears "suspect" (a silence
    /// release makes the peer's links walls until it is heard again).
    PeerAlive(PeerId),
    /// A target → source frame (`HeldAck`, `Ended`).
    Control {
        peer: PeerId,
        msg: Control,
    },

    /// ctl re-created the portal session (xdph changed owner, EIS EOF, or a
    /// re-zone failed): end any crossing as capture ended and re-arm, as for
    /// `Disabled`.
    SessionRestarted,

    // --- both peers capture, lock ----------------
    /// The TargetMachine on this peer took (`true`) or ended (`false`)
    /// control. While controlled, capture is off (an empty barrier list); a
    /// crossing still running when control starts ends as `preempted`.
    Controlled(bool),
    /// `ferry lock on|off` on this peer: while locked, no crossing starts
    /// from here (an empty barrier list, once the pointer is home).
    LockedHere(bool),
    /// A peer's `PeerState`: while `locked`, every link out of it is a wall.
    PeerLocked {
        peer: PeerId,
        locked: bool,
    },

    /// The answer to [`SourceAction::CheckCursor`]: Hyprland's cursor, local
    /// logical, or `None` when the read failed.
    CursorPolled(Option<Point>),

    /// `ferry go <side>`: move control to the machine on that side without the
    /// pointer. `cursor` is Hyprland's cursor, local logical, read by ctl (`None`: the read
    /// failed). From home, the machine warps and nudges the cursor through the linked edge
    /// ([`SourceAction::Go`]); while remote, it goes home or hops, as the go chord does.
    Go {
        side: Side,
        cursor: Option<Point>,
    },

    /// `now` reached [`SourceMachine::next_deadline`].
    Tick,
}

/// Portal calls, executed by `ctl` in the order emitted.
#[derive(Debug, Clone, PartialEq)]
pub enum PortalCmd {
    /// `Release(activation_id, cursor_position)`; the position is local
    /// logical. ctl also logs the reason.
    Release {
        activation_id: Option<u32>,
        pos: Option<Point>,
    },
    Disable,
    Enable,
    /// `GetZones` when `refresh_zones`, then `SetPointerBarriers` with
    /// `barriers`. Answered with [`SourceEvent::BarriersSet`].
    ///
    /// The machine fills `barriers` from its own snapshot of the topology
    /// and the up peers ([`crate::geom::barriers`]), and maps every
    /// `Activated.barrier_id` against that same list. ctl must send exactly
    /// this list: one it recomputed could differ if the topology or the up
    /// peers changed in between, and a contact would map to the wrong edge.
    SetBarriers {
        refresh_zones: bool,
        barriers: Vec<Barrier>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceAction {
    /// Encode and `send_datagram` on the capture thread (no hop).
    Datagram {
        peer: PeerId,
        motion: Motion,
    },
    /// Encode and `try_send_frame`; `Full` is a fault (fail open: the caller
    /// feeds [`SourceEvent::ForceRelease`]).
    Send {
        peer: PeerId,
        msg: Control,
    },
    Portal(PortalCmd),
    /// Read Hyprland's lock state now and answer with
    /// [`SourceEvent::LockPolled`].
    CheckLock,
    /// Read Hyprland's cursor now and answer with
    /// [`SourceEvent::CursorPolled`] (only while a barrier is parked).
    CheckCursor,
    /// The role or peer changed: publish to `ctl` for status and the bar.
    Status(SourceStatus),
    /// A key crossing: warp the cursor to `x, y` (local logical, next to the linked
    /// edge), then move it by `dx, dy` with relative motion, which activates the edge's barrier.
    Go {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// A `Go` that did nothing, and why: for the log.
    GoRefused(&'static str),
}

/// What `ferry status` shows of the source side.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    pub role: SourceRole,
    pub peer: Option<PeerId>,
    pub session: u32,
    pub last_leave: Option<LeaveReason>,
    pub gate: Gate,
    /// Peers released for silence and not heard since: their
    /// links are walls until a Ping, a Pong or a new `PeerUp`.
    pub suspect: Vec<PeerId>,
    /// Another peer drives this one: capture is off (`off_reason =
    /// controlled`).
    pub controlled: bool,
    /// Lock to this machine is on here (`off_reason = locked_here`).
    pub locked_here: bool,
    /// Up peers that are locked to themselves: their links are walls.
    pub locked_peers: Vec<PeerId>,
    /// The link of the crossing in progress, while `Remote` (the edge glow).
    pub edge: Option<EdgeLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRole {
    Local,
    Pending,
    Arming,
    Remote,
}

/// Whether capture may be enabled now (orthogonal to the crossing state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Barriers set, capture enabled.
    Armed,
    /// Not yet armed (start-up, or no link to a connected peer).
    Idle,
    /// Screen locked: `Disable` sent; `Enable` after unlock.
    Locked,
    /// Between `PrepareForSleep(true)` and `(false)`.
    Suspended,
    /// Waiting for the 100 ms settle after `ZonesChanged`/`Disabled`.
    Rezoning,
    /// The last `SetPointerBarriers` reported failures (doctor fault).
    BarriersFailed,
}

impl Gate {
    /// The `snake_case` name `ferry status` shows (`capture.gate`).
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Armed => "armed",
            Gate::Idle => "idle",
            Gate::Locked => "locked",
            Gate::Suspended => "suspended",
            Gate::Rezoning => "rezoning",
            Gate::BarriersFailed => "barriers_failed",
        }
    }
}

/// Counters for `ferry status`. ferryd mirrors them into
/// atomics after each wake-up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceCounters {
    /// Every portal `Activated` (a barrier fired), whatever came of it.
    pub activations: u64,
    pub crossings: u64,
    pub bounces: u64,
    pub lock_refusals: u64,
    pub repeat_pairs_dropped: u64,
    /// Held-key release-then-press pairs whose gap was over 1 ms and under 10 ms: forwarded as
    /// two edges, only counted (the user has not decided whether the gap widens).
    pub repeat_split_pairs: u64,
    /// The largest such gap, in µs.
    pub repeat_split_max_us: u64,
    /// Key edges stamped ahead of ferryd's clock (see `RepeatFilter::future_stamps`).
    pub repeat_future_stamps: u64,
    pub orphan_key_releases: u64,
    pub drag_leaves: u64,
    pub silence_releases: u64,
    pub pending_overflows: u64,
    pub datagrams_sent: u64,
    pub frames_sent: u64,
    /// `Keymap` frames sent (one per peer per new keymap or connection).
    pub keymaps_sent: u64,
    /// Lock polls that came back `Unknown` (the previous gate stood).
    pub lock_unknown: u64,
    /// `SetBarriers` answers with failed barriers.
    pub barrier_failures: u64,
    /// Barriers taken down after a bounce at a wall stretch (a dead corner,
    /// or a stretch with no link to a usable peer), BL-009.
    pub edge_parks: u64,
    /// EIS motion deltas or scroll values that were not finite: dropped.
    pub nonfinite_dropped: u64,
    /// Motions held back to keep to one datagram per ms; the next datagram carries them.
    pub motion_coalesced: u64,
    /// Motions not sent because the newest `HeldAck` was too old (A-04): the next `Held` or
    /// fence carries the position.
    pub motion_suppressed: u64,
    /// Key crossings that went out, home or to another peer.
    pub go_crossings: u64,
    /// Key crossings that did nothing: no usable link on that side, capture off, or the nudge's
    /// activation never came.
    pub go_refused: u64,
}

/// The source machine. See the design note §5.1 for the state table.
#[derive(Debug)]
pub struct SourceMachine {
    config: SourceConfig,
    topo: Option<Topology>,
    up: Vec<PeerId>,
    /// Peers released for silence and not heard since: their links are walls.
    suspect: Vec<PeerId>,
    /// Bumped on every change of `suspect`, so `handle` can tell a status
    /// change without cloning the list on every event.
    suspect_rev: u32,
    /// Another peer drives this one.
    controlled: bool,
    /// Lock to this machine.
    locked_here: bool,
    /// Peers that told us they are locked to themselves.
    locked_peers: Vec<PeerId>,
    /// Bumped on every change of `locked_peers`.
    locked_rev: u32,
    /// `controlled` or `locked_here` changed during a crossing: recompute
    /// the barriers once the crossing is over (a `Disable` under a crossing
    /// would end it).
    rearm_when_local: bool,
    gate: Gate,
    /// The gate to restore after `PrepareForSleep(false)`.
    gate_before_sleep: Gate,
    lock: LockCheck,
    /// A `LockPolled(Unlocked)` has arrived since start-up: before it, the
    /// gate never sends `Enable`.
    ever_unlocked: bool,
    /// An `Enable` was sent and no `Disable` or `Disabled` since.
    enabled: bool,
    /// The barriers of the last `SetBarriers`: the ids `Activated` echoes.
    barriers: Vec<Barrier>,
    /// Barriers taken down after a bounce at a wall stretch, until the cursor
    /// leaves it. Their ids stay those of the whole list, so a late
    /// `Activated` on one still maps to its edge.
    parked: Vec<Barrier>,
    /// The next cursor read while `parked` is not empty.
    park_poll_at: Micros,
    /// Every park ends by then ([`PARK_MAX`] after the first one).
    park_until: Micros,
    /// The last cursor read failed: a second failure in a row puts the
    /// parked barriers back (one read can time out on a busy compositor).
    park_read_failed: bool,
    /// `SetBarriers` sent and not yet answered by `BarriersSet`.
    barriers_outstanding: u32,
    /// The answer to the last `SetBarriers` decides the gate. Cleared when a
    /// lock, sleep or re-zone takes the gate elsewhere meanwhile.
    awaiting_barriers: bool,
    /// `SetBarriers` has never been sent, or zones changed while idle.
    zones_stale: bool,
    barrier_retries: u8,
    retry_at: Option<Micros>,
    crossing: Crossing,
    held: HeldSet,
    filter: RepeatFilter,
    keymap: Option<(u32, String)>,
    /// Per peer: the keymap serial last sent to it.
    keymap_sent: Vec<(PeerId, u32)>,
    session: u32,
    motion_seq: u32,
    next_lock_poll: Micros,
    rezone_at: Option<Micros>,
    /// Activations this machine released itself, newest last: their
    /// `Deactivated` echoes are not capture ending under a crossing.
    released: [Option<u32>; 4],
    /// Ending Pending sends `Release` with no activation id, and xdph 1.4.1
    /// rejects that: the capture goes on, and its `Activated` arrives later.
    /// Until this deadline, that `Activated` is released by its id instead
    /// of starting a crossing (row 11 confirmation, 2026-10-19).
    refuse_activation_until: Option<Micros>,
    last_leave: Option<LeaveReason>,
    /// The Pending/Arming buffer, kept between crossings.
    spare: Vec<SourceEvent>,
    /// A key crossing whose nudge is on its way: the activation on its link crosses
    /// at once, whatever the edge resistance and dwell.
    going: Option<Going>,
    counters: SourceCounters,
}

/// The crossing state.
#[derive(Debug, Clone, PartialEq)]
enum Crossing {
    Local,
    /// Between `start_emulating` and the portal's `Activated`.
    Pending(Pending),
    /// On a link, pushing against edge resistance / dwell.
    Arming(Arming),
    Remote(Box<Remote>),
}

#[derive(Debug, Clone, PartialEq)]
struct Pending {
    since: Micros,
    /// Summed relative motion since the activation edge.
    dx: f64,
    dy: f64,
    /// Key, button and scroll events buffered until the verdict; capped at
    /// [`PENDING_MAX`] (overflow: bounce).
    buffered: Vec<SourceEvent>,
}

/// A key crossing waiting for its activation.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Going {
    link: EdgeLink,
    /// Where the cursor was (plane): a key crossing home puts it back there.
    origin: Option<Point>,
    until: Micros,
}

/// How long a key crossing waits for the activation of its nudge (the VM spike saw it in
/// well under 10 ms).
const GO_TIMEOUT: Micros = Micros::from_millis(500);

/// The nudge's relative motion: from [`RELEASE_INSET`] inside to past the edge line.
const NUDGE_PX: f64 = 4.0;

/// Events buffered in `Pending` before overflow bounces the activation.
pub const PENDING_MAX: usize = 64;

/// The inset of every release position (S14 tested 2 px).
const RELEASE_INSET: f64 = 2.0;

/// A parked barrier goes back up once the cursor is farther than this from
/// its edge (or on a stretch of it that crosses). A bounce leaves the cursor
/// [`RELEASE_INSET`] inside, and a push keeps it on the last pixel row.
const PARK_NEAR_PX: f64 = 8.0;

/// The parts of [`SourceMachine::status`] that decide whether a `Status` action goes out:
/// role, peer, session, last leave, gate, suspect revision, and (controlled, locked here,
/// lock revision).
type StatusKey = (
    SourceRole,
    Option<PeerId>,
    u32,
    Option<LeaveReason>,
    Gate,
    u32,
    (bool, bool, u32),
);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Activation {
    id: u32,
    /// The home monitor whose barrier was hit, its plane rect and side.
    monitor: MonitorId,
    home: Rect,
    side: Side,
    /// The contact point: plane, clamped onto the barrier line.
    at: Point,
    /// Where the cursor was before a key crossing (plane): going home by key puts it back.
    /// It stays through hops, with the rest of the activation.
    origin: Option<Point>,
}

impl Activation {
    /// The release point for a bounce and for every exit except a crossing
    /// home: the contact point, 2 px inside the home monitor.
    fn inside(&self) -> Point {
        geom::inside(&self.home, self.side, self.at, RELEASE_INSET)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Arming {
    since: Micros,
    activation: Activation,
    link: EdgeLink,
    push: f64,
    buffered: Vec<SourceEvent>,
}

#[derive(Debug, Clone, PartialEq)]
struct Remote {
    peer: PeerId,
    /// The link crossed: what `SourceStatus::edge` reports.
    link: EdgeLink,
    /// The portal activation; its contact point is the exit.
    activation: Activation,
    /// The cursor model, plane coordinates, always inside a target monitor.
    model: Point,
    /// The newest `Held` sent this session: its `n` and send time.
    /// Stop-and-wait (amended 2026-10-16): the next `Held` leaves
    /// only once this one is acknowledged, so after a target → source cut at
    /// most one more snapshot reaches the target.
    held_n: u32,
    held_at: Micros,
    /// `held_n` has been acknowledged.
    held_acked: bool,
    /// Send time of the newest acknowledged `Held` (or of `Enter`).
    acked_at: Micros,
    /// When the next `Held` is due, provided `held_n` is acknowledged by
    /// then; otherwise it leaves when the ack arrives.
    next_snapshot: Micros,
    /// Return-direction edge resistance accumulator.
    return_push: f64,
    /// What of the model's motion goes out as datagrams (A-04).
    pace: MotionPacer,
}

/// Where a crossing's `Release` puts the cursor.
#[derive(Debug, Clone, Copy)]
enum ReleaseAt {
    /// No `Release`: the portal already ended capture.
    Nothing,
    /// The exit: the contact point, 2 px inside the home monitor.
    Exit,
    /// A given plane point (the continuation of a crossing home).
    Point(Point),
}

/// What one remote motion does (decided under borrows, applied after).
enum Step {
    Moved { send: bool },
    Home { continuation: Point },
    Hop { link: EdgeLink, contact: Point },
}

impl SourceMachine {
    pub fn new(config: SourceConfig) -> Self {
        Self {
            config,
            topo: None,
            up: Vec::new(),
            suspect: Vec::new(),
            suspect_rev: 0,
            controlled: false,
            locked_here: false,
            locked_peers: Vec::new(),
            locked_rev: 0,
            rearm_when_local: false,
            gate: Gate::Idle,
            gate_before_sleep: Gate::Idle,
            lock: LockCheck::Unknown,
            ever_unlocked: false,
            enabled: false,
            barriers: Vec::new(),
            parked: Vec::new(),
            park_poll_at: Micros::ZERO,
            park_until: Micros::ZERO,
            park_read_failed: false,
            barriers_outstanding: 0,
            awaiting_barriers: false,
            zones_stale: true,
            barrier_retries: 0,
            retry_at: None,
            crossing: Crossing::Local,
            held: HeldSet::default(),
            filter: RepeatFilter::default(),
            keymap: None,
            keymap_sent: Vec::new(),
            session: 0,
            motion_seq: 0,
            next_lock_poll: Micros::ZERO,
            rezone_at: None,
            released: [None; 4],
            refuse_activation_until: None,
            last_leave: None,
            spare: Vec::with_capacity(PENDING_MAX),
            going: None,
            counters: SourceCounters::default(),
        }
        .configured()
    }

    /// Apply the config to the parts that keep their own copy.
    fn configured(mut self) -> Self {
        self.apply_config();
        self
    }

    fn apply_config(&mut self) {
        self.filter.set_hold_after(Micros::from_millis(u64::from(
            self.config.repeat_hold_after_ms,
        )));
    }

    /// Feed one event. Never blocks, never allocates after warm-up except
    /// for `Keymap`, `Topology` and `Held` payloads.
    pub fn handle(&mut self, ev: SourceEvent, now: Micros, out: &mut Actions<SourceAction>) {
        let before = self.status_key();
        // A deadline that is due fires first, as if the timer had been on
        // time: nothing is forwarded past the liveness deadline.
        // A key event leaves the repeat filter's deadline to the filter,
        // which judges a pair by EIS time, not by when the event is handled.
        if !matches!(ev, SourceEvent::Tick) && self.next_deadline().is_some_and(|d| d <= now) {
            let flush_filter = !matches!(ev, SourceEvent::Key { .. });
            self.tick(now, flush_filter, out);
        }
        match ev {
            SourceEvent::StartEmulating => {
                if matches!(self.crossing, Crossing::Local) {
                    // A new capture: a refusal left from an older one lapses.
                    self.refuse_activation_until = None;
                    self.crossing = Crossing::Pending(Pending {
                        since: now,
                        dx: 0.0,
                        dy: 0.0,
                        buffered: self.take_buffer(),
                    });
                }
            }
            SourceEvent::StopEmulating => self.capture_ended(now, out),
            // A non-finite delta or scroll value is dropped here, at the
            // machine's boundary (A-01): a NaN fails every
            // comparison, so it would pass the clamps and freeze the model
            // off every monitor. `Activated`'s NaN position is deliberate
            // (no position from xdph) and is handled by `activated`.
            SourceEvent::Motion { dx, dy } if !(dx.is_finite() && dy.is_finite()) => {
                self.counters.nonfinite_dropped += 1;
            }
            SourceEvent::Scroll(s) if !s.value.is_finite() => {
                self.counters.nonfinite_dropped += 1;
            }
            SourceEvent::Motion { dx, dy } => self.motion(f64::from(dx), f64::from(dy), now, out),
            ev @ (SourceEvent::Key { .. }
            | SourceEvent::Button { .. }
            | SourceEvent::Scroll(_)
            | SourceEvent::BatchEnd) => self.input(ev, now, out),
            SourceEvent::Keymap { text } => {
                // Hyprland re-adds the EIS keyboard with the same keymap on
                // every activation, and whenever its active keyboard changes.
                // Only a new text is a new keymap: same serial, and
                // only a peer that lacks it gets a frame. ferryd makes the
                // text canonical first, so fcitx5's round trip of the
                // keyboard's keymap is the same text (design §5.1 as built).
                let serial = match &self.keymap {
                    Some((s, t)) if *t == text => *s,
                    Some((s, _)) => s.wrapping_add(1),
                    None => 1,
                };
                for i in 0..self.up.len() {
                    let peer = self.up[i];
                    if !self.keymap_sent.contains(&(peer, serial)) {
                        self.send_keymap(peer, serial, &text, out);
                    }
                }
                self.keymap = Some((serial, text));
            }
            SourceEvent::Activated {
                activation_id,
                barrier_id,
                x,
                y,
                lock,
            } => self.activated(activation_id, barrier_id, Point { x, y }, lock, now, out),
            SourceEvent::Deactivated { activation_id } => {
                if !self.released.contains(&Some(activation_id)) {
                    self.capture_ended(now, out);
                }
            }
            SourceEvent::Disabled => {
                self.enabled = false;
                self.capture_ended(now, out);
                if matches!(self.gate, Gate::Armed | Gate::Rezoning) {
                    self.start_rezone(now);
                }
            }
            SourceEvent::ZonesChanged => {
                self.capture_ended(now, out);
                match self.gate {
                    Gate::Armed | Gate::Rezoning | Gate::BarriersFailed => self.start_rezone(now),
                    Gate::Idle | Gate::Locked | Gate::Suspended => self.zones_stale = true,
                }
            }
            SourceEvent::BarriersSet { failed } => self.barriers_set(&failed, now, out),
            SourceEvent::LockPolled(check) => self.lock_polled(check, now, out),
            SourceEvent::PrepareForSleep { start: true } => {
                if self.gate != Gate::Suspended {
                    self.end_any(LeaveReason::Suspend, now, out);
                    self.gate_before_sleep = if self.awaiting_barriers {
                        // An arm was in flight: re-zone after the wake.
                        Gate::Rezoning
                    } else {
                        self.gate
                    };
                    self.awaiting_barriers = false;
                    self.gate = Gate::Suspended;
                }
            }
            SourceEvent::PrepareForSleep { start: false } => {
                if self.gate == Gate::Suspended {
                    self.gate = self.gate_before_sleep;
                    if matches!(self.gate, Gate::Rezoning | Gate::BarriersFailed) {
                        self.start_rezone(now);
                    }
                    out.push(SourceAction::CheckLock);
                    self.next_lock_poll = now.plus(self.poll_interval());
                }
            }
            SourceEvent::Topology(t) => {
                self.topo = Some(t);
                self.topology_changed(now, out);
                self.maybe_rearm(out);
            }
            SourceEvent::Config(c) => {
                self.config = c;
                self.apply_config();
            }
            SourceEvent::ForceRelease => self.end_any(LeaveReason::Forced, now, out),
            SourceEvent::Shutdown => self.end_any(LeaveReason::Shutdown, now, out),
            SourceEvent::PeerUp(p) => {
                if !self.up.contains(&p) {
                    self.up.push(p);
                }
                self.unsuspect(p);
                // A new connection: the keymap goes out on it whatever the
                // old one carried.
                if let Some((serial, text)) = self.keymap.take() {
                    self.send_keymap(p, serial, &text, out);
                    self.keymap = Some((serial, text));
                }
                // PeerUp while remote to p means the connection was
                // replaced under the crossing. The outstanding Held (or its
                // ack) may have died with the old one, and stop-and-wait would
                // then never send another: send a fresh one on the new
                // connection. A restarted target answers it `Ended(stale)`.
                if let Crossing::Remote(r) = &mut self.crossing
                    && r.peer == p
                {
                    // The cadence restarts from this one.
                    r.next_snapshot = now;
                    self.send_snapshot(now, out);
                }
                self.maybe_rearm(out);
            }
            SourceEvent::PeerDown(p) => {
                self.up.retain(|q| *q != p);
                self.keymap_sent.retain(|(q, _)| *q != p);
                // It says again at its next peer up (an older build never does).
                self.peer_locked(p, false);
                match &self.crossing {
                    Crossing::Remote(r) if r.peer == p => {
                        self.end_remote(LeaveReason::LinkLost, false, ReleaseAt::Exit, out);
                    }
                    Crossing::Arming(a) if a.link.to.peer == p => self.abort(true, now, out),
                    _ => {}
                }
                self.maybe_rearm(out);
            }
            SourceEvent::PeerAlive(p) => self.unsuspect(p),
            SourceEvent::Control { peer, msg } => self.control(peer, msg, now, out),
            SourceEvent::SessionRestarted => {
                self.enabled = false;
                self.capture_ended(now, out);
                // The old session's barriers and answers are gone.
                self.barriers.clear();
                self.barriers_outstanding = 0;
                self.awaiting_barriers = false;
                self.released = [None; 4];
                self.zones_stale = true;
                match self.gate {
                    // Kept; unlock arms with SetBarriers anyway.
                    Gate::Locked => {}
                    // Kept; the wake re-zones instead of restoring Armed.
                    Gate::Suspended => self.gate_before_sleep = Gate::Rezoning,
                    Gate::Idle | Gate::Armed | Gate::Rezoning | Gate::BarriersFailed => {
                        self.start_rezone(now);
                    }
                }
            }
            SourceEvent::Tick => self.tick(now, true, out),
            SourceEvent::CursorPolled(pos) => self.cursor_polled(pos, out),
            SourceEvent::Controlled(on) => {
                if on {
                    // A crossing that raced the Enter we accepted.
                    self.end_any(LeaveReason::Preempted, now, out);
                }
                if self.controlled != on {
                    self.controlled = on;
                    self.off_changed();
                }
            }
            SourceEvent::LockedHere(on) => {
                if self.locked_here != on {
                    self.locked_here = on;
                    if on && matches!(self.crossing, Crossing::Arming(_)) {
                        // About to cross out of a locked peer: a wall.
                        self.counters.bounces += 1;
                        self.abort(true, now, out);
                    }
                    self.off_changed();
                }
            }
            SourceEvent::PeerLocked { peer, locked } => self.peer_locked(peer, locked),
            SourceEvent::Go { side, cursor } => self.go(side, cursor, now, out),
        }
        if self.rearm_when_local && matches!(self.crossing, Crossing::Local) {
            self.rearm_when_local = false;
            self.maybe_rearm(out);
        }
        self.counters.repeat_pairs_dropped = self.filter.dropped_pairs;
        self.counters.repeat_split_pairs = self.filter.split_pairs;
        self.counters.repeat_split_max_us = self.filter.split_max_us;
        self.counters.repeat_future_stamps = self.filter.future_stamps;
        if self.status_key() != before {
            out.push(SourceAction::Status(self.status()));
        }
    }

    /// What [`Self::status`] shows, without the allocation.
    fn status_key(&self) -> StatusKey {
        let s = self.status_parts();
        (
            s.0,
            s.1,
            self.session,
            self.last_leave,
            self.gate,
            self.suspect_rev,
            (self.controlled, self.locked_here, self.locked_rev),
        )
    }

    /// `controlled` or `locked_here` changed: the barriers follow, now if
    /// the pointer is home, else when the crossing ends. A suspended gate
    /// re-zones on wake instead of restoring a stale `Armed`.
    fn off_changed(&mut self) {
        self.rearm_when_local = true;
        if self.gate == Gate::Suspended && self.gate_before_sleep == Gate::Armed {
            self.gate_before_sleep = Gate::Rezoning;
        }
    }

    fn peer_locked(&mut self, peer: PeerId, locked: bool) {
        let had = self.locked_peers.contains(&peer);
        if locked && !had {
            self.locked_peers.push(peer);
        } else if !locked && had {
            self.locked_peers.retain(|q| *q != peer);
        } else {
            return;
        }
        self.locked_rev = self.locked_rev.wrapping_add(1);
    }

    /// Every barrier of an edge that can be crossed somewhere: none while
    /// controlled or locked here, and none on an edge whose every link leads
    /// to a peer that is down or suspect. Such an edge is a wall as a whole,
    /// and a wall with no barrier costs nothing to push against.
    /// The ids are those of the list for all up peers, so a peer turning
    /// suspect never renumbers the other edges under an `Activated` in flight.
    fn edge_barriers(&self) -> Vec<Barrier> {
        let Some(t) = self
            .topo
            .as_ref()
            .filter(|_| !self.controlled && !self.locked_here)
        else {
            return Vec::new();
        };
        let mut all = geom::barriers(t, &self.up);
        all.retain(|b| {
            t.links
                .iter()
                .any(|l| l.from == b.monitor && l.side == b.side && self.usable(l.to.peer))
        });
        all
    }

    /// The barriers to send now: [`Self::edge_barriers`] less the parked ones.
    fn wanted_barriers(&self) -> Vec<Barrier> {
        let mut want = self.edge_barriers();
        want.retain(|b| !self.parked.contains(b));
        want
    }

    /// A link to `p` can be crossed: `p` is up and not suspect.
    fn usable(&self, p: PeerId) -> bool {
        self.up.contains(&p) && !self.suspect.contains(&p)
    }

    fn status_parts(&self) -> (SourceRole, Option<PeerId>) {
        match &self.crossing {
            Crossing::Local => (SourceRole::Local, None),
            Crossing::Pending(_) => (SourceRole::Pending, None),
            Crossing::Arming(a) => (SourceRole::Arming, Some(a.link.to.peer)),
            Crossing::Remote(r) => (SourceRole::Remote, Some(r.peer)),
        }
    }

    /// Heard from `p` (a Ping, a Pong or a new `PeerUp`): its links work
    /// again, and the barriers of their edges go back up once local.
    fn unsuspect(&mut self, p: PeerId) {
        if self.suspect.contains(&p) {
            self.suspect.retain(|q| *q != p);
            self.suspect_rev = self.suspect_rev.wrapping_add(1);
            self.rearm_when_local = true;
        }
    }

    /// The earliest of: the snapshot tick, the liveness deadline, the arming
    /// timeout, the pending timeout, the re-zone settle and the next lock
    /// poll. The caller arms one timer and feeds [`SourceEvent::Tick`].
    pub fn next_deadline(&self) -> Option<Micros> {
        let mut d = self.next_lock_poll;
        match &self.crossing {
            Crossing::Local => {}
            Crossing::Pending(p) => d = d.min(p.since.plus(self.pending_timeout())),
            Crossing::Arming(a) => d = d.min(a.since.plus(ARM_TIMEOUT)),
            Crossing::Remote(r) => {
                d = d.min(r.acked_at.plus(SOURCE_LIVENESS));
                // An unacknowledged Held holds the next one back: no tick
                // for it until the ack comes in (`control`).
                if r.held_acked {
                    d = d.min(r.next_snapshot);
                }
                if let Some(t) = r.pace.deadline() {
                    d = d.min(t);
                }
                if let Some(t) = self.filter.deadline() {
                    d = d.min(t);
                }
            }
        }
        if self.gate == Gate::Rezoning
            && let Some(t) = self.rezone_at
        {
            d = d.min(t);
        }
        if self.gate == Gate::BarriersFailed
            && let Some(t) = self.retry_at
        {
            d = d.min(t);
        }
        if self.parking() {
            d = d.min(self.park_poll_at.min(self.park_until));
        }
        if let Some(g) = &self.going {
            d = d.min(g.until);
        }
        Some(d)
    }

    pub fn status(&self) -> SourceStatus {
        let (role, peer) = self.status_parts();
        SourceStatus {
            role,
            peer,
            session: self.session,
            last_leave: self.last_leave,
            gate: self.gate,
            suspect: self.suspect.clone(),
            controlled: self.controlled,
            locked_here: self.locked_here,
            locked_peers: self.locked_peers.clone(),
            edge: match &self.crossing {
                Crossing::Remote(r) => Some(r.link),
                _ => None,
            },
        }
    }

    pub fn counters(&self) -> &SourceCounters {
        &self.counters
    }

    /// The serial of the current keymap, for the log: it moves only when a
    /// keymap text differs from the current one.
    pub fn keymap_serial(&self) -> Option<u32> {
        self.keymap.as_ref().map(|(serial, _)| *serial)
    }

    // ---- timers --------------------------------------------------------

    fn tick(&mut self, now: Micros, flush_filter: bool, out: &mut Actions<SourceAction>) {
        // A held-back release whose 1 ms passed is real.
        if flush_filter
            && matches!(self.crossing, Crossing::Remote(_))
            && let Some(code) = self.filter.expire(now)
        {
            self.key_edge(code, false, now, out);
        }
        if self.going.is_some_and(|g| now >= g.until) {
            self.going = None;
            self.go_refused("the nudge did not activate the barrier", out);
        }
        match &self.crossing {
            Crossing::Remote(r) if now >= r.acked_at.plus(SOURCE_LIVENESS) => {
                let peer = r.peer;
                self.counters.silence_releases += 1;
                self.end_remote(LeaveReason::Silence, true, ReleaseAt::Exit, out);
                if !self.suspect.contains(&peer) {
                    self.suspect.push(peer);
                    self.suspect_rev = self.suspect_rev.wrapping_add(1);
                    // Its edges are walls now: their barriers come down.
                    self.rearm_when_local = true;
                }
            }
            Crossing::Remote(r) if r.held_acked && now >= r.next_snapshot => {
                self.send_snapshot(now, out);
            }
            Crossing::Pending(p) if now >= p.since.plus(self.pending_timeout()) => {
                self.abort(true, now, out)
            }
            Crossing::Arming(a) if now >= a.since.plus(ARM_TIMEOUT) => self.abort(true, now, out),
            _ => {}
        }
        if let Crossing::Remote(r) = &mut self.crossing
            && let Some((dx, dy)) = r.pace.flush(now)
        {
            self.send_motion(dx, dy, now, out);
        }
        if self.gate == Gate::Rezoning && self.rezone_at.is_some_and(|t| now >= t) {
            self.rezone_at = None;
            self.arm(true, out);
        }
        if self.gate == Gate::BarriersFailed && self.retry_at.is_some_and(|t| now >= t) {
            self.retry_at = None;
            self.arm(true, out);
        }
        if self.parking() {
            if now >= self.park_until {
                self.parked.clear();
                self.resend_barriers(out);
            } else if now >= self.park_poll_at {
                out.push(SourceAction::CheckCursor);
                self.park_poll_at = now.plus(PARK_POLL);
            }
        }
        if now >= self.next_lock_poll {
            out.push(SourceAction::CheckLock);
            self.next_lock_poll = now.plus(self.poll_interval());
        }
    }

    /// 250 ms while a crossing is pending, arming or remote (at the
    /// default budget, [`crate::time::LockBudget::active_poll`]); 500 ms while locked; 2 s
    /// while local.
    fn poll_interval(&self) -> Micros {
        if !matches!(self.crossing, Crossing::Local) {
            self.config.lock_check_budget.active_poll()
        } else if self.gate == Gate::Locked {
            LOCK_POLL_LOCKED
        } else {
            LOCK_POLL_IDLE
        }
    }

    fn poll_within(&mut self, now: Micros, interval: Micros) {
        self.next_lock_poll = self.next_lock_poll.min(now.plus(interval));
    }

    // ---- the gate ------------------------------------------------------

    /// `SetBarriers` for the current topology and up peers, or `Disable` →
    /// Idle when there is nothing to arm.
    fn arm(&mut self, refresh_zones: bool, out: &mut Actions<SourceAction>) {
        // Every arm starts with all the barriers up: a park belongs to the
        // list it was made against.
        self.parked.clear();
        let want = self.edge_barriers();
        if want.is_empty() {
            if self.enabled || self.gate != Gate::Idle {
                out.push(SourceAction::Portal(PortalCmd::Disable));
            }
            self.enabled = false;
            self.gate = Gate::Idle;
            self.barriers.clear();
            self.awaiting_barriers = false;
            self.retry_at = None;
            return;
        }
        out.push(SourceAction::Portal(PortalCmd::SetBarriers {
            refresh_zones: refresh_zones || self.zones_stale,
            barriers: want.clone(),
        }));
        self.zones_stale = false;
        self.barriers = want;
        self.barriers_outstanding += 1;
        self.awaiting_barriers = true;
    }

    /// Topology or up peers changed: re-arm when the barriers differ and
    /// the gate is one that arms on its own (Idle, Armed, BarriersFailed).
    fn maybe_rearm(&mut self, out: &mut Actions<SourceAction>) {
        if !self.ever_unlocked || self.lock != LockCheck::Unlocked {
            return;
        }
        if !matches!(self.gate, Gate::Idle | Gate::Armed | Gate::BarriersFailed) {
            return;
        }
        // Nothing to arm leaves the gate Idle; a list that is all parked is
        // not nothing (the gate stays Armed with no barrier up).
        let unchanged = if self.edge_barriers().is_empty() {
            self.gate == Gate::Idle
        } else {
            self.gate != Gate::Idle && self.wanted_barriers() == self.barriers
        };
        if unchanged && self.gate != Gate::BarriersFailed {
            return;
        }
        self.barrier_retries = 0;
        self.arm(false, out);
    }

    fn barriers_set(&mut self, failed: &[u32], now: Micros, out: &mut Actions<SourceAction>) {
        self.barriers_outstanding = self.barriers_outstanding.saturating_sub(1);
        if self.barriers_outstanding > 0 || !self.awaiting_barriers {
            return;
        }
        self.awaiting_barriers = false;
        if !failed.is_empty() {
            self.counters.barrier_failures += 1;
            if self.enabled {
                out.push(SourceAction::Portal(PortalCmd::Disable));
                self.enabled = false;
            }
            self.gate = Gate::BarriersFailed;
            if self.barrier_retries < BARRIER_RETRY_MAX {
                self.barrier_retries += 1;
                self.retry_at = Some(now.plus(BARRIER_RETRY_DELAY));
            } else {
                self.retry_at = None;
            }
            return;
        }
        self.barrier_retries = 0;
        self.retry_at = None;
        if self.lock != LockCheck::Unlocked {
            if self.enabled {
                out.push(SourceAction::Portal(PortalCmd::Disable));
                self.enabled = false;
            }
            self.gate = Gate::Locked;
            return;
        }
        if !self.enabled {
            out.push(SourceAction::Portal(PortalCmd::Enable));
            self.enabled = true;
        }
        self.gate = Gate::Armed;
    }

    fn start_rezone(&mut self, now: Micros) {
        self.gate = Gate::Rezoning;
        self.rezone_at = Some(now.plus(REZONE_SETTLE));
        self.awaiting_barriers = false;
        self.retry_at = None;
    }

    fn lock_polled(&mut self, check: LockCheck, now: Micros, out: &mut Actions<SourceAction>) {
        match check {
            LockCheck::Unknown => self.counters.lock_unknown += 1,
            LockCheck::Locked => {
                self.lock = LockCheck::Locked;
                if !matches!(self.gate, Gate::Locked | Gate::Suspended) {
                    self.end_any(LeaveReason::Locked, now, out);
                    self.gate_locked(now, out);
                }
            }
            LockCheck::Unlocked => {
                self.lock = LockCheck::Unlocked;
                let first = !self.ever_unlocked;
                self.ever_unlocked = true;
                if self.gate == Gate::Locked || (first && self.gate != Gate::Suspended) {
                    self.arm(true, out);
                }
            }
        }
    }

    fn gate_locked(&mut self, now: Micros, out: &mut Actions<SourceAction>) {
        out.push(SourceAction::Portal(PortalCmd::Disable));
        self.enabled = false;
        self.gate = Gate::Locked;
        self.awaiting_barriers = false;
        self.rezone_at = None;
        self.retry_at = None;
        self.poll_within(now, LOCK_POLL_LOCKED);
    }

    // ---- walls: parked barriers --------------------------------
    //
    // A barrier must be a whole monitor edge, so a dead corner or
    // a stretch with no usable link is enforced after the activation: each
    // motion that pushes there is an `Activated`, a lock read and a
    // `Release`. After the first such bounce the barrier comes down, and the
    // machine reads the cursor every `PARK_POLL` until it leaves the stretch
    // (or moves away from the edge, or `PARK_MAX` passes): then the barrier
    // goes back up, and a push on the link crosses as before.

    /// Parked barriers are being watched: armed, local, some parked.
    fn parking(&self) -> bool {
        !self.parked.is_empty()
            && self.gate == Gate::Armed
            && matches!(self.crossing, Crossing::Local)
    }

    /// Take `b` down after a bounce at a wall stretch on it.
    fn park(&mut self, b: Barrier, now: Micros, out: &mut Actions<SourceAction>) {
        if self.gate != Gate::Armed || self.parked.contains(&b) {
            return;
        }
        if self.parked.is_empty() {
            self.park_until = now.plus(PARK_MAX);
            self.park_read_failed = false;
        }
        self.park_poll_at = now.plus(PARK_POLL);
        self.parked.push(b);
        self.counters.edge_parks += 1;
        self.resend_barriers(out);
    }

    /// A cursor read while parked: each parked barrier whose stretch the
    /// cursor has left goes back up. Two failed reads in a row put them all
    /// back: the machine no longer knows where the cursor is.
    fn cursor_polled(&mut self, pos: Option<Point>, out: &mut Actions<SourceAction>) {
        if !self.parking() {
            return;
        }
        let before = self.parked.len();
        match (pos, self.topo.as_ref()) {
            (None, _) if !self.park_read_failed => {
                self.park_read_failed = true;
                return;
            }
            (Some(p), Some(topo)) => {
                self.park_read_failed = false;
                let p = topo.to_plane(p);
                let parked = std::mem::take(&mut self.parked);
                self.parked = parked.into_iter().filter(|b| self.at_wall(b, p)).collect();
            }
            _ => self.parked.clear(),
        }
        if self.parked.len() != before {
            self.resend_barriers(out);
        }
    }

    /// The cursor (plane) is still on `b`'s monitor, near its edge, and not
    /// on a stretch of it that crosses.
    fn at_wall(&self, b: &Barrier, p: Point) -> bool {
        let Some(m) = self.topo.as_ref().and_then(|t| t.monitor(b.monitor)) else {
            return false;
        };
        let near = (b.side.across(p) - f64::from(b.side.line(&m.rect))).abs() <= PARK_NEAR_PX;
        m.rect.contains(p)
            && near
            && !self
                .link_at(b.monitor, b.side, b.side.along(p))
                .is_some_and(|l| self.usable(l.to.peer))
    }

    /// Send the wanted list after a park or an unpark. Only while armed:
    /// every other gate re-arms with the whole list (`arm` clears the parks).
    fn resend_barriers(&mut self, out: &mut Actions<SourceAction>) {
        if self.gate != Gate::Armed {
            return;
        }
        let want = self.wanted_barriers();
        if want == self.barriers {
            return;
        }
        out.push(SourceAction::Portal(PortalCmd::SetBarriers {
            refresh_zones: self.zones_stale,
            barriers: want.clone(),
        }));
        self.zones_stale = false;
        self.barriers = want;
        self.barriers_outstanding += 1;
        self.awaiting_barriers = true;
    }

    /// The barrier with this id, armed or parked.
    fn barrier(&self, id: u32) -> Option<&Barrier> {
        self.barriers
            .iter()
            .chain(self.parked.iter())
            .find(|b| b.id == id)
    }

    // ---- crossing: activation ------------------------------------------

    fn activated(
        &mut self,
        id: u32,
        barrier_id: u32,
        local: Point,
        lock: LockCheck,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        self.counters.activations += 1;
        let going = self.going.take();
        let pending = match std::mem::replace(&mut self.crossing, Crossing::Local) {
            Crossing::Local => Pending {
                since: now,
                dx: 0.0,
                dy: 0.0,
                buffered: self.take_buffer(),
            },
            Crossing::Pending(p) => p,
            other => {
                // Already arming or remote: a second activation is ignored.
                self.crossing = other;
                return;
            }
        };
        let activation = self.contact(id, barrier_id, local);
        let bounce_pos = activation.map(|a| self.to_local(a.inside()));
        if self.refuse_activation_until.take().is_some_and(|t| now < t) {
            // The capture that ending Pending meant to stop.
            self.counters.bounces += 1;
            self.recycle(pending.buffered);
            self.release(Some(id), bounce_pos, out);
            return;
        }
        if lock != LockCheck::Unlocked {
            // Only Unlocked can lead to Enter; nothing is forwarded.
            self.counters.lock_refusals += 1;
            self.recycle(pending.buffered);
            self.release(Some(id), bounce_pos, out);
            if lock == LockCheck::Locked {
                self.lock = LockCheck::Locked;
                if self.gate != Gate::Suspended {
                    self.gate_locked(now, out);
                }
            }
            return;
        }
        // A contact on a wall stretch (a dead corner, no link, or a link to a
        // peer that is down or suspect) parks its barrier after the bounce.
        let mut wall = None;
        let verdict = activation.and_then(|a| {
            if self.gate != Gate::Armed || self.controlled || self.locked_here {
                return None;
            }
            let Some(link) = self
                .link_at(a.monitor, a.side, a.side.along(a.at))
                .filter(|l| self.usable(l.to.peer))
            else {
                wall = self.barrier(barrier_id).copied();
                return None;
            };
            if !self.keymap_ok(link.to.peer) || orphan_button_release(&pending.buffered) {
                return None;
            }
            Some((a, link))
        });
        let Some((activation, link)) = verdict else {
            self.counters.bounces += 1;
            self.recycle(pending.buffered);
            self.release(Some(id), bounce_pos, out);
            if let Some(b) = wall {
                self.park(b, now, out);
            }
            return;
        };
        let r = f64::from(self.config.edge_resistance_px);
        let dwell = Micros::from_millis(u64::from(self.config.edge_dwell_ms));
        let push = activation.side.outward(pending.dx, pending.dy).max(0.0);
        // A key crossing's own nudge crosses at once.
        let keyed = going.filter(|g| now < g.until && g.link == link);
        if let Some(g) = keyed {
            self.counters.go_crossings += 1;
            let activation = Activation {
                origin: g.origin,
                ..activation
            };
            self.enter(link, activation, activation.at, pending.buffered, now, out);
        // R = 0 and no dwell crosses at once; so does a push already met.
        } else if push >= r && dwell == Micros::ZERO {
            self.enter(link, activation, activation.at, pending.buffered, now, out);
        } else {
            self.crossing = Crossing::Arming(Arming {
                since: now,
                activation,
                link,
                push,
                buffered: pending.buffered,
            });
        }
    }

    /// The contact point of an activation: `Activated` x,y to the plane,
    /// clamped onto the hit barrier's line and into the monitor's span.
    fn contact(&self, id: u32, barrier_id: u32, local: Point) -> Option<Activation> {
        // ferryd passes NaN when the portal sent no cursor position.
        if !local.x.is_finite() || !local.y.is_finite() {
            return None;
        }
        let topo = self.topo.as_ref()?;
        let b = self.barrier(barrier_id)?;
        let m = topo.monitor(b.monitor)?;
        let plane = topo.to_plane(local);
        let (lo, hi) = b.side.span(&m.rect);
        let along = b
            .side
            .along(plane)
            .clamp(f64::from(lo), f64::from(hi - 1).max(f64::from(lo)));
        let across = f64::from(b.side.line(&m.rect));
        Some(Activation {
            id,
            monitor: b.monitor,
            home: m.rect,
            side: b.side,
            at: b.side.point(along, across),
            origin: None,
        })
    }

    /// The link from `monitor` on `side` whose span minus the dead corners
    /// contains `along`.
    fn link_at(&self, monitor: MonitorId, side: Side, along: f64) -> Option<EdgeLink> {
        let c = f64::from(self.config.corner_dead_zone_px);
        self.topo.as_ref()?.links.iter().copied().find(|l| {
            l.from == monitor
                && l.side == side
                && along >= f64::from(l.span.0) + c
                && along < f64::from(l.span.1) - c
        })
    }

    fn keymap_ok(&self, peer: PeerId) -> bool {
        self.keymap
            .as_ref()
            .is_some_and(|(serial, _)| self.keymap_sent.contains(&(peer, *serial)))
    }

    /// Enter `link.to` at `contact` (a plane point on the link's line): the
    /// entry point is the same point clamped into the target monitor.
    fn enter(
        &mut self,
        link: EdgeLink,
        activation: Activation,
        contact: Point,
        mut buffered: Vec<SourceEvent>,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        let peer = link.to.peer;
        let entry = self
            .topo
            .as_ref()
            .and_then(|t| t.monitor(link.to))
            .map(|m| m.rect.clamp(contact));
        let (Some(entry), Some((serial, _))) = (entry, self.keymap.as_ref()) else {
            self.counters.bounces += 1;
            self.recycle(buffered);
            self.release(
                Some(activation.id),
                Some(self.to_local(activation.inside())),
                out,
            );
            return;
        };
        let keymap_serial = *serial;
        if !self.parked.is_empty() {
            // Crossed by another edge: every barrier is back once home.
            self.parked.clear();
            self.rearm_when_local = true;
        }
        self.session = self.session.wrapping_add(1);
        self.held.clear();
        self.filter.reset();
        self.crossing = Crossing::Remote(Box::new(Remote {
            peer,
            link,
            activation,
            model: entry,
            held_n: 1,
            held_at: now,
            held_acked: false,
            acked_at: now,
            next_snapshot: now.plus(SNAPSHOT_PERIOD),
            return_push: 0.0,
            pace: MotionPacer::new(now),
        }));
        let pos = self.pos();
        self.send(
            peer,
            Control::Enter {
                session: self.session,
                x: pos.x,
                y: pos.y,
                keymap_serial,
                motion_seq: self.motion_seq,
            },
            out,
        );
        self.send(
            peer,
            Control::Held {
                session: self.session,
                n: 1,
                keys: Vec::new(),
                buttons: Vec::new(),
                pos,
            },
            out,
        );
        self.counters.crossings += 1;
        self.poll_within(now, self.config.lock_check_budget.active_poll());
        // Replay what arrived before the verdict through the Remote rows.
        for ev in buffered.drain(..) {
            if !matches!(self.crossing, Crossing::Remote(_)) {
                break;
            }
            self.input(ev, now, out);
        }
        self.recycle(buffered);
    }

    // ---- crossing: input ---------------------------------------------------

    fn motion(&mut self, dx: f64, dy: f64, now: Micros, out: &mut Actions<SourceAction>) {
        match &mut self.crossing {
            Crossing::Local => {}
            Crossing::Pending(p) => {
                p.dx += dx;
                p.dy += dy;
            }
            Crossing::Arming(a) => {
                let outward = a.activation.side.outward(dx, dy);
                if outward < 0.0 {
                    // Pulled back: stay local, lose nothing.
                    self.abort(true, now, out);
                    return;
                }
                a.push += outward;
                let r = f64::from(self.config.edge_resistance_px);
                let dwell = Micros::from_millis(u64::from(self.config.edge_dwell_ms));
                if a.push >= r && now.since(a.since) >= dwell {
                    let Crossing::Arming(a) =
                        std::mem::replace(&mut self.crossing, Crossing::Local)
                    else {
                        return;
                    };
                    self.enter(a.link, a.activation, a.activation.at, a.buffered, now, out);
                }
            }
            Crossing::Remote(_) => self.remote_motion(dx, dy, now, out),
        }
    }

    fn remote_motion(&mut self, dx: f64, dy: f64, now: Micros, out: &mut Actions<SourceAction>) {
        let step = {
            let (Crossing::Remote(r), Some(topo)) = (&mut self.crossing, self.topo.as_ref()) else {
                return;
            };
            let c = f64::from(self.config.corner_dead_zone_px);
            let resistance = f64::from(self.config.edge_resistance_px);
            // A target locked to itself: every link out of it is a wall.
            let walled = self.locked_peers.contains(&r.peer);
            let block =
                walled || (self.config.block_while_button_held && !self.held.buttons().is_empty());
            remote_step(r, topo, Point { x: dx, y: dy }, c, resistance, block, |p| {
                !walled && self.up.contains(&p) && !self.suspect.contains(&p) && {
                    self.keymap
                        .as_ref()
                        .is_some_and(|(s, _)| self.keymap_sent.contains(&(p, *s)))
                }
            })
        };
        match step {
            Step::Moved { send: false } => {}
            Step::Moved { send: true } => {
                let Crossing::Remote(r) = &mut self.crossing else {
                    return;
                };
                match r.pace.motion(dx, dy, now) {
                    Pace::Send { dx, dy } => self.send_motion(dx, dy, now, out),
                    // The model moved and no datagram says so: a frame's position
                    // fence must still carry it, so its seq moves on.
                    Pace::Coalesced => {
                        self.motion_seq = self.motion_seq.wrapping_add(1);
                        self.counters.motion_coalesced += 1;
                    }
                    Pace::Suppressed => {
                        self.motion_seq = self.motion_seq.wrapping_add(1);
                        self.counters.motion_suppressed += 1;
                    }
                }
            }
            Step::Home { continuation } => {
                self.end_remote(LeaveReason::Home, true, ReleaseAt::Point(continuation), out);
            }
            Step::Hop { link, contact } => self.hop(link, contact, now, out),
        }
    }

    /// Leave the current target for `link.to`, entering at `contact`.
    fn hop(
        &mut self,
        link: EdgeLink,
        contact: Point,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        let Crossing::Remote(r) = std::mem::replace(&mut self.crossing, Crossing::Local) else {
            return;
        };
        self.send(
            r.peer,
            Control::Leave {
                session: self.session,
                reason: LeaveReason::Hop.code(),
            },
            out,
        );
        self.last_leave = Some(LeaveReason::Hop);
        let buffer = self.take_buffer();
        self.enter(link, r.activation, contact, buffer, now, out);
    }

    // ---- key crossings -------------------------------------------

    fn go_refused(&mut self, why: &'static str, out: &mut Actions<SourceAction>) {
        self.counters.go_refused += 1;
        out.push(SourceAction::GoRefused(why));
    }

    /// `ferry go <side>`: from home, warp and nudge through the linked edge on `side`; while
    /// remote, what the go chord does.
    fn go(
        &mut self,
        side: Side,
        cursor: Option<Point>,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        match self.crossing {
            Crossing::Remote(_) => {
                self.go_remote(side, now, out);
                return;
            }
            Crossing::Pending(_) | Crossing::Arming(_) => {
                return self.go_refused("a crossing is starting", out);
            }
            Crossing::Local => {}
        }
        if self.gate != Gate::Armed || self.controlled || self.locked_here {
            return self.go_refused("capture is off", out);
        }
        let Some(topo) = self.topo.as_ref() else {
            return self.go_refused("no layout", out);
        };
        let at = cursor.map(|p| topo.to_plane(p));
        let on_from = |l: &EdgeLink| {
            at.is_some_and(|p| topo.monitor(l.from).is_some_and(|m| m.rect.contains(p)))
        };
        let dist = |l: &EdgeLink| at.map_or(0.0, |p| dist2(p, link_mid(l)));
        let link = topo
            .links
            .iter()
            .filter(|l| {
                l.from.peer == topo.me
                    && l.side == side
                    && self.usable(l.to.peer)
                    && self.keymap_ok(l.to.peer)
                    && self
                        .edge_barriers()
                        .iter()
                        .any(|b| b.monitor == l.from && b.side == side)
            })
            // The cursor's own monitor first, then the nearest link.
            .min_by(|a, b| {
                (!on_from(a), dist(a))
                    .partial_cmp(&(!on_from(b), dist(b)))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        let (Some(link), Some(home)) = (link, link.and_then(|l| topo.monitor(l.from))) else {
            return self.go_refused("no machine on that side", out);
        };
        let warp = topo.to_local(geom::inside(
            &home.rect,
            side,
            link_mid(&link),
            RELEASE_INSET,
        ));
        let (dx, dy) = match side {
            Side::Left => (-NUDGE_PX, 0.0),
            Side::Right => (NUDGE_PX, 0.0),
            Side::Top => (0.0, -NUDGE_PX),
            Side::Bottom => (0.0, NUDGE_PX),
        };
        // A barrier parked after a bounce on this edge goes back up first.
        let before = self.parked.len();
        self.parked
            .retain(|b| !(b.monitor == link.from && b.side == side));
        if self.parked.len() != before {
            self.resend_barriers(out);
        }
        self.going = Some(Going {
            link,
            origin: at,
            until: now.plus(GO_TIMEOUT),
        });
        out.push(SourceAction::Go {
            x: warp.x,
            y: warp.y,
            dx,
            dy,
        });
    }

    /// The go chord (or `ferry go`) while remote: the link on `side` of the target monitor the
    /// model is on. Toward home, the crossing ends where the cursor was before a key crossing
    /// (else mid-link); toward another peer, it hops there, mid-link. Anything else does
    /// nothing. The drag block does not apply: the keys say where to go.
    fn go_remote(&mut self, side: Side, now: Micros, out: &mut Actions<SourceAction>) {
        let (Crossing::Remote(r), Some(topo)) = (&self.crossing, self.topo.as_ref()) else {
            return;
        };
        let Some(cur) = topo
            .monitors
            .iter()
            .find(|m| m.id.peer == r.peer && m.rect.contains(r.model))
        else {
            return self.go_refused("the pointer is on no monitor of the peer", out);
        };
        let along = side.along(r.model);
        let link = topo
            .links
            .iter()
            .filter(|l| l.from == cur.id && l.side == side)
            .min_by(|a, b| {
                span_dist(a, along)
                    .partial_cmp(&span_dist(b, along))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        let walled = self.locked_peers.contains(&r.peer);
        let origin = r.activation.origin;
        let Some(link) = link.filter(|_| !walled) else {
            return self.go_refused("no machine on that side", out);
        };
        let mid = link_mid(&link);
        if link.to.peer == topo.me {
            let Some(home) = topo.monitor(link.to) else {
                return self.go_refused("no machine on that side", out);
            };
            let at = origin
                .filter(|o| {
                    topo.monitors
                        .iter()
                        .any(|m| m.id.peer == topo.me && m.rect.contains(*o))
                })
                .unwrap_or_else(|| geom::inside(&home.rect, side.opposite(), mid, RELEASE_INSET));
            self.counters.go_crossings += 1;
            self.end_remote(LeaveReason::Home, true, ReleaseAt::Point(at), out);
        } else if link.to.peer != r.peer
            && self.usable(link.to.peer)
            && self.keymap_ok(link.to.peer)
        {
            self.counters.go_crossings += 1;
            self.hop(link, mid, now, out);
        } else {
            self.go_refused("no machine on that side", out);
        }
    }

    /// Key, button, scroll and batch-end events.
    fn input(&mut self, ev: SourceEvent, now: Micros, out: &mut Actions<SourceAction>) {
        match &mut self.crossing {
            Crossing::Local => {}
            Crossing::Pending(Pending { buffered, .. })
            | Crossing::Arming(Arming { buffered, .. }) => {
                if matches!(ev, SourceEvent::BatchEnd) {
                    // Only a batch end after a key matters to the filter.
                    if matches!(buffered.last(), Some(SourceEvent::Key { .. })) {
                        buffered.push(ev);
                    }
                    return;
                }
                if buffered.len() >= PENDING_MAX {
                    // Arming releases its activation now. Pending has none
                    // yet: its bounce is counted when the Activated is refused.
                    self.counters.pending_overflows += 1;
                    if matches!(self.crossing, Crossing::Arming(_)) {
                        self.counters.bounces += 1;
                    }
                    self.abort(true, now, out);
                    return;
                }
                buffered.push(ev);
            }
            Crossing::Remote(_) => match ev {
                SourceEvent::Key {
                    code,
                    pressed,
                    t_eis,
                } => self.remote_key(code, pressed, t_eis, now, out),
                SourceEvent::BatchEnd => {
                    if let Some(code) = self.filter.batch_end() {
                        self.key_edge(code, false, now, out);
                    }
                }
                SourceEvent::Button { code, pressed } => match self.held.button(code, pressed) {
                    Edge::Changed => {
                        let pos = self.pos();
                        self.send_current(
                            Control::Button {
                                session: self.session,
                                code,
                                pressed,
                                pos,
                            },
                            out,
                        );
                    }
                    Edge::AlreadyDown => {}
                    Edge::NotDown => {
                        // The drag rule: a local drag crossed.
                        self.counters.drag_leaves += 1;
                        self.end_remote(LeaveReason::Drag, true, ReleaseAt::Exit, out);
                    }
                },
                SourceEvent::Scroll(s) => {
                    let pos = self.pos();
                    self.send_current(
                        Control::Scroll {
                            session: self.session,
                            axis: s.axis,
                            source: s.source,
                            value: s.value,
                            value120: s.value120,
                            stop: s.stop,
                            pos,
                        },
                        out,
                    );
                }
                _ => {}
            },
        }
    }

    fn remote_key(
        &mut self,
        code: u16,
        pressed: bool,
        t: Micros,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        if code == crate::KEY_WAKEUP {
            return;
        }
        let mut edges = [(0u16, false); 2];
        let mut n = 0;
        self.filter.key(code, pressed, t, now, |c, p| {
            if let Some(slot) = edges.get_mut(n) {
                *slot = (c, p);
                n += 1;
            }
        });
        for &(c, p) in &edges[..n] {
            if !self.key_edge(c, p, now, out) {
                break;
            }
        }
    }

    /// One key edge after the repeat filter. Returns false once the
    /// crossing has ended or moved (the escape chord, the go chord).
    fn key_edge(
        &mut self,
        code: u16,
        pressed: bool,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) -> bool {
        if !matches!(self.crossing, Crossing::Remote(_)) {
            return false;
        }
        if pressed && let Some(side) = self.config.go_chord.side(code, self.held.keys()) {
            // The arrow is never forwarded, whatever comes of it; the modifiers were, and
            // the target's ReleaseAll on Leave clears them.
            let session = self.session;
            self.go_remote(side, now, out);
            return matches!(self.crossing, Crossing::Remote(_)) && self.session == session;
        }
        if pressed && self.config.escape_chord.matches(code, self.held.keys()) {
            // The trigger is never forwarded; the target's ReleaseAll on
            // Leave clears the modifiers that were.
            self.end_remote(LeaveReason::EscapeChord, true, ReleaseAt::Exit, out);
            return false;
        }
        match self.held.key(code, pressed) {
            Edge::Changed => self.send_current(
                Control::Key {
                    session: self.session,
                    code,
                    pressed,
                },
                out,
            ),
            Edge::AlreadyDown => {}
            Edge::NotDown => self.counters.orphan_key_releases += 1,
        }
        true
    }

    // ---- crossing: control from the target ---------------------------------

    fn control(
        &mut self,
        peer: PeerId,
        msg: Control,
        now: Micros,
        out: &mut Actions<SourceAction>,
    ) {
        let Crossing::Remote(r) = &mut self.crossing else {
            return;
        };
        if r.peer != peer {
            return;
        }
        match msg {
            // Only the outstanding Held can be acknowledged: stop-and-wait
            // keeps one in flight, so any other n is stale or bogus.
            Control::HeldAck { session, n }
                if session == self.session && n == r.held_n && !r.held_acked =>
            {
                // Dated from the send time (the S10 one-way-cut fix).
                r.acked_at = r.acked_at.max(r.held_at);
                r.held_acked = true;
                r.pace.acked(now);
                // A slow ack (RTT > 100 ms) only slows the cadence: the
                // snapshot that waited for it leaves now.
                if now >= r.next_snapshot {
                    self.send_snapshot(now, out);
                }
            }
            Control::Ended { session, reason } if session == self.session => {
                self.end_remote(LeaveReason::from_code(reason), false, ReleaseAt::Exit, out);
            }
            _ => {}
        }
    }

    // ---- endings -------------------------------------------------------

    /// End whatever crossing is active for `reason`: Remote sends `Leave`
    /// and releases at the exit; Pending and Arming release inside.
    fn end_any(&mut self, reason: LeaveReason, now: Micros, out: &mut Actions<SourceAction>) {
        match self.crossing {
            Crossing::Local => {}
            Crossing::Pending(_) | Crossing::Arming(_) => self.abort(true, now, out),
            Crossing::Remote(_) => self.end_remote(reason, true, ReleaseAt::Exit, out),
        }
    }

    /// The portal ended capture (`Deactivated`, `StopEmulating`,
    /// `Disabled`, `ZonesChanged`): `Leave(capture_ended)`, no `Release`.
    fn capture_ended(&mut self, now: Micros, out: &mut Actions<SourceAction>) {
        self.refuse_activation_until = None;
        match self.crossing {
            Crossing::Local => {}
            Crossing::Pending(_) | Crossing::Arming(_) => self.abort(false, now, out),
            Crossing::Remote(_) => {
                self.end_remote(LeaveReason::CaptureEnded, true, ReleaseAt::Nothing, out);
            }
        }
    }

    /// Pending or Arming → Local, dropping the buffer; with `release`, a
    /// `Release` inside (Arming) or with no position (Pending). Pending has no
    /// activation id yet, and xdph 1.4.1 rejects a `Release` without one, so
    /// the capture goes on: its late `Activated` is refused instead.
    fn abort(&mut self, release: bool, now: Micros, out: &mut Actions<SourceAction>) {
        match std::mem::replace(&mut self.crossing, Crossing::Local) {
            Crossing::Pending(p) => {
                self.recycle(p.buffered);
                if release {
                    self.release(None, None, out);
                    self.refuse_activation_until = Some(now.plus(REFUSE_LATE_ACTIVATION));
                }
            }
            Crossing::Arming(a) => {
                self.recycle(a.buffered);
                if release {
                    let pos = self.to_local(a.activation.inside());
                    self.release(Some(a.activation.id), Some(pos), out);
                }
            }
            other => self.crossing = other,
        }
    }

    fn end_remote(
        &mut self,
        why: LeaveReason,
        send_leave: bool,
        at: ReleaseAt,
        out: &mut Actions<SourceAction>,
    ) {
        let Crossing::Remote(r) = std::mem::replace(&mut self.crossing, Crossing::Local) else {
            return;
        };
        if send_leave {
            self.send(
                r.peer,
                Control::Leave {
                    session: self.session,
                    reason: why.code(),
                },
                out,
            );
        }
        self.last_leave = Some(why);
        let pos = match at {
            ReleaseAt::Nothing => None,
            ReleaseAt::Exit => Some(r.activation.inside()),
            ReleaseAt::Point(p) => Some(p),
        };
        if let Some(p) = pos {
            let p = self.to_local(p);
            self.release(Some(r.activation.id), Some(p), out);
        }
        self.held.clear();
        self.filter.reset();
    }

    fn pending_timeout(&self) -> Micros {
        self.config.lock_check_budget.pending_timeout()
    }

    fn release(&mut self, id: Option<u32>, pos: Option<Point>, out: &mut Actions<SourceAction>) {
        if let Some(id) = id {
            self.released.rotate_left(1);
            self.released[3] = Some(id);
        }
        out.push(SourceAction::Portal(PortalCmd::Release {
            activation_id: id,
            pos,
        }));
    }

    /// `Topology` while a crossing is active: Arming bounces (its link may
    /// be gone); Remote clamps the model onto the target's nearest monitor,
    /// or leaves when the target has none.
    fn topology_changed(&mut self, now: Micros, out: &mut Actions<SourceAction>) {
        match &mut self.crossing {
            Crossing::Arming(_) => {
                self.counters.bounces += 1;
                self.abort(true, now, out);
            }
            Crossing::Remote(r) => {
                let Some(topo) = self.topo.as_ref() else {
                    return;
                };
                let nearest = topo
                    .monitors
                    .iter()
                    .filter(|m| m.id.peer == r.peer)
                    .map(|m| m.rect.clamp(r.model))
                    .min_by(|a, b| {
                        dist2(*a, r.model)
                            .partial_cmp(&dist2(*b, r.model))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                match nearest {
                    Some(p) => r.model = p,
                    None => self.end_remote(LeaveReason::Layout, true, ReleaseAt::Exit, out),
                }
            }
            Crossing::Local | Crossing::Pending(_) => {}
        }
    }

    // ---- output helpers ------------------------------------------------

    fn send(&mut self, peer: PeerId, msg: Control, out: &mut Actions<SourceAction>) {
        self.counters.frames_sent += 1;
        out.push(SourceAction::Send { peer, msg });
    }

    fn send_current(&mut self, msg: Control, out: &mut Actions<SourceAction>) {
        if let Crossing::Remote(r) = &self.crossing {
            let peer = r.peer;
            self.send(peer, msg, out);
        }
    }

    fn send_keymap(
        &mut self,
        peer: PeerId,
        serial: u32,
        text: &str,
        out: &mut Actions<SourceAction>,
    ) {
        self.counters.keymaps_sent += 1;
        self.send(
            peer,
            Control::Keymap {
                serial,
                format: 1,
                text: text.to_owned(),
            },
            out,
        );
        match self.keymap_sent.iter_mut().find(|(q, _)| *q == peer) {
            Some(slot) => slot.1 = serial,
            None => self.keymap_sent.push((peer, serial)),
        }
    }

    fn send_snapshot(&mut self, now: Micros, out: &mut Actions<SourceAction>) {
        let pos = self.pos();
        let Crossing::Remote(r) = &mut self.crossing else {
            return;
        };
        r.held_n = r.held_n.wrapping_add(1);
        let n = r.held_n;
        r.held_at = now;
        r.held_acked = false;
        r.next_snapshot = r.next_snapshot.plus(SNAPSHOT_PERIOD);
        if r.next_snapshot <= now {
            r.next_snapshot = now.plus(SNAPSHOT_PERIOD);
        }
        let peer = r.peer;
        let msg = Control::Held {
            session: self.session,
            n,
            keys: self.held.keys().to_vec(),
            buttons: self.held.buttons().to_vec(),
            pos,
        };
        self.send(peer, msg, out);
    }

    fn send_motion(&mut self, dx: f64, dy: f64, now: Micros, out: &mut Actions<SourceAction>) {
        let Crossing::Remote(r) = &self.crossing else {
            return;
        };
        let peer = r.peer;
        let Some(p) = self.target_local(r.model, peer) else {
            return;
        };
        self.motion_seq = self.motion_seq.wrapping_add(1);
        self.counters.datagrams_sent += 1;
        out.push(SourceAction::Datagram {
            peer,
            motion: Motion {
                session_lo: self.session as u16,
                seq: self.motion_seq,
                x: p.x as f32,
                y: p.y as f32,
                dx: wire_delta(dx),
                dy: wire_delta(dy),
                t_us: now.low32(),
            },
        });
    }

    /// The position fence for an input frame: the newest motion seq sent
    /// and the model, target-local.
    fn pos(&self) -> Pos {
        let p = match &self.crossing {
            Crossing::Remote(r) => self.target_local(r.model, r.peer),
            _ => None,
        }
        .unwrap_or_default();
        Pos {
            motion_seq: self.motion_seq,
            x: p.x as f32,
            y: p.y as f32,
        }
    }

    fn target_local(&self, p: Point, peer: PeerId) -> Option<Point> {
        self.topo.as_ref()?.to_peer_local(peer, p)
    }

    /// Plane → this peer's local logical coordinates (what `Release` takes).
    fn to_local(&self, p: Point) -> Point {
        match &self.topo {
            Some(t) => t.to_local(p),
            None => p,
        }
    }

    fn take_buffer(&mut self) -> Vec<SourceEvent> {
        let mut v = std::mem::take(&mut self.spare);
        v.clear();
        v
    }

    fn recycle(&mut self, mut v: Vec<SourceEvent>) {
        if v.capacity() >= self.spare.capacity() {
            v.clear();
            self.spare = v;
        }
    }
}

/// The middle of a link, on its line (plane).
fn link_mid(l: &EdgeLink) -> Point {
    l.side.point(
        (f64::from(l.span.0) + f64::from(l.span.1)) / 2.0,
        f64::from(l.line),
    )
}

/// How far `along` is from a link's span: 0 inside it.
fn span_dist(l: &EdgeLink, along: f64) -> f64 {
    (f64::from(l.span.0) - along)
        .max(along - f64::from(l.span.1 - 1))
        .max(0.0)
}

fn dist2(a: Point, b: Point) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2)
}

/// A button release in the buffer whose press is not earlier in it: the
/// press happened before capture, so a local drag is crossing.
fn orphan_button_release(buffered: &[SourceEvent]) -> bool {
    buffered.iter().enumerate().any(|(i, ev)| match ev {
        SourceEvent::Button {
            code,
            pressed: false,
        } => !buffered[..i]
            .iter()
            .any(|e| matches!(e, SourceEvent::Button { code: c, pressed: true } if c == code)),
        _ => false,
    })
}

/// A delta for the wire's `f32`. Every delta that enters is finite, but the
/// pacer adds them up in `f64`, and `as` turns a sum past `f32::MAX` into an
/// infinity (a proptest found -3.8e37 plus -3.0e38): saturate it instead.
fn wire_delta(v: f64) -> f32 {
    v.clamp(-f64::from(f32::MAX), f64::from(f32::MAX)) as f32
}

/// Move the model by `d` and decide what the motion does (§5.1 Remote
/// motion rows). Moving between the target's own monitors is free; leaving
/// a monitor is tested per side crossed, nearest first: a link to me is the
/// way home (resistance, dead corners, return block), a link to another up
/// peer is a hop, anything else is a wall that clamps that axis.
fn remote_step(
    r: &mut Remote,
    topo: &Topology,
    d: Point,
    dead: f64,
    resistance: f64,
    block_return: bool,
    peer_ok: impl Fn(PeerId) -> bool,
) -> Step {
    let old = r.model;
    let mut p = Point {
        x: old.x + d.x,
        y: old.y + d.y,
    };
    let on_target = |q: Point| {
        topo.monitors
            .iter()
            .any(|m| m.id.peer == r.peer && m.rect.contains(q))
    };
    if on_target(p) {
        r.model = p;
        r.return_push = 0.0;
        return Step::Moved { send: true };
    }
    let Some(cur) = topo
        .monitors
        .iter()
        .find(|m| m.id.peer == r.peer && m.rect.contains(old))
    else {
        // The model is always on a target monitor; if not, stay put.
        return Step::Moved { send: false };
    };
    let rect = cur.rect;
    // The sides crossed (at most two on a diagonal), nearest first.
    let mut crossed: [Option<(f64, Side)>; 2] = [None, None];
    for (slot, sides) in crossed
        .iter_mut()
        .zip([[Side::Left, Side::Right], [Side::Top, Side::Bottom]])
    {
        for side in sides {
            if side.beyond(&rect, p) {
                let from = side.across(old);
                let to = side.across(p);
                let line = f64::from(side.line(&rect));
                let t = if to == from {
                    0.0
                } else {
                    (line - from) / (to - from)
                };
                *slot = Some((t.clamp(0.0, 1.0), side));
            }
        }
    }
    if let [Some(a), Some(b)] = crossed
        && b.0 < a.0
    {
        crossed = [Some(b), Some(a)];
    }
    let mut pushed_home = false;
    for (_, side) in crossed.into_iter().flatten() {
        // Where the (possibly wall-clamped) motion crosses this side's line.
        let line = f64::from(side.line(&rect));
        let (from, to) = (side.across(old), side.across(p));
        let t = if to == from {
            0.0
        } else {
            ((line - from) / (to - from)).clamp(0.0, 1.0)
        };
        let along = side.along(old) + t * (side.along(p) - side.along(old));
        let link = topo.links.iter().find(|l| {
            l.from == cur.id
                && l.side == side
                && along >= f64::from(l.span.0) + dead
                && along < f64::from(l.span.1) - dead
        });
        match link {
            Some(l) if l.to.peer == topo.me && !block_return => {
                pushed_home = true;
                r.return_push += side.outward(d.x, d.y).max(0.0);
                if r.return_push >= resistance
                    && let Some(home) = topo.monitor(l.to)
                {
                    let carried = side.point(along, side.across(p));
                    return Step::Home {
                        continuation: geom::inside(
                            &home.rect,
                            side.opposite(),
                            carried,
                            RELEASE_INSET,
                        ),
                    };
                }
            }
            Some(l) if l.to.peer != topo.me && l.to.peer != r.peer && peer_ok(l.to.peer) => {
                return Step::Hop {
                    link: *l,
                    contact: side.point(along, line),
                };
            }
            _ => {}
        }
        // A wall (or a blocked or not yet pushed link): clamp this axis.
        let inner = match side {
            Side::Left | Side::Top => line,
            Side::Right | Side::Bottom => line - 1.0,
        };
        p = side.point(side.along(p), inner);
    }
    if !pushed_home {
        r.return_push = 0.0;
    }
    let clamped = rect.clamp(p);
    let moved = clamped != old;
    r.model = clamped;
    Step::Moved { send: moved }
}

#[cfg(test)]
mod tests;
