//! The seam between the TargetMachine (on `net`) and the injection backend.
//!
//! The production backend is a [`DeviceInjector`] over ferry-inject's [`Owner`]. Tests replace
//! it with a [`Recorder`], or keep it and put [`FakeDevices`](crate::fake_devices::FakeDevices)
//! behind the owner, so a target session can run on the host without touching input.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_core::target::InjectCmd;
use ferry_inject::{
    Axis, AxisSource, DeviceEvent, InjectStats, InputCmd, InputEvent, InputSender, Opener, Owner,
    WaylandOpener,
};
use tokio::sync::mpsc;

use crate::ratelimit::log_limited;

/// What the TargetMachine's `Inject` actions go to. Called on the `net` thread, so nothing here
/// may block on the compositor or Hyprland: a backend that needs to does it on a
/// thread of its own.
pub trait Injector: Send {
    fn send(&mut self, cmd: InjectCmd);
    /// Shutdown: wait until everything sent so far has reached the compositor, at
    /// most `within`.
    fn sync(&mut self, _within: Duration) {}
    /// Called every 2 s by `net`; `active` is true while a source is entered here.
    fn maintain(&mut self, _active: bool) {}
    /// What the backend's devices counted since the last call, so the counts of one set
    /// of devices add up across sessions. Called by `net` every 2 s, after `maintain`.
    fn stats_delta(&mut self) -> InjectStats {
        InjectStats::default()
    }
    /// Sync, then drop the virtual devices, if the backend creates them on demand: at start-up,
    /// and on a peer that also captures, the moment a target session ends,
    /// so none exists while this peer captures. `None`: they are gone. `Some(seq)`: they go on
    /// the backend's own thread, and [`DeviceEvent::Closed`] with `seq` or a later one says when.
    fn drop_devices(&mut self) -> Option<u64> {
        None
    }
    /// An event from the backend's threads. The injector takes the ones about its devices and
    /// hands the rest back for net.
    fn event(&mut self, e: DeviceEvent) -> Option<DeviceEvent> {
        Some(e)
    }
    /// The backend failed since the last call (ferry-inject's thread is gone, or the devices
    /// could not be created): why. Every later command of the session would be lost and keys
    /// it held may be down, so net ends the session. Asked after every batch of commands
    /// and every event.
    fn take_gone(&mut self) -> Option<String> {
        None
    }
}

/// The `status` counters of ferry-inject's [`InjectStats`] . The overflows are
/// not among them: [`InputEvent::Overflow`] counts those as `inject_overflow`, with the ones of
/// the queue that waits for the devices.
pub fn publish_stats(live: &crate::live::SharedLive, d: InjectStats) {
    live.add("inject_coalesced", d.coalesced);
    live.add("inject_would_block", d.would_block);
    live.add("inject_queued", d.queued);
    live.add("inject_keyboards", d.keyboards);
    live.add("inject_layout_updates", d.layout_updates);
    live.add("inject_nonfinite_dropped", d.nonfinite_dropped);
}

/// Where target input goes.
pub enum InjectBackend {
    /// No injection: this peer refuses every `Enter` (`Ended(refused)`). The default for an
    /// in-process daemon, so a test can never inject into the live session by accident.
    Off,
    /// ferry-inject on the session's Wayland display (the `ferryd` binary).
    Wayland,
    /// The production path ([`DeviceInjector`], ferry-inject's owner thread) over other
    /// devices: [`FakeDevices`](crate::fake_devices::FakeDevices) in tests.
    Devices(Box<dyn Opener>),
    /// A replacement, for tests.
    Custom(Box<dyn Injector>),
}

impl std::fmt::Debug for InjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "Off",
            Self::Wayland => "Wayland",
            Self::Devices(_) => "Devices",
            Self::Custom(_) => "Custom",
        })
    }
}

/// A started backend, handed to the `net` thread (start-up step 3).
pub struct Started {
    pub injector: Option<Box<dyn Injector>>,
    /// The backend's own events (`Reset` becomes `TargetEvent::InjectReset`).
    pub events: Option<mpsc::UnboundedReceiver<DeviceEvent>>,
}

impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started")
            .field("injector", &self.injector.is_some())
            .finish_non_exhaustive()
    }
}

impl InjectBackend {
    /// Start the backend. ferry-inject runs its own start-up modifier reset on every new
    /// keyboard; the TargetMachine's `Start` adds its own `ModifierReset` on top.
    pub fn start(self) -> Started {
        let opener: Box<dyn Opener> = match self {
            Self::Off => {
                return Started {
                    injector: None,
                    events: None,
                };
            }
            Self::Custom(i) => {
                return Started {
                    injector: Some(i),
                    events: None,
                };
            }
            Self::Wayland => Box::new(WaylandOpener::default()),
            Self::Devices(o) => o,
        };
        match DeviceInjector::start(opener) {
            Ok((i, events)) => Started {
                injector: Some(Box::new(i)),
                events: Some(events),
            },
            Err(e) => {
                tracing::error!(error = %e, "cannot start ferry-inject; this peer refuses to be a target");
                Started {
                    injector: None,
                    events: None,
                }
            }
        }
    }
}

/// How long the virtual devices outlive the last session (see [`DeviceInjector`]).
pub const IDLE_DROP: Duration = Duration::from_secs(2);
/// After the devices could not be created, a session's first input within this ends the session
/// at once instead of asking again: one try per second at most, not one per motion.
pub const OPEN_RETRY: Duration = Duration::from_secs(1);
/// How many commands wait for the devices to exist: as many as ferry-inject's own channel takes.
pub const PENDING_CAP: usize = ferry_inject::CHANNEL_CAPACITY;

/// ferry-inject: an unbound virtual pointer and a virtual keyboard, the layout kept
/// in step with Hyprland's monitors by `follow_layout`.
///
/// **The devices exist only while they are needed**: for the start-up modifier reset, and from
/// the first input of a session until the session ends (a peer that also captures, design
/// m1-rest §3.3) or [`IDLE_DROP`] after the target is idle again (a target-only peer). A virtual
/// keyboard that merely exists in a compositor that is also capturing (this peer as a source)
/// makes Hyprland 0.56.2 re-create the EIS keyboard on the first modifier press during a
/// capture, with no `start_emulating`, so every later key of that crossing is lost (lab VMs a
/// and c, 2026-10-16; without ferryd's keyboard the same presses pass). The keymap last set is
/// kept and given to the next devices.
///
/// **net never waits for them**. Creating the devices waits on a Wayland connect, two
/// roundtrips and a Hyprland request, and dropping them on a sync and a thread's end. ferry-inject's
/// [`Owner`] does both on its own thread; this side only asks (`try_send`) and takes the answers
/// as [`DeviceEvent`]s. Input never goes through the owner: once the devices exist, it goes
/// straight to their channel ([`InputSender`]), the target path's one hop.
///
/// **The first input of a session waits for the devices, in order**. Until
/// `Opened` comes, commands queue here, at most [`PENDING_CAP`]: consecutive motion keeps only
/// the newest position, and a `ReleaseAll` drops the input queued before it (as ferry-inject's own
/// ring does: nothing may press a key after the release), keeping the newest keymap. A full
/// queue is ferry-inject's overflow: the queue is dropped and net gets `Overflow` and `Reset`.
/// Devices that cannot be created end the session as forced (`take_gone`), as a dead inject
/// thread does.
pub struct DeviceInjector {
    owner: Owner,
    /// Where net's events come from: the queue's own overflow goes there too.
    events: mpsc::UnboundedSender<DeviceEvent>,
    keymap: Option<String>,
    devices: DeviceState,
    /// The last open or close request.
    seq: u64,
    /// The last close request, until its `Closed` comes.
    closing: Option<u64>,
    last_used: Instant,
    /// When the devices last could not be created.
    failed_at: Option<Instant>,
    /// The backend failed, and net has not been told yet.
    gone: Option<String>,
    /// Since then, commands are dropped until the session's `ReleaseAll`: no new devices in the
    /// middle of a session that net is about to end.
    dead: bool,
    /// What [`Injector::stats_delta`] has handed out of the open devices' counters, which start
    /// at zero with each set of devices.
    seen: InjectStats,
    /// The counts of devices that closed since the last [`Injector::stats_delta`].
    carry: InjectStats,
}

enum DeviceState {
    None,
    /// Asked for; the input waits here.
    Opening {
        seq: u64,
        pending: VecDeque<InputCmd>,
    },
    Open(InputSender),
}

impl DeviceInjector {
    /// Start the owner thread and run the start-up modifier reset: the devices are created, the
    /// reset reaches the compositor, and they are dropped again, before this returns. It blocks
    /// the caller (start-up step 3, before net or capture depend on it), so a peer without a
    /// compositor knows at start-up that it cannot be a target.
    pub fn start(
        opener: Box<dyn Opener>,
    ) -> ferry_inject::Result<(Self, mpsc::UnboundedReceiver<DeviceEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = tx.clone();
        let owner = Owner::spawn(opener, move |e| {
            let _ = sink.send(e);
        })?;
        owner.reset(None)?;
        Ok((
            Self {
                owner,
                events: tx,
                keymap: None,
                devices: DeviceState::None,
                seq: 0,
                closing: None,
                last_used: Instant::now(),
                failed_at: None,
                gone: None,
                dead: false,
                seen: InjectStats::default(),
                carry: InjectStats::default(),
            },
            rx,
        ))
    }

    /// Ask for devices; the input waits until they exist.
    fn open(&mut self) {
        if self.failed_at.is_some_and(|t| t.elapsed() < OPEN_RETRY) {
            self.fail("the virtual devices could not be created a moment ago".into());
            return;
        }
        self.seq += 1;
        match self.owner.open(self.seq, self.keymap.clone()) {
            Ok(()) => {
                self.devices = DeviceState::Opening {
                    seq: self.seq,
                    pending: VecDeque::new(),
                };
            }
            Err(e) => self.fail(format!("cannot ask for the virtual devices: {e}")),
        }
    }

    /// Let go of the devices and ask the owner to drop them. Our sender goes first.
    fn close(&mut self) {
        // The counts the closing devices made since the last delta are not lost with them.
        self.carry = self.carry.plus(self.open_delta());
        self.seen = InjectStats::default();
        self.devices = DeviceState::None;
        self.seq += 1;
        match self.owner.close(self.seq) {
            Ok(()) => self.closing = Some(self.seq),
            // A gone owner answers nothing more, so nothing waits for it; a stuck one keeps the
            // last close it took.
            Err(e @ ferry_inject::Error::ThreadGone) => {
                tracing::error!(error = %e, "the device owner thread is gone");
                self.closing = None;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot ask for the virtual devices to be dropped")
            }
        }
    }

    /// The open devices' counts that [`Injector::stats_delta`] has not handed out yet.
    fn open_delta(&mut self) -> InjectStats {
        let DeviceState::Open(tx) = &self.devices else {
            return InjectStats::default();
        };
        let now = tx.stats();
        let d = now.since(self.seen);
        self.seen = now;
        d
    }

    /// The backend failed: net ends the session, and its commands are dropped until then.
    fn fail(&mut self, why: String) {
        self.dead = true;
        self.gone.get_or_insert(why);
    }

    fn forward(&mut self, cmd: InputCmd) {
        let DeviceState::Open(tx) = &self.devices else {
            return;
        };
        match tx.send(cmd) {
            Ok(()) => {}
            // The inject thread answers an overflow with a release of everything held.
            Err(ferry_inject::Error::Overflow) => {
                log_limited!(
                    warn,
                    "inject channel full: ferry-inject releases everything"
                );
            }
            Err(e @ ferry_inject::Error::ThreadGone) => {
                // Once per death, not per command: the devices go, net ends the session.
                self.close();
                self.fail(e.to_string());
            }
            Err(e) => log_limited!(error, error = %e, "inject"),
        }
    }

    /// Queue `cmd` until the devices exist.
    fn hold(&mut self, cmd: InputCmd) {
        let DeviceState::Opening { pending, .. } = &mut self.devices else {
            return;
        };
        match cmd {
            InputCmd::Motion { .. } if matches!(pending.back(), Some(InputCmd::Motion { .. })) => {
                if let Some(last) = pending.back_mut() {
                    *last = cmd;
                }
            }
            InputCmd::ReleaseAll => {
                let before = pending.len();
                pending.retain(|c| matches!(c, InputCmd::SetKeymap(_)));
                if pending.len() < before {
                    tracing::debug!(
                        dropped = before - pending.len(),
                        "a release before the devices exist: the input queued before it is dropped"
                    );
                }
                pending.push_back(InputCmd::ReleaseAll);
            }
            cmd if pending.len() >= PENDING_CAP => {
                // ferry-inject's overflow, before its thread exists: the queued input and this
                // command are dropped, and the TargetMachine forgets its held set.
                let queued = pending.len();
                pending.retain(|c| matches!(c, InputCmd::SetKeymap(_)));
                pending.push_back(InputCmd::ReleaseAll);
                drop(cmd);
                log_limited!(
                    warn,
                    queued,
                    "too much input waiting for the virtual devices: dropped"
                );
                for e in [InputEvent::Overflow { queued }, InputEvent::Reset] {
                    let _ = self.events.send(DeviceEvent::Input(e));
                }
            }
            cmd => pending.push_back(cmd),
        }
    }
}

/// `InjectCmd` → the current `InputCmd`.
pub fn input_cmd(cmd: InjectCmd) -> InputCmd {
    let axis = |a: u8| {
        if a == 1 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        }
    };
    let source = |s: u8| match s {
        1 => AxisSource::Finger,
        2 => AxisSource::Continuous,
        _ => AxisSource::Wheel,
    };
    match cmd {
        InjectCmd::ModifierReset | InjectCmd::ReleaseAll => InputCmd::ReleaseAll,
        InjectCmd::Motion { x, y } => InputCmd::Motion { x, y },
        InjectCmd::Key { code, pressed } => InputCmd::Key {
            code: u32::from(code),
            pressed,
        },
        InjectCmd::Button { code, pressed } => InputCmd::Button {
            button: u32::from(code),
            pressed,
        },
        InjectCmd::Scroll {
            axis: a,
            source: s,
            value,
            value120,
        } => InputCmd::Axis {
            axis: axis(a),
            source: source(s),
            value,
            value120,
        },
        InjectCmd::ScrollStop { axis: a, source: s } => InputCmd::AxisStop {
            axis: axis(a),
            source: source(s),
        },
        InjectCmd::SetKeymap(text) => InputCmd::SetKeymap(text),
    }
}

impl Injector for DeviceInjector {
    fn send(&mut self, cmd: InjectCmd) {
        self.last_used = Instant::now();
        if let InjectCmd::SetKeymap(text) = &cmd {
            self.keymap = Some(text.clone());
        }
        if self.dead {
            if cmd == InjectCmd::ReleaseAll {
                // The session's end: the next one creates new devices.
                self.dead = false;
            }
            return;
        }
        if matches!(self.devices, DeviceState::None) {
            match cmd {
                // Nothing exists, so nothing is held, and the next devices' keyboard runs the
                // modifier reset itself; the keymap waits for them.
                InjectCmd::ReleaseAll | InjectCmd::ModifierReset | InjectCmd::SetKeymap(_) => {
                    return;
                }
                _ => {
                    self.open();
                    if self.dead {
                        return;
                    }
                }
            }
        }
        let cmd = input_cmd(cmd);
        if matches!(self.devices, DeviceState::Open(_)) {
            self.forward(cmd);
        } else {
            self.hold(cmd);
        }
    }

    fn event(&mut self, e: DeviceEvent) -> Option<DeviceEvent> {
        match e {
            DeviceEvent::Opened { seq, sender } => {
                if let DeviceState::Opening { seq: s, pending } = &mut self.devices
                    && *s == seq
                {
                    let pending = std::mem::take(pending);
                    self.devices = DeviceState::Open(sender);
                    self.seen = InjectStats::default();
                    self.failed_at = None;
                    for cmd in pending {
                        self.forward(cmd);
                    }
                }
                // Otherwise the session ended first and a close is queued behind the open: the
                // sender goes here, and the owner drops the devices next.
                None
            }
            DeviceEvent::OpenFailed { seq, error } => {
                tracing::error!(%error, "cannot create the virtual devices");
                self.failed_at = Some(Instant::now());
                if matches!(&self.devices, DeviceState::Opening { seq: s, .. } if *s == seq) {
                    self.devices = DeviceState::None;
                    self.fail(format!("cannot create the virtual devices: {error}"));
                }
                None
            }
            DeviceEvent::Closed { seq } => {
                if self.closing.is_some_and(|c| seq >= c) {
                    self.closing = None;
                }
                Some(DeviceEvent::Closed { seq })
            }
            e @ DeviceEvent::Input(_) => Some(e),
        }
    }

    fn take_gone(&mut self) -> Option<String> {
        self.gone.take()
    }

    fn sync(&mut self, within: Duration) {
        if let Err(e) = self.owner.sync_within(within) {
            tracing::warn!(error = %e, "ferry-inject sync");
        }
    }

    fn drop_devices(&mut self) -> Option<u64> {
        self.dead = false;
        if !matches!(self.devices, DeviceState::None) {
            self.close();
        }
        self.closing
    }

    fn stats_delta(&mut self) -> InjectStats {
        let open = self.open_delta();
        std::mem::take(&mut self.carry).plus(open)
    }

    fn maintain(&mut self, active: bool) {
        if !active
            && !matches!(self.devices, DeviceState::None)
            && self.last_used.elapsed() >= IDLE_DROP
        {
            // The owner releases everything and destroys the devices.
            self.close();
            tracing::debug!("virtual devices dropped (target idle)");
        }
    }
}

/// Records every command with its arrival time. Clone it, hand one copy to the daemon
/// (`InjectBackend::Custom`), read the other.
#[derive(Clone, Default)]
pub struct Recorder {
    log: Arc<Mutex<Vec<(Instant, InjectCmd)>>>,
    /// `sync` and `drop_devices` calls, with when they came (a `drop_devices` is a sync, then a
    /// drop).
    devices: Arc<Mutex<Vec<(Instant, &'static str)>>>,
    /// [`Recorder::die`]: the next `send` is lost and reports the backend gone.
    die: Arc<Mutex<Option<String>>>,
    gone: Option<String>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every command so far, in order.
    pub fn commands(&self) -> Vec<InjectCmd> {
        self.log
            .lock()
            .map(|l| l.iter().map(|(_, c)| c.clone()).collect())
            .unwrap_or_default()
    }

    /// Every command so far, with when it arrived.
    pub fn timed(&self) -> Vec<(Instant, InjectCmd)> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// The next command is lost and the backend reports itself gone with `why`, as
    /// ferry-inject's thread does when it ends on a fatal Wayland error.
    pub fn die(&self, why: &str) {
        if let Ok(mut d) = self.die.lock() {
            *d = Some(why.to_string());
        }
    }

    pub fn clear(&self) {
        if let Ok(mut l) = self.log.lock() {
            l.clear();
        }
        if let Ok(mut l) = self.devices.lock() {
            l.clear();
        }
    }

    /// Every `sync` (`"sync"`) and `drop_devices` (`"sync"`, then `"drop"`) call so far, in
    /// order.
    pub fn device_calls(&self) -> Vec<(Instant, &'static str)> {
        self.devices.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Injector for Recorder {
    fn send(&mut self, cmd: InjectCmd) {
        if let Some(why) = self.die.lock().ok().and_then(|mut d| d.take()) {
            self.gone = Some(why);
            return;
        }
        if let Ok(mut l) = self.log.lock() {
            l.push((Instant::now(), cmd));
        }
    }

    fn sync(&mut self, _within: Duration) {
        if let Ok(mut l) = self.devices.lock() {
            l.push((Instant::now(), "sync"));
        }
    }

    fn drop_devices(&mut self) -> Option<u64> {
        if let Ok(mut l) = self.devices.lock() {
            let now = Instant::now();
            l.push((now, "sync"));
            l.push((now, "drop"));
        }
        None
    }

    fn take_gone(&mut self) -> Option<String> {
        self.gone.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_command() {
        assert!(matches!(
            input_cmd(InjectCmd::ModifierReset),
            InputCmd::ReleaseAll
        ));
        assert!(matches!(
            input_cmd(InjectCmd::Key {
                code: 30,
                pressed: true
            }),
            InputCmd::Key {
                code: 30,
                pressed: true
            }
        ));
        assert!(matches!(
            input_cmd(InjectCmd::Scroll {
                axis: 1,
                source: 0,
                value: 15.0,
                value120: Some(120)
            }),
            InputCmd::Axis {
                axis: Axis::Horizontal,
                source: AxisSource::Wheel,
                value120: Some(120),
                ..
            }
        ));
        assert!(matches!(
            input_cmd(InjectCmd::ScrollStop { axis: 0, source: 1 }),
            InputCmd::AxisStop {
                axis: Axis::Vertical,
                source: AxisSource::Finger
            }
        ));
        assert!(matches!(
            input_cmd(InjectCmd::Button {
                code: 0x110,
                pressed: false
            }),
            InputCmd::Button {
                button: 0x110,
                pressed: false
            }
        ));
    }

    use crate::fake_devices::{DeviceCall, FakeDevices};

    const KM: &str = "xkb_keymap { km };";

    fn key(code: u16, pressed: bool) -> InjectCmd {
        InjectCmd::Key { code, pressed }
    }

    /// A started injector over `fake`, whose start-up reset is done and forgotten.
    fn started(fake: &FakeDevices) -> (DeviceInjector, mpsc::UnboundedReceiver<DeviceEvent>) {
        let (i, rx) = DeviceInjector::start(Box::new(fake.clone())).unwrap();
        assert_eq!(
            fake.calls(),
            [DeviceCall::Open(None), DeviceCall::Sync, DeviceCall::Drop],
            "the start-up reset"
        );
        fake.clear();
        (i, rx)
    }

    /// Feeds the injector its events, as net does, until `done`; returns what it handed back.
    fn pump(
        i: &mut DeviceInjector,
        rx: &mut mpsc::UnboundedReceiver<DeviceEvent>,
        what: &str,
        done: impl Fn(&[DeviceEvent]) -> bool,
    ) -> Vec<DeviceEvent> {
        let until = Instant::now() + Duration::from_secs(5);
        let mut back = Vec::new();
        while !done(&back) {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            match rx.try_recv() {
                Ok(e) => back.extend(i.event(e)),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        back
    }

    fn closed(back: &[DeviceEvent]) -> bool {
        back.iter().any(|e| matches!(e, DeviceEvent::Closed { .. }))
    }

    /// The counts of every set of devices add up across sessions, and the ones a closing
    /// set made since the last delta are not lost with it.
    #[test]
    fn the_devices_counts_add_up_across_sessions() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        let stats = |coalesced, keyboards| InjectStats {
            coalesced,
            keyboards,
            ..InjectStats::default()
        };
        assert_eq!(i.stats_delta(), InjectStats::default(), "no devices yet");

        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        pump(&mut i, &mut rx, "the devices", |_| {
            !fake.commands().is_empty()
        });
        fake.set_stats(stats(3, 1));
        assert_eq!(i.stats_delta(), stats(3, 1));
        assert_eq!(i.stats_delta(), InjectStats::default(), "handed out once");
        fake.set_stats(stats(5, 1));
        assert_eq!(i.stats_delta(), stats(2, 0));

        // Counted after the last delta, then the session ends: the 2 more still arrive.
        fake.set_stats(stats(7, 1));
        i.send(InjectCmd::ReleaseAll);
        assert!(i.drop_devices().is_some());
        pump(&mut i, &mut rx, "Closed", closed);
        assert_eq!(i.stats_delta(), stats(2, 0));

        // New devices count from zero again.
        fake.clear();
        i.send(InjectCmd::Motion { x: 2.0, y: 2.0 });
        pump(&mut i, &mut rx, "the devices", |_| {
            !fake.commands().is_empty()
        });
        fake.set_stats(stats(1, 1));
        assert_eq!(i.stats_delta(), stats(1, 1));
    }

    #[test]
    fn publish_stats_names_the_status_counters() {
        let live = crate::live::SharedLive::default();
        publish_stats(
            &live,
            InjectStats {
                coalesced: 4,
                would_block: 1,
                overflows: 9,
                ..InjectStats::default()
            },
        );
        publish_stats(
            &live,
            InjectStats {
                coalesced: 1,
                ..InjectStats::default()
            },
        );
        let l = live.lock();
        assert_eq!(l.counters.get("inject_coalesced"), Some(&5));
        assert_eq!(l.counters.get("inject_would_block"), Some(&1));
        assert!(
            !l.counters.contains_key("inject_queued"),
            "zero is not shown"
        );
        assert!(
            !l.counters.contains_key("inject_overflow"),
            "InputEvent::Overflow counts those"
        );
    }

    /// The first input of a session never waits for the devices to be
    /// created; it waits for them in order, and reaches them right after they exist.
    #[test]
    fn the_first_input_waits_for_the_devices_in_order() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        fake.set_open_delay(Duration::from_millis(300));
        let t = Instant::now();
        i.send(InjectCmd::SetKeymap(KM.into()));
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        i.send(key(30, true));
        i.send(InjectCmd::Motion { x: 2.0, y: 2.0 });
        i.send(InjectCmd::Motion { x: 3.0, y: 3.0 });
        i.send(key(30, false));
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
        assert!(fake.calls().is_empty());
        pump(&mut i, &mut rx, "the input at the devices", |_| {
            fake.commands().len() == 4
        });
        assert_eq!(
            fake.calls(),
            [
                DeviceCall::Open(Some(KM.into())),
                DeviceCall::Cmd("Motion { x: 1.0, y: 1.0 }".into()),
                DeviceCall::Cmd("Key { code: 30, pressed: true }".into()),
                // consecutive motion keeps the newest position
                DeviceCall::Cmd("Motion { x: 3.0, y: 3.0 }".into()),
                DeviceCall::Cmd("Key { code: 30, pressed: false }".into()),
            ]
        );
        // Once they exist, input goes straight to them.
        i.send(key(31, true));
        pump(&mut i, &mut rx, "a direct key", |_| {
            fake.commands().len() == 5
        });
        assert!(i.take_gone().is_none());
    }

    /// A release that comes before the devices exist drops the input queued before it, as
    /// ferry-inject's own ring does: a key pressed in a session that already ended is never
    /// typed late. The newest keymap stays.
    #[test]
    fn a_release_before_the_devices_exist_drops_the_input_queued_before_it() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        fake.set_open_delay(Duration::from_millis(200));
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        i.send(key(42, true));
        i.send(InjectCmd::SetKeymap(KM.into()));
        i.send(InjectCmd::ReleaseAll);
        i.send(key(30, true));
        pump(&mut i, &mut rx, "the input at the devices", |_| {
            fake.commands().len() == 3
        });
        assert_eq!(
            fake.calls(),
            [
                DeviceCall::Open(None),
                DeviceCall::Cmd(format!("SetKeymap({KM:?})")),
                DeviceCall::Cmd("ReleaseAll".into()),
                DeviceCall::Cmd("Key { code: 30, pressed: true }".into()),
            ]
        );
    }

    /// More input than ferry-inject's channel takes, while the devices are made: the defined
    /// overflow. The queue is dropped, and net gets `Overflow` and `Reset`, so the
    /// TargetMachine forgets its held set; nothing is lost without a word.
    #[test]
    fn too_much_input_waiting_is_an_overflow() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        fake.set_open_delay(Duration::from_millis(200));
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        for n in 0..PENDING_CAP + 10 {
            i.send(key(30, n % 2 == 0));
        }
        let back = pump(&mut i, &mut rx, "Overflow and Reset", |b| b.len() >= 2);
        assert!(matches!(
            back[0],
            DeviceEvent::Input(InputEvent::Overflow {
                queued: PENDING_CAP
            })
        ));
        assert!(matches!(back[1], DeviceEvent::Input(InputEvent::Reset)));
        // The motion and 1023 keys filled the queue; the next key overflowed it; 10 came after.
        pump(&mut i, &mut rx, "the rest at the devices", |_| {
            fake.commands().len() == 1 + (PENDING_CAP + 10 - PENDING_CAP)
        });
        assert_eq!(fake.commands()[0], "ReleaseAll");
    }

    /// A session that ends before its devices exist (§3.3 on a capturing peer): they are made
    /// and dropped unused, and the `Closed` that net waits for comes after both.
    #[test]
    fn a_session_end_before_the_devices_exist_drops_them_unused() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        fake.set_open_delay(Duration::from_millis(200));
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        i.send(key(30, true));
        i.send(InjectCmd::ReleaseAll);
        let seq = i.drop_devices().expect("a close to wait for");
        assert_eq!(
            i.drop_devices(),
            Some(seq),
            "asking again waits for the same"
        );
        let back = pump(&mut i, &mut rx, "Closed", closed);
        assert!(matches!(back[..], [DeviceEvent::Closed { seq: s }] if s == seq));
        assert_eq!(i.drop_devices(), None, "nothing left to wait for");
        assert_eq!(
            fake.calls(),
            [DeviceCall::Open(None), DeviceCall::Sync, DeviceCall::Drop]
        );
    }

    /// Devices are dropped at a session's end and made again, with the newest keymap, for the
    /// next; the drop syncs first.
    #[test]
    fn devices_are_dropped_and_made_again_across_sessions() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        for session in 0..3u16 {
            let km = format!("xkb_keymap {{ s{session} }};");
            i.send(InjectCmd::SetKeymap(km.clone()));
            i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
            i.send(key(30 + session, true));
            pump(&mut i, &mut rx, "the devices", |_| {
                fake.commands().len() == 2
            });
            i.send(InjectCmd::ReleaseAll);
            assert!(i.drop_devices().is_some());
            pump(&mut i, &mut rx, "Closed", closed);
            assert_eq!(
                fake.calls(),
                [
                    DeviceCall::Open(Some(km)),
                    DeviceCall::Cmd("Motion { x: 1.0, y: 1.0 }".into()),
                    DeviceCall::Cmd(format!("Key {{ code: {}, pressed: true }}", 30 + session)),
                    DeviceCall::Cmd("ReleaseAll".into()),
                    DeviceCall::Sync,
                    DeviceCall::Drop,
                ],
                "session {session}"
            );
            fake.clear();
        }
        // A target-only peer keeps them while entered or recently used.
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        pump(&mut i, &mut rx, "the devices", |_| {
            !fake.commands().is_empty()
        });
        i.maintain(true);
        i.maintain(false);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!fake.calls().contains(&DeviceCall::Drop));
    }

    /// Devices that cannot be made end the session (B-05's path), and the next session within
    /// [`OPEN_RETRY`] ends at once instead of asking again: one try per second, not per motion.
    #[test]
    fn a_failed_open_ends_the_session_and_the_next_try_waits() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        fake.fail_opens(true);
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        i.send(key(30, true));
        assert!(i.take_gone().is_none(), "not known yet");
        let until = Instant::now() + Duration::from_secs(5);
        let why = loop {
            if let Ok(e) = rx.try_recv() {
                assert!(i.event(e).is_none(), "OpenFailed is the injector's");
            }
            if let Some(why) = i.take_gone() {
                break why;
            }
            assert!(Instant::now() < until, "no OpenFailed");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(why.contains("no compositor"), "{why}");
        // net ends the session: its ReleaseAll; then a new session at once.
        i.send(InjectCmd::ReleaseAll);
        i.send(InjectCmd::Motion { x: 2.0, y: 2.0 });
        assert!(i.take_gone().is_some(), "within a second: no new try");
        i.send(InjectCmd::ReleaseAll);
        std::thread::sleep(OPEN_RETRY);
        fake.fail_opens(false);
        i.send(InjectCmd::Motion { x: 3.0, y: 3.0 });
        pump(&mut i, &mut rx, "new devices", |_| {
            !fake.commands().is_empty()
        });
        assert_eq!(
            fake.calls(),
            [
                DeviceCall::Refused,
                DeviceCall::Open(None),
                DeviceCall::Cmd("Motion { x: 3.0, y: 3.0 }".into())
            ]
        );
        assert!(i.take_gone().is_none());
    }

    /// B-05 on this path: the devices' thread dies; the next send reports it once, the rest of
    /// the session is dropped, the dead devices are dropped, and the next session gets new ones.
    #[test]
    fn a_dead_device_thread_is_reported_once() {
        let fake = FakeDevices::new();
        let (mut i, mut rx) = started(&fake);
        i.send(InjectCmd::Motion { x: 1.0, y: 1.0 });
        pump(&mut i, &mut rx, "the devices", |_| {
            !fake.commands().is_empty()
        });
        fake.die();
        std::thread::sleep(Duration::from_millis(50));
        i.send(key(30, true));
        i.send(key(31, true));
        assert!(i.take_gone().is_some());
        assert!(i.take_gone().is_none(), "once");
        pump(&mut i, &mut rx, "the dead devices dropped", closed);
        i.send(InjectCmd::ReleaseAll);
        i.send(InjectCmd::Motion { x: 2.0, y: 2.0 });
        pump(&mut i, &mut rx, "new devices", |_| {
            fake.commands().len() == 2
        });
        assert_eq!(
            fake.calls(),
            [
                DeviceCall::Open(None),
                DeviceCall::Cmd("Motion { x: 1.0, y: 1.0 }".into()),
                DeviceCall::Drop,
                DeviceCall::Open(None),
                DeviceCall::Cmd("Motion { x: 2.0, y: 2.0 }".into()),
            ]
        );
    }

    #[test]
    fn recorder_keeps_order() {
        let r = Recorder::new();
        let mut sink: Box<dyn Injector> = Box::new(r.clone());
        sink.send(InjectCmd::ReleaseAll);
        sink.send(InjectCmd::Motion { x: 1.0, y: 2.0 });
        assert_eq!(
            r.commands(),
            vec![InjectCmd::ReleaseAll, InjectCmd::Motion { x: 1.0, y: 2.0 }]
        );
    }
}
