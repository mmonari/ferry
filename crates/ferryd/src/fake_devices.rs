//! Stand-in virtual devices behind ferry-inject's real owner thread, for in-process tests of the
//! whole target path: `InjectBackend::Devices(Box::new(fake.clone()))` runs the
//! production [`DeviceInjector`](crate::inject::DeviceInjector) and `Owner` with these in place
//! of the compositor. They record what reaches them, can be slow to create or to sync, as a
//! stalled compositor or Hyprland would be, and never touch any input device, so a daemon that
//! uses them may run on the live desktop.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_inject::{CHANNEL_CAPACITY, Devices, EventSink, InputCmd, InputSender, Opener};

/// What reached the fake devices.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceCall {
    /// Devices were created with this keymap (`None`: the machine's). Logged once creation is
    /// done, after the open delay.
    Open(Option<String>),
    /// A command, as ferry-inject's `InputCmd` prints it: `Key { code: 42, pressed: true }`.
    Cmd(String),
    /// A sync was answered (after the sync delay).
    Sync,
    /// The devices were dropped.
    Drop,
    /// Creating devices failed ([`FakeDevices::fail_opens`]).
    Refused,
}

/// Clone it, hand one copy to the daemon, keep the other.
#[derive(Clone, Default)]
pub struct FakeDevices {
    log: Arc<Mutex<Vec<(Instant, DeviceCall)>>>,
    open_delay: Arc<Mutex<Duration>>,
    sync_delay: Arc<Mutex<Duration>>,
    fail: Arc<Mutex<bool>>,
    /// The sender of the devices that exist, for [`FakeDevices::die`].
    current: Arc<Mutex<Option<InputSender>>>,
}

impl FakeDevices {
    pub fn new() -> Self {
        Self::default()
    }

    /// From now on, creating devices takes `d` (a Wayland connect, roundtrips and a Hyprland
    /// request that hang).
    pub fn set_open_delay(&self, d: Duration) {
        if let Ok(mut x) = self.open_delay.lock() {
            *x = d;
        }
    }

    /// From now on, a sync is answered `d` after everything before it.
    pub fn set_sync_delay(&self, d: Duration) {
        if let Ok(mut x) = self.sync_delay.lock() {
            *x = d;
        }
    }

    /// From now on, creating devices fails (or works again), as without a compositor.
    pub fn fail_opens(&self, fail: bool) {
        if let Ok(mut x) = self.fail.lock() {
            *x = fail;
        }
    }

    /// The devices' thread ends now, as ferry-inject's does on a fatal Wayland error: every
    /// later send to them is `ThreadGone`.
    pub fn die(&self) {
        if let Some(tx) = self.current.lock().ok().and_then(|mut c| c.take()) {
            let _ = tx.send(InputCmd::Stop);
        }
    }

    /// The counters the current devices report, as ferry-inject's thread would have counted them.
    pub fn set_stats(&self, s: ferry_inject::InjectStats) {
        if let Some(tx) = self.current.lock().ok().and_then(|c| c.clone()) {
            tx.set_detached_stats(s);
        }
    }

    /// Everything so far, with when it happened.
    pub fn timed(&self) -> Vec<(Instant, DeviceCall)> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Everything so far, in order.
    pub fn calls(&self) -> Vec<DeviceCall> {
        self.timed().into_iter().map(|(_, c)| c).collect()
    }

    /// The commands so far, in order.
    pub fn commands(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                DeviceCall::Cmd(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    pub fn clear(&self) {
        if let Ok(mut l) = self.log.lock() {
            l.clear();
        }
    }

    fn push(&self, c: DeviceCall) {
        if let Ok(mut l) = self.log.lock() {
            l.push((Instant::now(), c));
        }
    }

    fn delay(d: &Mutex<Duration>) -> Duration {
        d.lock().map(|d| *d).unwrap_or_default()
    }
}

/// One set of fake devices: a thread that drains the channel, as ferry-inject's thread does.
struct Fake {
    sender: InputSender,
    join: Option<std::thread::JoinHandle<()>>,
    log: FakeDevices,
}

impl Opener for FakeDevices {
    fn open(
        &mut self,
        keymap: Option<String>,
        _: EventSink,
    ) -> ferry_inject::Result<Box<dyn Devices>> {
        std::thread::sleep(Self::delay(&self.open_delay));
        if self.fail.lock().is_ok_and(|f| *f) {
            self.push(DeviceCall::Refused);
            return Err(ferry_inject::Error::Input("no compositor (fake)".into()));
        }
        self.push(DeviceCall::Open(keymap));
        let (sender, rx) = InputSender::detached(CHANNEL_CAPACITY);
        if let Ok(mut c) = self.current.lock() {
            *c = Some(sender.clone());
        }
        let log = self.clone();
        let join = std::thread::Builder::new()
            .name("fake-devices".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        InputCmd::Sync(reply) => {
                            std::thread::sleep(Self::delay(&log.sync_delay));
                            log.push(DeviceCall::Sync);
                            let _ = reply.send(());
                        }
                        InputCmd::Stop => return,
                        cmd => log.push(DeviceCall::Cmd(format!("{cmd:?}"))),
                    }
                }
            })?;
        Ok(Box::new(Fake {
            sender,
            join: Some(join),
            log: self.clone(),
        }))
    }
}

impl Devices for Fake {
    fn sender(&self) -> ferry_inject::Result<InputSender> {
        Ok(self.sender.clone())
    }

    fn sync(&self, within: Duration) -> ferry_inject::Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.sender.send(InputCmd::Sync(tx))?;
        rx.recv_timeout(within)
            .map_err(|_| ferry_inject::Error::Timeout(within))
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.sender.send(InputCmd::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.log.push(DeviceCall::Drop);
    }
}
