//! The `net` thread: a tokio current-thread runtime that owns the ferry-net endpoint, its
//! connections and the **TargetMachine**.
//!
//! | In | Out |
//! |---|---|
//! | ferry-net: connections, frames, datagrams, closes | target frames → TargetMachine (this thread) |
//! | [`NetCmd`] from `ctl` | `HeldAck` / `Ended` → the source peer, directly (no hop) |
//! | ferry-inject's `DeviceEvent`s | `InjectCmd` → ferry-inject's channel (the target path's one hop) |
//! | | `HeldAck` / `Ended` / `PeerUp` / `PeerDown` / `PeerAlive` → `capture` |
//! | | `PeerUp` / `Monitors` / `Layout` / links → `ctl` |
//!
//! **Hello**. The dialer sends `Hello` first and the acceptor answers with its
//! own. A peer whose first frame is not `Hello`, or that sends none within 2 s, is closed with
//! code 2. A different major is refused: this side sends `Error{1, "version mismatch: <ours>
//! vs <theirs>"}` and closes with code 1, and both sides show the peer as `refused` with that
//! text. A peer is **up** once both `Hello`s are in.
//!
//! **Ping.** The dialer sends a Ping datagram every 2 s and the acceptor answers with a Pong at
//! once. Each one is a liveness hint (`PeerAlive`) for the source's suspect-peer rule. Ping and
//! Pong never reach the TargetMachine.
//!
//! **Both peers capture**. Before an `Enter` reaches the TargetMachine, the
//! capture thread's published role tells it whether this peer's own source is busy
//! (`SourceBusy`). `Controlled(true)` goes to the capture thread at once; on `Controlled(false)`
//! a peer that captures syncs and drops its virtual devices first, so its barriers come back
//! only once the virtual keyboard is gone. ferry-inject's owner thread does the sync and
//! the drop, and `Controlled(false)` goes on when its `Closed` comes back: net never waits for
//! the compositor. `CheckCursor` goes to `ctl`, which reads Hyprland's cursor and answers
//! with `CursorPolled`.
//!
//! **PeerState** (#14) goes after `Hello` at peer up and on every change of lock to this
//! machine, only to a peer whose `Hello` listed `peer-state`. An incoming one becomes
//! `PeerLocked` for the capture thread and `peers[].locked` in `status`.

mod clip;

pub use clip::ClipInit;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::mpsc::TrySendError;
use std::time::Duration;

use bytes::Bytes;

use crate::ratelimit::log_limited;
use calloop::channel::SyncSender;
use ferry_core::api::LinkState;
use ferry_core::geom::{Layout, MonitorInfo};
use ferry_core::source::{SourceEvent, SourceRole};
use ferry_core::target::{InjectCmd, TargetAction, TargetEvent, TargetMachine};
use ferry_core::wire::{
    self, Control, DATAGRAM_LEN, Datagram, DecodeError, Decoded, PROTO_MAJOR, PROTO_MINOR,
};
use ferry_core::{Actions, PeerId};
use ferry_inject::{DeviceEvent, InputEvent};
use ferry_net::{
    CloseCode, CloseReason, ConnectError, Connection, Endpoint, EndpointConfig, Event, Events,
    Fingerprint, Identity, Manager, ManagerEvent, TrustSet,
};
use tokio::sync::{mpsc, oneshot};

use crate::chan::{CaptureCmd, Crossed, CtlMsg, LinkUpdate, NET_CAP, NetCmd};
use crate::clock;
use crate::ctl_out::CtlOut;
use crate::error::StartError;
use crate::inject::Injector;
use crate::keymap::{KeymapCanon, short_hash};
use crate::live::SharedLive;
use crate::netaddr::{Lookup, parse_host_port};
use crate::pairing::{NetPairing, PairInternal};
use crate::watchdog::Thread;

/// How often the RTT of live connections is refreshed in `status`, and the dialer's Ping.
const PING_EVERY: Duration = Duration::from_secs(2);
/// A peer that sends no `Hello` within this is closed with code 2.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
const GRACEFUL_CLOSE: Duration = Duration::from_millis(100);
/// After a version refusal, the dialer waits this long before it tries again.
const REFUSED_REDIAL: Duration = Duration::from_secs(30);
/// What `Hello.build` carries.
const BUILD: &str = env!("CARGO_PKG_VERSION");
/// While messages for ctl wait for room in its queue, the loop tries again this often.
const CTL_RETRY: Duration = Duration::from_millis(10);
/// While messages for the capture thread wait for room in its queue, the loop tries again this
/// often: a `PeerDown` it lost would leave a crossing to a gone peer to the silence watchdog.
const CAPTURE_RETRY: Duration = Duration::from_millis(2);

pub struct NetHandle {
    pub tx: mpsc::Sender<NetCmd>,
    pub join: std::thread::JoinHandle<()>,
    /// Where the endpoint actually bound (the config may say port 0).
    pub local_addr: SocketAddr,
}

/// Everything the thread needs besides the endpoint.
pub struct NetInit {
    pub identity: Identity,
    pub trust: TrustSet,
    pub bind: SocketAddr,
    pub name: String,
    pub monitors: Vec<MonitorInfo>,
    pub ctl: mpsc::Sender<CtlMsg>,
    pub capture: SyncSender<CaptureCmd>,
    pub live: SharedLive,
    /// This peer captures too: a session's end drops the virtual devices at once.
    pub capture_on: bool,
    /// ctl can read Hyprland's cursor: the TargetMachine's takeover poll runs.
    pub cursor_poll: bool,
    /// Lock to this machine at start (`state.toml`).
    pub locked: bool,
    /// How a host name becomes an address (`Options::lookup`; the system's in the binary).
    pub lookup: Lookup,
    /// Clipboard sharing.
    pub clip: ClipInit,
}

/// Past this many owed capture messages, liveness hints (`PeerAlive`, `HeldAck`) are dropped.
const CAPTURE_OWED_CAP: usize = 256;
/// At shutdown, how long net waits for what it injected to reach the compositor.
const SHUTDOWN_SYNC: Duration = Duration::from_millis(250);

/// Spawns the thread and waits until the endpoint is bound (or has failed to bind).
pub fn spawn(init: NetInit) -> Result<NetHandle, StartError> {
    let (tx, rx) = mpsc::channel(NET_CAP);
    let (bound_tx, bound_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    let join = std::thread::Builder::new()
        .name("net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime builds");
            rt.block_on(async move {
                let cfg = EndpointConfig {
                    bind: init.bind,
                    ..EndpointConfig::default()
                };
                let endpoint = match Endpoint::bind(init.identity, init.trust, cfg) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = bound_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let local = match endpoint.local_addr() {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = bound_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = bound_tx.send(Ok(local));
                let (clip, clip_ev, clip_in) = clip::ClipState::new(init.clip);
                let mut net = Net::new(
                    endpoint,
                    init.name,
                    init.monitors,
                    init.ctl,
                    init.capture,
                    init.live,
                    clip,
                );
                net.capture_on = init.capture_on;
                net.cursor_poll = init.cursor_poll;
                net.locked = init.locked;
                net.lookup = init.lookup;
                net.run(rx, clip_ev, clip_in).await;
            });
            // A lookup still waiting in getaddrinfo (nss-mdns waits about 5 s) sits on tokio's
            // blocking pool, and dropping the runtime would wait for it: past TimeoutStopSec.
            rt.shutdown_background();
        })
        .map_err(|source| StartError::Thread {
            name: "net",
            source,
        })?;
    match bound_rx.recv() {
        Ok(Ok(local_addr)) => Ok(NetHandle {
            tx,
            join,
            local_addr,
        }),
        Ok(Err(message)) => {
            let _ = join.join();
            Err(StartError::Net(message))
        }
        Err(_) => Err(StartError::Net("the net thread died while binding".into())),
    }
}

/// A host name resolved off the loop (a spawned task), on its way back to it.
struct Resolved {
    peer: Fingerprint,
    /// Which `dial` asked: a later one for the same peer supersedes this answer.
    generation: u64,
    addr: String,
    result: Result<SocketAddr, String>,
}

/// From a connection's drain task.
enum ConnEvent {
    Event(Event),
    HelloTimeout,
}

struct Peer {
    conn: Connection,
    /// The peer's `(major, minor)` once its `Hello` is in.
    hello: Option<(u16, u16)>,
    /// Our `Hello` went out.
    sent_hello: bool,
    /// Both `Hello`s are in and compatible.
    up: bool,
    /// This connection replaced one that was up (simultaneous dial): the machines still count
    /// the peer as up, so a failure before the new `Hello` is a `PeerDown`.
    replaced_up: bool,
    /// The peer's `Hello.features`.
    features: Vec<String>,
}

/// Counters that a peer can drive at line rate. They are plain fields of the net thread and are
/// copied into [`SharedLive`] once per loop iteration, so a flood costs one lock per wake-up, not
/// one per packet.
#[derive(Debug, Default)]
struct HotCounters {
    unknown_frames: u64,
    frames_over_cap: u64,
    bad_datagrams: u64,
    /// `Error` frames from peers.
    peer_errors: u64,
    /// `PeerState` frames from peers.
    peer_state_frames: u64,
    /// Changed since the last [`Net::publish_hot`].
    dirty: bool,
}

impl HotCounters {
    fn bump(&mut self, field: fn(&mut Self) -> &mut u64) {
        *field(self) += 1;
        self.dirty = true;
    }
}

struct Net {
    endpoint: Endpoint,
    manager: Manager,
    name: String,
    monitors: Vec<MonitorInfo>,
    peers: HashMap<Fingerprint, Peer>,
    /// Peers refused for a version mismatch, with the text `status` shows.
    refused: HashMap<Fingerprint, String>,
    /// Where each pinned peer is dialled, for a re-dial after a refusal.
    dial_addrs: HashMap<Fingerprint, String>,
    /// A dial failure that came while the peer still had a connection entry, so it could not
    /// be shown yet; `closed` shows it. ferry-net reports a failure only when it is new.
    unshown_dial_error: HashMap<Fingerprint, (LinkState, String)>,
    target: TargetMachine,
    tout: Actions<TargetAction>,
    /// `tout`'s actions are moved here to run, so none of them costs an allocation.
    tdo: Vec<TargetAction>,
    /// Received keymaps are made canonical before the TargetMachine compares them
    /// (`keymap.rs`).
    canon: KeymapCanon,
    injector: Option<Box<dyn Injector>>,
    started: bool,
    ctl: mpsc::Sender<CtlMsg>,
    /// Everything for ctl but the cursor poll goes through here: net never awaits ctl.
    out: CtlOut,
    capture: SyncSender<CaptureCmd>,
    /// What the capture thread's full queue did not take yet, in order (see `for_capture`).
    capture_owed: VecDeque<CaptureCmd>,
    live: SharedLive,
    hot: HotCounters,
    ev_tx: mpsc::Sender<(Fingerprint, usize, ConnEvent)>,
    redial_tx: mpsc::Sender<Fingerprint>,
    /// Name lookups run on spawned tasks and answer here, so the loop (and its watchdog beat)
    /// never waits for DNS or mDNS.
    resolved_tx: mpsc::Sender<Resolved>,
    lookup: Lookup,
    /// The lookup each peer is waiting for; an answer for any other generation is stale.
    resolving: HashMap<Fingerprint, u64>,
    next_lookup: u64,
    frame: Vec<u8>,
    ping_id: u32,
    /// When the last `Held` reached the TargetMachine (for the watchdog's log line).
    last_held: Option<std::time::Instant>,
    /// The source and `Enter` point of the session in progress, for the `edge` event when it
    /// comes home.
    entered: Option<(PeerId, (f64, f64))>,
    /// This peer captures too.
    capture_on: bool,
    /// ctl can read the cursor.
    cursor_poll: bool,
    /// Lock to this machine, as told to the peers.
    locked: bool,
    /// The last `SourceBusy` fed to the TargetMachine.
    source_busy: bool,
    /// `Controlled(false)` waits for the devices to be gone: the close request whose
    /// `Closed` sends it on.
    rearm_after: Option<u64>,
    /// The pairing window or dial, and its one attempt.
    pairing: NetPairing,
    pair_rx: Option<mpsc::Receiver<PairInternal>>,
    clip: clip::ClipState,
}

/// Sleeps until `at`, or forever.
async fn sleep_until_instant(at: Option<std::time::Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
    }
}

/// Sleeps until `deadline`, or forever.
async fn sleep_until(deadline: Option<ferry_core::Micros>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(clock::instant_at(d).into()).await,
        None => std::future::pending().await,
    }
}

async fn next_input_event(rx: &mut Option<mpsc::UnboundedReceiver<DeviceEvent>>) -> DeviceEvent {
    match rx {
        Some(r) => match r.recv().await {
            Some(e) => e,
            None => {
                *rx = None;
                std::future::pending().await
            }
        },
        None => std::future::pending().await,
    }
}

impl Net {
    fn new(
        endpoint: Endpoint,
        name: String,
        monitors: Vec<MonitorInfo>,
        ctl: mpsc::Sender<CtlMsg>,
        capture: SyncSender<CaptureCmd>,
        live: SharedLive,
        clip: clip::ClipState,
    ) -> Self {
        // The receivers are created in `run`; these senders are replaced there.
        let (ev_tx, _) = mpsc::channel(1);
        let (redial_tx, _) = mpsc::channel(1);
        let (resolved_tx, _) = mpsc::channel(1);
        let (pair_tx, pair_rx) = mpsc::channel(16);
        let out = CtlOut::new(ctl.clone(), live.clone());
        let pairing = NetPairing::new(
            endpoint.clone(),
            name.clone(),
            ctl.clone(),
            out.clone(),
            live.clone(),
            pair_tx,
        );
        Self {
            manager: Manager::new(endpoint.clone()),
            endpoint,
            name,
            monitors,
            peers: HashMap::new(),
            refused: HashMap::new(),
            dial_addrs: HashMap::new(),
            unshown_dial_error: HashMap::new(),
            target: TargetMachine::new(),
            tout: Actions::new(),
            tdo: Vec::with_capacity(Actions::<TargetAction>::TYPICAL_MAX),
            canon: KeymapCanon::new(),
            injector: None,
            started: false,
            ctl,
            out,
            capture,
            capture_owed: VecDeque::new(),
            live,
            hot: HotCounters::default(),
            ev_tx,
            redial_tx,
            resolved_tx,
            lookup: crate::netaddr::system_lookup(),
            resolving: HashMap::new(),
            next_lookup: 0,
            frame: Vec::with_capacity(256),
            ping_id: 0,
            last_held: None,
            entered: None,
            capture_on: false,
            cursor_poll: false,
            locked: false,
            source_busy: false,
            rearm_after: None,
            pairing,
            pair_rx: Some(pair_rx),
            clip,
        }
    }

    async fn run(
        mut self,
        mut cmds: mpsc::Receiver<NetCmd>,
        mut clip_ev: mpsc::UnboundedReceiver<crate::clip::ClipEvent>,
        mut clip_in: mpsc::UnboundedReceiver<clip::ClipIn>,
    ) {
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        let (redial_tx, mut redial_rx) = mpsc::channel(16);
        self.ev_tx = ev_tx;
        self.redial_tx = redial_tx;
        let (resolved_tx, mut resolved_rx) = mpsc::channel(16);
        self.resolved_tx = resolved_tx;
        let mut inject_events: Option<mpsc::UnboundedReceiver<DeviceEvent>> = None;
        // The thread wakes once per thread period (1 s) to beat; the 2 s work (Ping, RTT) rides on
        // every other one, so it keeps its own cadence whatever the period (a `Ticker` costs one
        // wake-up a tick; see `ticker.rs`).
        let beats = self.live.beats().clone();
        let period = beats.thread_period();
        let per_ping = (PING_EVERY.as_micros() / period.as_micros().max(1)).max(1) as u64;
        let mut beat = crate::ticker::Ticker::new(period);
        let mut ticks = 0u64;
        let mut shutdown_ack = None;
        let pair_ep = self.endpoint.clone();
        let mut pair_rx = self.pair_rx.take().expect("run once");

        loop {
            beats.beat(Thread::Net);
            beats.maybe_stall(Thread::Net);
            if beats.take_panic_net() {
                panic!("test hook: the net thread panics");
            }
            // A due deadline fires before `select!`. The `sleep_until`s below are
            // built anew each iteration, and tokio rounds a deadline up to its next 1 ms tick,
            // so while another branch is ready on every iteration (a backlog of frames) they
            // never fire.
            if self
                .target
                .next_deadline()
                .is_some_and(|d| d <= clock::now())
            {
                self.target_event(TargetEvent::Tick);
            }
            if self
                .pairing
                .window_end()
                .is_some_and(|t| t <= std::time::Instant::now())
            {
                self.pairing.window_due().await;
            }
            let deadline = self.target.next_deadline();
            let window_end = self.pairing.window_end();
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(NetCmd::Start(started)) => {
                        inject_events = started.events;
                        self.start(started.injector);
                    }
                    Some(NetCmd::Shutdown { ack }) => {
                        shutdown_ack = Some(ack);
                        break;
                    }
                    Some(cmd) => self.command(cmd).await,
                    None => break,
                },
                // Connections are taken only after start-up step 3 (`TargetEvent::Start`).
                ev = self.manager.next(), if self.started => match ev {
                    Some(ev) => self.manager_event(ev),
                    None => break,
                },
                Some((peer, conn_id, ev)) = ev_rx.recv() => self.conn_event(peer, conn_id, ev),
                Some(peer) = redial_rx.recv() => {
                    if let Some(addr) = self.dial_addrs.get(&peer).cloned() {
                        self.dial(peer, &addr);
                    }
                }
                Some(r) = resolved_rx.recv() => self.resolved(r),
                Some(ev) = clip_ev.recv() => self.clip_event(ev),
                Some(m) = clip_in.recv() => self.clip_in(m),
                e = next_input_event(&mut inject_events) => self.input_event(e),
                // Pairing: accepted ferry-pair/1 connections, the dial and
                // attempt tasks, and the end of the acceptor's window.
                Some(pc) = pair_ep.accept_pairing(), if self.started => self.pairing.incoming(pc),
                Some(msg) = pair_rx.recv() => self.pairing.internal(msg).await,
                () = sleep_until_instant(window_end) => self.pairing.window_due().await,
                () = sleep_until(deadline) => self.target_event(TargetEvent::Tick),
                () = beat.tick() => {
                    if ticks.is_multiple_of(per_ping) {
                        self.tick();
                    }
                    ticks += 1;
                }
                () = tokio::time::sleep(CTL_RETRY), if self.out.pending() => {}
                () = tokio::time::sleep(CAPTURE_RETRY), if !self.capture_owed.is_empty() => {}
            }
            // net never waits for another thread: what did not fit stays for the
            // next iteration.
            self.out.flush();
            self.flush_capture();
            self.publish_hot();
            if let Some(d) = beats.net_drag() {
                tokio::time::sleep(d).await;
            }
        }

        // Shutdown: ferry-inject sync() (bounded: the loop no longer runs, but a
        // stalled compositor must not hold the shutdown), then close_graceful(100 ms) on every
        // connection, which delivers the queued `Ended(shutdown)` / `Leave(shutdown)` first,
        // then the endpoint.
        self.pairing.shutdown();
        if let Some(i) = self.injector.as_mut() {
            i.sync(SHUTDOWN_SYNC);
        }
        let mut closing = tokio::task::JoinSet::new();
        for (_, p) in self.peers.drain() {
            closing.spawn(async move {
                p.conn
                    .close_graceful(CloseCode::SHUTDOWN, "", GRACEFUL_CLOSE)
                    .await;
            });
        }
        while closing.join_next().await.is_some() {}
        self.endpoint.close();
        let _ = tokio::time::timeout(Duration::from_millis(500), self.endpoint.wait_idle()).await;
        if let Some(ack) = shutdown_ack {
            let _: Result<(), ()> = ack.send(());
        }
    }

    /// Start-up step 3: the TargetMachine's modifier reset runs before any connection is
    /// taken. Without an injector this peer refuses every `Enter`.
    fn start(&mut self, injector: Option<Box<dyn Injector>>) {
        self.live.lock().inject = injector.is_some();
        self.injector = injector;
        self.target_event(TargetEvent::Start);
        if let Some(i) = self.injector.as_mut() {
            // No virtual keyboard stays while this peer captures. ferry-inject's backend already
            // ran its reset and dropped its devices in `InjectBackend::start`, so this waits for
            // nothing.
            i.drop_devices();
        }
        if self.injector.is_none() {
            tracing::info!("no injection on this peer: every Enter is refused");
            self.target_event(TargetEvent::AcceptEnters(false));
        } else if self.cursor_poll {
            self.target_event(TargetEvent::CursorPoll(true));
        }
        self.started = true;
        self.clip_start();
    }

    async fn command(&mut self, cmd: NetCmd) {
        match cmd {
            NetCmd::SetTrust { fps, ack } => {
                // A connected peer whose pin is gone is closed as unpaired.
                let dropped: Vec<Fingerprint> = self
                    .peers
                    .keys()
                    .filter(|fp| !fps.contains(fp))
                    .copied()
                    .collect();
                self.endpoint.set_trust(fps.clone());
                // A pin change may be what the other side was waiting for (it re-paired): a
                // refused dial starts again now, not after its backoff.
                for fp in &fps {
                    self.manager.retry_now(fp);
                }
                for fp in dropped {
                    tracing::info!(peer = %fp, "pin removed: closing as unpaired");
                    self.manager.forget_with(&fp, CloseCode::UNPAIRED);
                    self.dial_addrs.remove(&fp);
                    self.resolving.remove(&fp);
                }
                let _ = ack.send(());
            }
            NetCmd::Dial { peer, addr } => {
                self.dial_addrs.insert(peer, addr.clone());
                self.dial(peer, &addr);
            }
            NetCmd::OwnMonitors(m) => {
                self.monitors = m;
                let msg = Control::Monitors {
                    monitors: self.monitors.clone(),
                };
                self.broadcast(&msg);
            }
            NetCmd::Send { peer, msg } => {
                if self.peers.get(&peer).is_some_and(|p| p.up) {
                    self.send(&peer, &msg);
                }
            }
            NetCmd::Broadcast(msg) => self.broadcast(&msg),
            NetCmd::ForceRelease => self.target_event(TargetEvent::ForceRelease),
            NetCmd::ShutdownMachine { ack } => {
                self.target_event(TargetEvent::Shutdown);
                let _ = ack.send(());
            }
            NetCmd::CursorPolled(pos) => self.target_event(TargetEvent::CursorPolled(pos)),
            NetCmd::ClipConfig { enabled, max_bytes } => self.clip_config(enabled, max_bytes),
            NetCmd::ClipEntered(peer) => self.clip_partner(peer, true),
            NetCmd::SetLocked(on) => {
                self.locked = on;
                let up: Vec<Fingerprint> = self
                    .peers
                    .iter()
                    .filter(|(_, p)| p.up)
                    .map(|(fp, _)| *fp)
                    .collect();
                for fp in up {
                    self.send_peer_state(&fp);
                }
            }
            cmd @ (NetCmd::PairAccept { .. }
            | NetCmd::PairStart { .. }
            | NetCmd::PairAnswer { .. }
            | NetCmd::PairCancel { .. }) => self.pairing.command(cmd).await,
            NetCmd::Start(_) | NetCmd::Shutdown { .. } => unreachable!("handled in run"),
        }
    }

    // --- connections ------------------------------------------------------------------------

    fn manager_event(&mut self, ev: ManagerEvent) {
        match ev {
            ManagerEvent::Connected(nc) => {
                let peer = nc.conn.peer_fp();
                let conn = nc.conn;
                tracing::info!(peer = %peer, remote = %conn.remote_addr(), dialed = conn.is_dialer(), "connected");
                if !conn.is_dialer() {
                    // `ferry doctor` and `ferry setup` read this: a peer got through the firewall.
                    self.live.bump("inbound_accepted");
                }
                self.unshown_dial_error.remove(&peer);
                // A replaced connection (simultaneous dial) is not a PeerDown.
                let replaced_up = self.peers.get(&peer).is_some_and(|p| p.up || p.replaced_up);
                let mut p = Peer {
                    conn: conn.clone(),
                    hello: None,
                    sent_hello: false,
                    up: false,
                    replaced_up,
                    features: Vec::new(),
                };
                if conn.is_dialer() {
                    self.send_hello_on(&mut p);
                }
                self.peers.insert(peer, p);
                // Either side closes a peer whose Hello does not come within 2 s:
                // the acceptor too, which sends nothing until the dialer's Hello.
                let (tx, id) = (self.ev_tx.clone(), conn.id());
                tokio::spawn(async move {
                    tokio::time::sleep(HELLO_TIMEOUT).await;
                    let _ = tx.send((peer, id, ConnEvent::HelloTimeout)).await;
                });
                tokio::spawn(drain(nc.events, conn, self.ev_tx.clone()));
            }
            // ferry-net reports the first failure and each change of error, not every retry
            ManagerEvent::DialFailed {
                peer,
                addr,
                error,
                attempts,
                retry_in,
            } => {
                tracing::info!(peer = %peer, %addr, %error, attempts, ?retry_in, "dial failed; retrying");
                let (state, text) = (link_state_for(&error), error.to_string());
                if self.peers.contains_key(&peer) {
                    self.unshown_dial_error.insert(peer, (state, text));
                } else {
                    self.report(peer, state, None, None, Some(text));
                }
            }
            ManagerEvent::InboundFailed(e) => {
                tracing::debug!(remote = %e.remote, error = %e.error, "inbound connection failed");
                self.out.push(CtlMsg::InboundFailed(e.error));
            }
        }
    }

    fn conn_event(&mut self, peer: Fingerprint, conn_id: usize, ev: ConnEvent) {
        // Only the live connection of a peer counts; a replaced one's events are stale.
        if !self
            .peers
            .get(&peer)
            .is_some_and(|p| p.conn.id() == conn_id)
        {
            return;
        }
        match ev {
            ConnEvent::Event(Event::Frame(body)) => self.frame(peer, &body),
            ConnEvent::Event(Event::Datagram(d)) => self.datagram(peer, &d),
            ConnEvent::Event(Event::Closed(reason)) => self.closed(peer, reason),
            ConnEvent::HelloTimeout => {
                if self.peers.get(&peer).is_some_and(|p| p.hello.is_none()) {
                    tracing::warn!(peer = %peer, "no Hello within 2 s: closing");
                    self.close(&peer, CloseCode::PROTOCOL, "no Hello");
                }
            }
        }
    }

    fn closed(&mut self, peer: Fingerprint, reason: CloseReason) {
        let Some(p) = self.peers.remove(&peer) else {
            return;
        };
        tracing::info!(peer = %peer, reason = %reason, "disconnected");
        // A peer that closes with `UNPAIRED` has removed this machine's pin. Stop
        // dialling it, since it refuses our key now, and tell ctl before the `Offline` below so
        // the pin goes and the link is not reported as lost.
        if matches!(&reason, CloseReason::Peer { code, .. } if *code == CloseCode::UNPAIRED) {
            self.manager.forget(&peer);
            self.dial_addrs.remove(&peer);
            self.resolving.remove(&peer);
            self.out.push(CtlMsg::Revoked { peer });
        }
        if p.up || p.replaced_up {
            self.peer_down(peer);
        }
        if p.up {
            // Last_seen
            self.out.push(CtlMsg::PeerSeen {
                peer,
                remote: p.conn.remote_addr(),
                dialed: p.conn.is_dialer(),
                up: false,
            });
        }
        let (state, detail) = match self.refused.get(&peer) {
            Some(d) => (LinkState::Refused, d.clone()),
            None => (LinkState::Offline, reason.to_string()),
        };
        self.report(peer, state, None, None, Some(detail));
        // a redial that failed before this close was seen is newer than the close
        if let Some((state, text)) = self.unshown_dial_error.remove(&peer) {
            self.report(peer, state, None, None, Some(text));
        }
        if self.refused.contains_key(&peer) && p.conn.is_dialer() {
            // A refused peer is incompatible: no tight redial (codes 1–255).
            self.manager.forget(&peer);
            let tx = self.redial_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(REFUSED_REDIAL).await;
                let _ = tx.send(peer).await;
            });
        }
    }

    fn peer_down(&mut self, peer: Fingerprint) {
        let id = PeerId(peer.0);
        self.live.lock().locked_peers.retain(|q| *q != id);
        self.for_capture(CaptureCmd::Event(SourceEvent::PeerDown(id)));
        self.for_capture(CaptureCmd::Conn {
            peer: id,
            conn: None,
        });
        self.target_event(TargetEvent::PeerDown(id));
        self.clip_peer_down(peer);
    }

    // --- Hello ------------------------------------------------------------------------------

    fn hello_msg(&self) -> Control {
        Control::Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            name: self.name.clone(),
            features: wire::FEATURES.iter().map(|f| (*f).to_string()).collect(),
            build: BUILD.to_string(),
            monitors: self.monitors.clone(),
        }
    }

    fn send_hello_on(&mut self, p: &mut Peer) {
        let hello = self.hello_msg();
        hello.encode(&mut self.frame);
        if let Err(e) = p.conn.try_send_frame(Bytes::copy_from_slice(&self.frame)) {
            tracing::warn!(error = %e, "cannot send Hello");
        }
        p.sent_hello = true;
    }

    fn hello(
        &mut self,
        peer: Fingerprint,
        (major, minor): (u16, u16),
        name: String,
        features: Vec<String>,
        monitors: Vec<MonitorInfo>,
    ) {
        // The name is shown in logs: one that fails the check is replaced, as in pairing.
        // The pin's name, not this one, is what the UI shows.
        let name = match crate::config::validate_peer_name(&name) {
            Ok(()) => name,
            Err(why) => {
                let shown = PeerId(peer.0).default_name();
                log_limited!(warn, peer = %peer, why, shown, "a peer's Hello name is not allowed: replaced");
                shown
            }
        };
        let Some(mut p) = self.peers.remove(&peer) else {
            return;
        };
        if p.hello.is_some() {
            self.peers.insert(peer, p);
            self.live.bump("duplicate_hello");
            return;
        }
        p.hello = Some((major, minor));
        p.features = features;
        // The acceptor answers with its own Hello, even to a peer it refuses, so the other
        // side can show both versions.
        if !p.sent_hello {
            self.send_hello_on(&mut p);
        }
        let proto = format!("{major}.{minor}");
        if major != PROTO_MAJOR {
            let detail = format!("version mismatch: {PROTO_MAJOR}.{PROTO_MINOR} vs {proto}");
            tracing::warn!(peer = %peer, name, %detail, "refusing the peer");
            let err = Control::Error {
                code: wire::ErrorCode::Version as u16,
                message: detail.clone(),
            };
            err.encode(&mut self.frame);
            let _ = p.conn.try_send_frame(Bytes::copy_from_slice(&self.frame));
            let conn = p.conn.clone();
            tokio::spawn(async move {
                conn.close_graceful(CloseCode::VERSION, "version mismatch", GRACEFUL_CLOSE)
                    .await;
            });
            self.refused.insert(peer, detail.clone());
            let was_up = p.replaced_up;
            p.replaced_up = false;
            self.peers.insert(peer, p);
            if was_up {
                self.peer_down(peer);
            }
            let remote = self
                .peers
                .get(&peer)
                .map(|p| p.conn.remote_addr().to_string());
            self.report_full(
                peer,
                LinkState::Refused,
                remote,
                None,
                Some(proto),
                Some(detail),
            );
            return;
        }
        self.refused.remove(&peer);
        p.up = true;
        p.replaced_up = false;
        let conn = p.conn.clone();
        self.peers.insert(peer, p);
        tracing::info!(peer = %peer, name, %proto, monitors = monitors.len(), "peer up");
        // ctl (known monitors, topology, layout push) in order, as soon as it has room; the
        // machines at once. The capture thread gets the handle before PeerUp, so the keymap it
        // sends at PeerUp can go out.
        self.out.push(CtlMsg::PeerUp { peer, monitors });
        // The address that worked, and last_seen
        self.out.push(CtlMsg::PeerSeen {
            peer,
            remote: conn.remote_addr(),
            dialed: conn.is_dialer(),
            up: true,
        });
        let id = PeerId(peer.0);
        self.for_capture(CaptureCmd::Conn {
            peer: id,
            conn: Some(conn.clone()),
        });
        self.for_capture(CaptureCmd::Event(SourceEvent::PeerUp(id)));
        self.target_event(TargetEvent::PeerUp(id));
        // Lock to this machine, after Hello.
        self.send_peer_state(&peer);
        self.clip_peer_up(peer, &conn);
        self.report_full(
            peer,
            LinkState::Connected,
            Some(conn.remote_addr().to_string()),
            rtt_us(&conn),
            Some(proto),
            None,
        );
    }

    // --- frames and datagrams ----------------------------------------------------------------

    fn frame(&mut self, peer: Fingerprint, body: &[u8]) {
        let msg = match Control::decode(body) {
            Ok(Decoded::Msg(m)) => m,
            Ok(Decoded::Unknown(v)) => {
                tracing::debug!(peer = %peer, variant = v, "unknown frame skipped");
                self.hot.bump(|h| &mut h.unknown_frames);
                return;
            }
            Err(DecodeError::Limit { field, cap }) => {
                tracing::debug!(peer = %peer, field, cap, "frame over a cap dropped");
                self.hot.bump(|h| &mut h.frames_over_cap);
                log_limited!(warn, peer = %peer, field, cap, "frames over a cap dropped");
                return;
            }
            Err(e) => {
                tracing::warn!(peer = %peer, error = %e, "undecodable frame: closing");
                self.close(&peer, CloseCode::PROTOCOL, "bad frame");
                return;
            }
        };
        tracing::trace!(peer = %peer.short(), variant = msg.variant(), session = ?msg.session(), "frame in");
        let has_hello = self.peers.get(&peer).is_some_and(|p| p.hello.is_some());
        match msg {
            Control::Hello {
                proto_major,
                proto_minor,
                name,
                features,
                monitors,
                ..
            } => {
                self.hello(peer, (proto_major, proto_minor), name, features, monitors);
            }
            Control::Error { code, message } => {
                // Per frame at debug; a peer can send these at line rate. One warning a
                // second says that it happens, and `peer_errors` how often.
                tracing::debug!(peer = %peer, code, message = %message.escape_debug(), "peer error");
                self.hot.bump(|h| &mut h.peer_errors);
                log_limited!(warn, peer = %peer, code, "peer errors (the text is at debug)");
                if code == wire::ErrorCode::Version as u16 {
                    self.refused.entry(peer).or_insert(message);
                }
            }
            _ if !has_hello => {
                tracing::warn!(peer = %peer, variant = msg.variant(), "first frame is not Hello: closing");
                self.close(&peer, CloseCode::PROTOCOL, "Hello first");
            }
            _ if !self.peers.get(&peer).is_some_and(|p| p.up) => {
                // Refused peers' last frames.
            }
            Control::Keymap {
                serial,
                format,
                text,
            } => {
                let c = self.canon.canonical(text);
                tracing::debug!(
                    peer = %peer.short(),
                    serial,
                    raw_len = c.raw_len,
                    raw_hash = %c.raw_hash,
                    len = c.text.len(),
                    hash = %c.hash(),
                    compiled = c.compiled,
                    "keymap received"
                );
                self.target_event(TargetEvent::Control {
                    peer: PeerId(peer.0),
                    msg: Control::Keymap {
                        serial,
                        format,
                        text: c.text,
                    },
                });
            }
            Control::Enter { .. } => {
                self.feed_source_busy();
                self.target_event(TargetEvent::Control {
                    peer: PeerId(peer.0),
                    msg,
                });
            }
            Control::Leave { .. }
            | Control::Key { .. }
            | Control::Button { .. }
            | Control::Scroll { .. }
            | Control::Held { .. } => {
                if matches!(msg, Control::Held { .. }) {
                    self.last_held = Some(std::time::Instant::now());
                }
                self.target_event(TargetEvent::Control {
                    peer: PeerId(peer.0),
                    msg,
                });
            }
            Control::HeldAck { .. } | Control::Ended { .. } => {
                self.for_capture(CaptureCmd::Event(SourceEvent::Control {
                    peer: PeerId(peer.0),
                    msg,
                }));
            }
            // Latest wins: a flood of these costs ctl one message.
            Control::Monitors { monitors } => self.out.monitors(peer, monitors),
            ref m @ Control::Layout { .. } => {
                if let Some(layout) = Layout::from_wire(m) {
                    self.out.layout(peer, layout);
                }
            }
            // Reserved for M2's conflict UI.
            Control::LayoutAck { .. } => {}
            Control::PeerState { locked } => {
                let id = PeerId(peer.0);
                let changed = {
                    let mut l = self.live.lock();
                    let was = l.locked_peers.contains(&id);
                    l.locked_peers.retain(|q| *q != id);
                    if locked {
                        l.locked_peers.push(id);
                    }
                    was != locked
                };
                // One info line for a change, and at most one a second; a peer that toggles at
                // line rate is counted.
                tracing::debug!(peer = %peer.short(), locked, "peer state");
                self.hot.bump(|h| &mut h.peer_state_frames);
                if changed {
                    log_limited!(info, peer = %peer.short(), locked, "peer state");
                    // `peers` and `status` show it; no message to ctl carries it
                    self.live.changed();
                }
                self.for_capture(CaptureCmd::Event(SourceEvent::PeerLocked {
                    peer: id,
                    locked,
                }));
            }
            msg @ (Control::ClipOffer { .. }
            | Control::ClipRequest { .. }
            | Control::ClipAbort { .. }) => self.clip_frame(peer, msg),
        }
    }

    /// `SourceBusy` for the TargetMachine, from the role the capture thread last published:
    /// fed before every `Enter`, and only when it changed.
    fn feed_source_busy(&mut self) {
        let busy = self
            .live
            .lock()
            .source
            .as_ref()
            .is_some_and(|s| s.role != SourceRole::Local);
        if busy != self.source_busy {
            self.source_busy = busy;
            self.target_event(TargetEvent::SourceBusy(busy));
        }
    }

    /// `PeerState{locked}` to `peer`, if it is up and listed `peer-state`.
    fn send_peer_state(&mut self, peer: &Fingerprint) {
        let msg = Control::PeerState {
            locked: self.locked,
        };
        if self
            .peers
            .get(peer)
            .is_some_and(|p| p.up && msg.may_send_to(&p.features))
        {
            self.send(peer, &msg);
        }
    }

    fn datagram(&mut self, peer: Fingerprint, bytes: &[u8]) {
        if !self.peers.get(&peer).is_some_and(|p| p.up) {
            return;
        }
        let d = match Datagram::decode(bytes) {
            Ok(d) => d,
            Err(_) => {
                self.hot.bump(|h| &mut h.bad_datagrams);
                return;
            }
        };
        let id = PeerId(peer.0);
        match d {
            Datagram::Ping { id: ping, t_us } => {
                self.send_datagram(&peer, Datagram::Pong { id: ping, t_us });
                self.for_capture(CaptureCmd::Event(SourceEvent::PeerAlive(id)));
            }
            Datagram::Pong { .. } => {
                self.for_capture(CaptureCmd::Event(SourceEvent::PeerAlive(id)));
            }
            Datagram::Motion(_) => self.target_event(TargetEvent::Datagram { peer: id, dgram: d }),
        }
    }

    // --- the TargetMachine ------------------------------------------------------------------

    fn target_event(&mut self, ev: TargetEvent) {
        self.target.handle(ev, clock::now(), &mut self.tout);
        let mut actions = std::mem::take(&mut self.tdo);
        actions.extend(self.tout.drain());
        for a in actions.drain(..) {
            match a {
                TargetAction::Inject(cmd) => {
                    if let InjectCmd::SetKeymap(text) = &cmd {
                        tracing::debug!(
                            len = text.len(),
                            hash = %short_hash(text),
                            swaps = self.target.counters().keymap_swaps,
                            "keymap applied: a new virtual keyboard"
                        );
                    }
                    match self.injector.as_mut() {
                        Some(i) => i.send(cmd),
                        None => tracing::trace!(?cmd, "no injector"),
                    }
                }
                TargetAction::Send { peer, msg } => {
                    let fp = Fingerprint(peer.0);
                    self.send(&fp, &msg);
                }
                TargetAction::Controlled(true) => {
                    // A `Controlled(false)` still waiting for its devices to go is void: the
                    // capture thread never saw it, so its capture stays off for this session.
                    if self.rearm_after.take().is_none() {
                        self.for_capture(CaptureCmd::Event(SourceEvent::Controlled(true)));
                    }
                }
                TargetAction::Controlled(false) => {
                    // On a peer that captures, the session's end drops the devices at
                    // once (sync first, so the ReleaseAll has reached the compositor), and only
                    // then do the barriers come back. The owner thread syncs and drops; its
                    // `Closed` sends this on (`input_event`).
                    match self.injector.as_mut() {
                        Some(i) if self.capture_on => match i.drop_devices() {
                            Some(seq) => self.rearm_after = Some(seq),
                            None => {
                                self.for_capture(CaptureCmd::Event(SourceEvent::Controlled(false)))
                            }
                        },
                        _ => self.for_capture(CaptureCmd::Event(SourceEvent::Controlled(false))),
                    }
                }
                TargetAction::CheckCursor => {
                    if self.ctl.try_send(CtlMsg::CheckCursor).is_err() {
                        self.live.bump("ctl_queue_full");
                    }
                }
                TargetAction::Status(s) => {
                    tracing::debug!(entered_by = ?s.entered_by, session = s.session,
                        last_end = s.last_end.map(|r| r.as_str()), "target status");
                    if s.entered_by.is_none()
                        && s.last_end == Some(wire::LeaveReason::Watchdog)
                        && let Some(at) = self.last_held
                    {
                        tracing::info!(
                            session = s.session,
                            after_last_held_ms = at.elapsed().as_millis(),
                            "target watchdog: released everything"
                        );
                    }
                    if let Some(c) = target_crossed(&mut self.entered, &s) {
                        if let Crossed::Target {
                            peer, home: false, ..
                        } = c
                        {
                            self.clip_partner(Fingerprint(peer.0), false);
                        }
                        if self.ctl.try_send(CtlMsg::Crossed(c)).is_err() {
                            self.live.bump("ctl_queue_full");
                        }
                    }
                    self.live.target_status(s);
                }
            }
        }
        self.tdo = actions;
        self.live.lock().target_counters = self.target.counters().clone();
        if let Some(why) = self.injector.as_mut().and_then(|i| i.take_gone()) {
            self.inject_failed(&why);
        }
    }

    /// The injection backend failed: what the session sends next would be lost, and keys
    /// it held may be down. ferry-inject's held set is gone with it (`InjectReset`), the
    /// session ends as forced, so the source releases (`Ended`), and the devices go: the next
    /// session creates new ones, whose modifier reset clears the seat.
    fn inject_failed(&mut self, why: &str) {
        log_limited!(
            warn,
            why,
            "ferry-inject failed: the session ends and the devices are rebuilt"
        );
        self.live.bump("inject_failures");
        self.target_event(TargetEvent::InjectReset);
        self.target_event(TargetEvent::ForceRelease);
        if let Some(i) = self.injector.as_mut() {
            // Their `Closed` matters only to a `Controlled(false)`, which asked on its own.
            let _ = i.drop_devices();
        }
    }

    fn input_event(&mut self, e: DeviceEvent) {
        let e = match self.injector.as_mut() {
            Some(i) => i.event(e),
            None => Some(e),
        };
        match e {
            Some(DeviceEvent::Input(e)) => self.inject_event(e),
            Some(DeviceEvent::Closed { seq }) => {
                if self.rearm_after.is_some_and(|after| seq >= after) {
                    self.rearm_after = None;
                    self.for_capture(CaptureCmd::Event(SourceEvent::Controlled(false)));
                }
            }
            Some(DeviceEvent::Opened { .. } | DeviceEvent::OpenFailed { .. }) | None => {}
        }
        if let Some(why) = self.injector.as_mut().and_then(|i| i.take_gone()) {
            self.inject_failed(&why);
        }
    }

    /// An event of ferry-inject's own thread.
    fn inject_event(&mut self, e: InputEvent) {
        match e {
            InputEvent::Reset => self.target_event(TargetEvent::InjectReset),
            InputEvent::Overflow { queued } => {
                log_limited!(
                    warn,
                    queued,
                    "ferry-inject overflowed and released everything"
                );
                self.live.bump("inject_overflow");
            }
            InputEvent::KeymapRejected(why) => log_limited!(warn, %why, "keymap rejected"),
            // Fatal when the thread ends on a Wayland error; else a keyboard that could not be
            // re-created. Either way the session ends and the devices are rebuilt.
            InputEvent::Error(why) => self.inject_failed(&why),
            InputEvent::Ready | InputEvent::Rebound(_) => {}
        }
    }

    // --- sending ------------------------------------------------------------------------------

    fn send(&mut self, peer: &Fingerprint, msg: &Control) {
        let Some(p) = self.peers.get(peer) else {
            return;
        };
        msg.encode(&mut self.frame);
        if let Err(e) = p.conn.try_send_frame(Bytes::copy_from_slice(&self.frame)) {
            tracing::debug!(peer = %peer, variant = msg.variant(), error = %e, "frame not sent");
            self.live.bump("frames_not_sent");
        }
    }

    fn broadcast(&mut self, msg: &Control) {
        let up: Vec<Fingerprint> = self
            .peers
            .iter()
            .filter(|(_, p)| p.up)
            .map(|(fp, _)| *fp)
            .collect();
        for fp in up {
            self.send(&fp, msg);
        }
    }

    fn send_datagram(&self, peer: &Fingerprint, d: Datagram) {
        if let Some(p) = self.peers.get(peer) {
            let mut b = [0u8; DATAGRAM_LEN];
            d.encode(&mut b);
            let _ = p.conn.send_datagram(Bytes::copy_from_slice(&b));
        }
    }

    fn close(&self, peer: &Fingerprint, code: CloseCode, reason: &str) {
        if let Some(p) = self.peers.get(peer) {
            p.conn.close(code, reason);
        }
    }

    /// One message for the capture thread, after any that are still owed. A full queue keeps
    /// it for the next iteration (RG-CONC-02 as amended): a lost `PeerDown`, `Conn{None}` or
    /// `Ended` would leave a crossing to a gone peer, or to a target that ended it, to the
    /// silence watchdog. Past [`CAPTURE_OWED_CAP`] only liveness hints are dropped (the next one
    /// says the same).
    fn for_capture(&mut self, cmd: CaptureCmd) {
        if self.capture_owed.is_empty() {
            match self.capture.try_send(cmd) {
                Ok(()) => return,
                Err(TrySendError::Full(cmd)) => {
                    self.live.bump("capture_queue_full");
                    log_limited!(warn, "net → capture channel full: kept for the next try");
                    self.capture_owed.push_back(cmd);
                }
                Err(TrySendError::Disconnected(_)) => self.capture_gone(),
            }
            return;
        }
        let hint = matches!(
            cmd,
            CaptureCmd::Event(
                SourceEvent::PeerAlive(_)
                    | SourceEvent::Control {
                        msg: Control::HeldAck { .. },
                        ..
                    }
            )
        );
        if hint && self.capture_owed.len() >= CAPTURE_OWED_CAP {
            self.live.bump("capture_hints_dropped");
            return;
        }
        self.capture_owed.push_back(cmd);
    }

    /// Hands the capture thread what it owes, in order, as far as its queue takes it.
    fn flush_capture(&mut self) {
        while let Some(cmd) = self.capture_owed.pop_front() {
            match self.capture.try_send(cmd) {
                Ok(()) => {}
                Err(TrySendError::Full(cmd)) => {
                    self.capture_owed.push_front(cmd);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.capture_owed.clear();
                    self.capture_gone();
                    return;
                }
            }
        }
    }

    fn capture_gone(&self) {
        // Shutdown stops the capture thread before this one.
        if self.live.lock().shutting_down {
            tracing::debug!("net → capture: the capture thread has stopped for the shutdown");
        } else {
            tracing::error!("net → capture channel closed: the capture thread is gone");
        }
    }

    /// Copies the counters a peer can drive into `status`'s, if any moved.
    fn publish_hot(&mut self) {
        if !std::mem::take(&mut self.hot.dirty) {
            return;
        }
        let h = &self.hot;
        let mut l = self.live.lock();
        for (name, n) in [
            ("unknown_frames", h.unknown_frames),
            ("frames_over_cap", h.frames_over_cap),
            ("bad_datagrams", h.bad_datagrams),
            ("peer_errors", h.peer_errors),
            ("peer_state_frames", h.peer_state_frames),
        ] {
            if n > 0 {
                l.counters.insert(name, n);
            }
        }
    }

    /// Every 2 s: the dialer's Ping, and the RTT in `status`.
    fn tick(&mut self) {
        self.live
            .lock()
            .counters
            .insert("handshakes_refused", self.endpoint.handshakes_refused());
        self.live
            .lock()
            .counters
            .insert("handshakes_retried", self.endpoint.handshakes_retried());
        self.live
            .lock()
            .counters
            .insert("pairing_refused", self.endpoint.pairing_refused());
        let active = self.target.status().entered_by.is_some();
        if let Some(i) = self.injector.as_mut() {
            i.maintain(active);
            crate::inject::publish_stats(&self.live, i.stats_delta());
        }
        self.clip_tick();
        self.ping_id = self.ping_id.wrapping_add(1);
        let t_us = clock::now().low32();
        let up: Vec<(Fingerprint, bool, String, Option<u32>)> = self
            .peers
            .iter()
            .filter(|(_, p)| p.up)
            .map(|(fp, p)| {
                (
                    *fp,
                    p.conn.is_dialer(),
                    p.conn.remote_addr().to_string(),
                    rtt_us(&p.conn),
                )
            })
            .collect();
        for (fp, dialer, remote, rtt) in up {
            if dialer {
                self.send_datagram(
                    &fp,
                    Datagram::Ping {
                        id: self.ping_id,
                        t_us,
                    },
                );
            }
            self.report(fp, LinkState::Connected, Some(remote), rtt, None);
        }
    }

    // --- ctl ----------------------------------------------------------------------------------

    fn report(
        &self,
        peer: Fingerprint,
        state: LinkState,
        remote: Option<String>,
        rtt_us: Option<u32>,
        detail: Option<String>,
    ) {
        self.report_full(peer, state, remote, rtt_us, None, detail);
    }

    /// The link state for `status`: latest wins.
    fn report_full(
        &self,
        peer: Fingerprint,
        state: LinkState,
        remote: Option<String>,
        rtt_us: Option<u32>,
        proto: Option<String>,
        detail: Option<String>,
    ) {
        self.out.link(LinkUpdate {
            peer,
            state,
            remote,
            rtt_us,
            proto,
            detail,
        });
    }

    /// Dials `addr`. An IP literal dials at once. A name is resolved on a spawned task and
    /// dialled when its answer comes back through [`Net::resolved`]: a lookup can take seconds
    /// (nss-mdns waits about 5 s), and this loop beats the daemon watchdog (WatchdogSec=2s).
    fn dial(&mut self, peer: Fingerprint, addr: &str) {
        // A newer request for the peer supersedes an answer still on its way.
        self.resolving.remove(&peer);
        let hp = match parse_host_port(addr, ferry_net::DEFAULT_PORT) {
            Ok(hp) => hp,
            Err(e) => {
                self.cannot_dial(peer, addr, &e.to_string());
                return;
            }
        };
        if let Some(sock) = hp.literal() {
            tracing::info!(peer = %peer, addr = %sock, "dialing");
            self.manager.dial(peer, sock);
            return;
        }
        self.next_lookup += 1;
        let generation = self.next_lookup;
        self.resolving.insert(peer, generation);
        let (tx, lookup, addr) = (
            self.resolved_tx.clone(),
            self.lookup.clone(),
            addr.to_string(),
        );
        tokio::spawn(async move {
            let result = hp.resolve_with(&lookup).await.map_err(|e| e.to_string());
            let _ = tx
                .send(Resolved {
                    peer,
                    generation,
                    addr,
                    result,
                })
                .await;
        });
    }

    fn resolved(&mut self, r: Resolved) {
        if self.resolving.get(&r.peer) != Some(&r.generation) {
            tracing::debug!(peer = %r.peer, addr = %r.addr, "a stale lookup answer: dropped");
            return;
        }
        self.resolving.remove(&r.peer);
        match r.result {
            Ok(sock) => {
                tracing::info!(peer = %r.peer, addr = %sock, "dialing");
                self.manager.dial(r.peer, sock);
            }
            Err(e) => self.cannot_dial(r.peer, &r.addr, &e),
        }
    }

    fn cannot_dial(&self, peer: Fingerprint, addr: &str, error: &str) {
        tracing::warn!(peer = %peer, addr, error, "cannot dial");
        self.report(
            peer,
            LinkState::Offline,
            None,
            None,
            Some(format!("cannot resolve {addr}: {error}")),
        );
    }
}

fn rtt_us(conn: &Connection) -> Option<u32> {
    u32::try_from(conn.rtt().as_micros()).ok()
}

/// A dial that failed because the other side or this side refuses the pair is `refused`;
/// one that could not reach the peer is `offline` and will be retried.
fn link_state_for(e: &ConnectError) -> LinkState {
    match e {
        ConnectError::UnknownPeer(_)
        | ConnectError::FingerprintChanged { .. }
        | ConnectError::NotTrustedByPeer(_)
        | ConnectError::RefusedByDialer
        | ConnectError::VersionMismatch
        | ConnectError::BadCertificate
        | ConnectError::DatagramsUnsupported => LinkState::Refused,
        _ => LinkState::Offline,
    }
}

/// Forwards one connection's events to the thread's loop, in order, until it ends.
async fn drain(
    mut events: Events,
    conn: Connection,
    tx: mpsc::Sender<(Fingerprint, usize, ConnEvent)>,
) {
    let (peer, id) = (conn.peer_fp(), conn.id());
    while let Some(ev) = events.next().await {
        let last = matches!(ev, Event::Closed(_));
        if tx.send((peer, id, ConnEvent::Event(ev))).await.is_err() || last {
            return;
        }
    }
}

/// Used by `ctl` to wait for a `NetCmd` with an ack.
pub async fn ask(
    net: &mpsc::Sender<NetCmd>,
    make: impl FnOnce(oneshot::Sender<()>) -> NetCmd,
    timeout: Duration,
) -> bool {
    let (ack, acked) = oneshot::channel();
    if net.send(make(ack)).await.is_err() {
        return false;
    }
    matches!(tokio::time::timeout(timeout, acked).await, Ok(Ok(())))
}

/// The crossing that a new target status starts or brings home, for the `edge` event.
/// `entered` keeps the source and `Enter` point of the session in progress, so that its end can
/// name the edge. A preemption by another source starts a new crossing.
fn target_crossed(
    entered: &mut Option<(PeerId, (f64, f64))>,
    s: &ferry_core::target::TargetStatus,
) -> Option<Crossed> {
    let now = s.entered_by.zip(s.entry);
    if now.is_some() && now.map(|n| n.0) != entered.map(|e| e.0) {
        *entered = now;
        let (peer, entry) = now?;
        return Some(Crossed::Target {
            peer,
            entry,
            home: false,
        });
    }
    if now.is_none() {
        let (peer, entry) = entered.take()?;
        return (s.last_end == Some(wire::LeaveReason::Home)).then_some(Crossed::Target {
            peer,
            entry,
            home: true,
        });
    }
    None
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use ferry_core::target::TargetStatus;

    fn status(
        by: Option<PeerId>,
        entry: Option<(f64, f64)>,
        end: Option<wire::LeaveReason>,
    ) -> TargetStatus {
        TargetStatus {
            entered_by: by,
            entry,
            session: 1,
            last_end: end,
        }
    }

    /// An `Enter` is told to ctl once; its end is told only when it came home; a
    /// preemption by another source is a new crossing.
    #[test]
    fn an_entered_session_is_told_in_and_home_once_each() {
        let (a, b) = (PeerId([1; 32]), PeerId([2; 32]));
        let mut entered = None;
        let crossed = |peer, home| Crossed::Target {
            peer,
            entry: (900.0, 0.0),
            home,
        };
        let by = |p| status(Some(p), Some((900.0, 0.0)), None);
        assert_eq!(
            target_crossed(&mut entered, &by(a)),
            Some(crossed(a, false))
        );
        assert_eq!(target_crossed(&mut entered, &by(a)), None);
        assert_eq!(
            target_crossed(&mut entered, &by(b)),
            Some(crossed(b, false))
        );
        let home = status(None, None, Some(wire::LeaveReason::Home));
        assert_eq!(target_crossed(&mut entered, &home), Some(crossed(b, true)));
        assert_eq!(target_crossed(&mut entered, &home), None);
        target_crossed(&mut entered, &by(a));
        let lost = status(None, None, Some(wire::LeaveReason::Watchdog));
        assert_eq!(target_crossed(&mut entered, &lost), None);
        assert_eq!(entered, None);
    }
}
