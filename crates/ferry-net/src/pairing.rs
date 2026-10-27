//! The acceptor's pairing gate (design `m1-rest-design.md` §2.2): whether an incoming
//! connection may be offered `ferry-pair/1`, one attempt at a time, and the per-address limit.
//!
//! Each incoming connection gets its own server config. It offers `[ferry/1, ferry-pair/1]` only
//! when three facts hold: a pairing window is open ([`crate::Endpoint::set_pairing`]), no attempt
//! is running, and the source address has a pairing token. Otherwise it offers `[ferry/1]`, and a
//! `ferry-pair/1` dialer fails with TLS alert 120 (`no_application_protocol`), which
//! [`crate::Endpoint::dial_pairing`] reports as [`crate::ConnectError::NotAccepting`].
//!
//! The offer is a peek: quinn shows no ALPN before `accept_with`. The attempt is claimed, and the
//! address charged, only when the ClientHello really asks for `ferry-pair/1` (the certificate
//! resolver in `tls.rs`). A claim lasts until the [`crate::PairingConnection`] is dropped.

use crate::Fingerprint;
use crate::connection::{Connection, Events, NewConnection};
use crate::limit::SourceLimiter;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Pairing handshakes one source address may start in a burst, across windows.
pub const PAIRING_BURST: u32 = 3;
/// One pairing token comes back every 20 s: at most 3 attempts per minute per address.
pub const PAIRING_REFILL_EVERY: Duration = Duration::from_secs(20);

/// An open pairing window on the acceptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingWindow {
    /// When it closes. Connections that arrive later are not offered `ferry-pair/1`.
    pub until: Instant,
}

#[derive(Debug)]
struct GateState {
    window: Option<PairingWindow>,
    busy: bool,
    limiter: SourceLimiter,
}

#[derive(Debug)]
pub(crate) struct PairingGate {
    state: Mutex<GateState>,
    refused: AtomicU64,
}

impl PairingGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                window: None,
                busy: false,
                limiter: SourceLimiter::with_rate(
                    f64::from(PAIRING_BURST),
                    1.0 / PAIRING_REFILL_EVERY.as_secs_f64(),
                ),
            }),
            refused: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_window(&self, window: Option<PairingWindow>) {
        self.lock().window = window;
    }

    fn open(s: &GateState, now: Instant) -> bool {
        s.window.is_some_and(|w| now < w.until) && !s.busy
    }

    /// The three facts of §2.2, without taking anything: offer `ferry-pair/1`?
    pub(crate) fn offer(&self, ip: IpAddr, now: Instant) -> bool {
        let s = self.lock();
        Self::open(&s, now) && s.limiter.has_token(ip, now)
    }

    /// The ClientHello asks for `ferry-pair/1` on a connection that offered it: start the
    /// attempt, or refuse it if another one started meanwhile or the window closed.
    pub(crate) fn claim(self: &Arc<Self>, ip: IpAddr, now: Instant) -> Option<Claim> {
        let mut s = self.lock();
        if Self::open(&s, now) && s.limiter.admit(ip, now) {
            s.busy = true;
            Some(Claim { gate: self.clone() })
        } else {
            drop(s);
            self.refuse();
            None
        }
    }

    /// A pairing handshake was turned away (no window, busy, or over the address's limit).
    pub(crate) fn refuse(&self) {
        self.refused.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

/// The running attempt: while it lives, the acceptor offers only `ferry/1`.
#[derive(Debug)]
pub(crate) struct Claim {
    gate: Arc<PairingGate>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.gate.lock().busy = false;
    }
}

/// A connection on `ferry-pair/1`: from [`crate::Endpoint::accept_pairing`] or
/// [`crate::Endpoint::dial_pairing`]. It carries frames like a session connection; datagrams
/// are not used. It never reaches the [`crate::Manager`].
///
/// On the acceptor, the attempt it belongs to counts as running until this value is dropped
/// (one attempt at a time).
#[derive(Debug)]
pub struct PairingConnection {
    pub conn: Connection,
    pub events: Events,
    /// The peer's address: on the acceptor, the dialer's bound port, which is dialable.
    pub remote: SocketAddr,
    _claim: Option<Claim>,
}

impl PairingConnection {
    pub(crate) fn new(nc: NewConnection, claim: Option<Claim>) -> Self {
        let remote = nc.conn.remote_addr();
        Self {
            conn: nc.conn,
            events: nc.events,
            remote,
            _claim: claim,
        }
    }

    /// The fingerprint the peer proved in the TLS handshake (any key: it is not pinned yet).
    pub fn peer_fp(&self) -> Fingerprint {
        self.conn.peer_fp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, n])
    }

    #[test]
    fn nothing_is_offered_without_an_open_window() {
        let (g, t0) = (Arc::new(PairingGate::new()), Instant::now());
        assert!(!g.offer(ip(1), t0));
        g.set_window(Some(PairingWindow {
            until: t0 + Duration::from_secs(120),
        }));
        assert!(g.offer(ip(1), t0));
        assert!(
            !g.offer(ip(1), t0 + Duration::from_secs(120)),
            "closed at its end"
        );
        g.set_window(None);
        assert!(!g.offer(ip(1), t0));
        assert!(g.claim(ip(1), t0).is_none());
        assert_eq!(g.refused(), 1);
    }

    #[test]
    fn one_attempt_at_a_time() {
        let (g, t0) = (Arc::new(PairingGate::new()), Instant::now());
        g.set_window(Some(PairingWindow {
            until: t0 + Duration::from_secs(120),
        }));
        let claim = g.claim(ip(1), t0).expect("the first attempt");
        assert!(!g.offer(ip(2), t0), "busy");
        assert!(g.claim(ip(2), t0).is_none());
        drop(claim);
        assert!(g.offer(ip(2), t0), "free again once the attempt ends");
    }

    #[test]
    fn three_attempts_per_minute_per_address() {
        let (g, t0) = (Arc::new(PairingGate::new()), Instant::now());
        g.set_window(Some(PairingWindow {
            until: t0 + Duration::from_secs(600),
        }));
        for _ in 0..PAIRING_BURST {
            drop(g.claim(ip(1), t0).expect("within the burst"));
        }
        assert!(!g.offer(ip(1), t0), "the fourth is not offered");
        assert!(g.claim(ip(1), t0).is_none());
        assert!(g.offer(ip(2), t0), "another address is unaffected");
        // one token back after 20 s, not before
        assert!(!g.offer(ip(1), t0 + Duration::from_secs(19)));
        assert!(g.offer(ip(1), t0 + Duration::from_secs(20)));
        drop(
            g.claim(ip(1), t0 + Duration::from_secs(20))
                .expect("refilled"),
        );
        assert!(!g.offer(ip(1), t0 + Duration::from_secs(20)));
    }
}
