//! The `capture` thread: a calloop loop that owns the SourceMachine.
//!
//! | Source | What it feeds |
//! |---|---|
//! | the EIS fd ([`ferry_capture::EisSource`], one per portal session) | `SourceEvent`s from EIS |
//! | the [`CaptureCmd`] channel (from `ctl` and `net`) | portal signals, lock answers, peers, acks, connection handles, a new EIS fd |
//! | one deadline, the dispatch timeout | `Tick` at `SourceMachine::next_deadline()` |
//!
//! Every action leaves from here: motion as a 28-byte datagram straight onto the connection
//! (`send_datagram`, no hop), frames with `try_send_frame` (`Full` → `ForceRelease`, fail open),
//! both cut from one 4 KiB slab (architecture §3 step 3, `cut`),
//! and `PortalCmd` / `CheckLock` with `try_send` to the portal driver's own queue (a task on
//! `ctl`, but not `ctl`'s queue: nothing `net` sends can fill it, B-01).
//!
//! **Nothing that ends a capture is dropped** (RG-CONC-02 as amended). That queue is full only
//! when the driver is stuck. A message that does not fit is kept, in order with those after it,
//! and the loop tries again every [`DRIVER_RETRY`]; the first one also asks `ctl` for an IPC
//! release, which ends any capture whatever the driver is doing. Past
//! [`DRIVER_OWED_CAP`] only messages that cannot leave capture on are dropped and counted.
//!
//! **The timer.** The loop computes `next_deadline()` after every wake-up and dispatches with
//! the time left as its timeout, so there is one timer and it is always re-armed. When the
//! deadline is due, the loop still polls once with a zero timeout first: a pending EIS read is
//! drained before the `Tick`, so the repeat filter's 1 ms deadline never beats a key event that
//! has already arrived.
//!
//! With `input.capture = "off"` there is no SourceMachine: the loop takes only `Shutdown`.
//!
//! **Stall probes** (`probe.rs`). A suspect peer, or a crossing's target unheard for
//! 200 ms, gets a `Ping` datagram every 100 ms from here, on the same connection handles. The
//! prober's deadline joins the machine's in the dispatch timeout.
//!
//! **The heartbeat.** The dispatch timeout is never longer than the threads' wake period (1 s), and
//! the loop stamps `Beats::beat(Capture)` after every dispatch, so `ctl` can tell a hung loop from
//! an idle one (`watchdog.rs`).

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use calloop::channel::{Channel, Event, SyncSender, sync_channel};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use ferry_capture::{EisOutput, EisSource};
use ferry_core::config::SourceConfig;
use ferry_core::source::{
    PortalCmd, SourceAction, SourceEvent, SourceMachine, SourceRole, SourceStatus,
};
use ferry_core::wire::{DATAGRAM_LEN, Datagram, LeaveReason};
use ferry_core::{Actions, PeerId};
use ferry_net::{Connection, FrameError};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::chan::{CAPTURE_CAP, CaptureCmd, Crossed, CtlMsg};
use crate::clock;
use crate::error::StartError;
use crate::keymap::KeymapCanon;
use crate::live::SharedLive;
use crate::portal_driver::DriverMsg;
use crate::probe::Prober;
use crate::ratelimit::log_limited;
use crate::watchdog::Thread;

/// While messages for the portal driver wait for room in its queue, the loop tries again this
/// often.
pub const DRIVER_RETRY: Duration = Duration::from_millis(2);
/// Past this many waiting driver messages, only those that end a capture are kept.
pub const DRIVER_OWED_CAP: usize = 256;
/// Datagrams and frames are cut from a slab of this size: one allocation per 146 datagrams.
const SLAB: usize = 4096;
/// At the end of the loop (shutdown), how long the capture thread still tries to hand the
/// driver what it owes.
const DRIVER_EXIT_GRACE: Duration = Duration::from_secs(1);

pub struct CaptureHandle {
    pub tx: SyncSender<CaptureCmd>,
    pub join: std::thread::JoinHandle<()>,
}

/// Everything the loop's callbacks reach.
struct Capture {
    /// `None` with `input.capture = "off"`.
    machine: Option<SourceMachine>,
    out: Actions<SourceAction>,
    conns: HashMap<PeerId, Connection>,
    ctl: mpsc::Sender<CtlMsg>,
    /// The portal driver's queue: portal commands, lock checks, EIS ends, `SleepHandled`.
    driver: mpsc::Sender<DriverMsg>,
    /// What the driver's full queue has not taken yet, in order.
    owed: VecDeque<DriverMsg>,
    live: SharedLive,
    handle: LoopHandle<'static, Capture>,
    /// The current EIS source and its generation.
    eis: Option<(u64, RegistrationToken)>,
    /// Events the machine caused itself (`ForceRelease` after a full frame queue).
    pending: VecDeque<SourceEvent>,
    frame: Vec<u8>,
    /// What every datagram and frame is cut from (`cut`).
    slab: BytesMut,
    /// EIS keymaps are made canonical before the machine compares them (`keymap.rs`).
    canon: KeymapCanon,
    /// The role of the last published status: "crossing ended" is logged only when a crossing
    /// (Remote) ends, not for a bounce out of Pending or Arming (BL-009: the M1 exit read 16
    /// bounces as 16 crossings home, because every one logged the last crossing's reason).
    role: SourceRole,
    /// The link of the crossing in progress, for the `edge` event when it comes home.
    edge: Option<ferry_core::geom::EdgeLink>,
    /// Pings to a peer that has gone quiet while it matters.
    prober: Prober,
    stop: bool,
}

pub fn spawn(
    config: Option<SourceConfig>,
    ctl: mpsc::Sender<CtlMsg>,
    driver: mpsc::Sender<DriverMsg>,
    live: SharedLive,
) -> Result<CaptureHandle, StartError> {
    let (tx, rx): (SyncSender<CaptureCmd>, Channel<CaptureCmd>) = sync_channel(CAPTURE_CAP);
    let join = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || run(config, rx, ctl, driver, live))
        .map_err(|source| StartError::Thread {
            name: "capture",
            source,
        })?;
    Ok(CaptureHandle { tx, join })
}

fn run(
    config: Option<SourceConfig>,
    rx: Channel<CaptureCmd>,
    ctl: mpsc::Sender<CtlMsg>,
    driver: mpsc::Sender<DriverMsg>,
    live: SharedLive,
) {
    let mut event_loop: EventLoop<'static, Capture> = match EventLoop::try_new() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot create the capture loop");
            return;
        }
    };
    let mut state = Capture {
        machine: config.map(SourceMachine::new),
        out: Actions::new(),
        conns: HashMap::new(),
        ctl,
        driver,
        owed: VecDeque::new(),
        live,
        handle: event_loop.handle(),
        eis: None,
        pending: VecDeque::new(),
        frame: Vec::with_capacity(256),
        slab: BytesMut::with_capacity(SLAB),
        canon: KeymapCanon::new(),
        role: SourceRole::Local,
        edge: None,
        prober: Prober::new(),
        stop: false,
    };
    if let Some(m) = &state.machine {
        state.live.source_status(m.status());
    }
    let inserted = event_loop
        .handle()
        .insert_source(rx, |event, (), state: &mut Capture| match event {
            Event::Msg(cmd) => state.command(cmd),
            Event::Closed => state.stop = true,
        });
    if let Err(e) = inserted {
        tracing::error!(error = %e, "cannot watch the capture channel");
        return;
    }

    let beats = state.live.beats().clone();
    let period = beats.thread_period();
    while !state.stop {
        // The one timer: dispatch until the machine's next deadline, or the watchdog's period if
        // that is sooner (rest design §6.2: the thread wakes at least once a period and says so).
        // A due deadline still polls once (zero timeout), so a pending EIS read is drained before
        // the Tick.
        let machine_deadline = state
            .machine
            .as_ref()
            .and_then(SourceMachine::next_deadline);
        let deadline = match (machine_deadline, state.prober.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut timeout = deadline.map(clock::until).map_or(period, |t| t.min(period));
        if !state.owed.is_empty() {
            timeout = timeout.min(DRIVER_RETRY);
        }
        if let Err(e) = event_loop.dispatch(Some(timeout), &mut state) {
            tracing::error!(error = %e, "the capture loop failed");
            break;
        }
        beats.beat(Thread::Capture);
        beats.maybe_stall(Thread::Capture);
        if state.stop {
            break;
        }
        let due = state
            .machine
            .as_ref()
            .and_then(SourceMachine::next_deadline)
            .is_some_and(|d| d <= clock::now());
        if due {
            state.feed(SourceEvent::Tick);
        }
        state.probe();
        state.flush_driver();
        state.publish_counters();
    }
    // The shutdown's Release (and whatever else is owed) still goes to the driver, which runs
    // on ctl while ctl waits for this thread.
    let until = std::time::Instant::now() + DRIVER_EXIT_GRACE;
    state.flush_driver();
    while !state.owed.is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(DRIVER_RETRY);
        state.flush_driver();
    }
    if !state.owed.is_empty() {
        tracing::error!(
            owed = state.owed.len(),
            "the portal driver took nothing for 1 s at shutdown"
        );
    }
    state.publish_counters();
}

impl Capture {
    fn command(&mut self, cmd: CaptureCmd) {
        match cmd {
            CaptureCmd::Event(ev) => {
                self.heard(&ev);
                let sleep = matches!(ev, SourceEvent::PrepareForSleep { start: true });
                self.feed(ev);
                if sleep {
                    // The release (if any) is already queued to the driver, ahead of this.
                    self.for_driver(DriverMsg::SleepHandled);
                }
            }
            CaptureCmd::Conn { peer, conn } => match conn {
                Some(c) => {
                    self.conns.insert(peer, c);
                    self.prober.connected(peer);
                }
                None => {
                    self.conns.remove(&peer);
                    self.prober.forget(peer);
                }
            },
            CaptureCmd::Eis { generation, fd } => self.new_eis(generation, fd),
            CaptureCmd::Shutdown => {
                // Design §9 shutdown step 1: Leave(shutdown) + Release while remote.
                self.feed(SourceEvent::Shutdown);
                if let Some((_, token)) = self.eis.take() {
                    self.handle.remove(token);
                }
                self.stop = true;
            }
        }
    }

    /// One portal session's EIS socket replaces the previous one (design §9: each EIS fd
    /// carries a generation, so ctl ignores a stale end).
    fn new_eis(&mut self, generation: u64, fd: std::os::fd::OwnedFd) {
        if let Some((old, token)) = self.eis.take() {
            tracing::debug!(old, generation, "replacing the EIS source");
            self.handle.remove(token);
        }
        if self.machine.is_none() {
            return;
        }
        let source = match EisSource::new(fd) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, generation, "cannot use the EIS socket");
                self.for_driver(DriverMsg::EisEnded {
                    generation,
                    end: ferry_capture::EisEnd::Error(e.to_string()),
                });
                return;
            }
        };
        let inserted = self
            .handle
            .insert_source(source, move |out, (), state: &mut Capture| match out {
                EisOutput::Event(ev) => state.feed(ev),
                EisOutput::KeyboardLost => state.keyboard_lost(),
                EisOutput::Ended(end) => {
                    tracing::info!(generation, ?end, "EIS ended");
                    if state.eis.is_some_and(|(g, _)| g == generation) {
                        state.eis = None;
                    }
                    state.for_driver(DriverMsg::EisEnded { generation, end });
                }
            });
        match inserted {
            Ok(token) => {
                tracing::info!(generation, "EIS source ready");
                self.eis = Some((generation, token));
            }
            Err(e) => {
                tracing::error!(error = %e.error, generation, "cannot watch the EIS socket");
                self.for_driver(DriverMsg::EisEnded {
                    generation,
                    end: ferry_capture::EisEnd::Error(e.error.to_string()),
                });
            }
        }
    }

    /// Hyprland re-created the EIS keyboard during a capture and will not start it before the
    /// next activation: no key would reach the target for the rest of this crossing, and a
    /// modifier already forwarded would stay down there. Fail open: end the crossing now
    /// (`Leave` + `Release` at the exit). The next crossing starts the new keyboard.
    fn keyboard_lost(&mut self) {
        let active = self
            .machine
            .as_ref()
            .is_some_and(|m| m.status().role != SourceRole::Local);
        self.live.bump("eis_keyboard_lost");
        if active {
            tracing::warn!("Hyprland re-created the EIS keyboard during capture: releasing");
            self.feed(SourceEvent::ForceRelease);
        } else {
            tracing::debug!("EIS keyboard re-created while local");
        }
    }

    /// Feed one event, then carry out its actions, then any events those caused.
    fn feed(&mut self, ev: SourceEvent) {
        match ev {
            SourceEvent::Keymap { text } => self.keymap(text),
            ev => self.handle(ev),
        }
    }

    fn handle(&mut self, ev: SourceEvent) {
        let Some(machine) = self.machine.as_mut() else {
            return;
        };
        let now = clock::now();
        if let SourceEvent::Key {
            code,
            pressed,
            t_eis,
        } = &ev
        {
            tracing::trace!(
                code,
                pressed,
                t_eis = t_eis.0,
                age_us = now.0.wrapping_sub(t_eis.0) as i64,
                "EIS key"
            );
        }
        machine.handle(ev, now, &mut self.out);
        self.apply();
        while let Some(ev) = self.pending.pop_front() {
            let Some(machine) = self.machine.as_mut() else {
                return;
            };
            machine.handle(ev, clock::now(), &mut self.out);
            self.apply();
        }
    }

    /// An EIS keymap: canonical first, then the machine. The debug line shows the raw text
    /// (what Hyprland gave), the canonical one, and whether it was a new keymap: on a machine
    /// with fcitx5 the raw hash alternates and the canonical one does not.
    fn keymap(&mut self, raw: String) {
        let Some(machine) = self.machine.as_ref() else {
            return;
        };
        let serial_before = machine.keymap_serial();
        let sent_before = machine.counters().keymaps_sent;
        let c = self.canon.canonical(raw);
        let (len, hash) = (c.text.len(), c.hash());
        let debug = tracing::enabled!(tracing::Level::DEBUG);
        self.handle(SourceEvent::Keymap { text: c.text });
        if debug && let Some(machine) = self.machine.as_ref() {
            tracing::debug!(
                raw_len = c.raw_len,
                raw_hash = %c.raw_hash,
                len,
                hash = %hash,
                compiled = c.compiled,
                serial = machine.keymap_serial().unwrap_or(0),
                new = machine.keymap_serial() != serial_before,
                frames = machine.counters().keymaps_sent - sent_before,
                "keymap seen"
            );
        }
    }

    /// A frame or datagram from a peer ends its stall probe (`probe.rs`).
    fn heard(&mut self, ev: &SourceEvent) {
        let p = match ev {
            SourceEvent::PeerAlive(p) | SourceEvent::PeerUp(p) => *p,
            SourceEvent::Control { peer, .. } => *peer,
            _ => return,
        };
        if let Some(e) = self.prober.heard(p, clock::now()) {
            log_episode(&self.conns, p, &e, "heard again");
        }
    }

    /// Sends the stall probes that are due.
    fn probe(&mut self) {
        if self.machine.is_none() {
            return;
        }
        for (peer, id, started) in self.prober.due(clock::now()) {
            let Some(conn) = self.conns.get(&peer) else {
                continue;
            };
            if let Some(why) = started {
                tracing::debug!(peer = %peer, ?why, "stall probe started");
                self.live.bump("stall_probes_started");
            }
            let mut b = [0u8; DATAGRAM_LEN];
            Datagram::Ping {
                id,
                t_us: clock::now().low32(),
            }
            .encode(&mut b);
            let _ = conn.send_datagram(cut(&mut self.slab, &b));
        }
    }

    fn apply(&mut self) {
        let Self {
            out,
            conns,
            ctl,
            driver,
            owed,
            live,
            pending,
            frame,
            slab,
            role,
            edge,
            prober,
            ..
        } = self;
        for action in out.drain() {
            match action {
                SourceAction::Datagram { peer, motion } => {
                    let Some(conn) = conns.get(&peer) else {
                        continue;
                    };
                    let mut b = [0u8; DATAGRAM_LEN];
                    Datagram::Motion(motion).encode(&mut b);
                    // Unreliable by design; a closed connection shows up as PeerDown.
                    let _ = conn.send_datagram(cut(slab, &b));
                }
                SourceAction::Send { peer, msg } => {
                    let Some(conn) = conns.get(&peer) else {
                        tracing::debug!(variant = msg.variant(), "no connection for a frame");
                        continue;
                    };
                    msg.encode(frame);
                    match conn.try_send_frame(cut(slab, frame)) {
                        Ok(()) => {}
                        Err(FrameError::Full) => {
                            // Fail open: the queue of 256 is full, the peer is not
                            // keeping up. Release rather than drop a key edge.
                            log_limited!(warn, peer = %peer, "frame queue full: force release");
                            live.bump("ctrl_overflow");
                            if !pending.contains(&SourceEvent::ForceRelease) {
                                pending.push_back(SourceEvent::ForceRelease);
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "frame not sent"),
                    }
                }
                SourceAction::Portal(cmd) => {
                    send_driver(driver, owed, ctl, live, DriverMsg::Portal(cmd));
                }
                SourceAction::CheckLock => {
                    send_driver(driver, owed, ctl, live, DriverMsg::CheckLock);
                }
                SourceAction::CheckCursor => send_ctl(ctl, live, CtlMsg::SourceCheckCursor),
                SourceAction::Go { x, y, dx, dy } => {
                    tracing::info!(x, y, dx, dy, "key crossing: nudge");
                    send_ctl(ctl, live, CtlMsg::SourceGo { x, y, dx, dy });
                }
                SourceAction::GoRefused(why) => tracing::info!(why, "key crossing: nothing to do"),
                SourceAction::Status(s) => {
                    tracing::debug!(role = ?s.role, gate = s.gate.as_str(), session = s.session,
                        last_leave = s.last_leave.map(|r| r.as_str()), "source status");
                    if s.role == SourceRole::Local
                        && *role == SourceRole::Remote
                        && let Some(reason) = s.last_leave
                    {
                        tracing::debug!(reason = reason.as_str(), "crossing ended");
                    }
                    if let Some(c) = source_crossed(*role, edge, &s) {
                        send_ctl(ctl, live, CtlMsg::Crossed(c));
                    }
                    *role = s.role;
                    for (p, e) in prober.status(&s, clock::now()) {
                        log_episode(conns, p, &e, "ended");
                    }
                    live.source_status(s);
                }
            }
        }
    }

    fn for_driver(&mut self, msg: DriverMsg) {
        send_driver(&self.driver, &mut self.owed, &self.ctl, &self.live, msg);
    }

    /// Hands the driver what it is owed, in order, as far as its queue takes it.
    fn flush_driver(&mut self) {
        while let Some(msg) = self.owed.pop_front() {
            match self.driver.try_send(msg) {
                Ok(()) => {}
                Err(TrySendError::Full(msg)) => {
                    self.owed.push_front(msg);
                    return;
                }
                Err(TrySendError::Closed(_)) => {
                    driver_gone(&self.live, self.owed.len() + 1);
                    self.owed.clear();
                    return;
                }
            }
        }
    }

    fn publish_counters(&self) {
        if let Some(m) = &self.machine {
            self.live.lock().source_counters = m.counters().clone();
        }
    }
}

/// `bytes` as a `Bytes` cut from `slab`: a new slab only once this one is
/// used up, and the old one is reused in place when nothing still holds a piece of it.
fn cut(slab: &mut BytesMut, bytes: &[u8]) -> Bytes {
    if slab.capacity() < bytes.len() {
        slab.reserve(SLAB.max(bytes.len()));
    }
    slab.extend_from_slice(bytes);
    slab.split_to(bytes.len()).freeze()
}

/// One debug line per stall-probe episode, with quinn's view of the path (BL-008, phase 2 reads
/// these to time the recovery on the source).
fn log_episode(
    conns: &HashMap<PeerId, Connection>,
    peer: PeerId,
    e: &crate::probe::Episode,
    how: &'static str,
) {
    let path = conns.get(&peer).map(Connection::path);
    tracing::debug!(
        peer = %peer,
        why = ?e.why,
        probes = e.probes,
        probing_ms = clock::now().since(e.since).0 / 1000,
        rtt_us = path.map(|p| p.rtt.as_micros() as u64),
        cwnd = path.map(|p| p.cwnd),
        lost = path.map(|p| p.lost_packets),
        congestion_events = path.map(|p| p.congestion_events),
        "stall probe {how}"
    );
}

/// For `ctl`: only the parked barrier's cursor read and the IPC release a stuck driver needs.
/// A lost cursor read leaves the barrier parked (no capture); a lost IPC release is backed by
/// the `Release` this thread keeps retrying.
/// The crossing that a new status starts or brings home, for the `edge` event. `edge`
/// keeps the link of the crossing in progress, so that its end can name it.
fn source_crossed(
    was: SourceRole,
    edge: &mut Option<ferry_core::geom::EdgeLink>,
    s: &SourceStatus,
) -> Option<Crossed> {
    if was != SourceRole::Remote && s.role == SourceRole::Remote {
        *edge = s.edge;
        return s.edge.map(|link| Crossed::Source { link, home: false });
    }
    if was == SourceRole::Remote && s.role != SourceRole::Remote {
        let link = edge.take()?;
        return (s.last_leave == Some(LeaveReason::Home))
            .then_some(Crossed::Source { link, home: true });
    }
    None
}

fn send_ctl(ctl: &mpsc::Sender<CtlMsg>, live: &SharedLive, msg: CtlMsg) {
    if let Err(e) = ctl.try_send(msg) {
        live.bump("ctl_queue_full");
        log_limited!(error, error = %e, "capture → ctl channel full; message dropped");
    }
}

/// Whether losing `msg` could leave capture on: a `Release` or `Disable` the portal never got,
/// or an EIS end the driver never re-created the session for.
fn ends_capture(msg: &DriverMsg) -> bool {
    matches!(
        msg,
        DriverMsg::Portal(PortalCmd::Release { .. } | PortalCmd::Disable)
            | DriverMsg::EisEnded { .. }
    )
}

/// One message for the portal driver, after any still owed (see the module doc).
fn send_driver(
    driver: &mpsc::Sender<DriverMsg>,
    owed: &mut VecDeque<DriverMsg>,
    ctl: &mpsc::Sender<CtlMsg>,
    live: &SharedLive,
    msg: DriverMsg,
) {
    if !owed.is_empty() {
        if owed.len() >= DRIVER_OWED_CAP && !ends_capture(&msg) {
            live.bump("driver_msgs_dropped");
            log_limited!(warn, ?msg, "the portal driver is stuck: dropped");
            return;
        }
        owed.push_back(msg);
        return;
    }
    match driver.try_send(msg) {
        Ok(()) => {}
        Err(TrySendError::Full(msg)) => {
            live.bump("driver_queue_full");
            log_limited!(
                warn,
                ?msg,
                "the portal driver's queue is full: kept for the next try; IPC release now"
            );
            owed.push_back(msg);
            send_ctl(ctl, live, CtlMsg::IpcRelease);
        }
        Err(TrySendError::Closed(_)) => driver_gone(live, 1),
    }
}

fn driver_gone(live: &SharedLive, lost: usize) {
    // Shutdown ends the driver after this thread.
    if live.lock().shutting_down {
        tracing::debug!(
            lost,
            "capture → driver: the driver has stopped for the shutdown"
        );
    } else {
        tracing::error!(
            lost,
            "capture → driver channel closed: the portal driver is gone"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::geom::{EdgeLink, MonitorId, Side};
    use ferry_core::source::Gate;

    fn status(role: SourceRole, edge: Option<EdgeLink>, last: Option<LeaveReason>) -> SourceStatus {
        SourceStatus {
            role,
            peer: None,
            session: 1,
            last_leave: last,
            gate: Gate::Armed,
            suspect: vec![],
            controlled: false,
            locked_here: false,
            locked_peers: vec![],
            edge,
        }
    }

    /// A crossing is told to ctl once going out and once coming home over the same
    /// link; a crossing that ends any other way, and every status in between, is not.
    #[test]
    fn a_crossing_is_told_out_and_home_once_each() {
        let m = |p| MonitorId {
            peer: PeerId([p; 32]),
            index: 0,
        };
        let link = EdgeLink {
            from: m(1),
            side: Side::Bottom,
            to: m(2),
            line: 1080,
            span: (277, 1643),
        };
        let mut edge = None;
        let out = status(SourceRole::Remote, Some(link), None);
        let back = |r| status(SourceRole::Local, None, Some(r));
        assert_eq!(
            source_crossed(SourceRole::Arming, &mut edge, &out),
            Some(Crossed::Source { link, home: false })
        );
        assert_eq!(source_crossed(SourceRole::Remote, &mut edge, &out), None);
        assert_eq!(
            source_crossed(SourceRole::Remote, &mut edge, &back(LeaveReason::Home)),
            Some(Crossed::Source { link, home: true })
        );
        assert_eq!(edge, None);
        source_crossed(SourceRole::Arming, &mut edge, &out);
        assert_eq!(
            source_crossed(SourceRole::Remote, &mut edge, &back(LeaveReason::Silence)),
            None
        );
        assert_eq!(edge, None);
    }

    fn spawn_off() -> CaptureHandle {
        let (tx, _rx) = mpsc::channel(8);
        let (driver, _drx) = mpsc::channel(8);
        spawn(None, tx, driver, SharedLive::default()).unwrap()
    }

    /// Consecutive datagrams are neighbours in one slab, so they cost no allocation each;
    /// a frame larger than the slab still goes out whole.
    #[test]
    fn datagrams_are_cut_from_one_slab() {
        let mut slab = BytesMut::with_capacity(SLAB);
        let first = cut(&mut slab, &[1; DATAGRAM_LEN]);
        let mut prev = first.clone();
        for i in 1..SLAB / DATAGRAM_LEN {
            let b = cut(&mut slab, &[i as u8; DATAGRAM_LEN]);
            assert_eq!(b.as_ptr() as usize, prev.as_ptr() as usize + DATAGRAM_LEN);
            assert_eq!(&b[..], &[i as u8; DATAGRAM_LEN]);
            prev = b;
        }
        assert_eq!(&first[..], &[1; DATAGRAM_LEN]);
        let big = cut(&mut slab, &[7; 3 * SLAB]);
        assert_eq!(&big[..], &[7; 3 * SLAB][..]);
        assert_eq!(
            &first[..],
            &[1; DATAGRAM_LEN],
            "a piece outlives the slab it came from"
        );
    }

    #[test]
    fn capture_off_accepts_commands_and_stops_on_shutdown() {
        let h = spawn_off();
        h.tx.try_send(CaptureCmd::Event(SourceEvent::ForceRelease))
            .unwrap();
        h.tx.try_send(CaptureCmd::Shutdown).unwrap();
        h.join.join().unwrap();
    }

    #[test]
    fn stops_when_the_sender_is_dropped() {
        let h = spawn_off();
        drop(h.tx);
        h.join.join().unwrap();
    }

    #[test]
    fn the_machine_polls_the_lock_at_once_and_answers_go_back_in() {
        let (tx, _ctl_rx) = mpsc::channel(8);
        let (driver, mut rx) = mpsc::channel(8);
        let live = SharedLive::default();
        let h = spawn(Some(SourceConfig::default()), tx, driver, live.clone()).unwrap();
        // The first deadline is the immediate lock poll.
        let first = rx.blocking_recv().unwrap();
        assert!(matches!(first, DriverMsg::CheckLock), "{first:?}");
        h.tx.try_send(CaptureCmd::Event(SourceEvent::LockPolled(
            ferry_core::source::LockCheck::Unlocked,
        )))
        .unwrap();
        // Unlocked, but no topology and no peer: nothing to arm, the gate stays idle.
        h.tx.try_send(CaptureCmd::Event(SourceEvent::PrepareForSleep {
            start: true,
        }))
        .unwrap();
        let next = rx.blocking_recv().unwrap();
        assert!(matches!(next, DriverMsg::SleepHandled), "{next:?}");
        h.tx.try_send(CaptureCmd::Shutdown).unwrap();
        h.join.join().unwrap();
        assert_eq!(
            live.lock().source.as_ref().unwrap().gate,
            ferry_core::source::Gate::Suspended
        );
    }

    /// A full driver queue keeps the messages, in order, until the driver reads again,
    /// and asks ctl for an IPC release once.
    #[test]
    fn a_full_driver_queue_keeps_what_ends_a_capture_in_order() {
        let (ctl, mut ctl_rx) = mpsc::channel(8);
        let (driver, mut rx) = mpsc::channel(1);
        let live = SharedLive::default();
        let mut owed = VecDeque::new();
        let release = || {
            DriverMsg::Portal(PortalCmd::Release {
                activation_id: Some(1),
                pos: None,
            })
        };
        send_driver(&driver, &mut owed, &ctl, &live, DriverMsg::CheckLock);
        send_driver(&driver, &mut owed, &ctl, &live, release());
        send_driver(&driver, &mut owed, &ctl, &live, DriverMsg::SleepHandled);
        assert_eq!(owed.len(), 2);
        assert!(matches!(ctl_rx.try_recv(), Ok(CtlMsg::IpcRelease)));
        assert!(ctl_rx.try_recv().is_err(), "one IPC release per episode");
        assert_eq!(live.lock().counters.get("driver_queue_full"), Some(&1));

        // Past the cap, a lock check is dropped and a Release is not.
        for _ in 0..DRIVER_OWED_CAP {
            send_driver(&driver, &mut owed, &ctl, &live, DriverMsg::CheckLock);
        }
        let n = owed.len();
        send_driver(&driver, &mut owed, &ctl, &live, DriverMsg::CheckLock);
        assert_eq!(owed.len(), n);
        send_driver(&driver, &mut owed, &ctl, &live, release());
        assert_eq!(owed.len(), n + 1);

        let mut got = vec![rx.try_recv().unwrap()];
        while let Some(msg) = owed.pop_front() {
            driver.try_send(msg).unwrap();
            got.push(rx.try_recv().unwrap());
        }
        assert!(matches!(got[0], DriverMsg::CheckLock));
        assert!(matches!(
            got[1],
            DriverMsg::Portal(PortalCmd::Release { .. })
        ));
        assert!(matches!(got[2], DriverMsg::SleepHandled));
        assert!(matches!(
            got.last(),
            Some(DriverMsg::Portal(PortalCmd::Release { .. }))
        ));
    }
}
