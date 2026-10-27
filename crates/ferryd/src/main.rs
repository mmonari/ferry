//! `ferryd`: the ferry daemon. Logs to stderr (the journal, under systemd).
#![expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "the binary's own --help, --version and start-up errors, before logging exists"
)]

use std::process::ExitCode;

use ferryd::notify::Mode;
use ferryd::{Daemon, InjectBackend, Options, Paths};
use tracing::level_filters::LevelFilter;

const USAGE: &str = "usage: ferryd [--log-level error|warn|info|debug|trace] [--no-input]\n\
    \x20             [--notify-bus ADDRESS]\n\
    \n\
    --no-input    network only: never capture, never inject (every Enter is refused), and no\n\
    \x20             desktop toasts (the `notice` events on the socket go on)\n\
    --notify-bus  send the toasts to the D-Bus at ADDRESS, not to the session bus; with\n\
    \x20             --no-input this is the only way to get desktop toasts\n\
    \n\
    Environment: FERRY_LOG sets the level too (the flag wins).\n";

/// Where the toasts go. The session bus is the real desktop, so only a daemon that runs with
/// input does that: a `--no-input` daemon is a development one, started on a desktop that is in
/// use, and it must not pop notifications on it. A bus named with `--notify-bus` is the
/// developer's own.
fn toast_mode(bus: Option<String>, input: bool) -> Mode {
    match (bus, input) {
        (Some(address), _) => Mode::Bus(address),
        (None, true) => Mode::Session,
        (None, false) => Mode::Off,
    }
}

fn main() -> ExitCode {
    let mut level = std::env::var("FERRY_LOG").ok();
    let mut input = true;
    let mut notify_bus = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log-level" => level = args.next(),
            "--no-input" => input = false,
            "--notify-bus" => {
                // a missing address must not fall back to the session bus
                let Some(address) = args.next() else {
                    eprintln!("ferryd: --notify-bus needs an address\n{USAGE}");
                    return ExitCode::from(2);
                };
                notify_bus = Some(address);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("ferryd {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("ferryd: unexpected argument {other:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let filter = match level.as_deref().map(str::parse::<LevelFilter>) {
        None => LevelFilter::INFO,
        Some(Ok(l)) => l,
        Some(Err(_)) => {
            eprintln!(
                "ferryd: bad log level {:?}\n{USAGE}",
                level.unwrap_or_default()
            );
            return ExitCode::from(2);
        }
    };
    let builder = tracing_subscriber::fmt()
        .with_max_level(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false);
    // The journal stamps its own time.
    if stderr_is_journal() {
        builder.without_time().init();
    } else {
        builder.init();
    }

    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ferryd: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut opts = Options::new(paths);
    opts.sd_notify = true;
    opts.handle_signals = true;
    opts.panic_hook = true;
    opts.toasts.mode = toast_mode(notify_bus, input);
    if input {
        opts.capture = true;
        opts.inject = InjectBackend::Wayland;
    }
    match Daemon::start(opts) {
        Ok(d) => {
            d.wait();
            if ferryd::panicked() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("ferryd: {e}");
            ExitCode::FAILURE
        }
    }
}

/// systemd.exec(5), `$JOURNAL_STREAM`: the variable names the journal stream as `device:inode`,
/// and a process may take its stderr for the journal only when `fstat(2)` of fd 2 matches both
/// numbers. The variable alone proves nothing: it is inherited by everything a terminal runs
/// under a systemd unit, including a ferryd whose stderr is a file or the terminal itself.
fn stderr_is_journal() -> bool {
    let var = std::env::var_os("JOURNAL_STREAM");
    let Ok(st) = rustix::fs::fstat(std::io::stderr()) else {
        return false;
    };
    journal_stream_matches(
        var.as_deref().and_then(|v| v.to_str()),
        st.st_dev,
        st.st_ino,
    )
}

/// True when `var` (the value of `$JOURNAL_STREAM`, if set) is `device:inode`, both decimal,
/// and equals `(dev, ino)`.
fn journal_stream_matches(var: Option<&str>, dev: u64, ino: u64) -> bool {
    let Some((d, i)) = var.and_then(|v| v.split_once(':')) else {
        return false;
    };
    matches!((d.parse::<u64>(), i.parse::<u64>()), (Ok(d), Ok(i)) if d == dev && i == ino)
}

#[cfg(test)]
mod tests {
    use super::{journal_stream_matches, toast_mode};
    use ferryd::notify::Mode;

    #[test]
    fn a_daemon_with_input_toasts_on_the_session_bus() {
        assert!(matches!(toast_mode(None, true), Mode::Session));
    }

    #[test]
    fn a_daemon_without_input_never_reaches_the_session_bus() {
        // the development daemon on a desktop in use
        assert!(matches!(toast_mode(None, false), Mode::Off));
    }

    #[test]
    fn a_named_bus_wins_with_or_without_input() {
        for input in [true, false] {
            match toast_mode(Some("unix:path=/tmp/b".into()), input) {
                Mode::Bus(a) => assert_eq!(a, "unix:path=/tmp/b"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn matches_when_device_and_inode_agree() {
        assert!(journal_stream_matches(Some("8:123456"), 8, 123_456));
    }

    #[test]
    fn a_different_device_or_inode_is_not_the_journal() {
        assert!(!journal_stream_matches(Some("8:123456"), 9, 123_456));
        assert!(!journal_stream_matches(Some("8:123456"), 8, 123_457));
    }

    #[test]
    fn an_unset_variable_is_not_the_journal() {
        assert!(!journal_stream_matches(None, 8, 5));
    }

    #[test]
    fn a_malformed_value_is_not_the_journal() {
        for bad in ["", "8", "8:", ":5", "a:b", "8:5:2", "8 :5", "-1:5", "8:0x5"] {
            assert!(!journal_stream_matches(Some(bad), 8, 5), "{bad:?}");
        }
    }

    #[test]
    fn a_value_too_large_for_u64_is_not_the_journal() {
        assert!(!journal_stream_matches(
            Some("8:99999999999999999999999"),
            8,
            5
        ));
    }
}
