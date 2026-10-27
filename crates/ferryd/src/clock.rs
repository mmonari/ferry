//! `Micros` from `CLOCK_MONOTONIC`, the clock EIS stamps events with, and the way
//! back to an `Instant` for a timer.

use std::time::{Duration, Instant};

use ferry_core::Micros;

/// Now, on `CLOCK_MONOTONIC`, in µs.
pub fn now() -> Micros {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let secs = u64::try_from(t.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(t.tv_nsec).unwrap_or(0);
    Micros(secs * 1_000_000 + nanos / 1000)
}

/// How long until `deadline`, zero when it is due.
pub fn until(deadline: Micros) -> Duration {
    Duration::from_micros(deadline.since(now()).0)
}

/// The `Instant` at which `deadline` falls (for tokio's `sleep_until`).
pub fn instant_at(deadline: Micros) -> Instant {
    Instant::now() + until(deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_and_maps_back() {
        let a = now();
        let b = now();
        assert!(b >= a);
        assert_eq!(until(a), Duration::ZERO);
        let later = a.plus(Micros::from_millis(500));
        let d = until(later);
        assert!(
            d <= Duration::from_millis(500) && d > Duration::from_millis(400),
            "{d:?}"
        );
    }
}
