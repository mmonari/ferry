// from gliff crates/hypr-input/src/lib.rs @ 089662f
//! Input injection into Hyprland: a virtual keyboard (with an xkb state that
//! derives modifiers from key events) and one virtual pointer over the whole
//! desktop (or, for the S8 control, one output-bound pointer per monitor).
//!
//! Everything runs on a thread of its own. The owner holds an [`Input`] and
//! sends it [`InputCmd`]s; nothing in between adds a hop.
//!
//! **Devices on demand** ([`Owner`]). Creating the devices waits on the
//! compositor (a Wayland connect, two roundtrips) and on Hyprland (the
//! monitors), and dropping them waits for a sync and the thread's end. A
//! caller that must never block (ferryd's `net` thread) leaves that to an
//! [`Owner`]: a long-lived thread that opens, syncs and drops the devices on
//! request and answers with a [`DeviceEvent`]. Input still goes straight to
//! the devices' channel through the [`InputSender`] the owner hands back.
//!
//! # Command surface
//!
//! | Command | Meaning |
//! |---|---|
//! | [`InputCmd::Key`] | key press or release, evdev code |
//! | [`InputCmd::Button`] | pointer button press or release, evdev `BTN_*` |
//! | [`InputCmd::Motion`] | absolute position, global logical layout coordinates |
//! | [`InputCmd::MotionRel`] | relative motion, logical pixels |
//! | [`InputCmd::Axis`], [`InputCmd::AxisStop`] | scroll: `axis`, then `axis_source`, then `frame` |
//! | [`InputCmd::SetKeymap`] | a keymap change: always a **new** virtual keyboard |
//! | [`InputCmd::SetLayout`] | the target's monitor layout, for absolute motion |
//! | [`InputCmd::ReleaseAll`] | release every key and button this crate holds |
//! | [`InputCmd::Sync`] | reply once the compositor has handled everything before it |
//!
//! # Guarantees
//!
//! - **Start-up reset.** Every virtual keyboard the crate creates
//!   first releases the eight modifier keycodes ([`MODIFIER_KEYCODES`]) and
//!   sends `modifiers(0,0,0,0)`, before any forwarded key. A keyboard created
//!   without a keymap carries this machine's ([`system_keymap`]), so the reset
//!   does not change the layout the seat shows.
//! - **No hop, one flush per wake-up.** The thread drains its channel, then
//!   flushes once. It never blocks in `flush`.
//! - **Keys are never dropped.** While the Wayland socket would block, keys,
//!   buttons and scroll wait in a ring of 256 in order; consecutive motion (and
//!   scroll of the same axis) coalesces. A ring that overflows, or a command
//!   channel that does (1024), ends in a defined release: [`InputEvent::Overflow`],
//!   `release_all`, new devices, and a count in [`InjectStats`].
//! - **`release_all` works from a held set the crate keeps itself**, so it
//!   releases what the compositor was told, whatever the caller thinks is held.
//! - **A keymap re-upload is never sent.** Hyprland ignores it; a keymap
//!   change creates a new keyboard, replays the held keys on it and retires the
//!   old one after a grace period.
//!
//! [`Input::follow_layout`] keeps the pointer's layout in step with the
//! compositor's monitors.

#![forbid(unsafe_code)]

mod held;
mod keymap;
mod keymap_watch;
mod layout_follow;
mod nudge;
mod owner;
pub mod pointer;
mod ring;
mod thread;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use calloop::channel::SyncSender;
use std::sync::mpsc::TrySendError;

pub use ferry_wl::{OutputInfo, Target};
pub use keymap::{
    KeyState, KeymapNames, canonical_keymap, key_name, keymap_from_names, system_keymap,
};
pub use keymap_watch::{KeymapSink, watch_keymap};
pub use nudge::nudge;
pub use owner::{DeviceEvent, Devices, OWNER_CAPACITY, Opener, Owner, WaylandOpener};
pub use pointer::{Layout, OutputRect};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Wayland(#[from] ferry_wl::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("xkb: {0}")]
    Xkb(String),
    #[error("input: {0}")]
    Input(String),
    #[error("input thread is gone")]
    ThreadGone,
    /// A sync the compositor did not answer in time.
    #[error("the compositor did not answer within {0:?}")]
    Timeout(Duration),
    /// The command channel is full. The command was not queued. The thread
    /// answers it with a release of everything held ([`InputEvent::Overflow`]),
    /// so the owner needs no recovery of its own beyond counting it.
    #[error("input command channel is full")]
    Overflow,
    #[error("hyprland ipc: {0}")]
    Ipc(#[from] ferry_hypr_ipc::Error),
}

impl From<wayland_client::DispatchError> for Error {
    fn from(e: wayland_client::DispatchError) -> Self {
        Error::Wayland(ferry_wl::Error::Wayland(e.to_string()))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The eight modifier keycodes (evdev): Ctrl L/R, Shift L/R, Alt L/R, Super L/R.
/// Released on every new virtual keyboard.
pub const MODIFIER_KEYCODES: [u32; 8] = [29, 97, 42, 54, 56, 100, 125, 126];

/// Capacity of the command channel between the owner and the inject thread
/// (architecture §3: 1024 commands).
pub const CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisSource {
    Wheel,
    Finger,
    Continuous,
}

/// How absolute motion reaches the compositor (spike S8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerMode {
    /// One `create_virtual_pointer` with no output; `motion_absolute` is
    /// expressed against the layout bounding box. The shipping mode.
    Unbound,
    /// One `create_virtual_pointer_with_output` per monitor, picked by
    /// containment. Kept as the S8 control; not used by ferry.
    PerOutput,
}

#[derive(Debug, Clone)]
pub struct InputConfig {
    pub target: Target,
    pub pointer: PointerMode,
    /// Logical layout of the target's monitors (from Hyprland IPC, which
    /// knows fractional scales). [`Input::follow_layout`] keeps it current.
    pub layout: Layout,
    /// Full xkb keymap text (format v1). `None` uses this machine's keymap: the names
    /// Hyprland lists for its main keyboard, compiled here; plain US if Hyprland can't say.
    pub keymap: Option<String>,
}

/// One command for the inject thread. Commands run in the order sent.
#[derive(Debug)]
pub enum InputCmd {
    /// A key edge. `code` is an evdev keycode (xkb keycode minus 8).
    Key { code: u32, pressed: bool },
    /// Absolute pointer position in global logical layout coordinates.
    /// Aims at the centre of the pixel.
    Motion { x: f64, y: f64 },
    /// Relative motion, logical pixels.
    MotionRel { dx: f64, dy: f64 },
    /// A button edge. `button` is an evdev `BTN_*` code.
    Button { button: u32, pressed: bool },
    /// One scroll event, sent as `axis`, `axis_source`, `frame`.
    /// `value120` is the hi-res wheel delta (one notch = 120) and is only
    /// meaningful with [`AxisSource::Wheel`]; the wheel value is then
    /// `value120 * 15 / 120` and `value` is ignored.
    Axis {
        axis: Axis,
        source: AxisSource,
        value: f64,
        value120: Option<i32>,
    },
    /// End of a finger or continuous scroll on `axis`. `source` is the source
    /// of the scroll that ends (`axis_stop`, then `axis_source`, then `frame`).
    AxisStop { axis: Axis, source: AxisSource },
    /// Use this xkb keymap (text, format v1). A different keymap makes a new
    /// virtual keyboard; the same text is a no-op.
    SetKeymap(String),
    /// The target's monitor layout, for absolute motion.
    SetLayout(Layout),
    /// Release every key and button held, and send `modifiers(0,0,0,0)`.
    ReleaseAll,
    /// Reply once the compositor has handled every request sent so far.
    Sync(mpsc::Sender<()>),
    /// End the thread: release everything, destroy the devices.
    Stop,
}

/// What the inject thread tells its owner.
#[derive(Debug)]
pub enum InputEvent {
    /// The devices exist and the start-up reset reached the compositor.
    Ready,
    /// A bound pointer was re-bound after its output changed (`PerOutput`).
    Rebound(String),
    /// A queue overflowed. Everything held was released and the devices were
    /// rebuilt; queued input was dropped. `queued` is how much waited.
    Overflow {
        queued: usize,
    },
    /// The devices were rebuilt after an [`InputEvent::Overflow`] and nothing
    /// is held any more: the owner clears its own held set.
    Reset,
    /// A [`InputCmd::SetKeymap`] was refused (the text does not compile, or
    /// the new keyboard could not be created). The old keymap stays in force.
    KeymapRejected(String),
    Error(String),
}

pub type EventSink = Box<dyn FnMut(InputEvent) + Send>;

/// Counters the hot path keeps (relaxed atomics, no logging).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InjectStats {
    /// Commands merged into a queued one (motion, scroll).
    pub coalesced: u64,
    /// Flushes that hit `WouldBlock`.
    pub would_block: u64,
    /// Commands that had to wait in the ring because the socket was blocked.
    pub queued: u64,
    /// Defined releases after a ring or channel overflow (`inject_overflow`).
    pub overflows: u64,
    /// Virtual keyboards created, the first one included.
    pub keyboards: u64,
    /// Layouts applied by [`Input::follow_layout`].
    pub layout_updates: u64,
    /// Motion or scroll commands with a value that was not finite: dropped.
    /// ferry-core's target machine already drops them; this is the second
    /// guard.
    pub nonfinite_dropped: u64,
}

impl InjectStats {
    /// The counts that came after `earlier`, field by field (saturating at zero).
    pub fn since(self, earlier: InjectStats) -> InjectStats {
        InjectStats {
            coalesced: self.coalesced.saturating_sub(earlier.coalesced),
            would_block: self.would_block.saturating_sub(earlier.would_block),
            queued: self.queued.saturating_sub(earlier.queued),
            overflows: self.overflows.saturating_sub(earlier.overflows),
            keyboards: self.keyboards.saturating_sub(earlier.keyboards),
            layout_updates: self.layout_updates.saturating_sub(earlier.layout_updates),
            nonfinite_dropped: self
                .nonfinite_dropped
                .saturating_sub(earlier.nonfinite_dropped),
        }
    }

    /// Both counts added, field by field (saturating).
    pub fn plus(self, other: InjectStats) -> InjectStats {
        InjectStats {
            coalesced: self.coalesced.saturating_add(other.coalesced),
            would_block: self.would_block.saturating_add(other.would_block),
            queued: self.queued.saturating_add(other.queued),
            overflows: self.overflows.saturating_add(other.overflows),
            keyboards: self.keyboards.saturating_add(other.keyboards),
            layout_updates: self.layout_updates.saturating_add(other.layout_updates),
            nonfinite_dropped: self
                .nonfinite_dropped
                .saturating_add(other.nonfinite_dropped),
        }
    }
}

#[derive(Default)]
pub(crate) struct Counters {
    pub coalesced: AtomicU64,
    pub would_block: AtomicU64,
    pub queued: AtomicU64,
    pub overflows: AtomicU64,
    pub keyboards: AtomicU64,
    pub layout_updates: AtomicU64,
    pub nonfinite_dropped: AtomicU64,
}

/// State shared between the owner, the inject thread and the layout follower.
#[derive(Default)]
pub(crate) struct Shared {
    pub counters: Counters,
    /// Set when `send` found the channel full; the thread answers it.
    pub overflow: AtomicBool,
    /// Wakes the layout follower when an output changes on the Wayland side.
    pub layout_wake: Mutex<Option<std::os::unix::net::UnixStream>>,
}

impl Shared {
    pub(crate) fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn wake_layout(&self) {
        use std::io::Write;
        if let Ok(guard) = self.layout_wake.lock()
            && let Some(wake) = guard.as_ref()
        {
            let _ = (&*wake).write(&[1]);
        }
    }

    fn stats(&self) -> InjectStats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        InjectStats {
            coalesced: get(&c.coalesced),
            would_block: get(&c.would_block),
            queued: get(&c.queued),
            overflows: get(&c.overflows),
            keyboards: get(&c.keyboards),
            layout_updates: get(&c.layout_updates),
            nonfinite_dropped: get(&c.nonfinite_dropped),
        }
    }
}

/// A non-blocking handle to the inject thread's command channel. Clones go
/// to whoever sends input (ferryd's `net` thread, through [`Owner`]), so the
/// input itself takes one hop. It works as long as the thread runs; after
/// that every send is [`Error::ThreadGone`].
#[derive(Clone)]
pub struct InputSender {
    tx: SyncSender<InputCmd>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for InputSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputSender")
    }
}

impl InputSender {
    /// The devices' counters so far, readable from any thread without a hop through the
    /// inject thread. They start at zero with each set of devices.
    pub fn stats(&self) -> InjectStats {
        self.shared.stats()
    }

    /// Queue a command. Never blocks. A full channel returns
    /// [`Error::Overflow`] and makes the thread release everything held.
    pub fn send(&self, cmd: InputCmd) -> Result<()> {
        match self.tx.try_send(cmd) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                // A flag that publishes no data (the thread reads its queue, not this): Relaxed.
                self.shared.overflow.store(true, Ordering::Relaxed);
                Err(Error::Overflow)
            }
            Err(TrySendError::Disconnected(_)) => Err(Error::ThreadGone),
        }
    }

    /// Set the counters a [`detached`](Self::detached) sender reports, as the inject thread's
    /// would have counted them: for test doubles of [`Devices`].
    pub fn set_detached_stats(&self, s: InjectStats) {
        let c = &self.shared.counters;
        let set = |a: &AtomicU64, v: u64| a.store(v, Ordering::Relaxed);
        set(&c.coalesced, s.coalesced);
        set(&c.would_block, s.would_block);
        set(&c.queued, s.queued);
        set(&c.overflows, s.overflows);
        set(&c.keyboards, s.keyboards);
        set(&c.layout_updates, s.layout_updates);
        set(&c.nonfinite_dropped, s.nonfinite_dropped);
    }

    /// A sender with no inject thread behind it, for test doubles of
    /// [`Devices`]: what it sends arrives on the returned channel
    /// (`Channel::recv`), `capacity` deep.
    pub fn detached(capacity: usize) -> (Self, calloop::channel::Channel<InputCmd>) {
        let (tx, rx) = calloop::channel::sync_channel(capacity);
        let shared = Arc::new(Shared::default());
        (Self { tx, shared }, rx)
    }
}

/// Handle to the inject thread.
pub struct Input {
    cmd: Option<InputSender>,
    shared: Arc<Shared>,
    join: Option<std::thread::JoinHandle<()>>,
    follower: Mutex<Option<layout_follow::Follower>>,
}

impl Input {
    /// Start the thread, create the devices, run the start-up modifier reset
    /// and return once it reached the compositor.
    pub fn start(config: InputConfig, sink: EventSink) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let (cmd, join) = thread::spawn(config, sink, ready_tx, Arc::clone(&shared))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd: Some(InputSender {
                    tx: cmd,
                    shared: Arc::clone(&shared),
                }),
                shared,
                join: Some(join),
                follower: Mutex::new(None),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::ThreadGone),
        }
    }

    /// Queue a command. Never blocks. A full channel returns
    /// [`Error::Overflow`] and makes the thread release everything held.
    pub fn send(&self, cmd: InputCmd) -> Result<()> {
        self.cmd.as_ref().ok_or(Error::ThreadGone)?.send(cmd)
    }

    /// A handle that sends to this thread, for a caller that must not hold
    /// the `Input` itself (see [`Owner`]).
    pub fn sender(&self) -> Result<InputSender> {
        self.cmd.clone().ok_or(Error::ThreadGone)
    }

    /// A key edge: [`InputCmd::Key`].
    pub fn key(&self, code: u32, pressed: bool) -> Result<()> {
        self.send(InputCmd::Key { code, pressed })
    }

    /// A button edge: [`InputCmd::Button`].
    pub fn button(&self, button: u32, pressed: bool) -> Result<()> {
        self.send(InputCmd::Button { button, pressed })
    }

    /// Absolute motion: [`InputCmd::Motion`].
    pub fn motion(&self, x: f64, y: f64) -> Result<()> {
        self.send(InputCmd::Motion { x, y })
    }

    /// Relative motion: [`InputCmd::MotionRel`].
    pub fn motion_rel(&self, dx: f64, dy: f64) -> Result<()> {
        self.send(InputCmd::MotionRel { dx, dy })
    }

    /// One scroll event: [`InputCmd::Axis`].
    pub fn scroll(
        &self,
        axis: Axis,
        source: AxisSource,
        value: f64,
        value120: Option<i32>,
    ) -> Result<()> {
        self.send(InputCmd::Axis {
            axis,
            source,
            value,
            value120,
        })
    }

    /// End of a finger or continuous scroll: [`InputCmd::AxisStop`].
    pub fn scroll_stop(&self, axis: Axis, source: AxisSource) -> Result<()> {
        self.send(InputCmd::AxisStop { axis, source })
    }

    /// A keymap change: [`InputCmd::SetKeymap`].
    pub fn set_keymap(&self, text: String) -> Result<()> {
        self.send(InputCmd::SetKeymap(text))
    }

    /// A new monitor layout: [`InputCmd::SetLayout`].
    pub fn set_layout(&self, layout: Layout) -> Result<()> {
        self.send(InputCmd::SetLayout(layout))
    }

    /// Release every key and button held: [`InputCmd::ReleaseAll`].
    pub fn release_all(&self) -> Result<()> {
        self.send(InputCmd::ReleaseAll)
    }

    /// Block until the compositor has handled everything sent so far.
    pub fn sync(&self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.send(InputCmd::Sync(tx))?;
        rx.recv().map_err(|_| Error::ThreadGone)
    }

    /// [`Input::sync`], waiting at most `within`: [`Error::Timeout`] after
    /// that, with the sync still queued.
    pub fn sync_within(&self, within: Duration) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.send(InputCmd::Sync(tx))?;
        match rx.recv_timeout(within) {
            Ok(()) => Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout(within)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::ThreadGone),
        }
    }

    /// Counters since start.
    pub fn stats(&self) -> InjectStats {
        self.shared.stats()
    }

    /// Keep the pointer's layout equal to the compositor's monitors.
    ///
    /// Reads the layout now, then again after every monitor change: a monitor
    /// added or removed or a config reload (Hyprland's event socket,
    /// `socket2`), and a mode, scale or position change (a `wl_output` event
    /// on the inject connection, because `socket2` stays silent for those). A
    /// burst is debounced for 100 ms (a hot-plug passes through a transient
    /// 1x1 output, S11), and only a layout that differs is sent.
    ///
    /// The first layout is queued before this returns. Calling it twice is an
    /// error. The follower stops when the `Input` is dropped.
    pub fn follow_layout(&self, target: &Target) -> Result<()> {
        let tx = self.cmd.as_ref().ok_or(Error::ThreadGone)?.tx.clone();
        let mut slot = self
            .follower
            .lock()
            .map_err(|_| Error::Input("layout follower lock poisoned".into()))?;
        if slot.is_some() {
            return Err(Error::Input("already following the layout".into()));
        }
        *slot = Some(layout_follow::spawn(target, tx, Arc::clone(&self.shared))?);
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // The follower holds a sender clone: stop it first, or the channel
        // never closes.
        if let Ok(mut slot) = self.follower.lock()
            && let Some(f) = slot.take()
        {
            f.stop();
        }
        if let Some(cmd) = self.cmd.take() {
            let _ = cmd.tx.try_send(InputCmd::ReleaseAll);
            // Stop rather than wait for the channel to close: an
            // `InputSender` clone may outlive this handle. The thread
            // releases, destroys its devices and ends. `send` waits only
            // while the channel is full, which the thread drains.
            let _ = cmd.tx.send(InputCmd::Stop);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Protocol names the input path needs, for probing.
pub const REQUIRED_GLOBALS: &[&str] = &[
    "zwp_virtual_keyboard_manager_v1",
    "zwlr_virtual_pointer_manager_v1",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_since_and_plus_are_per_field_and_saturate() {
        let a = InjectStats {
            coalesced: 5,
            would_block: 2,
            keyboards: 1,
            ..InjectStats::default()
        };
        let b = InjectStats {
            coalesced: 9,
            would_block: 2,
            keyboards: 3,
            ..InjectStats::default()
        };
        let d = b.since(a);
        assert_eq!((d.coalesced, d.would_block, d.keyboards), (4, 0, 2));
        assert_eq!(a.since(b), InjectStats::default(), "never below zero");
        assert_eq!(a.plus(d), b);
    }

    /// The counters are readable through the handle that outlives the owner's reach.
    #[test]
    fn a_sender_reads_the_devices_counters() {
        let (sender, _rx) = InputSender::detached(4);
        assert_eq!(sender.stats(), InjectStats::default());
        sender.set_detached_stats(InjectStats {
            coalesced: 1,
            ..InjectStats::default()
        });
        assert_eq!(sender.stats().coalesced, 1);
    }
}
