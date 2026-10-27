//! The two Hyprland IPC calls the capture side needs: the lock check (ADR 0017,
//! design note §6) and the start-up release. Both go over the
//! request socket (`.socket.sock`), never through a `hyprctl` process.
//!
//! Both are `ctl` calls: [`lock_check_async`] on its tokio loop, [`lock_check`]
//! anywhere a blocking call is fine. Each costs one connection and one
//! `j/monitors` round trip.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ferry_core::source::LockCheck;
use ferry_hypr_ipc::Instance;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The whole lock check (connect, write, read, parse) must finish within this,
/// or it counts as [`LockCheck::Unknown`] . The default of
/// `[safety] lock_check_budget_ms`; ferryd passes the configured one.
pub const LOCK_CHECK_TIMEOUT: Duration = Duration::from_millis(50);

/// The request behind the lock check. Plain `monitors`: a disabled monitor
/// cannot show a lock surface.
const MONITORS: &str = "j/monitors";

/// Map one IPC answer to a [`LockCheck`]: `Locked` when any monitor's
/// `solitaryBlockedBy` contains `LOCK`, `Unknown` on any error, including an
/// answer that cannot tell.
pub fn lock_from_answer(answer: Result<String, ferry_hypr_ipc::Error>) -> LockCheck {
    match answer.and_then(|json| ferry_hypr_ipc::session_locked_from_json(&json)) {
        Ok(true) => LockCheck::Locked,
        Ok(false) => LockCheck::Unlocked,
        Err(e) => {
            tracing::debug!(error = %e, "lock check: unknown");
            LockCheck::Unknown
        }
    }
}

/// Where a lock check failed. Logged with every `Unknown` so a slow compositor
/// can be told from a missing socket or an unusable answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connect,
    Write,
    Read,
    /// The whole budget ran out; the phase it was in is not known.
    Timeout,
    /// An answer arrived but cannot tell (bad JSON, no monitors).
    Parse,
    /// The blocking [`lock_check`], whose single call does not split the phases.
    Blocking,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Connect => "connect",
            Phase::Write => "write",
            Phase::Read => "read",
            Phase::Timeout => "timeout",
            Phase::Parse => "parse",
            Phase::Blocking => "blocking",
        }
    }
}

/// At most one `Unknown` warning per this interval; the rest are counted.
const WARN_INTERVAL: Duration = Duration::from_secs(1);

/// Rate limit for the `Unknown` warning: one per [`WARN_INTERVAL`], with the
/// number of warnings swallowed since the last one.
#[derive(Debug, Default)]
struct WarnLimiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl WarnLimiter {
    /// `Some(suppressed since the last warning)` when this one may be logged.
    fn admit(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < WARN_INTERVAL => {
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

static WARN_LIMITER: Mutex<WarnLimiter> = Mutex::new(WarnLimiter {
    last: None,
    suppressed: 0,
});

/// The phase for a failed async exchange step: a timeout is its own phase,
/// anything else is the step that raised it.
fn phase_of(step: Phase, err: &std::io::Error) -> Phase {
    if err.kind() == std::io::ErrorKind::TimedOut {
        Phase::Timeout
    } else {
        step
    }
}

/// Count and log one `Unknown` (rate limited), and return it. Diagnostics
/// only: the result is the same `Unknown` as before.
fn unknown(phase: Phase, kind: &str, elapsed: Duration) -> LockCheck {
    let admitted = WARN_LIMITER
        .lock()
        .map(|mut l| l.admit(Instant::now()))
        .unwrap_or(Some(0));
    if let Some(suppressed) = admitted {
        tracing::warn!(
            elapsed_us = elapsed.as_micros() as u64,
            phase = phase.name(),
            error_kind = kind,
            suppressed,
            "lock check: unknown"
        );
    }
    LockCheck::Unknown
}

/// Read the lock state now, blocking for at most `timeout`. Used per
/// activation and for the poll ([`ferry_core::source::SourceAction::CheckLock`],
/// answered with `SourceEvent::LockPolled`).
pub fn lock_check(ipc: &Instance, timeout: Duration) -> LockCheck {
    let t0 = Instant::now();
    let answer = ipc.request_within(MONITORS, timeout);
    let check = lock_from_answer(answer.as_ref().map(Clone::clone).map_err(clone_err));
    if check == LockCheck::Unknown {
        let (phase, kind) = match &answer {
            Err(ferry_hypr_ipc::Error::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
                (Phase::Timeout, format!("{:?}", e.kind()))
            }
            Err(ferry_hypr_ipc::Error::Io(e)) => (Phase::Blocking, format!("{:?}", e.kind())),
            Err(_) => (Phase::Blocking, "other".to_owned()),
            Ok(_) => (Phase::Parse, "unusable_answer".to_owned()),
        };
        return unknown(phase, &kind, t0.elapsed());
    }
    check
}

fn clone_err(e: &ferry_hypr_ipc::Error) -> ferry_hypr_ipc::Error {
    match e {
        ferry_hypr_ipc::Error::Io(e) => ferry_hypr_ipc::Error::Io(std::io::Error::from(e.kind())),
        other => ferry_hypr_ipc::Error::Command {
            command: MONITORS.to_owned(),
            response: other.to_string(),
        },
    }
}

/// [`lock_check`] for the `ctl` thread's tokio loop: it never blocks the loop,
/// and gives up after `timeout`.
pub async fn lock_check_async(ipc: &Instance, timeout: Duration) -> LockCheck {
    let t0 = Instant::now();
    let path = ipc.socket_path();
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|e| (Phase::Connect, e))?;
        stream
            .write_all(MONITORS.as_bytes())
            .await
            .map_err(|e| (Phase::Write, e))?;
        let mut answer = Vec::with_capacity(4096);
        stream
            .read_to_end(&mut answer)
            .await
            .map_err(|e| (Phase::Read, e))?;
        Ok::<_, (Phase, std::io::Error)>(String::from_utf8_lossy(&answer).into_owned())
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Ok(json)) => {
            let check = lock_from_answer(Ok(json));
            if check == LockCheck::Unknown {
                return unknown(Phase::Parse, "unusable_answer", t0.elapsed());
            }
            check
        }
        Ok(Err((step, e))) => unknown(phase_of(step, &e), &format!("{:?}", e.kind()), t0.elapsed()),
        Err(_) => unknown(Phase::Timeout, "Elapsed", t0.elapsed()),
    }
}

/// End any input capture through Hyprland IPC, `hl.dsp.release_input_capture()`.
/// Ferryd calls it on every start, in every backend, before
/// `CreateSession`: it clears a capture orphaned by a killed
/// ferryd. Blocking, for at most ferry-hypr-ipc's 5 s request timeout; the
/// connect never blocks.
pub fn release_input_capture(ipc: &Instance) -> Result<(), ferry_hypr_ipc::Error> {
    ipc.release_input_capture()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    fn fake_instance(dir: &std::path::Path) -> Instance {
        Instance {
            signature: "test".into(),
            dir: dir.to_path_buf(),
        }
    }

    /// A one-shot fake Hyprland that answers `j/monitors` with `answer`, after
    /// `delay`.
    fn serve(dir: &std::path::Path, answer: &'static str, delay: Duration) {
        let listener = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], MONITORS.as_bytes());
            std::thread::sleep(delay);
            let _ = s.write_all(answer.as_bytes());
        });
    }

    const UNLOCKED: &str = r#"[{"name":"Virtual-1","solitaryBlockedBy":["WINDOWED","CANDIDATE"]}]"#;
    const LOCKED: &str =
        r#"[{"name":"Virtual-1","solitaryBlockedBy":["LOCK","WINDOWED","CANDIDATE"]}]"#;

    #[test]
    fn answer_mapping() {
        assert_eq!(lock_from_answer(Ok(UNLOCKED.into())), LockCheck::Unlocked);
        assert_eq!(lock_from_answer(Ok(LOCKED.into())), LockCheck::Locked);
        assert_eq!(lock_from_answer(Ok("[]".into())), LockCheck::Unknown);
        assert_eq!(
            lock_from_answer(Ok("unknown request".into())),
            LockCheck::Unknown
        );
    }

    #[test]
    fn blocking_check_against_a_fake_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let ipc = fake_instance(tmp.path());
        // Wrong path: no socket.
        assert_eq!(lock_check(&ipc, LOCK_CHECK_TIMEOUT), LockCheck::Unknown);
        serve(tmp.path(), LOCKED, Duration::ZERO);
        assert_eq!(lock_check(&ipc, LOCK_CHECK_TIMEOUT), LockCheck::Locked);
    }

    #[test]
    fn blocking_check_times_out_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let ipc = fake_instance(tmp.path());
        serve(tmp.path(), UNLOCKED, Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        assert_eq!(lock_check(&ipc, LOCK_CHECK_TIMEOUT), LockCheck::Unknown);
        assert!(t0.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_check_answers_and_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let ipc = fake_instance(tmp.path());
        assert_eq!(
            lock_check_async(&ipc, LOCK_CHECK_TIMEOUT).await,
            LockCheck::Unknown
        );
        serve(tmp.path(), UNLOCKED, Duration::ZERO);
        assert_eq!(
            lock_check_async(&ipc, LOCK_CHECK_TIMEOUT).await,
            LockCheck::Unlocked
        );
        std::fs::remove_file(tmp.path().join(".socket.sock")).unwrap();
        serve(tmp.path(), UNLOCKED, Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        assert_eq!(
            lock_check_async(&ipc, LOCK_CHECK_TIMEOUT).await,
            LockCheck::Unknown
        );
        assert!(t0.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn warn_limiter_admits_one_per_interval_and_counts_the_rest() {
        let mut l = WarnLimiter::default();
        let t = Instant::now();
        assert_eq!(l.admit(t), Some(0));
        assert_eq!(l.admit(t + Duration::from_millis(10)), None);
        assert_eq!(l.admit(t + Duration::from_millis(999)), None);
        assert_eq!(l.admit(t + WARN_INTERVAL), Some(2));
        assert_eq!(l.admit(t + WARN_INTERVAL + Duration::from_millis(1)), None);
        assert_eq!(l.admit(t + WARN_INTERVAL * 2), Some(1));
    }

    #[test]
    fn phase_classification() {
        let timed_out = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(phase_of(Phase::Connect, &timed_out), Phase::Timeout);
        assert_eq!(phase_of(Phase::Connect, &refused), Phase::Connect);
        assert_eq!(phase_of(Phase::Read, &refused), Phase::Read);
        assert_eq!(phase_of(Phase::Write, &refused), Phase::Write);
        assert_eq!(Phase::Parse.name(), "parse");
    }
}
