//! A cheap check on the accept path, before any TLS work: a token bucket per source IP and a
//! cap on handshakes in flight.
//!
//! Anything on the LAN can send Initial packets as fast as it likes, and every accepted one
//! costs a TLS handshake (key exchange, signature, certificate parse). The gate runs on
//! `Incoming` in `accept_loop`, before `accept_with`, and refuses the excess with quinn's
//! `Incoming::refuse` (a CONNECTION_REFUSED close in one small packet, no handshake state).
//!
//! The source address of an Initial can be spoofed, so a per-IP bucket cannot stop a
//! determined attacker from spending a victim's tokens; it bounds the work per address, and the
//! in-flight cap bounds the total.
//!
//! Slot exhaustion (A-09): every admitted handshake holds one of the in-flight slots until it
//! ends or times out, so a host (or spoofed sources) that starts handshakes and never finishes
//! them could keep every slot taken and turn real peers away. Two defences. Once more than half
//! the slots are taken, an Initial whose address is not yet validated gets a stateless Retry
//! ([`HandshakeGate::should_retry`]): a spoofed source never sees the token and never takes a
//! slot. And an IPv6 host owns a whole /64, so the buckets are keyed by the /64
//! ([`source_key`]), not by the single address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Handshakes one source IP may start in a burst.
pub const HANDSHAKE_BURST: u32 = 8;
/// Handshake tokens a source IP earns back per second.
pub const HANDSHAKE_REFILL_PER_SEC: u32 = 2;
/// Handshakes (TLS plus the open exchange) that may run at once, over all sources.
pub const MAX_HANDSHAKES_IN_FLIGHT: usize = 32;
/// A source IP unseen for longer than this is forgotten. Its bucket would have refilled
/// long before.
pub const LIMITER_IDLE: Duration = Duration::from_secs(60);
/// Source IPs tracked at once. When full, the least recently seen is dropped to make room.
pub const LIMITER_MAX_ENTRIES: usize = 1024;

/// More than this many handshakes in flight: an unvalidated Initial is sent a Retry first.
pub const RETRY_ABOVE_IN_FLIGHT: usize = MAX_HANDSHAKES_IN_FLIGHT / 2;

/// What a source is counted as. An IPv4 host is its address. An IPv6 host is its /64, because a
/// subnet hands one host that many addresses and a per-address bucket would never run dry.
pub(crate) fn source_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let high = u128::from(v6) & (u128::MAX << 64);
            IpAddr::V6(Ipv6Addr::from(high))
        }
        v4 => v4,
    }
}

/// How often idle entries are swept (when the map is not full).
const SWEEP_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    /// When the bucket was last touched: the refill reference and the idle clock.
    seen: Instant,
}

/// Token buckets keyed by source IP. The clock is a parameter, so tests control time.
#[derive(Debug)]
pub(crate) struct SourceLimiter {
    buckets: HashMap<IpAddr, Bucket>,
    last_sweep: Option<Instant>,
    /// Tokens a new or idle source starts with, and the most it holds.
    burst: f64,
    /// Tokens earned back per second.
    refill_per_sec: f64,
}

impl Default for SourceLimiter {
    /// The handshake gate's rate: [`HANDSHAKE_BURST`], refilling [`HANDSHAKE_REFILL_PER_SEC`].
    fn default() -> Self {
        Self::with_rate(
            f64::from(HANDSHAKE_BURST),
            f64::from(HANDSHAKE_REFILL_PER_SEC),
        )
    }
}

impl SourceLimiter {
    pub(crate) fn with_rate(burst: f64, refill_per_sec: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            last_sweep: None,
            burst,
            refill_per_sec,
        }
    }

    /// Takes a token for `ip` if it has one.
    pub(crate) fn admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        let ip = source_key(ip);
        self.sweep(now);
        if !self.buckets.contains_key(&ip) && self.buckets.len() >= LIMITER_MAX_ENTRIES {
            self.evict_least_recent();
        }
        let (burst, rate) = (self.burst, self.refill_per_sec);
        let b = self.buckets.entry(ip).or_insert(Bucket {
            tokens: burst,
            seen: now,
        });
        let dt = now.saturating_duration_since(b.seen).as_secs_f64();
        b.tokens = (b.tokens + dt * rate).min(burst);
        b.seen = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Whether `ip` has a token now, without taking it or touching its bucket.
    pub(crate) fn has_token(&self, ip: IpAddr, now: Instant) -> bool {
        self.buckets.get(&source_key(ip)).is_none_or(|b| {
            let dt = now.saturating_duration_since(b.seen).as_secs_f64();
            (b.tokens + dt * self.refill_per_sec).min(self.burst) >= 1.0
        })
    }

    /// Drops entries idle for more than [`LIMITER_IDLE`]; runs when the map is full or every
    /// [`SWEEP_EVERY`].
    fn sweep(&mut self, now: Instant) {
        let due = self
            .last_sweep
            .is_none_or(|t| now.saturating_duration_since(t) >= SWEEP_EVERY);
        if due || self.buckets.len() >= LIMITER_MAX_ENTRIES {
            self.last_sweep = Some(now);
            self.buckets
                .retain(|_, b| now.saturating_duration_since(b.seen) <= LIMITER_IDLE);
        }
    }

    fn evict_least_recent(&mut self) {
        if let Some(oldest) = self
            .buckets
            .iter()
            .min_by_key(|(_, b)| b.seen)
            .map(|(ip, _)| *ip)
        {
            self.buckets.remove(&oldest);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.len()
    }
}

/// The accept-path gate: the per-source limiter, the in-flight cap and the refusal counter.
#[derive(Debug)]
pub(crate) struct HandshakeGate {
    limiter: Mutex<SourceLimiter>,
    in_flight: Arc<Semaphore>,
    refused: AtomicU64,
    retried: AtomicU64,
}

impl HandshakeGate {
    pub(crate) fn new() -> Self {
        Self {
            limiter: Mutex::new(SourceLimiter::default()),
            in_flight: Arc::new(Semaphore::new(MAX_HANDSHAKES_IN_FLIGHT)),
            refused: AtomicU64::new(0),
            retried: AtomicU64::new(0),
        }
    }

    /// Whether an Initial from an address that is `validated` or not should be answered with a
    /// stateless Retry instead of taking a slot: more than [`RETRY_ABOVE_IN_FLIGHT`] are taken
    /// and the address is unproven. Counts the Retry when it says yes.
    pub(crate) fn should_retry(&self, validated: bool) -> bool {
        let taken = MAX_HANDSHAKES_IN_FLIGHT - self.in_flight.available_permits();
        let retry = !validated && taken > RETRY_ABOVE_IN_FLIGHT;
        if retry {
            self.retried.fetch_add(1, Ordering::Relaxed);
        }
        retry
    }

    /// Lets a handshake from `ip` start, or counts a refusal. The permit holds one in-flight
    /// slot until it is dropped.
    pub(crate) fn enter(&self, ip: IpAddr, now: Instant) -> Option<OwnedSemaphorePermit> {
        let permit = self.in_flight.clone().try_acquire_owned().ok();
        // the per-source bucket is only charged for a handshake that can actually start
        let admitted = permit.is_some()
            && self
                .limiter
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .admit(ip, now);
        if admitted {
            permit
        } else {
            self.refused.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub(crate) fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    pub(crate) fn retried(&self) -> u64 {
        self.retried.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, n])
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_burst_from_one_address_is_cut_at_the_burst_size() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        let admitted = (0..HANDSHAKE_BURST + 5)
            .filter(|_| l.admit(ip(1), t0))
            .count();
        assert_eq!(admitted, HANDSHAKE_BURST as usize);
    }

    #[test]
    fn a_second_address_is_unaffected() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        while l.admit(ip(1), t0) {}
        assert!(!l.admit(ip(1), t0));
        let admitted = (0..HANDSHAKE_BURST + 5)
            .filter(|_| l.admit(ip(2), t0))
            .count();
        assert_eq!(admitted, HANDSHAKE_BURST as usize);
    }

    #[test]
    fn tokens_refill_at_the_configured_rate_and_stop_at_the_burst() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        while l.admit(ip(1), t0) {}
        // 2 per second: nothing at 400 ms, one at 500 ms, and only one of them
        assert!(!l.admit(ip(1), t0 + ms(400)));
        assert!(l.admit(ip(1), t0 + ms(500)));
        assert!(!l.admit(ip(1), t0 + ms(500)));
        // a long quiet spell refills to the burst and no further
        let later = t0 + Duration::from_secs(30);
        let admitted = (0..HANDSHAKE_BURST + 5)
            .filter(|_| l.admit(ip(1), later))
            .count();
        assert_eq!(admitted, HANDSHAKE_BURST as usize);
    }

    #[test]
    fn refused_attempts_do_not_postpone_the_refill() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        while l.admit(ip(1), t0) {}
        // hammering at 10 per second earns tokens like waiting does
        let admitted = (1..=20)
            .filter(|i| l.admit(ip(1), t0 + ms(100 * i)))
            .count();
        assert_eq!(admitted, 4, "2 s at 2 per second");
    }

    #[test]
    fn idle_entries_are_evicted_after_the_idle_time() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        for n in 0..10 {
            l.admit(ip(n), t0);
        }
        assert_eq!(l.len(), 10);
        // just inside the idle time they stay, even past a sweep
        l.admit(ip(200), t0 + LIMITER_IDLE);
        assert_eq!(l.len(), 11);
        l.admit(ip(200), t0 + LIMITER_IDLE + SWEEP_EVERY + ms(1));
        assert_eq!(l.len(), 1, "only the address seen since is left");
    }

    #[test]
    fn the_map_never_exceeds_its_hard_cap_and_drops_the_least_recent() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        let addr = |n: usize| IpAddr::from([10, 0, (n >> 8) as u8, n as u8]);
        for n in 0..LIMITER_MAX_ENTRIES + 500 {
            // all within the idle time, so only the cap can make room
            l.admit(addr(n), t0 + ms(n as u64));
            assert!(l.len() <= LIMITER_MAX_ENTRIES);
        }
        assert_eq!(l.len(), LIMITER_MAX_ENTRIES);
        assert!(!l.buckets.contains_key(&addr(0)), "the oldest went first");
        assert!(l.buckets.contains_key(&addr(LIMITER_MAX_ENTRIES + 499)));
    }

    #[test]
    fn at_most_the_cap_of_handshakes_are_in_flight() {
        let (g, t0) = (HandshakeGate::new(), Instant::now());
        let addr = |n: usize| IpAddr::from([10, 0, 0, n as u8]);
        let held: Vec<_> = (0..MAX_HANDSHAKES_IN_FLIGHT)
            .map(|n| g.enter(addr(n), t0).expect("under the cap"))
            .collect();
        assert_eq!(g.refused(), 0);
        // a fresh address is still refused while the slots are taken, and is not charged
        assert!(g.enter(addr(100), t0).is_none());
        assert_eq!(g.refused(), 1);
        drop(held);
        assert!(g.enter(addr(100), t0).is_some(), "slots free up on drop");
    }

    #[test]
    fn an_unvalidated_source_gets_a_retry_once_over_half_the_slots_are_taken() {
        let (g, t0) = (HandshakeGate::new(), Instant::now());
        let addr = |n: usize| IpAddr::from([10, 0, 0, n as u8]);
        let mut held = Vec::new();
        for n in 0..RETRY_ABOVE_IN_FLIGHT {
            assert!(!g.should_retry(false), "{n} taken: no retry yet");
            held.push(g.enter(addr(n), t0).expect("a slot"));
        }
        // exactly half taken: still no Retry; one more slot taken tips it
        assert!(!g.should_retry(false));
        held.push(g.enter(addr(100), t0).expect("a slot"));
        assert!(g.should_retry(false), "over half the slots are taken");
        assert!(!g.should_retry(true), "a validated address needs none");
        assert_eq!(g.retried(), 1);
        drop(held);
        assert!(!g.should_retry(false), "the slots are free again");
    }

    #[test]
    fn an_ipv6_host_is_one_bucket_per_64() {
        let (mut l, t0) = (SourceLimiter::default(), Instant::now());
        let host = |n: u16| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, n, n, n, n));
        let admitted = (0..4 * u16::try_from(HANDSHAKE_BURST).unwrap())
            .filter(|n| l.admit(host(*n), t0))
            .count();
        assert_eq!(admitted, HANDSHAKE_BURST as usize, "one bucket for the /64");
        // another /64 has its own
        let other = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 3, 0, 0, 0, 1));
        assert!(l.admit(other, t0));
        assert!(!l.has_token(host(77), t0));
        // IPv4 stays per address, and a mapped IPv4 address is its IPv4 address
        assert!(l.admit(ip(9), t0));
        assert_eq!(source_key(ip(9)), ip(9));
        assert_eq!(
            source_key(IpAddr::V6(Ipv6Addr::new(
                0, 0, 0, 0, 0, 0xffff, 0xc000, 0x209
            ))),
            ip(9)
        );
    }

    #[test]
    fn the_gate_counts_per_source_refusals() {
        let (g, t0) = (HandshakeGate::new(), Instant::now());
        let results: Vec<_> = (0..12).map(|_| g.enter(ip(1), t0)).collect();
        assert_eq!(results.iter().filter(|p| p.is_some()).count(), 8);
        assert_eq!(g.refused(), 4);
        assert!(g.enter(ip(2), t0).is_some());
        assert_eq!(g.refused(), 4);
    }
}
