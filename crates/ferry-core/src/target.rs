//! The target side of a crossing: session bookkeeping,
//! the held set, the snapshot watchdog and the position fence, as one pure
//! machine.
//!
//! The `net` thread owns it, so `HeldAck` leaves without a channel hop; its
//! [`InjectCmd`]s go to ferry-inject's thread over ferry-inject's channel
//! (the one hop of the target path).
//!
//! **Both peers capture**: [`TargetAction::Controlled`]
//! brackets every stretch of sessions, so the peer's own capture is off while
//! another peer drives it, and an `Enter` that meets this peer's own busy
//! source is refused ([`TargetEvent::SourceBusy`]).
//!
//! **Takeover**: while entered, and while ferryd can read Hyprland's
//! cursor ([`TargetEvent::CursorPoll`]), the machine asks for the cursor every
//! [`CURSOR_POLL`]. A read is *suspect* when the cursor is more than 2 px from
//! the last injected position, no motion was injected for 30 ms and no key or
//! button for 250 ms. A suspect read is read again [`CURSOR_RECHECK`] later;
//! two suspect reads in a row whose positions differ by at least 1 px mean a
//! local hand moves the pointer: `ReleaseAll`, `Ended{takeover}`. A warp
//! lands once and stays still, so it never counts. The `Enter` itself counts as an injected key:
//! on a peer that captures, the session's first input re-creates the virtual devices, and
//! its first motions can land tens of ms late (lab VMs, 2026-10-18).

use crate::actions::Actions;
use crate::held::{Edge, HeldSet};
use crate::peer::PeerId;
use crate::time::{CURSOR_POLL, CURSOR_RECHECK, Micros, TAKEOVER_KEY_QUIET, TAKEOVER_MOTION_QUIET};
use crate::wire::datagram::seq_newer;
use crate::wire::{Control, Datagram, LeaveReason, Pos};

#[derive(Debug, Clone, PartialEq)]
pub enum TargetEvent {
    /// ferryd started and ferry-inject is ready: run the modifier reset.
    Start,
    PeerUp(PeerId),
    PeerDown(PeerId),
    /// A source → target frame (`Keymap`, `Enter`, `Leave`, `Key`, `Button`,
    /// `Scroll`, `Held`).
    Control {
        peer: PeerId,
        msg: Control,
    },
    /// A `Motion` datagram (`Ping`/`Pong` never reach the machine).
    Datagram {
        peer: PeerId,
        dgram: Datagram,
    },
    /// ferry-inject reported a ring overflow and rebuilt its devices: it has
    /// already released everything.
    InjectReset,
    /// Whether this peer takes sessions at all. `false` (no injection
    /// backend) refuses every `Enter` with `Ended{refused}`, and ends a
    /// running session the same way. Lock to this machine is not this: a
    /// locked peer may still be entered.
    AcceptEnters(bool),
    /// This peer's own source is Pending, Arming or Remote (`true`), or
    /// Local again (`false`). While busy, an `Enter` is refused.
    SourceBusy(bool),
    /// ferryd can (`true`) or cannot read Hyprland's cursor: the takeover
    /// poll runs only while it can. Off at start.
    CursorPoll(bool),
    /// The answer to [`TargetAction::CheckCursor`]: the cursor in this peer's
    /// local logical coordinates, or `None` when the read failed.
    CursorPolled(Option<(f64, f64)>),
    /// `ferry release`.
    ForceRelease,
    Shutdown,
    Tick,
}

/// What the inject thread does. Mapped 1:1 onto `ferry_inject::InputCmd`
/// by ferryd.
#[derive(Debug, Clone, PartialEq)]
pub enum InjectCmd {
    /// Release the eight modifier keycodes, then `modifiers(0,0,0,0)`.
    ModifierReset,
    /// Target-local logical position (unbound pointer, pixel-centre aiming
    /// inside ferry-inject).
    Motion {
        x: f64,
        y: f64,
    },
    Key {
        code: u16,
        pressed: bool,
    },
    Button {
        code: u16,
        pressed: bool,
    },
    Scroll {
        axis: u8,
        source: u8,
        value: f64,
        value120: Option<i32>,
    },
    ScrollStop {
        axis: u8,
        source: u8,
    },
    /// A new virtual keyboard with this keymap, held keys replayed.
    SetKeymap(String),
    /// Every held key and button up, `modifiers(0,0,0,0)`, `frame`.
    ReleaseAll,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TargetAction {
    Inject(InjectCmd),
    /// Encode and `try_send_frame` (`HeldAck`, `Ended`).
    Send {
        peer: PeerId,
        msg: Control,
    },
    Status(TargetStatus),
    /// `true` when an `Enter` starts control from idle; `false` when the
    /// session ends (after its `ReleaseAll` and `Ended`). ferryd drops the
    /// virtual devices on a `false` and only then turns its own capture back
    /// on. A preemption by another source keeps
    /// control and emits neither.
    Controlled(bool),
    /// Read Hyprland's cursor now and answer [`TargetEvent::CursorPolled`].
    CheckCursor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetStatus {
    pub entered_by: Option<PeerId>,
    /// The `Enter` point of the session in progress, local logical (the edge glow).
    pub entry: Option<(f64, f64)>,
    pub session: u32,
    pub last_end: Option<LeaveReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetCounters {
    pub enters: u64,
    pub watchdog_releases: u64,
    pub stale_datagrams: u64,
    pub stale_frames: u64,
    pub orphan_releases: u64,
    pub snapshot_releases: u64,
    pub keymap_swaps: u64,
    pub refused: u64,
    /// Cursor reads that were suspect (each starts a recheck).
    pub takeover_suspects: u64,
    /// Sessions ended because a local hand moved the pointer.
    pub takeovers: u64,
    /// Peer positions or scroll values that were not finite: dropped (an
    /// `Enter` is refused) before they reach ferry-inject.
    pub nonfinite_dropped: u64,
}

#[derive(Debug)]
pub struct TargetMachine {
    state: State,
    held: HeldSet,
    /// Latest keymap per connected source peer: (peer, serial, text).
    keymaps: Vec<(PeerId, u32, String)>,
    /// The keymap the virtual keyboard has now, by (peer, serial).
    applied: Option<(PeerId, u32)>,
    /// Its text. A keymap whose text equals this one changes nothing on the
    /// keyboard, so it is neither re-sent to ferry-inject nor counted as a
    /// swap (first light counted 159 swaps for 31 crossings).
    applied_text: Option<String>,
    accept_enters: bool,
    source_busy: bool,
    cursor_poll: bool,
    /// Per peer: the last session already answered with `Ended`, so a stream
    /// of stale frames gets one answer.
    stale_answered: Vec<(PeerId, u32)>,
    /// The session of the last `Enter` accepted (for status).
    last_session: u32,
    last_end: Option<LeaveReason>,
    counters: TargetCounters,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Idle,
    Entered(Entered),
}

#[derive(Debug, Clone, PartialEq)]
struct Entered {
    peer: PeerId,
    session: u32,
    /// The `Enter` point, local logical.
    entry: (f64, f64),
    last_snapshot: Micros,
    /// Newest motion seq injected (from a datagram or a position fence).
    motion_seq: u32,
    /// Takeover: the last position injected, and when.
    injected: (f64, f64),
    motion_at: Micros,
    /// When a key or button edge was last injected.
    key_at: Option<Micros>,
    next_poll: Micros,
    /// A suspect read is pending its recheck at this time.
    recheck_at: Option<Micros>,
    /// The position of the last suspect read.
    suspect: Option<(f64, f64)>,
}

impl Entered {
    fn injected_motion(&mut self, x: f64, y: f64, now: Micros) {
        self.injected = (x, y);
        self.motion_at = now;
    }
}

impl TargetMachine {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            held: HeldSet::default(),
            keymaps: Vec::new(),
            applied: None,
            applied_text: None,
            accept_enters: true,
            source_busy: false,
            cursor_poll: false,
            stale_answered: Vec::new(),
            last_session: 0,
            last_end: None,
            counters: TargetCounters::default(),
        }
    }

    pub fn handle(&mut self, ev: TargetEvent, now: Micros, out: &mut Actions<TargetAction>) {
        let before = self.status();
        // A watchdog that is due fires first, exactly as if the timer had
        // been on time: nothing is ever applied past the deadline.
        self.expire(now, out);
        match ev {
            TargetEvent::Start => {
                if self.entered().is_some() {
                    self.release_all(out);
                    self.go_idle(out);
                }
                self.held.clear();
                out.push(TargetAction::Inject(InjectCmd::ModifierReset));
            }
            TargetEvent::PeerUp(p) => self.stale_answered.retain(|(q, _)| *q != p),
            TargetEvent::PeerDown(p) => {
                if self.entered().is_some_and(|e| e.peer == p) {
                    self.release_all(out);
                    // No `Ended`: the link it would go on is gone.
                    self.last_end = Some(LeaveReason::LinkLost);
                    self.go_idle(out);
                }
                self.keymaps.retain(|(q, _, _)| *q != p);
                if self.applied.is_some_and(|(q, _)| q == p) {
                    // A restarted source reuses serials for other text.
                    self.applied = None;
                }
                self.stale_answered.retain(|(q, _)| *q != p);
            }
            TargetEvent::Control { peer, msg } => self.control(peer, msg, now, out),
            TargetEvent::Datagram { peer, dgram } => self.datagram(peer, dgram, now, out),
            TargetEvent::InjectReset => self.held.clear(),
            TargetEvent::AcceptEnters(on) => {
                self.accept_enters = on;
                if !on && self.entered().is_some() {
                    self.release_all(out);
                    self.end(LeaveReason::Refused, out);
                }
            }
            TargetEvent::SourceBusy(busy) => self.source_busy = busy,
            TargetEvent::CursorPoll(on) => {
                self.cursor_poll = on;
                if let State::Entered(e) = &mut self.state {
                    e.next_poll = now.plus(CURSOR_POLL);
                    e.recheck_at = None;
                    e.suspect = None;
                }
            }
            TargetEvent::CursorPolled(pos) => self.cursor_polled(pos, now, out),
            TargetEvent::ForceRelease => {
                self.release_all(out);
                self.end(LeaveReason::Forced, out);
            }
            TargetEvent::Shutdown => {
                self.release_all(out);
                self.end(LeaveReason::Shutdown, out);
            }
            TargetEvent::Tick => {}
        }
        self.poll(now, out);
        let after = self.status();
        if after != before {
            out.push(TargetAction::Status(after));
        }
    }

    /// The earliest of the watchdog (last snapshot + 650 ms) and, while the
    /// takeover poll runs, the next cursor read. `None` while idle.
    pub fn next_deadline(&self) -> Option<Micros> {
        let e = self.entered()?;
        let watchdog = self.watchdog_deadline()?;
        if !self.cursor_poll {
            return Some(watchdog);
        }
        let poll = e.recheck_at.map_or(e.next_poll, |r| r.min(e.next_poll));
        Some(watchdog.min(poll))
    }

    /// The watchdog deadline while entered: last snapshot + 650 ms.
    pub fn watchdog_deadline(&self) -> Option<Micros> {
        self.entered()
            .map(|e| e.last_snapshot.plus(crate::time::TARGET_WATCHDOG))
    }

    pub fn status(&self) -> TargetStatus {
        TargetStatus {
            entered_by: self.entered().map(|e| e.peer),
            entry: self.entered().map(|e| e.entry),
            session: self.entered().map_or(self.last_session, |e| e.session),
            last_end: self.last_end,
        }
    }

    pub fn counters(&self) -> &TargetCounters {
        &self.counters
    }

    fn entered(&self) -> Option<&Entered> {
        match &self.state {
            State::Entered(e) => Some(e),
            State::Idle => None,
        }
    }

    /// The current session if `peer` runs `session`.
    fn current(&mut self, peer: PeerId, session: u32) -> Option<&mut Entered> {
        match &mut self.state {
            State::Entered(e) if e.peer == peer && e.session == session => Some(e),
            _ => None,
        }
    }

    fn expire(&mut self, now: Micros, out: &mut Actions<TargetAction>) {
        if self.watchdog_deadline().is_some_and(|d| now >= d) {
            self.counters.watchdog_releases += 1;
            self.release_all(out);
            self.end(LeaveReason::Watchdog, out);
        }
    }

    fn release_all(&mut self, out: &mut Actions<TargetAction>) {
        out.push(TargetAction::Inject(InjectCmd::ReleaseAll));
        self.held.clear();
    }

    /// If entered: answer `Ended{reason}` to the source and go idle. The
    /// caller has already released.
    fn end(&mut self, reason: LeaveReason, out: &mut Actions<TargetAction>) {
        if let State::Entered(e) = &self.state {
            let (peer, session) = (e.peer, e.session);
            self.send_ended(peer, session, reason, out);
            self.go_idle(out);
        }
    }

    /// Entered → Idle: control ends (`Controlled(false)`, after the release
    /// the caller already pushed).
    fn go_idle(&mut self, out: &mut Actions<TargetAction>) {
        if matches!(self.state, State::Entered(_)) {
            self.state = State::Idle;
            out.push(TargetAction::Controlled(false));
        }
    }

    /// Emit a cursor read when one is due. Only while entered.
    fn poll(&mut self, now: Micros, out: &mut Actions<TargetAction>) {
        if !self.cursor_poll {
            return;
        }
        let State::Entered(e) = &mut self.state else {
            return;
        };
        let recheck = e.recheck_at.is_some_and(|t| now >= t);
        let regular = now >= e.next_poll;
        if !(recheck || regular) {
            return;
        }
        if recheck {
            e.recheck_at = None;
        }
        if regular {
            e.next_poll = now.plus(CURSOR_POLL);
        }
        out.push(TargetAction::CheckCursor);
    }

    /// A cursor read came back: takeover when this read and the previous one
    /// are both suspect and the cursor moved between them.
    fn cursor_polled(
        &mut self,
        pos: Option<(f64, f64)>,
        now: Micros,
        out: &mut Actions<TargetAction>,
    ) {
        let (Some((x, y)), true) = (pos, self.cursor_poll) else {
            return;
        };
        let State::Entered(e) = &mut self.state else {
            return;
        };
        let off = (x - e.injected.0).abs() > TAKEOVER_OFF_PX
            || (y - e.injected.1).abs() > TAKEOVER_OFF_PX;
        let quiet = now.since(e.motion_at) >= TAKEOVER_MOTION_QUIET
            && e.key_at.is_none_or(|k| now.since(k) >= TAKEOVER_KEY_QUIET);
        if !(off && quiet) {
            e.suspect = None;
            e.recheck_at = None;
            return;
        }
        match e.suspect {
            Some((sx, sy))
                if (x - sx).abs() >= TAKEOVER_MOVE_PX || (y - sy).abs() >= TAKEOVER_MOVE_PX =>
            {
                self.counters.takeovers += 1;
                self.release_all(out);
                self.end(LeaveReason::Takeover, out);
            }
            prev => {
                if prev.is_none() {
                    self.counters.takeover_suspects += 1;
                }
                e.suspect = Some((x, y));
                e.recheck_at = Some(now.plus(CURSOR_RECHECK));
            }
        }
    }

    fn send_ended(
        &mut self,
        peer: PeerId,
        session: u32,
        reason: LeaveReason,
        out: &mut Actions<TargetAction>,
    ) {
        out.push(TargetAction::Send {
            peer,
            msg: Control::Ended {
                session,
                reason: reason.code(),
            },
        });
        match self.stale_answered.iter_mut().find(|(q, _)| *q == peer) {
            Some(slot) => slot.1 = session,
            None => self.stale_answered.push((peer, session)),
        }
        self.last_end = Some(reason);
    }

    /// An input frame for a session this target does not run: answer once
    /// per (peer, session) with `Ended(stale)`.
    fn stale(&mut self, peer: PeerId, session: u32, out: &mut Actions<TargetAction>) {
        self.counters.stale_frames += 1;
        if !self.stale_answered.contains(&(peer, session)) {
            self.send_ended(peer, session, LeaveReason::Stale, out);
        }
    }

    /// `fence(pos)`: a frame's position is newer than the last motion
    /// injected, so inject it first and adopt its seq. A position that is
    /// not finite is dropped (the caller counts it, on `false`) and the rest
    /// of the frame still applies: a NaN warps ferry-inject's cursor to a
    /// corner and blinds the takeover check (A-02).
    #[must_use]
    fn fence(e: &mut Entered, pos: Pos, now: Micros, out: &mut Actions<TargetAction>) -> bool {
        if !(pos.x.is_finite() && pos.y.is_finite()) {
            return false;
        }
        if seq_newer(pos.motion_seq, e.motion_seq) {
            let (x, y) = (f64::from(pos.x), f64::from(pos.y));
            out.push(TargetAction::Inject(InjectCmd::Motion { x, y }));
            e.injected_motion(x, y, now);
            e.motion_seq = pos.motion_seq;
        }
        true
    }

    /// A key or button edge was injected (the takeover poll's warp guard).
    fn key_injected(&mut self, now: Micros) {
        if let State::Entered(e) = &mut self.state {
            e.key_at = Some(now);
        }
    }

    fn edge(
        &mut self,
        button: bool,
        code: u16,
        pressed: bool,
        now: Micros,
        out: &mut Actions<TargetAction>,
    ) {
        let edge = if button {
            self.held.button(code, pressed)
        } else {
            self.held.key(code, pressed)
        };
        match edge {
            Edge::Changed => {
                out.push(TargetAction::Inject(if button {
                    InjectCmd::Button { code, pressed }
                } else {
                    InjectCmd::Key { code, pressed }
                }));
                self.key_injected(now);
            }
            Edge::NotDown => self.counters.orphan_releases += 1,
            Edge::AlreadyDown => {}
        }
    }

    fn control(&mut self, p: PeerId, msg: Control, now: Micros, out: &mut Actions<TargetAction>) {
        match msg {
            Control::Keymap {
                serial,
                format,
                text,
            } => {
                if format != 1 {
                    return;
                }
                let apply = match self.entered() {
                    None => true,
                    Some(e) => e.peer == p,
                };
                if apply && self.applied != Some((p, serial)) {
                    self.apply_keymap(p, serial, &text, out);
                }
                match self.keymaps.iter_mut().find(|(q, _, _)| *q == p) {
                    Some(slot) => *slot = (p, serial, text),
                    None => self.keymaps.push((p, serial, text)),
                }
            }
            Control::Enter {
                session,
                x,
                y,
                keymap_serial,
                motion_seq,
            } => self.enter(p, session, (x, y), keymap_serial, motion_seq, now, out),
            Control::Leave { session, reason } => {
                if self.current(p, session).is_some() {
                    self.release_all(out);
                    self.last_end = Some(LeaveReason::from_code(reason));
                    self.go_idle(out);
                }
            }
            Control::Key {
                session,
                code,
                pressed,
            } => {
                if self.current(p, session).is_some() {
                    self.edge(false, code, pressed, now, out);
                } else {
                    self.stale(p, session, out);
                }
            }
            Control::Button {
                session,
                code,
                pressed,
                pos,
            } => match self.current(p, session) {
                Some(e) => {
                    if !Self::fence(e, pos, now, out) {
                        self.counters.nonfinite_dropped += 1;
                    }
                    self.edge(true, code, pressed, now, out);
                }
                None => self.stale(p, session, out),
            },
            Control::Scroll {
                session,
                axis,
                source,
                value,
                value120,
                stop,
                pos,
            } => match self.current(p, session) {
                Some(e) => {
                    let fenced = Self::fence(e, pos, now, out);
                    let finite = stop || value.is_finite();
                    if !(fenced && finite) {
                        self.counters.nonfinite_dropped += 1;
                    }
                    if finite {
                        out.push(TargetAction::Inject(if stop {
                            InjectCmd::ScrollStop { axis, source }
                        } else {
                            InjectCmd::Scroll {
                                axis,
                                source,
                                value: f64::from(value),
                                value120,
                            }
                        }));
                    }
                }
                None => self.stale(p, session, out),
            },
            Control::Held {
                session,
                n,
                keys,
                buttons,
                pos,
            } => {
                let Some(e) = self.current(p, session) else {
                    self.stale(p, session, out);
                    return;
                };
                e.last_snapshot = now;
                let mut released = 0u64;
                self.held.reconcile(&keys, &buttons, |button, code| {
                    released += 1;
                    out.push(TargetAction::Inject(if button {
                        InjectCmd::Button {
                            code,
                            pressed: false,
                        }
                    } else {
                        InjectCmd::Key {
                            code,
                            pressed: false,
                        }
                    }));
                });
                self.counters.snapshot_releases += released;
                if released > 0 {
                    self.key_injected(now);
                }
                if let Some(e) = self.current(p, session)
                    && !Self::fence(e, pos, now, out)
                {
                    self.counters.nonfinite_dropped += 1;
                }
                out.push(TargetAction::Send {
                    peer: p,
                    msg: Control::HeldAck { session, n },
                });
            }
            // Wrong direction or not the machine's business: dropped.
            Control::Hello { .. }
            | Control::Error { .. }
            | Control::Monitors { .. }
            | Control::Layout { .. }
            | Control::LayoutAck { .. }
            | Control::HeldAck { .. }
            | Control::Ended { .. }
            | Control::PeerState { .. }
            | Control::ClipOffer { .. }
            | Control::ClipRequest { .. }
            | Control::ClipAbort { .. } => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of one Enter frame, `now` and `out`; a struct would only rename \
                  the frame's own fields at the one call site"
    )]
    fn enter(
        &mut self,
        p: PeerId,
        session: u32,
        (x, y): (f32, f32),
        keymap_serial: u32,
        motion_seq: u32,
        now: Micros,
        out: &mut Actions<TargetAction>,
    ) {
        let keymap = self
            .keymaps
            .iter()
            .position(|(q, s, _)| *q == p && *s == keymap_serial);
        // A non-finite entry point is refused like a missing keymap (A-02).
        let finite = x.is_finite() && y.is_finite();
        if !finite {
            self.counters.nonfinite_dropped += 1;
        }
        let Some(keymap) = keymap.filter(|_| finite && self.accept_enters && !self.source_busy)
        else {
            // Refused. If p was entered, its old session is over too: a
            // source runs one session at a time.
            if self.entered().is_some_and(|e| e.peer == p) {
                self.release_all(out);
                self.go_idle(out);
            }
            self.counters.refused += 1;
            self.send_ended(p, session, LeaveReason::Refused, out);
            return;
        };
        match self.entered().cloned() {
            Some(e) if e.peer != p => {
                self.release_all(out);
                self.send_ended(e.peer, e.session, LeaveReason::Preempted, out);
            }
            Some(_) => self.release_all(out),
            // Control starts: this peer's own capture goes off first.
            None => out.push(TargetAction::Controlled(true)),
        }
        if self.applied != Some((p, keymap_serial)) {
            let text = std::mem::take(&mut self.keymaps[keymap].2);
            self.apply_keymap(p, keymap_serial, &text, out);
            self.keymaps[keymap].2 = text;
        }
        let (x, y) = (f64::from(x), f64::from(y));
        out.push(TargetAction::Inject(InjectCmd::Motion { x, y }));
        self.held.clear();
        self.state = State::Entered(Entered {
            peer: p,
            session,
            entry: (x, y),
            last_snapshot: now,
            motion_seq,
            injected: (x, y),
            motion_at: now,
            // The devices may be created now: the first motions can lag (see the module doc).
            key_at: Some(now),
            next_poll: now.plus(CURSOR_POLL),
            recheck_at: None,
            suspect: None,
        });
        self.last_session = session;
        self.counters.enters += 1;
    }

    /// Make `(p, serial)` the applied keymap. Only a text that differs from
    /// the keyboard's current one goes to ferry-inject and counts as a swap.
    fn apply_keymap(
        &mut self,
        p: PeerId,
        serial: u32,
        text: &str,
        out: &mut Actions<TargetAction>,
    ) {
        self.applied = Some((p, serial));
        if self.applied_text.as_deref() != Some(text) {
            out.push(TargetAction::Inject(InjectCmd::SetKeymap(text.to_owned())));
            self.applied_text = Some(text.to_owned());
            self.counters.keymap_swaps += 1;
        }
    }

    fn datagram(
        &mut self,
        p: PeerId,
        dgram: Datagram,
        now: Micros,
        out: &mut Actions<TargetAction>,
    ) {
        let Datagram::Motion(m) = dgram else {
            return;
        };
        match &mut self.state {
            // Only `x, y` are injected; `dx, dy` are informational.
            _ if !(m.x.is_finite() && m.y.is_finite()) => self.counters.nonfinite_dropped += 1,
            State::Entered(e)
                if e.peer == p
                    && m.session_lo == e.session as u16
                    && seq_newer(m.seq, e.motion_seq) =>
            {
                let (x, y) = (f64::from(m.x), f64::from(m.y));
                out.push(TargetAction::Inject(InjectCmd::Motion { x, y }));
                e.injected_motion(x, y, now);
                e.motion_seq = m.seq;
            }
            _ => self.counters.stale_datagrams += 1,
        }
    }
}

/// A read further than this from the last injected position, on either
/// axis, is off.
const TAKEOVER_OFF_PX: f64 = 2.0;
/// Two suspect reads at least this far apart, on either axis, are a moving
/// hand.
const TAKEOVER_MOVE_PX: f64 = 1.0;

impl Default for TargetMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
