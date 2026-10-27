//! The devices' owner: one long-lived thread that creates, syncs and
//! drops the virtual devices, so the caller never waits on the compositor or
//! Hyprland.
//!
//! The caller asks with [`Owner::open`] and [`Owner::close`], which never
//! block, and the answers come back as [`DeviceEvent`]s through the sink, in
//! the order asked. An open hands back an [`InputSender`]: input goes from the
//! caller straight to the devices' channel, as before, and never through this
//! thread. Only one set of devices exists at a time.

use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::Duration;

use crate::{
    Error, EventSink, Input, InputConfig, InputEvent, InputSender, Layout, PointerMode, Result,
    Target,
};

/// Capacity of the request channel. Requests are rare (an open and a close
/// per session), so a full channel means the thread has been stuck for many
/// sessions.
pub const OWNER_CAPACITY: usize = 64;

/// How long a close waits for the compositor to take what was sent before
/// the devices go. A compositor that does not answer gets the release and
/// the destroy requests when it reads its socket again.
const CLOSE_SYNC: Duration = Duration::from_secs(1);

/// How long dropping the [`Owner`] waits for its thread to drop the devices and end: the close
/// sync, then the inject thread's own shutdown wait. A thread stuck longer (in a Wayland connect
/// that hangs) is left behind, so the caller's own end is bounded.
const STOP_WAIT: Duration = Duration::from_millis(1500);

/// What the owner thread tells the caller.
#[derive(Debug)]
pub enum DeviceEvent {
    /// The devices of open request `seq` exist and their start-up reset
    /// reached the compositor: send input through `sender`.
    Opened { seq: u64, sender: InputSender },
    /// Open request `seq` failed: no devices exist.
    OpenFailed { seq: u64, error: String },
    /// Close request `seq` is done: everything sent before it reached the
    /// compositor (or the sync timed out), and the devices are gone.
    Closed { seq: u64 },
    /// An event of the devices' own thread.
    Input(InputEvent),
}

/// Makes the virtual devices. The owner thread calls it, so it may block.
pub trait Opener: Send + 'static {
    /// Create the devices with `keymap` (`None`: this machine's). The
    /// devices' own events go to `sink`.
    fn open(&mut self, keymap: Option<String>, sink: EventSink) -> Result<Box<dyn Devices>>;
}

/// Virtual devices that exist. Dropping them releases everything and
/// destroys them, and may block.
pub trait Devices: Send {
    /// Where input goes: the devices' own channel.
    fn sender(&self) -> Result<InputSender>;
    /// Block until the compositor has handled everything sent so far, at
    /// most `within`.
    fn sync(&self, within: Duration) -> Result<()>;
}

impl Devices for Input {
    fn sender(&self) -> Result<InputSender> {
        Input::sender(self)
    }

    fn sync(&self, within: Duration) -> Result<()> {
        self.sync_within(within)
    }
}

/// The shipping devices: an unbound pointer and a keyboard on
/// `target`, the layout kept in step with Hyprland's monitors by
/// [`Input::follow_layout`].
#[derive(Debug, Clone, Default)]
pub struct WaylandOpener {
    pub target: Target,
}

impl Opener for WaylandOpener {
    fn open(&mut self, keymap: Option<String>, sink: EventSink) -> Result<Box<dyn Devices>> {
        let input = Input::start(
            InputConfig {
                target: self.target.clone(),
                pointer: PointerMode::Unbound,
                layout: Layout::default(),
                keymap,
            },
            sink,
        )?;
        input.follow_layout(&self.target)?;
        Ok(Box::new(input))
    }
}

enum Request {
    Open {
        seq: u64,
        keymap: Option<String>,
    },
    Close {
        seq: u64,
    },
    /// Create the devices, sync, drop them, and answer on `reply`.
    Reset {
        keymap: Option<String>,
        reply: mpsc::Sender<Result<()>>,
    },
    Sync {
        within: Duration,
        reply: mpsc::Sender<()>,
    },
}

type Sink = Arc<dyn Fn(DeviceEvent) + Send + Sync>;

/// Handle to the owner thread. Dropping it drops the devices (a final sync
/// first) and joins the thread, waiting at most [`STOP_WAIT`].
pub struct Owner {
    tx: Option<SyncSender<Request>>,
    join: Option<std::thread::JoinHandle<()>>,
    /// Closes when the thread ends.
    done: mpsc::Receiver<()>,
}

impl Owner {
    /// Start the thread. No devices exist until the first [`Owner::open`]
    /// or [`Owner::reset`].
    pub fn spawn(
        opener: Box<dyn Opener>,
        sink: impl Fn(DeviceEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(OWNER_CAPACITY);
        let (done_tx, done) = mpsc::channel::<()>();
        let sink: Sink = Arc::new(sink);
        let join = std::thread::Builder::new()
            .name("ferry-devices".into())
            .spawn(move || {
                let _done = done_tx;
                run(opener, &sink, &rx);
            })?;
        Ok(Self {
            tx: Some(tx),
            join: Some(join),
            done,
        })
    }

    /// Create the devices for open request `seq`; [`DeviceEvent::Opened`] or
    /// [`DeviceEvent::OpenFailed`] follows. Devices that still exist are
    /// closed first. Never blocks.
    pub fn open(&self, seq: u64, keymap: Option<String>) -> Result<()> {
        self.request(Request::Open { seq, keymap })
    }

    /// Sync, then drop the devices; [`DeviceEvent::Closed`] follows, also when
    /// none existed. Never blocks.
    pub fn close(&self, seq: u64) -> Result<()> {
        self.request(Request::Close { seq })
    }

    /// Create the devices, so their start-up reset reaches the compositor,
    /// then drop them. **Blocks** until done: for start-up, before any
    /// thread that must not wait depends on it.
    pub fn reset(&self, keymap: Option<String>) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.request(Request::Reset { keymap, reply })?;
        rx.recv().map_err(|_| Error::ThreadGone)?
    }

    /// Block until the devices, if any, have taken everything sent so far,
    /// at most `within`, whatever the thread is busy with.
    pub fn sync_within(&self, within: Duration) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.request(Request::Sync { within, reply })?;
        match rx.recv_timeout(within) {
            Ok(()) => Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout(within)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::ThreadGone),
        }
    }

    fn request(&self, r: Request) -> Result<()> {
        let tx = self.tx.as_ref().ok_or(Error::ThreadGone)?;
        match tx.try_send(r) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Error::Overflow),
            Err(TrySendError::Disconnected(_)) => Err(Error::ThreadGone),
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // The thread drops the devices once the channel closes.
        drop(self.tx.take());
        if let Err(mpsc::RecvTimeoutError::Timeout) = self.done.recv_timeout(STOP_WAIT) {
            tracing::warn!("the device owner thread did not end in time: left behind");
            return;
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Reports a panic of the owner thread as an error of the devices, so the
/// caller ends its session instead of waiting for an answer.
struct PanicGuard<'a>(&'a Sink);

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            (self.0)(DeviceEvent::Input(InputEvent::Error(
                "the device owner thread panicked".into(),
            )));
        }
    }
}

fn run(mut opener: Box<dyn Opener>, sink: &Sink, rx: &mpsc::Receiver<Request>) {
    let _guard = PanicGuard(sink);
    let device_sink = || -> EventSink {
        let sink = Arc::clone(sink);
        Box::new(move |e| sink(DeviceEvent::Input(e)))
    };
    let mut devices: Option<Box<dyn Devices>> = None;
    while let Ok(req) = rx.recv() {
        match req {
            Request::Open { seq, keymap } => {
                close(&mut devices);
                let opened = opener
                    .open(keymap, device_sink())
                    .and_then(|d| Ok((d.sender()?, d)));
                match opened {
                    Ok((sender, d)) => {
                        tracing::debug!(seq, "virtual devices created");
                        devices = Some(d);
                        sink(DeviceEvent::Opened { seq, sender });
                    }
                    Err(e) => sink(DeviceEvent::OpenFailed {
                        seq,
                        error: e.to_string(),
                    }),
                }
            }
            Request::Close { seq } => {
                close(&mut devices);
                sink(DeviceEvent::Closed { seq });
            }
            Request::Reset { keymap, reply } => {
                close(&mut devices);
                let mut reset = opener.open(keymap, device_sink()).map(Some);
                if let Ok(d) = &mut reset {
                    close(d);
                }
                let _ = reply.send(reset.map(|_| ()));
            }
            Request::Sync { within, reply } => {
                if let Some(d) = &devices
                    && let Err(e) = d.sync(within)
                {
                    tracing::warn!(error = %e, "ferry-inject sync");
                }
                let _ = reply.send(());
            }
        }
    }
    close(&mut devices);
}

/// Sync, then drop the devices, if any.
fn close(devices: &mut Option<Box<dyn Devices>>) {
    if let Some(d) = devices.take() {
        if let Err(e) = d.sync(CLOSE_SYNC) {
            tracing::warn!(error = %e, "ferry-inject sync before the drop");
        }
        drop(d);
        tracing::debug!("virtual devices dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputCmd;
    use std::sync::Mutex;
    use std::time::Instant;

    /// What the fake devices saw, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Seen {
        Open(Option<String>),
        Cmd(String),
        Sync,
        Drop,
    }

    #[derive(Clone, Default)]
    struct Fake {
        seen: Arc<Mutex<Vec<Seen>>>,
        open_delay: Arc<Mutex<Duration>>,
        fail: Arc<Mutex<bool>>,
    }

    impl Fake {
        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
        fn push(&self, s: Seen) {
            self.seen.lock().unwrap().push(s);
        }
    }

    struct FakeDevices {
        sender: InputSender,
        fake: Fake,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl Opener for Fake {
        fn open(&mut self, keymap: Option<String>, _: EventSink) -> Result<Box<dyn Devices>> {
            std::thread::sleep(*self.open_delay.lock().unwrap());
            if *self.fail.lock().unwrap() {
                return Err(Error::Input("no compositor".into()));
            }
            self.push(Seen::Open(keymap));
            let (sender, rx) = InputSender::detached(16);
            let fake = self.clone();
            let join = std::thread::spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        InputCmd::Sync(reply) => {
                            let _ = reply.send(());
                        }
                        InputCmd::Stop => return,
                        cmd => fake.push(Seen::Cmd(format!("{cmd:?}"))),
                    }
                }
            });
            Ok(Box::new(FakeDevices {
                sender,
                fake: self.clone(),
                join: Some(join),
            }))
        }
    }

    impl Devices for FakeDevices {
        fn sender(&self) -> Result<InputSender> {
            Ok(self.sender.clone())
        }
        fn sync(&self, within: Duration) -> Result<()> {
            let (tx, rx) = mpsc::channel();
            self.sender.send(InputCmd::Sync(tx))?;
            rx.recv_timeout(within)
                .map_err(|_| Error::Timeout(within))?;
            self.fake.push(Seen::Sync);
            Ok(())
        }
    }

    impl Drop for FakeDevices {
        fn drop(&mut self) {
            let _ = self.sender.tx.send(InputCmd::Stop);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
            self.fake.push(Seen::Drop);
        }
    }

    fn owner(fake: &Fake) -> (Owner, mpsc::Receiver<DeviceEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let owner = Owner::spawn(Box::new(fake.clone()), move |e| {
            let _ = tx.lock().unwrap().send(e);
        })
        .unwrap();
        (owner, rx)
    }

    fn next(rx: &mpsc::Receiver<DeviceEvent>) -> DeviceEvent {
        rx.recv_timeout(Duration::from_secs(5)).expect("an event")
    }

    /// An open that blocks does not block the caller; the answer comes later.
    #[test]
    fn a_slow_open_never_blocks_the_caller() {
        let fake = Fake::default();
        *fake.open_delay.lock().unwrap() = Duration::from_millis(500);
        let (owner, rx) = owner(&fake);
        let t = Instant::now();
        owner.open(1, Some("km".into())).unwrap();
        owner.close(2).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        let DeviceEvent::Opened { seq: 1, sender } = next(&rx) else {
            panic!("not Opened")
        };
        assert!(t.elapsed() >= Duration::from_millis(500));
        drop(sender);
        assert!(matches!(next(&rx), DeviceEvent::Closed { seq: 2 }));
        assert_eq!(
            fake.seen(),
            [Seen::Open(Some("km".into())), Seen::Sync, Seen::Drop]
        );
    }

    /// Input sent through the handed-back sender reaches the devices in order, and a close syncs
    /// it before the devices go; a sender kept past the close does not hold the close up.
    #[test]
    fn input_goes_straight_to_the_devices_and_close_syncs_first() {
        let fake = Fake::default();
        let (owner, rx) = owner(&fake);
        owner.open(1, None).unwrap();
        let DeviceEvent::Opened { sender, .. } = next(&rx) else {
            panic!("not Opened")
        };
        sender
            .send(InputCmd::Key {
                code: 30,
                pressed: true,
            })
            .unwrap();
        sender.send(InputCmd::ReleaseAll).unwrap();
        owner.close(2).unwrap();
        assert!(matches!(next(&rx), DeviceEvent::Closed { seq: 2 }));
        assert_eq!(
            fake.seen(),
            [
                Seen::Open(None),
                Seen::Cmd("Key { code: 30, pressed: true }".into()),
                Seen::Cmd("ReleaseAll".into()),
                Seen::Sync,
                Seen::Drop
            ]
        );
        assert!(matches!(
            sender.send(InputCmd::ReleaseAll),
            Err(Error::ThreadGone)
        ));
    }

    /// One set at a time: a second open closes the first; a failed open says why.
    #[test]
    fn one_set_at_a_time_and_failures_answer() {
        let fake = Fake::default();
        let (owner, rx) = owner(&fake);
        owner.open(1, None).unwrap();
        owner.open(2, Some("b".into())).unwrap();
        assert!(matches!(next(&rx), DeviceEvent::Opened { seq: 1, .. }));
        assert!(matches!(next(&rx), DeviceEvent::Opened { seq: 2, .. }));
        *fake.fail.lock().unwrap() = true;
        owner.open(3, None).unwrap();
        match next(&rx) {
            DeviceEvent::OpenFailed { seq: 3, error } => assert!(error.contains("no compositor")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            fake.seen(),
            [
                Seen::Open(None),
                Seen::Sync,
                Seen::Drop,
                Seen::Open(Some("b".into())),
                Seen::Sync,
                Seen::Drop
            ]
        );
    }

    /// Start-up: `reset` creates, syncs and drops, and blocks until done.
    #[test]
    fn reset_creates_syncs_and_drops() {
        let fake = Fake::default();
        let (owner, _rx) = owner(&fake);
        owner.reset(None).unwrap();
        assert_eq!(fake.seen(), [Seen::Open(None), Seen::Sync, Seen::Drop]);
        *fake.fail.lock().unwrap() = true;
        assert!(owner.reset(None).is_err());
    }

    /// The shutdown sync is bounded even while the thread is stuck in an open.
    #[test]
    fn sync_within_is_bounded() {
        let fake = Fake::default();
        *fake.open_delay.lock().unwrap() = Duration::from_millis(800);
        let (owner, _rx) = owner(&fake);
        owner.open(1, None).unwrap();
        let t = Instant::now();
        assert!(matches!(
            owner.sync_within(Duration::from_millis(100)),
            Err(Error::Timeout(_))
        ));
        assert!(
            t.elapsed() < Duration::from_millis(400),
            "{:?}",
            t.elapsed()
        );
    }

    /// Dropping an owner stuck in an open that hangs is bounded.
    #[test]
    fn dropping_a_stuck_owner_is_bounded() {
        let fake = Fake::default();
        *fake.open_delay.lock().unwrap() = Duration::from_secs(4);
        let (owner, _rx) = owner(&fake);
        owner.open(1, None).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let t = Instant::now();
        drop(owner);
        let took = t.elapsed();
        assert!(
            took >= STOP_WAIT && took < STOP_WAIT + Duration::from_millis(500),
            "{took:?}"
        );
    }

    /// Dropping the owner drops the devices.
    #[test]
    fn dropping_the_owner_drops_the_devices() {
        let fake = Fake::default();
        let (owner, rx) = owner(&fake);
        owner.open(1, None).unwrap();
        assert!(matches!(next(&rx), DeviceEvent::Opened { .. }));
        drop(owner);
        assert_eq!(fake.seen(), [Seen::Open(None), Seen::Sync, Seen::Drop]);
    }
}
