//! Connection manager: dial with backoff, accept, and keep one connection per peer.

use crate::{
    CloseCode, ConnectError, Connection, Endpoint, Fingerprint, IncomingError, NewConnection,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// First retry delay after a failed dial; it doubles up to [`BACKOFF_MAX`] .
/// A dial that timed out is retried at once: the attempt itself took the handshake timeout.
pub const BACKOFF_MIN: Duration = Duration::from_millis(100);
pub const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The first retry after the peer refused this machine ([`ConnectError::NotTrustedByPeer`]: it
/// unpaired this one, or never paired it). It doubles up to [`REFUSED_BACKOFF_MAX`], so a peer
/// that keeps its pin to a machine that dropped it does not dial once a second for ever.
/// A connection resets it, and so does [`Manager::retry_now`].
pub const REFUSED_BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const REFUSED_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long an inbound connection that is bound to lose a simultaneous dial waits for this
/// side's own dial to finish before it is reported anyway (see [`Manager`]).
pub const SIMULTANEOUS_HOLD: Duration = Duration::from_millis(300);

/// What [`Manager::next`] reports.
#[derive(Debug)]
pub enum ManagerEvent {
    /// A connection to `conn.peer_fp()` is up. It replaces any earlier connection to that peer,
    /// which the manager has closed with [`CloseCode::DUPLICATE`]; that one's [`Events`]
    /// then ends with `Closed(Local(DUPLICATE))`, which is not an error.
    ///
    /// When both peers dial at once, the side whose own dial wins reports only that
    /// connection: the peer's losing connection, which arrives first, is held back until the
    /// own dial finishes and is then closed without being reported. If the own dial takes
    /// longer than 300 ms the held one is reported after all, and the later winner replaces
    /// it as described above.
    ///
    /// The higher side holds nothing back. Its own dial often finishes before the lower
    /// side's dial arrives, so it reports its own connection first and then, as a
    /// replacement, the lower side's winner: two `Connected` events for one peer coming up.
    /// Only its last report is the connection both sides keep.
    ///
    /// [`Events`]: crate::Events
    Connected(NewConnection),
    /// Dialling `peer` started to fail, or now fails differently. The next attempt follows
    /// after `retry_in`.
    ///
    /// Reported for the first failed attempt and then only when the error changes, so a peer
    /// that stays unreachable or keeps refusing gives one event, not one per second. `attempts`
    /// counts the failed attempts in a row, this one included. The count starts again once a
    /// connection to the peer is up, or when [`Manager::dial`] changes the address. Every
    /// attempt is still logged at debug level.
    DialFailed {
        peer: Fingerprint,
        addr: SocketAddr,
        error: ConnectError,
        attempts: u32,
        retry_in: Duration,
    },
    /// An inbound connection did not open. Either this side refused it (for example
    /// [`ConnectError::UnknownPeer`]), or the dialer gave up first
    /// ([`ConnectError::Abandoned`], [`ConnectError::TimedOut`]), which is not a refusal.
    InboundFailed(IncomingError),
}

enum Internal {
    Dialed(
        Result<NewConnection, ConnectError>,
        Fingerprint,
        SocketAddr,
        Duration,
    ),
    Closed(Fingerprint, usize),
}

struct DialTask {
    addr: SocketAddr,
    /// The error of the last failed attempt, and the failed attempts in a row.
    failing: Option<(ConnectError, u32)>,
    connected: watch::Sender<bool>,
    /// `true` from the start of a dial attempt until the manager has taken its outcome. A flag
    /// that publishes no data, so every access is `Relaxed`.
    attempting: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

/// An inbound connection kept back because this side's own dial to the same peer is under
/// way and would win the tie-break.
struct Held {
    nc: NewConnection,
    until: Instant,
}

/// Keeps at most one live connection per peer.
///
/// - [`Manager::dial`] keeps dialling a peer until it is connected, and again after the
///   connection ends, backing off from 100 ms to 1 s between failures.
/// - Inbound connections are accepted from the endpoint.
/// - When two connections to one peer are open, as when both peers dial each other at once,
///   both sides keep the same one: the connection dialled by the lower fingerprint.
///   Between two connections dialled by the same side, the newer wins.
/// - On the lower side of a simultaneous dial, the peer's inbound connection reaches the
///   manager before the side's own dial finishes. It is held for up to 300 ms
///   ([`SIMULTANEOUS_HOLD`]) and dropped unreported once the own dial wins (see
///   [`ManagerEvent::Connected`]), so ferryd sees the survivor and not a connection that is
///   closed a moment later.
///
/// The manager holds a handle to each live connection, so a connection stays open until it is
/// closed ([`Connection::close`], [`Manager::forget`]) or fails. Drive it by awaiting
/// [`Manager::next`] in a loop on the endpoint's runtime.
pub struct Manager {
    endpoint: Endpoint,
    me: Fingerprint,
    hold: Duration,
    live: HashMap<Fingerprint, Connection>,
    held: HashMap<Fingerprint, Held>,
    dialers: HashMap<Fingerprint, DialTask>,
    tx: mpsc::Sender<Internal>,
    rx: mpsc::Receiver<Internal>,
}

impl Manager {
    pub fn new(endpoint: Endpoint) -> Self {
        Self::with_hold(endpoint, SIMULTANEOUS_HOLD)
    }

    /// [`Manager::new`] with another bound than [`SIMULTANEOUS_HOLD`] on how long the losing
    /// inbound connection of a simultaneous dial is held. A test that asserts what a simultaneous
    /// dial reports passes one longer than any dial takes, so that the own dial, and not the
    /// host's scheduling, ends the hold.
    pub fn with_hold(endpoint: Endpoint, hold: Duration) -> Self {
        let (tx, rx) = mpsc::channel(64);
        Self {
            me: endpoint.fingerprint(),
            hold,
            endpoint,
            live: HashMap::new(),
            held: HashMap::new(),
            dialers: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The live connection to `peer`, if any.
    pub fn connection(&self, peer: &Fingerprint) -> Option<&Connection> {
        self.live.get(peer)
    }

    /// Starts dialling `peer` at `addr`, now and whenever it is not connected. Calling it again
    /// with a new address retargets the dialler.
    pub fn dial(&mut self, peer: Fingerprint, addr: SocketAddr) {
        if self.dialers.get(&peer).is_some_and(|d| d.addr == addr) {
            return;
        }
        if let Some(old) = self.dialers.remove(&peer) {
            old.task.abort();
        }
        self.spawn_dialer(peer, addr);
    }

    /// Dials `peer` again at once, with the backoff reset, when its dials are failing: what
    /// changed the trust on either side may have made it reachable. A peer that is not dialled,
    /// or whose last dial did not fail, is left alone.
    pub fn retry_now(&mut self, peer: &Fingerprint) {
        let Some(addr) = self
            .dialers
            .get(peer)
            .filter(|d| d.failing.is_some())
            .map(|d| d.addr)
        else {
            return;
        };
        if let Some(old) = self.dialers.remove(peer) {
            old.task.abort();
        }
        self.spawn_dialer(*peer, addr);
    }

    fn spawn_dialer(&mut self, peer: Fingerprint, addr: SocketAddr) {
        let (connected, watch_rx) = watch::channel(self.live.contains_key(&peer));
        let attempting = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(dial_loop(
            self.endpoint.clone(),
            peer,
            addr,
            watch_rx,
            attempting.clone(),
            self.tx.clone(),
        ));
        self.dialers.insert(
            peer,
            DialTask {
                addr,
                failing: None,
                connected,
                attempting,
                task,
            },
        );
    }

    /// Stops dialling `peer` and closes its connection with [`CloseCode::SHUTDOWN`]. To tell the
    /// peer it was unpaired, use [`Manager::forget_with`] and [`CloseCode::UNPAIRED`].
    pub fn forget(&mut self, peer: &Fingerprint) {
        self.forget_with(peer, CloseCode::SHUTDOWN);
    }

    /// Stops dialling `peer` and closes its connection with `code`, which the peer sees as
    /// [`CloseReason::Peer`](crate::CloseReason::Peer). Unpairing or revoking a pin
    ///  closes with [`CloseCode::UNPAIRED`]; a plain stop uses
    /// [`CloseCode::SHUTDOWN`] ([`Manager::forget`]).
    pub fn forget_with(&mut self, peer: &Fingerprint, code: CloseCode) {
        if let Some(d) = self.dialers.remove(peer) {
            d.task.abort();
        }
        if let Some(h) = self.held.remove(peer) {
            h.nc.conn.close(code, "");
        }
        if let Some(c) = self.live.remove(peer) {
            c.close(code, "");
        }
    }

    /// The next connection, failed dial or refusal. `None` once the endpoint is closed.
    /// Cancel-safe.
    pub async fn next(&mut self) -> Option<ManagerEvent> {
        loop {
            let release_at = self.held.values().map(|h| h.until).min();
            tokio::select! {
                Some(msg) = self.rx.recv() => if let Some(ev) = self.internal(msg) {
                    return Some(ev);
                },
                inc = self.endpoint.accept() => match inc? {
                    Ok(nc) => if let Some(ev) = self.inbound(nc) {
                        return Some(ev);
                    },
                    Err(e) => return Some(ManagerEvent::InboundFailed(e)),
                },
                () = tokio::time::sleep_until(release_at.unwrap_or_else(Instant::now)),
                    if release_at.is_some() =>
                {
                    // the own dial took too long: report the held connection after all
                    let now = Instant::now();
                    let due: Vec<_> = self.held.iter().filter(|(_, h)| h.until <= now).map(|(p, _)| *p).collect();
                    for peer in due {
                        if let Some(ev) = self.settle(peer) {
                            return Some(ev);
                        }
                    }
                },
            }
        }
    }

    fn internal(&mut self, msg: Internal) -> Option<ManagerEvent> {
        match msg {
            Internal::Dialed(res, peer, addr, retry_in) => {
                if let Some(d) = self.dialers.get(&peer) {
                    d.attempting.store(false, Ordering::Relaxed);
                }
                match res {
                    Ok(nc) => {
                        let ev = self.adopt(nc);
                        // the held inbound lost to this one, or is all there is
                        let held = self.settle(peer);
                        ev.or(held)
                    }
                    Err(error) => {
                        // an inbound connection held meanwhile is the answer, as with a peer
                        // that connected while the dial was failing
                        if self.held.contains_key(&peer) {
                            self.settle(peer)
                        } else {
                            self.dial_failed(peer, addr, error, retry_in)
                        }
                    }
                }
            }
            Internal::Closed(peer, id) => {
                if self.live.get(&peer).is_some_and(|c| c.id() == id) {
                    self.live.remove(&peer);
                    self.set_connected(&peer, false);
                }
                None
            }
        }
    }

    /// Counts a failed attempt, and reports it if it is the first or its error is new.
    fn dial_failed(
        &mut self,
        peer: Fingerprint,
        addr: SocketAddr,
        error: ConnectError,
        retry_in: Duration,
    ) -> Option<ManagerEvent> {
        let d = self.dialers.get_mut(&peer)?;
        if d.addr != addr {
            return None; // an attempt at the old address, finished after a retarget
        }
        let (attempts, changed) = match d.failing.take() {
            Some((last, n)) => (n + 1, last != error),
            None => (1, true),
        };
        d.failing = Some((error.clone(), attempts));
        tracing::debug!(peer = %peer.short(), %addr, %error, attempts, ?retry_in, "dial failed");
        changed.then_some(ManagerEvent::DialFailed {
            peer,
            addr,
            error,
            attempts,
            retry_in,
        })
    }

    /// An accepted connection. It is held back instead of reported when it was dialled by the
    /// higher fingerprint while this side, the lower one, is dialling too: this side's own
    /// connection wins the tie-break, so reporting this one would only be followed by its
    /// replacement.
    fn inbound(&mut self, nc: NewConnection) -> Option<ManagerEvent> {
        let peer = nc.conn.peer_fp();
        let own_dial_wins = self.me < peer
            && self
                .dialers
                .get(&peer)
                .is_some_and(|d| d.attempting.load(Ordering::Relaxed));
        let none_live = self
            .live
            .get(&peer)
            .is_none_or(|c| c.close_reason().is_some());
        if nc.conn.is_dialer() || !own_dial_wins || !none_live {
            return self.adopt(nc);
        }
        let held = Held {
            nc,
            until: Instant::now() + self.hold,
        };
        if let Some(older) = self.held.insert(peer, held) {
            older.nc.conn.close(CloseCode::DUPLICATE, "duplicate");
        }
        None
    }

    /// Resolves the connection held for `peer`, if any: adopts it (which closes it as a
    /// duplicate if a better connection is live by now), and reports it if it is the one kept.
    fn settle(&mut self, peer: Fingerprint) -> Option<ManagerEvent> {
        let held = self.held.remove(&peer)?;
        self.adopt(held.nc)
    }

    fn set_connected(&mut self, peer: &Fingerprint, up: bool) {
        if let Some(d) = self.dialers.get_mut(peer) {
            if up {
                d.failing = None;
            }
            d.connected.send_replace(up);
        }
    }

    /// `true` if `new` should replace the open connection `old` to the same peer.
    fn prefer(&self, new: &Connection, old: &Connection) -> bool {
        let dialer = |c: &Connection| if c.is_dialer() { self.me } else { c.peer_fp() };
        let (by_new, by_old) = (dialer(new), dialer(old));
        if by_new == by_old {
            true
        } else {
            by_new == self.me.min(new.peer_fp())
        }
    }

    fn adopt(&mut self, nc: NewConnection) -> Option<ManagerEvent> {
        let peer = nc.conn.peer_fp();
        if nc.conn.close_reason().is_some() {
            return None; // lost the dedupe on the other side before we saw it
        }
        if let Some(old) = self.live.get(&peer)
            && old.close_reason().is_none()
        {
            if !self.prefer(&nc.conn, old) {
                nc.conn.close(CloseCode::DUPLICATE, "duplicate");
                return None;
            }
            old.close(CloseCode::DUPLICATE, "duplicate");
        }
        let (conn, tx) = (nc.conn.clone(), self.tx.clone());
        tokio::spawn(async move {
            conn.closed().await;
            let _ = tx.send(Internal::Closed(conn.peer_fp(), conn.id())).await;
        });
        self.live.insert(peer, nc.conn.clone());
        self.set_connected(&peer, true);
        Some(ManagerEvent::Connected(nc))
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        for d in self.dialers.values() {
            d.task.abort();
        }
    }
}

async fn dial_loop(
    endpoint: Endpoint,
    peer: Fingerprint,
    addr: SocketAddr,
    mut connected: watch::Receiver<bool>,
    attempting: Arc<AtomicBool>,
    tx: mpsc::Sender<Internal>,
) {
    let mut backoff = Backoff::default();
    loop {
        if connected.wait_for(|up| !*up).await.is_err() {
            return;
        }
        // Cleared by the manager when it takes the outcome from the channel, so that an inbound
        // connection it sees first is still known to race with this attempt.
        attempting.store(true, Ordering::Relaxed);
        match endpoint.dial(addr, peer).await {
            Ok(nc) => {
                backoff = Backoff::default();
                let conn = nc.conn.clone();
                if tx
                    .send(Internal::Dialed(Ok(nc), peer, addr, BACKOFF_MIN))
                    .await
                    .is_err()
                {
                    return;
                }
                // The manager marks the peer connected when it adopts this (or a better)
                // connection. If it drops this one as already closed, dial again.
                tokio::select! {
                    r = connected.wait_for(|up| *up) => if r.is_err() { return },
                    _ = conn.closed() => {}
                }
            }
            Err(_) if *connected.borrow() => {
                // connected inbound meanwhile; no outcome goes to the manager
                attempting.store(false, Ordering::Relaxed);
            }
            Err(e) => {
                let retry_in = backoff.after(&e);
                if tx
                    .send(Internal::Dialed(Err(e), peer, addr, retry_in))
                    .await
                    .is_err()
                {
                    return;
                }
                if !retry_in.is_zero() {
                    tokio::time::sleep(retry_in).await;
                }
            }
        }
    }
}

/// How long a dial loop waits after a failed attempt. Two schedules: a refusal by the peer
/// ([`ConnectError::NotTrustedByPeer`]) backs off from [`REFUSED_BACKOFF_MIN`] to
/// [`REFUSED_BACKOFF_MAX`]; any other fast failure from [`BACKOFF_MIN`] to [`BACKOFF_MAX`]. A
/// timed-out attempt already waited the whole handshake timeout, so the next one starts at once:
/// a peer that restarts while an attempt is in flight is found by the next attempt instead of a
/// second later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Backoff {
    fast: Duration,
    refused: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            fast: BACKOFF_MIN,
            refused: REFUSED_BACKOFF_MIN,
        }
    }
}

impl Backoff {
    /// The wait before the next attempt, after one that failed with `error`.
    fn after(&mut self, error: &ConnectError) -> Duration {
        let (next, max) = match error {
            ConnectError::TimedOut => return Duration::ZERO,
            ConnectError::NotTrustedByPeer(_) => (&mut self.refused, REFUSED_BACKOFF_MAX),
            _ => (&mut self.fast, BACKOFF_MAX),
        };
        let wait = *next;
        *next = (wait * 2).min(max);
        wait
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    fn secs(waits: &[Duration]) -> Vec<u64> {
        waits.iter().map(Duration::as_secs).collect()
    }

    #[test]
    fn a_peer_that_refuses_is_dialled_less_and_less_often_up_to_a_minute() {
        let mut b = Backoff::default();
        let refused = ConnectError::NotTrustedByPeer(Fingerprint([7; 32]));
        let waits: Vec<Duration> = (0..9).map(|_| b.after(&refused)).collect();
        assert_eq!(secs(&waits), [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn other_failures_keep_the_fast_schedule_and_a_timeout_retries_at_once() {
        let mut b = Backoff::default();
        let refused = ConnectError::NotTrustedByPeer(Fingerprint([7; 32]));
        b.after(&refused);
        b.after(&refused);
        let waits: Vec<Duration> = (0..6).map(|_| b.after(&ConnectError::Abandoned)).collect();
        assert_eq!(
            waits,
            [100, 200, 400, 800, 1000, 1000].map(Duration::from_millis)
        );
        assert_eq!(b.after(&ConnectError::TimedOut), Duration::ZERO);
        // the refusal schedule went on where it was
        assert_eq!(b.after(&refused), Duration::from_secs(4));
        // a connection starts both again
        assert_eq!(Backoff::default().after(&refused), REFUSED_BACKOFF_MIN);
    }
}
