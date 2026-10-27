//! A periodic tick that costs one wake-up, for the watchdog's heartbeat.
//!
//! `tokio::time::interval` costs two voluntary context switches per tick (measured: 21 for ten
//! 500 ms ticks): the driver parks for the timer rounded to whole milliseconds, wakes a hair
//! early, finds nothing due and parks again. The heartbeat's budget is 4 wake-ups a second at
//! idle (of NFR-RES-01's 10 for a connected daemon), so its ticks come from a `timerfd` that the
//! runtime's reactor watches: one wake-up each, on the kernel's own deadline.

use std::io;
use std::os::fd::OwnedFd;
use std::time::Duration;

use rustix::time::{Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec};
use tokio::io::unix::AsyncFd;

/// Ticks every `period`, starting one `period` from now. Create it inside the runtime.
pub enum Ticker {
    Fd(AsyncFd<OwnedFd>),
    /// If the kernel or the reactor refuses a timerfd: the tokio interval, at two wake-ups a tick.
    Interval(tokio::time::Interval),
}

fn spec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: d.subsec_nanos().into(),
    }
}

fn timerfd(period: Duration) -> io::Result<AsyncFd<OwnedFd>> {
    let fd = rustix::time::timerfd_create(
        TimerfdClockId::Monotonic,
        TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
    )?;
    let period = period.max(Duration::from_millis(1));
    rustix::time::timerfd_settime(
        &fd,
        TimerfdTimerFlags::empty(),
        &Itimerspec {
            it_interval: spec(period),
            it_value: spec(period),
        },
    )?;
    // tokio deprecates `AsyncFd::new` because it accepts any `AsRawFd`, and its replacement is
    // `unsafe`, which this workspace forbids. An `OwnedFd` is an open descriptor that stays open
    // and the same for the `AsyncFd`'s whole life, which is all the unsafe contract asks.
    #[expect(
        deprecated,
        reason = "AsyncFd::new is deprecated for its safe-looking signature; its replacement is \
                  unsafe, which this workspace forbids (RG-UNSAFE-02)"
    )]
    AsyncFd::new(fd)
}

impl Ticker {
    pub fn new(period: Duration) -> Self {
        match timerfd(period) {
            Ok(fd) => Self::Fd(fd),
            Err(e) => {
                tracing::warn!(error = %e, "no timerfd: ticking with tokio's timer");
                let mut i = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
                i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                Self::Interval(i)
            }
        }
    }

    /// Waits for the next tick. A tick missed while the task ran elsewhere is not made up.
    pub async fn tick(&mut self) {
        match self {
            Self::Interval(i) => {
                i.tick().await;
            }
            Self::Fd(fd) => loop {
                let Ok(mut ready) = fd.readable().await else {
                    // the reactor is gone: nothing will wake this task again
                    std::future::pending::<()>().await;
                    return;
                };
                let mut count = [0u8; 8];
                match rustix::io::read(fd.get_ref(), &mut count) {
                    Ok(_) => return,
                    Err(rustix::io::Errno::AGAIN) => ready.clear_ready(),
                    Err(e) => {
                        tracing::warn!(error = %e, "timerfd read failed");
                        ready.clear_ready();
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn ticks_on_the_period_and_skips_the_ones_it_missed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = Ticker::new(Duration::from_millis(50));
            assert!(matches!(t, Ticker::Fd(_)), "a timerfd works here");
            let t0 = Instant::now();
            t.tick().await;
            t.tick().await;
            let two = t0.elapsed();
            assert!(
                two >= Duration::from_millis(95) && two < Duration::from_millis(400),
                "{two:?}"
            );
            // 5 periods pass unread: one tick is ready at once, not five
            tokio::time::sleep(Duration::from_millis(260)).await;
            let t1 = Instant::now();
            t.tick().await;
            assert!(t1.elapsed() < Duration::from_millis(20));
            t.tick().await;
            assert!(
                t1.elapsed() >= Duration::from_millis(30),
                "the missed ticks were queued"
            );
        });
    }
}
