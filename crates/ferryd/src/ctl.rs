//! The `ctl` thread: a tokio current-thread runtime that owns the socket API, the state files,
//! the trust store, the layout and its topology, and (through [`portal_driver`]) the portal
//! session, the lock checks and logind.
//!
//! Everything it owns lives in one [`State`] behind a `RefCell`, because the runtime is a single
//! thread: a request handler borrows it, never across an `.await`.
//!
//! [`portal_driver`]: crate::portal_driver

use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixListener as StdListener;
use std::rc::Rc;
use std::time::Duration;

use calloop::channel::SyncSender;
use ferry_core::api::{LinkState, PeerRef};
use ferry_core::source::SourceEvent;
use ferry_hypr_ipc::Instance;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use crate::chan::{CaptureCmd, CtlMsg, LinkUpdate, NetCmd};
use crate::fake_portal::FakePortal;
use crate::layout_store::peer_id;
use crate::live::SharedLive;
use crate::monitors::{self, MonitorSource};
use crate::net_thread;
use crate::paths::Paths;
use crate::portal_driver::{self, Driver, DriverMsg};
use crate::ratelimit::log_limited;
use crate::server::{self, SocketGuard};
use crate::state::State;
use crate::watchdog::{self, Notifier};

/// What request handlers share.
pub struct Ctl {
    pub state: RefCell<State>,
    pub paths: Paths,
    pub net: mpsc::Sender<NetCmd>,
    pub capture: SyncSender<CaptureCmd>,
    pub live: SharedLive,
    /// The capture side runs on this peer (the config allows it and the daemon was started
    /// with input enabled).
    pub capture_on: bool,
    /// Hyprland IPC, when this daemon may use it (lock checks, the IPC release).
    pub ipc: Option<Instance>,
    /// Tests: serves the portal commands in place of the portal.
    pub fake_portal: Option<FakePortal>,
    /// The portal driver's queue (the capture thread fills it), for the shutdown's `Flush` and
    /// `Close`. `None` without the capture side.
    pub driver: Option<mpsc::Sender<DriverMsg>>,
    /// The uid allowed to talk to the socket.
    pub uid: u32,
    /// The pairing a socket connection started.
    pub pairing: RefCell<Option<crate::pairing::CtlPairing>>,
    /// A peer's new monitors wait to be saved ([`KNOWN_SAVE_EVERY`]).
    pub known_unsaved: Cell<bool>,
    /// The toasts: the hooks below call it, and a task on this runtime shows them.
    pub notify: crate::notify::Notifier,
    /// The frontends that subscribed to events.
    pub hub: crate::events::Hub,
}

/// Everything `Daemon::start` hands to the thread.
pub struct CtlInit {
    pub state: State,
    pub paths: Paths,
    pub net: mpsc::Sender<NetCmd>,
    pub capture: SyncSender<CaptureCmd>,
    pub live: SharedLive,
    pub capture_on: bool,
    pub ipc: Option<Instance>,
    pub fake_portal: Option<FakePortal>,
    /// The portal driver's queue: what the capture thread sends before start-up step 4 waits
    /// in it (its first `CheckLock` comes at once).
    pub driver: Option<(mpsc::Sender<DriverMsg>, mpsc::Receiver<DriverMsg>)>,
    pub net_thread: std::thread::JoinHandle<()>,
    pub capture_thread: std::thread::JoinHandle<()>,
    pub rx: mpsc::Receiver<CtlMsg>,
    pub tx: mpsc::Sender<CtlMsg>,
    pub listener: StdListener,
    pub guard: SocketGuard,
    pub monitors: MonitorSource,
    pub handle_signals: bool,
    pub uid: u32,
    /// Where `READY=1`, `WATCHDOG=1` and `STOPPING=1` go.
    pub notify: Notifier,
    /// systemd's watchdog is on: send `WATCHDOG=1` every tick while both threads are alive.
    pub watchdog: bool,
    /// Fires once the socket is being served.
    pub ready: std::sync::mpsc::Sender<()>,
    /// Where the toasts go. [`crate::notify::Options::default`] sends nowhere.
    pub toasts: crate::notify::Options,
}

/// Runs the thread's loop until `Shutdown` or a signal. Blocks.
pub fn run(init: CtlInit) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime builds");
    let local = LocalSet::new();
    local.block_on(&rt, main(init));
}

type Signals = Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>;

/// SIGINT and SIGTERM handlers, installed before the daemon says it is ready, so a signal that
/// follows `READY=1` at once is never the default action.
fn install_signals(enabled: bool) -> Signals {
    use tokio::signal::unix::{SignalKind, signal};
    if !enabled {
        return None;
    }
    match (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) {
        (Ok(int), Ok(term)) => Some((int, term)),
        _ => {
            tracing::warn!("cannot install signal handlers");
            None
        }
    }
}

async fn wait_for_signal(signals: Signals) {
    let Some((mut int, mut term)) = signals else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = int.recv() => tracing::info!("SIGINT"),
        _ = term.recv() => tracing::info!("SIGTERM"),
    }
}

async fn main(init: CtlInit) {
    let CtlInit {
        state,
        paths,
        net,
        capture,
        live,
        capture_on,
        ipc,
        fake_portal,
        driver,
        net_thread,
        capture_thread,
        mut rx,
        tx,
        listener,
        guard,
        monitors: source,
        handle_signals,
        uid,
        notify,
        watchdog,
        ready,
        toasts,
    } = init;

    let listener = match tokio::net::UnixListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot serve the socket");
            return;
        }
    };
    let (driver, mut driver_rx) = match driver {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };
    let (notifier, notify_rx) = crate::notify::channel(live.clone());
    let notify_enabled = state.config.notify.enabled;
    let hub_signal = live.signal().clone();
    let ctl = Rc::new(Ctl {
        state: RefCell::new(state),
        paths,
        net,
        capture,
        live,
        capture_on,
        ipc,
        fake_portal,
        driver,
        uid,
        pairing: RefCell::new(None),
        known_unsaved: Cell::new(false),
        notify: notifier,
        hub: crate::events::Hub::new(hub_signal),
    });
    crate::notify::spawn(&ctl, notify_rx, toasts, notify_enabled);

    tokio::task::spawn_local(crate::events::run(ctl.clone()));
    tokio::task::spawn_local(monitors::watch(source, tx));
    let signals = install_signals(handle_signals);
    // The heartbeat runs on this loop's executor: its own tick proves ctl alive, and it goes on
    // through the shutdown order, whose awaits keep driving it.
    let beats = ctl.live.beats().clone();
    if watchdog {
        tokio::task::spawn_local(watchdog::heartbeat(beats.clone(), notify.clone()));
    }
    let _ = ready.send(());

    let mut signal = std::pin::pin!(wait_for_signal(signals));
    loop {
        // Test hook (`Daemon::pause_ctl`): the queue stays unread, the socket is still served.
        let paused = beats.ctl_paused();
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::task::spawn_local(server::serve_connection(stream, ctl.clone()));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            () = tokio::time::sleep(paused.unwrap_or_default()), if paused.is_some() => {}
            msg = rx.recv(), if paused.is_none() => {
                let quiet = msg.as_ref().is_some_and(CtlMsg::is_quiet);
                match msg {
                    Some(CtlMsg::Shutdown) | None => break,
                    Some(CtlMsg::Begin) => begin(&ctl, driver_rx.take()).await,
                    Some(msg) => apply(&ctl, msg).await,
                }
                // A frontend may see the effect of the message, and so may
                // `attention`, which is a toast.
                if !quiet {
                    ctl.hub.poke();
                    ctl.notify.poke();
                }
            }
            () = &mut signal => break,
        }
    }

    beats.set_stopping();
    save_known(&ctl);
    shutdown(&ctl, &mut rx, &notify, net_thread, capture_thread).await;
    drop(listener);
    drop(guard);
}

/// Start-up steps 4 and 5: the IPC release and the portal session (unless capture
/// is off), then dial the pinned peers that have an address.
async fn begin(ctl: &Ctl, driver: Option<mpsc::Receiver<DriverMsg>>) {
    if ctl.capture_on
        && let Some(rx) = driver
    {
        if let Some(fake) = ctl.fake_portal.clone() {
            tokio::task::spawn_local(fake.run(rx, ctl.capture.clone(), ctl.live.clone()));
        } else {
            let budget = ctl.state.borrow().config.lock_check_budget();
            let driver = Driver::new(
                ctl.ipc.clone(),
                budget.as_duration(),
                ctl.capture.clone(),
                ctl.live.clone(),
            );
            tokio::task::spawn_local(driver.run(rx));
        }
        push_source_config(ctl).await;
        push_topology(ctl);
    }

    let to_dial: Vec<_> = {
        let mut s = ctl.state.borrow_mut();
        let pins: Vec<_> = s
            .trust
            .pins()
            .iter()
            .filter_map(|p| p.addr.clone().map(|a| (p.fingerprint, a)))
            .collect();
        for (fp, _) in &pins {
            s.apply_link(LinkUpdate {
                peer: *fp,
                state: LinkState::Connecting,
                remote: None,
                rtt_us: None,
                proto: None,
                detail: None,
            });
        }
        pins
    };
    for (peer, addr) in to_dial {
        to_net(ctl, NetCmd::Dial { peer, addr });
    }
}

/// A command for net from ctl's main loop. The loop never awaits net (RG-CONC-02 as amended,
/// B-07): the send waits on a task of its own. Tasks are first polled in the order they were
/// spawned, and a full queue serves its waiting senders in order, so these commands keep theirs.
pub fn to_net(ctl: &Ctl, cmd: NetCmd) {
    let net = ctl.net.clone();
    tokio::task::spawn_local(async move {
        let _ = net.send(cmd).await;
    });
}

async fn apply(ctl: &Rc<Ctl>, msg: CtlMsg) {
    match msg {
        CtlMsg::Link(up) => {
            tracing::debug!(peer = %up.peer, state = ?up.state, detail = ?up.detail, "link");
            let (peer, state) = (up.peer, up.state.clone());
            // A trusted peer's link is a toast; the link of one that is not pinned
            // (any more) is not.
            let name = {
                let mut s = ctl.state.borrow_mut();
                s.apply_link(up);
                s.trust.get(&peer).map(|p| p.name.clone())
            };
            if let Some(name) = name {
                ctl.notify.link(peer, &name, &state);
            }
        }
        CtlMsg::Revoked { peer } => {
            // FR-PAIR-08. The toast first and at once, so the `Offline` that follows is not a
            // "Lost" toast; the pin goes in a task of its own, because it waits for net
            // (RG-CONC-02: this loop never does).
            let name = ctl.state.borrow().trust.get(&peer).map(|p| p.name.clone());
            if let Some(name) = name {
                ctl.notify.unpaired(peer, &name);
                let ctl = ctl.clone();
                tokio::task::spawn_local(async move {
                    crate::pairing::revoked(&ctl, peer, &name).await;
                });
            }
        }
        CtlMsg::InboundFailed(error) => {
            use ferry_net::ConnectError as E;
            let mut s = ctl.state.borrow_mut();
            match error {
                // The dialer gave up before the connection opened. A peer that restarts while
                // a dial to it is in flight sees one of these, so it is counted
                // apart from refusals and not logged as one.
                E::Abandoned | E::TimedOut => {
                    s.bump("inbound_abandoned");
                    tracing::debug!(%error, "an inbound handshake was abandoned by its dialer");
                }
                E::UnknownPeer(fp) => {
                    let pairing = ctl.pairing.borrow();
                    let pairing_with = pairing
                        .as_ref()
                        .and_then(|p| p.peer_fingerprint())
                        .is_some_and(|p| p == fp.to_string());
                    match s.inbound_unknown(fp, pairing_with) {
                        crate::state::Inbound::Refused { first: true } => {
                            tracing::warn!(fingerprint = %fp, "refused unknown peer: its fingerprint is not pinned here; `ferry trust {fp}` pins it");
                        }
                        // a peer that this one unpaired keeps dialling: once is enough
                        crate::state::Inbound::Refused { first: false } => {
                            tracing::debug!(fingerprint = %fp, "refused unknown peer again");
                        }
                        crate::state::Inbound::BeforePin => {
                            tracing::debug!(fingerprint = %fp, "the peer just paired dialed before its pin was live here; it redials");
                        }
                    }
                }
                // Counted by the endpoint (`pairing_refused`); the dialer sees "not accepting
                // pairing". Someone ran `ferry pair` at this machine: not a fault here.
                E::PairingRefused => {
                    tracing::info!(
                        "refused a pairing request: no `ferry pair --accept` window is open here, another attempt runs, or that address tried too often"
                    );
                }
                other => {
                    s.bump("inbound_refused");
                    tracing::warn!(error = %other, "refused an inbound connection");
                }
            }
        }
        CtlMsg::OwnMonitors(reading) => {
            // The reading gets the anchor shift that keeps the group where it was
            // (M3 design 4.1). The layout is not touched: no revision for a monitor change.
            let (monitors, remember) = {
                let mut s = ctl.state.borrow_mut();
                let before = s.layout.anchors.shift;
                let (monitors, remember) = s.layout.observe_own_monitors(&reading);
                if s.layout.anchors.shift != before {
                    tracing::info!(from = ?before, to = ?s.layout.anchors.shift,
                        "Hyprland moved the own monitors; the group is kept in place");
                }
                s.own_monitors = monitors.clone();
                (monitors, remember)
            };
            if remember {
                save_soon(ctl);
            }
            push_topology(ctl);
            // The peers learn within 1 s; the next Hello carries them too.
            to_net(ctl, NetCmd::OwnMonitors(monitors));
        }
        CtlMsg::HyprConfigReloaded => {
            if ctl.capture_on {
                push_source_config(ctl).await;
            }
        }
        CtlMsg::PeerUp { peer, monitors } => {
            set_known(ctl, peer, monitors);
            // A peer paired here as the initiator is placed now that its
            // monitors are known; every peer gets the new revision.
            if let Some(wire) = crate::api::place_paired(ctl, peer) {
                to_net(ctl, NetCmd::Broadcast(wire));
            }
            push_topology(ctl);
            // The whole layout goes to the peer at peer up; it keeps the higher
            // revision.
            let msg = {
                let s = ctl.state.borrow();
                (!s.layout.layout.origins.is_empty()).then(|| s.layout.layout.to_wire())
            };
            if let Some(msg) = msg {
                to_net(ctl, NetCmd::Send { peer, msg });
            }
        }
        CtlMsg::PeerMonitors { peer, monitors } => {
            set_known(ctl, peer, monitors);
            push_topology(ctl);
        }
        CtlMsg::PeerLayout { peer, layout } => on_peer_layout(ctl, peer, layout),
        CtlMsg::IpcRelease => {
            if ctl.capture_on {
                tokio::task::spawn_local(portal_driver::ipc_release(ctl.ipc.clone()));
            }
        }
        CtlMsg::CheckCursor => {
            // The takeover poll: a task, so a slow compositor never holds
            // up the socket. No IPC: no answer, and net never turned the poll on.
            if let Some(ipc) = ctl.ipc.clone() {
                let net = ctl.net.clone();
                tokio::task::spawn_local(async move {
                    let pos = cursor_pos(&ipc, CURSOR_TIMEOUT).await;
                    let _ = net.send(NetCmd::CursorPolled(pos)).await;
                });
            }
        }
        CtlMsg::SourceCheckCursor => {
            // A parked barrier: a task, like the takeover poll. With no IPC the answer
            // is `None` at once, and the machine puts the barrier back.
            let capture = ctl.capture.clone();
            let ipc = ctl.ipc.clone();
            tokio::task::spawn_local(async move {
                let pos = match ipc {
                    Some(ipc) => cursor_pos(&ipc, CURSOR_TIMEOUT).await,
                    None => None,
                };
                tracing::debug!(?pos, "parked barrier: cursor read");
                let pos = pos.map(|(x, y)| ferry_core::geom::Point { x, y });
                let _ = capture.try_send(CaptureCmd::Event(SourceEvent::CursorPolled(pos)));
            });
        }
        CtlMsg::SourceGo { x, y, dx, dy } => {
            if let Some(fake) = &ctl.fake_portal {
                fake.nudged(x, y, dx, dy);
            } else if ctl.ipc.is_some() {
                // Blocking (IPC and Wayland round trips) on a thread of its own, not
                // `spawn_blocking`: its join would hold up this runtime's drop. If the activation
                // never comes, the machine's own timeout says so in the log.
                let spawned = std::thread::Builder::new()
                    .name("ferry-nudge".into())
                    .spawn(move || {
                        let target = ferry_inject::Target::default();
                        if let Err(e) = ferry_inject::nudge(&target, x, y, dx, dy) {
                            tracing::warn!(error = %e, "key crossing: the nudge failed");
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!(error = %e, "key crossing: no thread for the nudge");
                }
            }
        }
        CtlMsg::Pairing(note) => crate::pairing::on_note(ctl, note).await,
        CtlMsg::Notice(notice) => ctl.hub.publish_notice(&notice),
        CtlMsg::Ui(view) => ctl.hub.publish_ui(view),
        CtlMsg::Crossed(c) => {
            if let crate::chan::Crossed::Source { link, home: false } = c {
                let peer = ferry_net::Fingerprint(link.to.peer.0);
                let _ = ctl.net.try_send(NetCmd::ClipEntered(peer));
            }
            if ctl.hub.wants(ferry_core::api::Topic::Edge)
                && ctl.state.borrow().config.crossing.edge_glow
                && let Some(edge) = ctl.state.borrow().edge_event(c)
            {
                ctl.hub.publish_edge(&edge);
            }
        }
        CtlMsg::PeerSeen {
            peer,
            remote,
            dialed,
            up,
        } => crate::pairing::seen(ctl, peer, remote, dialed, up).await,
        CtlMsg::Begin | CtlMsg::Shutdown => {}
    }
}

/// A peer's new monitors reach `layout.toml` at most this long after they came, and at most
/// once per this long: a peer that sends `Monitors` in a loop costs one save (two
/// fsyncs) per period, not one per frame. They are in memory at once.
const KNOWN_SAVE_EVERY: Duration = Duration::from_millis(250);

/// Records a peer's monitors (`[[known]]`) and saves the file soon.
fn set_known(
    ctl: &Rc<Ctl>,
    peer: ferry_net::Fingerprint,
    monitors: Vec<ferry_core::geom::MonitorInfo>,
) {
    {
        let mut s = ctl.state.borrow_mut();
        let id = peer_id(peer);
        if s.layout.known_monitors(id) == Some(monitors.as_slice()) {
            return;
        }
        s.layout.set_known(id, monitors);
    }
    save_soon(ctl);
}

/// Saves `layout.toml` within [`KNOWN_SAVE_EVERY`]: what changed without a revision (a peer's
/// monitors, this peer's monitor memory) is in memory at once and on disk soon.
fn save_soon(ctl: &Rc<Ctl>) {
    if ctl.known_unsaved.replace(true) {
        return;
    }
    let ctl = ctl.clone();
    tokio::task::spawn_local(async move {
        tokio::time::sleep(KNOWN_SAVE_EVERY).await;
        save_known(&ctl);
    });
}

/// Saves the layout if a peer's monitors wait for it.
fn save_known(ctl: &Ctl) {
    if !ctl.known_unsaved.replace(false) {
        return;
    }
    let layout = ctl.state.borrow().layout.clone();
    if let Err(e) = layout.save(&ctl.paths.layout_file()) {
        // Still in memory: layout place works until the next restart.
        tracing::error!(error = %e, "cannot save the peer's monitors");
    }
}

/// A peer pushed its layout: the higher revision is kept, and saved.
///
/// FR-LAYOUT-10 (M3 design 4.3) rides on it. A peer that pushes a revision it made itself is told
/// when this peer holds it, by being sent the layout back; and when the revision that replaces
/// this peer's own edit comes from a peer that never held that edit, the edit lost.
fn on_peer_layout(ctl: &Rc<Ctl>, peer: ferry_net::Fingerprint, pushed: ferry_core::geom::Layout) {
    let from = peer_id(peer);
    // the sender made this revision itself: it is waiting to hear that it was kept
    let direct = pushed.rev.peer == from;
    let pushed_rev = pushed.rev;
    let (kept, echo, lost) = {
        let mut s = ctl.state.borrow_mut();
        let current = s.layout.layout.rev;
        if pushed_rev == current {
            // the same layout: this peer holds ours too
            s.edits.held_by(current, from);
            (false, direct.then(|| s.layout.layout.to_wire()), None)
        } else {
            let mut next = s.layout.clone();
            if !next.merge(pushed) {
                tracing::debug!(from = %peer, "layout from a peer: ours is not lower; kept ours");
                return;
            }
            if let Err(e) = next.save(&ctl.paths.layout_file()) {
                tracing::error!(error = %e, "cannot save the pushed layout");
                return;
            }
            tracing::info!(from = %peer, rev = next.layout.rev.counter,
                by = %next.layout.rev.peer, "layout from a peer: kept (higher revision)");
            s.layout = next;
            let lost = s
                .edits
                .replaced(current, pushed_rev)
                .filter(|winner| *winner != s.me.id)
                .map(|winner| PeerRef {
                    name: s.name_of(winner),
                    fingerprint: winner.to_string(),
                });
            (true, direct.then(|| s.layout.layout.to_wire()), lost)
        }
    };
    if kept {
        push_topology(ctl);
    }
    if let Some(msg) = echo {
        to_net(ctl, NetCmd::Send { peer, msg });
    }
    if let Some(winner) = lost {
        on_layout_replaced(ctl, &winner);
    }
}

/// This peer's own edit of the layout lost to `winner`'s higher revision, which was
/// not made on top of it. It logs, counts (`layout_replaced` in `status`) and shows the toast
/// `Layout replaced by <peer>` (M3 design 3), which is also a `notice` event.
pub fn on_layout_replaced(ctl: &Ctl, winner: &PeerRef) {
    ctl.state.borrow_mut().bump("layout_replaced");
    tracing::info!(
        winner = %winner.name,
        fingerprint = %winner.fingerprint,
        "layout replaced: this peer's own edit lost to a higher revision"
    );
    crate::notify::layout_replaced(ctl, &winner.name);
}

/// The topology from the layout, the known monitors and the pins, to the capture thread (the
/// barriers follow it, design §5.1).
pub fn push_topology(ctl: &Ctl) {
    if !ctl.capture_on {
        return;
    }
    let topo = ctl.state.borrow().topology();
    tracing::debug!(
        monitors = topo.monitors.len(),
        links = topo.links.len(),
        "topology"
    );
    if let Err(e) = ctl
        .capture
        .try_send(CaptureCmd::Event(SourceEvent::Topology(topo)))
    {
        log_limited!(error, error = %e, "cannot hand the topology to the capture thread");
    }
}

/// `[crossing]` plus the repeat hold from `input:repeat_delay`.
pub(crate) async fn push_source_config(ctl: &Ctl) {
    let delay = portal_driver::repeat_delay(&ctl.ipc).await;
    let hold = delay.map_or(0, portal_driver::repeat_hold_after);
    tracing::info!(repeat_delay_ms = ?delay, repeat_hold_after_ms = hold, "source config");
    let enforce = portal_driver::enforce_permissions(&ctl.ipc).await;
    ctl.live.lock().enforce_permissions = enforce;
    let cfg = ctl.state.borrow().config.source_config(hold);
    match cfg {
        Ok(cfg) => {
            let _ = ctl
                .capture
                .try_send(CaptureCmd::Event(SourceEvent::Config(cfg)));
        }
        Err(e) => tracing::error!(error = %e, "invalid [crossing] config"),
    }
}

/// Design §9 shutdown order: `Shutdown` to both machines → portal `Release` → ferry-inject
/// `sync()` → `close_graceful(100 ms)` → session close.
async fn shutdown(
    ctl: &Ctl,
    rx: &mut mpsc::Receiver<CtlMsg>,
    notify: &Notifier,
    net_thread: std::thread::JoinHandle<()>,
    capture_thread: std::thread::JoinHandle<()>,
) {
    tracing::info!("shutting down");
    ctl.live.lock().shutting_down = true;
    let _ = notify.send(&[sd_notify::NotifyState::Stopping]);
    // 1. Shutdown to both machines. The capture thread queues Leave(shutdown) on the
    //    connection and the portal Release to us, then stops; the TargetMachine queues
    //    ReleaseAll and Ended(shutdown).
    stop_capture(ctl, capture_thread).await;
    net_thread::ask(
        &ctl.net,
        |ack| NetCmd::ShutdownMachine { ack },
        Duration::from_secs(1),
    )
    .await;

    // 2. The portal Release (and anything else the capture thread queued), in order: the
    //    capture thread put it in the driver's queue before it stopped.
    while let Ok(msg) = rx.try_recv() {
        tracing::debug!(?msg, "dropped at shutdown");
    }
    if let Some(d) = &ctl.driver {
        driver_ack(d, DriverMsg::Flush).await;
    }

    // 3. ferry-inject sync(), then close_graceful(100 ms) on every connection, then the
    //    endpoint (the net thread does all three).
    net_thread::ask(
        &ctl.net,
        |ack| NetCmd::Shutdown { ack },
        Duration::from_secs(2),
    )
    .await;
    join_within(net_thread, "net", NET_STOP).await;

    // 4. Close the portal session.
    if let Some(d) = &ctl.driver {
        driver_ack(d, DriverMsg::Close).await;
    }

    // 5. Take down the pair-code toast, if one is open: a critical notification would outlive
    //    the daemon. Last, so nothing about input waits for the desktop.
    ctl.notify.shutdown().await;
}

/// How long the shutdown tries again to give the capture thread its `Shutdown` while that
/// thread's queue is full, and how long it then waits for the thread to end. The thread hands the
/// driver what it still owes for at most `DRIVER_EXIT_GRACE` (1 s) before it returns.
const CAPTURE_SHUTDOWN_SEND: Duration = Duration::from_secs(2);
const CAPTURE_STOP: Duration = Duration::from_secs(3);
/// How often the two waits above look again.
const SHUTDOWN_POLL: Duration = Duration::from_millis(2);

/// Shutdown step 1 for the capture thread: `Shutdown`, then wait for the thread to end.
///
/// The queue can be full here: a slow thread leaves it so after a burst of topology pushes (a peer
/// that floods `Monitors`). The message used to be sent once with `try_send` and dropped when it
/// did not fit, and the join that followed never returned, because the thread never learned that
/// it had to stop: `Daemon::shutdown` hung for good, and the shutdown's `Leave` and portal
/// `Release` were never sent. So it is tried again until it fits. The thread keeps draining its
/// queue, and ctl's main loop, which pushes the topology into it, has ended. Both waits are
/// bounded: a thread that takes nothing or does not end in time is logged and left
/// behind, and the order goes on. No `spawn_blocking`: a join that never returns would also hold
/// up this runtime's drop.
async fn stop_capture(ctl: &Ctl, thread: std::thread::JoinHandle<()>) {
    use std::sync::mpsc::TrySendError;
    use std::time::Instant;

    let until = Instant::now() + CAPTURE_SHUTDOWN_SEND;
    loop {
        match ctl.capture.try_send(CaptureCmd::Shutdown) {
            // `Disconnected`: the thread has ended already; the join says how.
            Ok(()) | Err(TrySendError::Disconnected(_)) => break,
            Err(TrySendError::Full(_)) if Instant::now() < until => {
                tokio::time::sleep(SHUTDOWN_POLL).await;
            }
            Err(TrySendError::Full(_)) => {
                tracing::error!(
                    secs = CAPTURE_SHUTDOWN_SEND.as_secs(),
                    "the capture thread's queue stayed full: it is not told to stop"
                );
                return;
            }
        }
    }
    join_within(thread, "capture", CAPTURE_STOP).await;
}

/// How long the shutdown waits for the net thread to end once it was asked to (its `Shutdown`
/// itself is bounded at 2 s).
const NET_STOP: Duration = Duration::from_secs(3);

/// Joins `thread` if it ends within `within`; otherwise logs and leaves it behind.
/// No `spawn_blocking`: a join that never returns would also hold up this runtime's drop, and
/// with it `Daemon::shutdown`.
async fn join_within(thread: std::thread::JoinHandle<()>, name: &str, within: Duration) {
    let until = std::time::Instant::now() + within;
    while !thread.is_finished() {
        if std::time::Instant::now() >= until {
            tracing::error!(
                thread = name,
                secs = within.as_secs(),
                "the thread did not stop in time"
            );
            return;
        }
        tokio::time::sleep(SHUTDOWN_POLL).await;
    }
    if thread.join().is_err() {
        tracing::error!(thread = name, "the thread panicked");
    }
}

/// Sends the driver a message with an ack and waits for the ack, all within 1 s.
async fn driver_ack(d: &mpsc::Sender<DriverMsg>, make: fn(oneshot::Sender<()>) -> DriverMsg) {
    let (ack, acked) = oneshot::channel();
    let asked = async {
        d.send(make(ack)).await.ok()?;
        acked.await.ok()
    };
    if tokio::time::timeout(Duration::from_secs(1), asked)
        .await
        .is_err()
    {
        tracing::warn!("the portal driver did not answer within 1 s");
    }
}

/// The whole cursor read (connect, write, read, parse) must finish within this.
pub const CURSOR_TIMEOUT: Duration = Duration::from_millis(50);

/// Hyprland's `j/cursorpos`: the cursor in this peer's local logical coordinates. `None` on
/// any error or after `timeout`.
pub async fn cursor_pos(ipc: &Instance, timeout: Duration) -> Option<(f64, f64)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = ipc.socket_path();
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        stream.write_all(b"j/cursorpos").await?;
        let mut answer = Vec::with_capacity(64);
        stream.read_to_end(&mut answer).await?;
        Ok::<_, std::io::Error>(answer)
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Ok(answer)) => match serde_json::from_slice::<ferry_hypr_ipc::CursorPos>(&answer) {
            Ok(c) => Some((c.x, c.y)),
            Err(e) => {
                tracing::debug!(error = %e, "cursor read: not a position");
                None
            }
        },
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "cursor read failed");
            None
        }
        Err(_) => {
            tracing::debug!("cursor read timed out");
            None
        }
    }
}

/// `ferry release`: both machines get `ForceRelease`, and ctl dispatches
/// `release_input_capture()` over IPC on a task of its own, so neither a portal call in progress
/// nor a full net queue holds it up.
pub async fn force_release(ctl: &Ctl) -> bool {
    if ctl.capture_on {
        tokio::task::spawn_local(portal_driver::ipc_release(ctl.ipc.clone()));
    }
    let capture_ok = ctl
        .capture
        .try_send(CaptureCmd::Event(SourceEvent::ForceRelease))
        .is_ok();
    let net_ok = ctl.net.send(NetCmd::ForceRelease).await.is_ok();
    capture_ok && net_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// A fake Hyprland command socket under a temp runtime dir that answers every command
    /// with `answer` after `delay`.
    fn fake_hyprland(answer: &'static str, delay: Duration) -> (tempfile::TempDir, Instance) {
        let run = tempfile::tempdir().unwrap();
        let dir = run.path().join("hypr/fake_1_1");
        std::fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).unwrap_or(0);
                if &buf[..n] == b"j/cursorpos" {
                    std::thread::sleep(delay);
                    let _ = s.write_all(answer.as_bytes());
                }
            }
        });
        let ipc = Instance::discover_in(run.path(), Some("fake_1_1")).unwrap();
        (run, ipc)
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn the_cursor_read_parses_hyprlands_answer() {
        let (_dir, ipc) = fake_hyprland(r#"{"x": 523, "y": 1079.5}"#, Duration::ZERO);
        assert_eq!(
            block_on(cursor_pos(&ipc, CURSOR_TIMEOUT)),
            Some((523.0, 1079.5))
        );
    }

    #[test]
    fn a_slow_or_bad_cursor_read_is_none() {
        let (_dir, ipc) = fake_hyprland(r#"{"x": 1, "y": 2}"#, Duration::from_millis(200));
        let t0 = std::time::Instant::now();
        assert_eq!(block_on(cursor_pos(&ipc, CURSOR_TIMEOUT)), None);
        assert!(t0.elapsed() < Duration::from_millis(150));
        let (_dir, ipc) = fake_hyprland("unknown request", Duration::ZERO);
        assert_eq!(block_on(cursor_pos(&ipc, CURSOR_TIMEOUT)), None);
    }
}
