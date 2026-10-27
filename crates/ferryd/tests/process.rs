//! The real `ferryd` binary: config, sd-notify, signals, and the second-instance refusal.
//! Temp HOME and XDG_RUNTIME_DIR; Hyprland and the real state dirs are never touched.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::io::Read;
use std::os::unix::net::UnixDatagram;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use serde_json::json;
use support::*;

struct Proc {
    env: Env,
    notify: UnixDatagram,
    child: Child,
}

/// `[peer] port = 0`: the child binds a port the kernel picks and keeps it. A port read from a
/// closed socket and written to `config.toml` could be taken by another test before the child
/// binds it. `id` reports the port in use.
const ANY_PORT: u16 = 0;

fn command(env: &Env, notify_path: Option<&std::path::Path>) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ferryd"));
    // Network only: the binary must never capture or inject on the host.
    c.arg("--no-input")
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_RUNTIME_DIR", &env.run)
        .env("FERRY_LOG", "debug")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = notify_path {
        c.env("NOTIFY_SOCKET", p);
    }
    c
}

impl Proc {
    fn start(config: &str) -> Self {
        Self::start_with(config, &[])
    }

    fn start_with(config: &str, envs: &[(&str, &str)]) -> Self {
        let env = Env::new();
        let cfg = env.paths().config_file;
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, config).unwrap();
        let notify_path = env.tmp.path().join("notify.sock");
        let notify = UnixDatagram::bind(&notify_path).unwrap();
        notify
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let child = command(&env, Some(&notify_path))
            .envs(envs.iter().copied())
            .spawn()
            .unwrap();
        Self { env, notify, child }
    }

    fn expect_notify(&self, what: &str) {
        let mut buf = [0u8; 256];
        let n = self
            .notify
            .recv(&mut buf)
            .unwrap_or_else(|e| panic!("no {what}: {e}"));
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap().trim(), what);
    }

    fn signal(&self, sig: Signal) {
        kill_process(Pid::from_raw(self.child.id() as i32).unwrap(), sig).unwrap();
    }

    fn wait(&mut self) -> (std::process::ExitStatus, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "ferryd did not exit");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut err = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (status, err)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn starts_ready_serves_a_kernel_picked_port_and_stops_on_sigterm() {
    let mut p = Proc::start(&format!(
        "[peer]\nname = \"proc\"\nport = {ANY_PORT}\n[future]\nx = 1\n[peer2]\n"
    ));
    p.expect_notify("READY=1");

    let mut c = Client::connect(&p.env.socket());
    assert_eq!(c.hello["data"]["peer"], "proc");
    let port = c.result("id", json!(null))["port"].as_u64().unwrap();
    assert_ne!(port, 0, "id reports the port the kernel picked");
    // the identity went to HOME/.local/share/ferry with private modes
    let key = p.env.paths().data_dir.join("key.der");
    assert!(key.exists());

    p.signal(Signal::TERM);
    p.expect_notify("STOPPING=1");
    let (status, err) = p.wait();
    assert!(status.success(), "{status:?}\n{err}");
    assert!(!p.env.socket().exists(), "the socket is removed on SIGTERM");
    assert!(err.contains("unknown section [future]"), "{err}");
    assert!(err.contains("ferryd ready"), "{err}");
    assert!(err.contains("shutting down"), "{err}");
}

#[test]
fn the_binary_sends_watchdog_heartbeats_when_systemd_asks_for_them() {
    // WatchdogSec=1s: heartbeats every 500 ms, as under the unit (2 s and 1 s).
    let mut p = Proc::start_with(
        &format!("[peer]\nport = {ANY_PORT}\n"),
        &[("WATCHDOG_USEC", "1000000")],
    );
    p.expect_notify("READY=1");
    let t0 = Instant::now();
    for _ in 0..3 {
        p.expect_notify("WATCHDOG=1");
    }
    assert!(
        t0.elapsed() < Duration::from_millis(2500),
        "three heartbeats took {:?}",
        t0.elapsed()
    );
    p.signal(Signal::TERM);
    let (status, err) = p.wait();
    assert!(status.success(), "{status:?}\n{err}");
    assert!(err.contains("systemd watchdog on"), "{err}");
}

#[test]
fn sigint_stops_it_too() {
    let mut p = Proc::start(&format!("[peer]\nport = {ANY_PORT}\n"));
    p.expect_notify("READY=1");
    p.signal(Signal::INT);
    let (status, _) = p.wait();
    assert!(status.success());
    assert!(!p.env.socket().exists());
}

#[test]
fn a_second_ferryd_refuses_to_start() {
    let mut p = Proc::start(&format!("[peer]\nport = {ANY_PORT}\n"));
    p.expect_notify("READY=1");

    let second = command(&p.env, None).output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    let err = String::from_utf8_lossy(&second.stderr);
    assert!(err.contains("already running"), "{err}");

    // the first is unharmed and still owns the socket
    let mut c = Client::connect(&p.env.socket());
    c.result("status", json!(null));
    p.signal(Signal::TERM);
    assert!(p.wait().0.success());
}

#[test]
fn a_port_in_use_is_a_clear_start_failure() {
    let taken = std::net::UdpSocket::bind("[::]:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let env = Env::new();
    let cfg = env.paths().config_file;
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, format!("[peer]\nport = {port}\n")).unwrap();
    let out = command(&env, None).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot bind UDP"), "{err}");
    assert!(!env.socket().exists(), "no socket is left behind");
}

#[test]
fn bad_arguments_are_a_usage_error() {
    let env = Env::new();
    let out = command(&env, None).arg("--nope").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = command(&env, None)
        .args(["--log-level", "loud"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = command(&env, None).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ferryd "));
    let out = command(&env, None).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("--no-input"));
}

/// Run ferryd with its stderr in a file until it is ready, stop it, and return the log.
/// `journal` maps the stderr file to the value of `$JOURNAL_STREAM` the child inherits.
fn log_with_stderr_in_file(journal: impl FnOnce(&std::fs::File) -> Option<String>) -> String {
    let env = Env::new();
    let cfg = env.paths().config_file;
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, format!("[peer]\nport = {ANY_PORT}\n")).unwrap();
    let notify_path = env.tmp.path().join("notify.sock");
    let notify = UnixDatagram::bind(&notify_path).unwrap();
    notify
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let log_path = env.tmp.path().join("ferryd.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut cmd = command(&env, Some(&notify_path));
    if let Some(v) = journal(&log) {
        cmd.env("JOURNAL_STREAM", v);
    }
    let mut child = cmd.stderr(log).spawn().unwrap();
    let mut buf = [0u8; 256];
    let n = notify.recv(&mut buf).expect("no READY=1");
    assert_eq!(std::str::from_utf8(&buf[..n]).unwrap().trim(), "READY=1");
    kill_process(Pid::from_raw(child.id() as i32).unwrap(), Signal::TERM).unwrap();
    assert!(child.wait().unwrap().success());
    std::fs::read_to_string(log_path).unwrap()
}

fn ready_line(log: &str) -> &str {
    log.lines()
        .find(|l| l.contains("ferryd ready"))
        .unwrap_or_else(|| panic!("no ready line in {log:?}"))
}

#[test]
fn journal_stream_drops_the_timestamp_only_when_stderr_is_that_stream() {
    use std::os::unix::fs::MetadataExt;
    // A terminal under a systemd unit has the variable set while stderr is not the journal:
    // the timestamps must stay (they were lost in a real test).
    let log = log_with_stderr_in_file(|_| Some("1:2".into()));
    assert!(
        ready_line(&log).starts_with(|c: char| c.is_ascii_digit()),
        "{log}"
    );
    // Same file, variable naming this very file: stderr is "the journal", so no timestamp.
    let log = log_with_stderr_in_file(|f| {
        let m = f.metadata().unwrap();
        Some(format!("{}:{}", m.dev(), m.ino()))
    });
    assert!(
        !ready_line(&log).starts_with(|c: char| c.is_ascii_digit()),
        "{log}"
    );
}
