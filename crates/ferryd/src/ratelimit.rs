//! Rate-limited log lines (G-9).
//!
//! A line that a peer or a stuck neighbour thread can make ferryd write once per frame or per
//! message would flood the journal: journald drops a unit's messages past 10,000 per 30 s, the
//! daemon's own faults among them. Such a site logs through [`log_limited!`] instead: at
//! most one line per site per [`LOG_INTERVAL`], carrying how many it swallowed since. The
//! per-frame detail goes to `debug`, and a counter says how many happened.

use std::time::{Duration, Instant};

/// One line per site per this interval.
pub const LOG_INTERVAL: Duration = Duration::from_secs(1);

/// Admits one event per `interval` and counts the ones it refuses.
#[derive(Debug)]
pub struct LogLimiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl LogLimiter {
    pub const fn new() -> Self {
        Self {
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(suppressed since the last admitted event)` when this one may be logged.
    pub fn admit(&mut self, now: Instant, interval: Duration) -> Option<u64> {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < interval => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

impl Default for LogLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// `log_limited!(warn, field = x, "text")`: `tracing::warn!` at most once per
/// [`LOG_INTERVAL`] for this call site, with a `suppressed` field. Works on any thread: the
/// limiter is a `static` of the site.
macro_rules! log_limited {
    ($level:ident, $($arg:tt)+) => {{
        static LIMITER: ::std::sync::Mutex<$crate::ratelimit::LogLimiter> =
            ::std::sync::Mutex::new($crate::ratelimit::LogLimiter::new());
        let admitted = LIMITER
            .lock()
            .map(|mut l| l.admit(::std::time::Instant::now(), $crate::ratelimit::LOG_INTERVAL))
            .unwrap_or(Some(0));
        if let Some(suppressed) = admitted {
            ::tracing::$level!(suppressed, $($arg)+);
        }
    }};
}
pub(crate) use log_limited;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_per_interval_and_the_rest_counted() {
        let t0 = Instant::now();
        let mut l = LogLimiter::new();
        assert_eq!(l.admit(t0, LOG_INTERVAL), Some(0));
        for i in 1..=999 {
            assert_eq!(l.admit(t0 + Duration::from_micros(i), LOG_INTERVAL), None);
        }
        assert_eq!(l.admit(t0 + LOG_INTERVAL, LOG_INTERVAL), Some(999));
        assert_eq!(l.admit(t0 + LOG_INTERVAL, LOG_INTERVAL), None);
    }
}
