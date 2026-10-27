//! ferryd: the daemon. It joins the pure machines (ferry-core), the transport (ferry-net), the
//! capture side (ferry-capture) and injection (ferry-inject) on four threads.
//!
//! # Threads
//!
//! | Thread | Loop | Module | Owns |
//! |---|---|---|---|
//! | `ctl` | tokio current-thread | [`ctl`], [`portal_driver`], [`server`], [`api`], [`notify`] | socket API, config and state files, layout → topology, the portal session, IPC lock checks, logind, the toasts (a task of its own, fed by a bounded queue) |
//! | `net` | tokio current-thread | [`net_thread`] | ferry-net endpoint and connections, the Hello exchange, the **TargetMachine**, Ping/Pong |
//! | `capture` | calloop | [`capture_thread`] | the EIS source, the **SourceMachine**, its one timer, the connection handles |
//! | `inject` | ferry-inject's own | [`inject`] | the virtual devices; ferry-inject's owner thread creates, syncs and drops them, so `net` never waits on the compositor |
//!
//! # Input is opt-in
//!
//! [`Options::new`] starts a daemon that never captures and never injects: `capture` is off and
//! [`InjectBackend::Off`] refuses every `Enter`. Tests and the CLI's end-to-end runs use it on the
//! live desktop safely. The `ferryd` binary opts in.

#![forbid(unsafe_code)]

pub mod api;
pub mod capture_thread;
pub mod chan;
pub mod clock;
pub mod config;
pub mod config_api;
pub mod ctl;
pub mod ctl_out;
pub mod doctor;
pub mod error;
pub mod events;
pub mod fake_devices;
pub mod fake_peer;
pub mod fake_portal;
pub mod fsutil;
pub mod inject;
pub mod keymap;
pub mod layout_store;
pub mod layout_sync;
pub mod live;
pub mod monitors;
pub mod net_thread;
pub mod netaddr;
pub mod notify;
pub mod pairing;
pub mod paths;
pub mod portal_driver;
pub mod probe;
pub mod ratelimit;
pub mod server;
pub mod state;
pub mod state_file;
pub mod ticker;
pub mod trust_store;
pub mod watchdog;

use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferry_core::PeerId;
use ferry_net::Identity;
use tokio::sync::mpsc;

pub use error::StartError;
pub use fake_devices::{DeviceCall, FakeDevices};
pub use fake_portal::FakePortal;
pub use inject::{InjectBackend, Injector, Recorder};
pub use monitors::MonitorSource;
pub use paths::Paths;

use chan::{CTL_CAP, CtlMsg, NetCmd};
use config::{CaptureMode, Config};
use layout_store::LayoutStore;
use live::SharedLive;
use state::{Me, State};
use state_file::StateFile;
use trust_store::TrustStore;
use watchdog::Notifier;

/// How to start a daemon. `Options::new(paths)` is the production setting; tests override the
/// rest.
pub struct Options {
    pub paths: Paths,
    /// `None`: `Identity::load_or_create(paths.data_dir)`.
    pub identity: Option<Identity>,
    /// `None`: `[::]:<config port>`. Tests bind `127.0.0.1:0`.
    pub bind: Option<SocketAddr>,
    /// `None`: the `[peer] name` of the config, else the hostname.
    pub name: Option<String>,
    pub monitors: MonitorSource,
    /// Send `READY=1` when `NOTIFY_SOCKET` is set, and `WATCHDOG=1` when `WATCHDOG_USEC` is.
    pub sd_notify: bool,
    /// Send the notifications to this datagram socket instead of `$NOTIFY_SOCKET` (tests; a
    /// leading `@` names an abstract socket).
    pub notify_socket: Option<PathBuf>,
    /// The watchdog interval, as if `WATCHDOG_USEC` said so (tests). `None`: the environment's,
    /// when `sd_notify` is on. The threads then wake, and `ctl` sends `WATCHDOG=1`, every half of
    /// it (at most every second).
    pub watchdog: Option<Duration>,
    /// Stop on SIGINT and SIGTERM. A test process leaves this off.
    pub handle_signals: bool,
    /// The uid the socket serves. `None`: this process's uid. Tests set another one to see a
    /// client of a different user turned away.
    pub socket_uid: Option<u32>,
    /// Run the capture side (portal, EIS, IPC release, lock checks, logind) when the config
    /// does not say `input.capture = "off"`. Off by default.
    pub capture: bool,
    /// Where target input goes. [`InjectBackend::Off`] by default.
    pub inject: InjectBackend,
    /// Tests: run the capture side (the SourceMachine) against this stand-in instead of the
    /// portal. It turns capture on without `capture`, and touches no compositor.
    pub fake_portal: Option<FakePortal>,
    /// Tests: how the net thread resolves host names. `None`: the system's resolver.
    pub lookup: Option<netaddr::Lookup>,
    /// Install the panic hook: a panic in any thread releases capture
    /// over Hyprland IPC and asks for the shutdown. It is process-wide, so it is off by default
    /// and the `ferryd` binary turns it on; [`panicked`] tells whether it ran.
    pub panic_hook: bool,
    /// Where the toasts go. [`notify::Mode::Off`] by default, so a test or a
    /// library user never reaches a bus; only the `ferryd` binary chooses the session bus.
    pub toasts: notify::Options,
}

impl Options {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            identity: None,
            bind: None,
            name: None,
            monitors: MonitorSource::default(),
            sd_notify: false,
            notify_socket: None,
            watchdog: None,
            handle_signals: false,
            socket_uid: None,
            capture: false,
            inject: InjectBackend::Off,
            fake_portal: None,
            lookup: None,
            panic_hook: false,
            toasts: notify::Options::default(),
        }
    }
}

/// A running daemon. Dropping it without `shutdown` or `wait` detaches the threads.
pub struct Daemon {
    socket_path: PathBuf,
    net_addr: SocketAddr,
    fingerprint: ferry_net::Fingerprint,
    tx: mpsc::Sender<CtlMsg>,
    join: Option<std::thread::JoinHandle<()>>,
    live: SharedLive,
}

impl Daemon {
    /// Start-up, design §9, in order:
    ///
    /// 1. config, identity, trust and layout;
    /// 2. bind the socket and the QUIC endpoint, then `READY=1`;
    /// 3. ferry-inject starts → `TargetEvent::Start` (before any connection is taken);
    /// 4. `ctl`: the IPC release, then `CreateSession` → `ConnectToEIS` (unless capture is off);
    /// 5. dial the pinned peers that have an address.
    pub fn start(opts: Options) -> Result<Self, StartError> {
        let Options {
            paths,
            identity,
            bind,
            name,
            monitors,
            sd_notify,
            notify_socket,
            watchdog,
            handle_signals,
            socket_uid,
            capture: capture_allowed,
            inject,
            fake_portal,
            lookup,
            panic_hook,
            toasts,
        } = opts;

        // 1. config, identity, trust and layout
        let (config, warnings) = Config::load(&paths.config_file).map_err(StartError::Config)?;
        for w in &warnings {
            tracing::warn!(file = %paths.config_file.display(), "{w}");
        }
        let budget = config.lock_check_budget();
        if budget != ferry_core::time::LockBudget::DEFAULT {
            tracing::info!(
                budget_ms = budget.millis(),
                active_poll_ms = budget.active_poll().0 / 1000,
                pending_timeout_ms = budget.pending_timeout().0 / 1000,
                "lock check budget"
            );
        }
        let identity = match identity {
            Some(i) => i,
            None => Identity::load_or_create(&paths.data_dir)?,
        };
        let fp = identity.fingerprint();
        let me = Me {
            name: name.unwrap_or_else(|| {
                if config.peer.name.is_empty() {
                    config::hostname()
                } else {
                    config.peer.name.clone()
                }
            }),
            fp,
            id: PeerId(fp.0),
        };
        let trust = TrustStore::load(&paths.trusted_file()).map_err(StartError::Store)?;
        let mut layout =
            LayoutStore::load(&paths.layout_file(), me.id).map_err(StartError::Store)?;
        let locked = StateFile::load(&paths.state_file())
            .map_err(StartError::Store)?
            .locked;
        let mut remember_monitors = false;
        let own_monitors = match monitors.read() {
            // Monitors that Hyprland moved while ferryd was away still stay in place
            Ok(reading) => {
                let (monitors, remember) = layout.observe_own_monitors(&reading);
                remember_monitors = remember;
                monitors
            }
            Err(e) => {
                tracing::info!(error = %e, "no monitors from Hyprland; the own monitor list is empty");
                Vec::new()
            }
        };
        let capture_on =
            (capture_allowed || fake_portal.is_some()) && config.input.capture != CaptureMode::Off;
        let source_config = if capture_on {
            // Validated with the config; the repeat hold follows from Hyprland in step 4.
            Some(config.source_config(0).map_err(|e| {
                StartError::Config(fsutil::StoreError::corrupt(&paths.config_file, e))
            })?)
        } else {
            None
        };
        // Hyprland IPC, for the lock checks and the IPC release: only when input is enabled.
        let ipc = if capture_allowed {
            monitors.instance().ok()
        } else {
            None
        };
        tracing::info!(capture = capture_on, inject = ?inject, locked, "input");

        // 2. the socket, then the QUIC endpoint
        let (listener, guard) = server::bind(&paths)?;
        // Only now: a second ferryd on this state directory has been refused by the bind, and
        // must not write over the layout of the one that runs.
        if remember_monitors && let Err(e) = layout.save(&paths.layout_file()) {
            tracing::warn!(error = %e, "cannot save the monitor memory");
        }
        let (tx, rx) = mpsc::channel(CTL_CAP);
        if panic_hook {
            // Before any thread that captures or injects exists (B-12: it used to be installed
            // after `start` returned, past the start-up modifier reset).
            install_panic_hook(ShutdownHandle { tx: tx.clone() }, ipc.clone());
        }
        let live = SharedLive::default();
        // The daemon watchdog: the threads wake and beat every `period`.
        let notifier = match notify_socket {
            Some(p) => Notifier::Socket(p),
            None if sd_notify && std::env::var_os("NOTIFY_SOCKET").is_some() => Notifier::Env,
            None => Notifier::Off,
        };
        let watchdog = watchdog.or_else(|| {
            if sd_notify {
                watchdog::from_env()
            } else {
                None
            }
        });
        let watchdog = watchdog.filter(|_| !matches!(notifier, Notifier::Off));
        if sd_notify {
            live.lock().service =
                watchdog::own_service().map(|name| live::Service { name, watchdog });
        }
        if let Some(interval) = watchdog {
            live.beats().set_period(watchdog::period_for(interval));
            tracing::info!(
                interval_ms = interval.as_millis() as u64,
                "systemd watchdog on"
            );
        }
        // The portal driver's queue: the capture thread fills it from now on, the driver reads it
        // from start-up step 4.
        let (driver_tx, driver_rx) = mpsc::channel(chan::DRIVER_CAP);
        let capture =
            capture_thread::spawn(source_config, tx.clone(), driver_tx.clone(), live.clone())?;
        if locked {
            // Before anything can arm: the channel is in order.
            let _ = capture.tx.try_send(chan::CaptureCmd::Event(
                ferry_core::source::SourceEvent::LockedHere(true),
            ));
        }
        let bind =
            bind.unwrap_or_else(|| SocketAddr::from((Ipv6Addr::UNSPECIFIED, config.peer.port)));
        let net = match net_thread::spawn(net_thread::NetInit {
            identity,
            trust: trust.trust_set(),
            bind,
            name: me.name.clone(),
            monitors: own_monitors.clone(),
            ctl: tx.clone(),
            capture: capture.tx.clone(),
            live: live.clone(),
            capture_on,
            cursor_poll: ipc.is_some(),
            locked,
            lookup: lookup.unwrap_or_else(netaddr::system_lookup),
        }) {
            Ok(n) => n,
            Err(e) => {
                let _ = capture.tx.try_send(chan::CaptureCmd::Shutdown);
                let _ = capture.join.join();
                return Err(e);
            }
        };

        let state = State {
            me: me.clone(),
            port: net.local_addr.port(),
            saved: config.clone(),
            config,
            config_warnings: warnings,
            trust,
            layout,
            edits: Default::default(),
            own_monitors,
            links: Default::default(),
            refusals: Vec::new(),
            refusals_warned: std::collections::HashSet::new(),
            place_on_up: std::collections::HashSet::new(),
            counters: Default::default(),
            last_release: None,
            live: live.clone(),
            capture_on,
            locked,
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let net_tx = net.tx.clone();
        let init = ctl::CtlInit {
            state,
            paths: paths.clone(),
            net: net.tx,
            capture: capture.tx,
            live: live.clone(),
            capture_on,
            ipc,
            fake_portal,
            driver: capture_on.then_some((driver_tx, driver_rx)),
            net_thread: net.join,
            capture_thread: capture.join,
            rx,
            tx: tx.clone(),
            listener,
            guard,
            monitors,
            handle_signals,
            uid: socket_uid.unwrap_or_else(|| rustix::process::getuid().as_raw()),
            notify: notifier.clone(),
            watchdog: watchdog.is_some(),
            ready: ready_tx,
            toasts,
        };
        let join = std::thread::Builder::new()
            .name("ctl".into())
            .spawn(move || ctl::run(init))
            .map_err(|source| StartError::Thread {
                name: "ctl",
                source,
            })?;
        if ready_rx.recv().is_err() {
            let _ = join.join();
            return Err(StartError::Net(
                "the ctl thread stopped before it served the socket".into(),
            ));
        }

        // READY=1: the socket and the endpoint are up.
        if let Err(e) = notifier.send(&[sd_notify::NotifyState::Ready]) {
            tracing::warn!(error = %e, "sd_notify READY=1 failed");
        }

        // 3. ferry-inject, then `TargetEvent::Start` on the net thread, which takes no
        //    connection before it.
        let started = inject.start();
        if net_tx.try_send(NetCmd::Start(started)).is_err() {
            tracing::error!("the net thread did not take the start");
        }
        // 4. and 5. on ctl.
        if tx.try_send(CtlMsg::Begin).is_err() {
            tracing::error!("the ctl thread did not take the start");
        }

        tracing::info!(
            peer = %me.name,
            fingerprint = %fp,
            socket = %paths.socket_path().display(),
            net = %net.local_addr,
            "ferryd ready"
        );
        Ok(Self {
            socket_path: paths.socket_path(),
            net_addr: net.local_addr,
            fingerprint: fp,
            tx,
            join: Some(join),
            live,
        })
    }

    /// Test hook for the daemon watchdog: make a thread sleep for `d` at its next wake-up, as a
    /// hang would.
    #[doc(hidden)]
    pub fn stall(&self, thread: watchdog::Thread, d: Duration) {
        self.live.beats().stall(thread, d);
    }

    /// Test hook for a backlog on the net thread: after every event it handles, the net loop
    /// waits `d` (asynchronously, as a handler that awaits another thread would), so its queues
    /// fill behind it. Zero ends it.
    #[doc(hidden)]
    pub fn drag_net(&self, d: Duration) {
        self.live.beats().drag_net(d);
    }

    /// Test hook for a stuck `ctl`: its main loop leaves its queue unread for `d`, while the
    /// socket and the portal driver go on.
    #[doc(hidden)]
    pub fn pause_ctl(&self, d: Duration) {
        self.live.beats().pause_ctl(d);
    }

    /// Test hook: fill `ctl`'s queue with harmless messages (an abandoned inbound handshake,
    /// counted as `inbound_abandoned`) until it is full. Returns how many went in.
    #[doc(hidden)]
    pub fn fill_ctl_queue(&self) -> usize {
        let mut n = 0;
        while self
            .tx
            .try_send(CtlMsg::InboundFailed(ferry_net::ConnectError::Abandoned))
            .is_ok()
        {
            n += 1;
        }
        n
    }

    /// Test hook: a message into `ctl`'s queue, as `net` or `capture` would send it. `false`
    /// when the queue is full or `ctl` has stopped.
    #[doc(hidden)]
    pub fn send_ctl(&self, msg: CtlMsg) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    /// Test hook: hands a toast to the `notice` subscribers, as the notifier will.
    /// The message goes to ctl like any other thread's.
    #[doc(hidden)]
    pub fn publish_notice(&self, notice: ferry_core::api::Notice) {
        let _ = self.tx.try_send(CtlMsg::Notice(notice));
    }

    /// Test hook: asks the `ui` subscribers to open a view.
    #[doc(hidden)]
    pub fn publish_ui(&self, view: ferry_core::api::UiView) {
        let _ = self.tx.try_send(CtlMsg::Ui(view));
    }

    /// Test hook for the panic hook: the net thread panics at its next wake-up.
    #[doc(hidden)]
    pub fn panic_net(&self) {
        self.live.beats().panic_net();
    }

    /// What a panic hook needs: a way to ask for the §9 shutdown from any thread.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            tx: self.tx.clone(),
        }
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Where the QUIC endpoint is bound.
    pub fn net_addr(&self) -> SocketAddr {
        self.net_addr
    }

    pub fn fingerprint(&self) -> ferry_net::Fingerprint {
        self.fingerprint
    }

    /// Runs the §9 shutdown order and waits for it. Removes the socket.
    pub fn shutdown(mut self) {
        let _ = self.tx.blocking_send(CtlMsg::Shutdown);
        self.wait_inner();
    }

    /// Blocks until the daemon stops (a signal, or `shutdown` from elsewhere).
    pub fn wait(mut self) {
        self.wait_inner();
    }

    fn wait_inner(&mut self) {
        if let Some(j) = self.join.take()
            && j.join().is_err()
        {
            tracing::error!("the ctl thread panicked");
        }
    }
}

/// Asks a running daemon to stop with the §9 shutdown order. Never blocks.
#[derive(Clone)]
pub struct ShutdownHandle {
    tx: mpsc::Sender<CtlMsg>,
}

impl ShutdownHandle {
    pub fn request(&self) {
        let _ = self.tx.try_send(CtlMsg::Shutdown);
    }

    /// Like [`request`](Self::request), but a full queue is tried again until `limit` has
    /// passed. Blocks the calling thread for at most `limit`.
    pub fn request_within(&self, limit: Duration) {
        let until = Instant::now() + limit;
        loop {
            match self.tx.try_send(CtlMsg::Shutdown) {
                Err(mpsc::error::TrySendError::Full(_)) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => return,
            }
        }
    }
}

static PANICKED: AtomicBool = AtomicBool::new(false);

/// Whether the panic hook ([`Options::panic_hook`]) ran in this process.
pub fn panicked() -> bool {
    PANICKED.load(Ordering::Relaxed)
}

/// How long the panic hook waits for Hyprland's IPC release, and then for room in ctl's queue:
/// A panicking thread must not hang on a wedged compositor.
pub const PANIC_RELEASE: Duration = Duration::from_millis(250);

/// A panic runs the release sequence on a best-effort basis. Hyprland's
/// `release_input_capture()` ends any capture at once, whatever state the machines are in;
/// then the normal shutdown order is requested (it runs if `ctl` is alive). The process exits
/// with a failure, so systemd restarts it, and the restart's modifier reset clears the target.
fn install_panic_hook(handle: ShutdownHandle, ipc: Option<ferry_hypr_ipc::Instance>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        // Only the swap's atomicity matters (the sequence runs once); `main` reads the flag
        // after joining ctl.
        if PANICKED.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(ipc) = &ipc {
            let _ = ipc.request_within(ferry_hypr_ipc::RELEASE_INPUT_CAPTURE, PANIC_RELEASE);
        }
        handle.request_within(PANIC_RELEASE);
    }));
}
