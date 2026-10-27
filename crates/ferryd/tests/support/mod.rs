//! Shared helpers: a temp HOME and XDG_RUNTIME_DIR, an in-process daemon, and a socket client.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_core::geom::{MonitorInfo, WireRect};
use ferry_net::Identity;
use ferryd::{Daemon, MonitorSource, Options, Paths};
use serde_json::{Value, json};

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub run: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("fy").tempdir().unwrap();
        let home = tmp.path().join("home");
        let run = tmp.path().join("run");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        Self { tmp, home, run }
    }

    pub fn paths(&self) -> Paths {
        Paths::under_home(&self.home, &self.run)
    }

    pub fn socket(&self) -> PathBuf {
        self.paths().socket_path()
    }
}

pub fn mon(name: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
    MonitorInfo {
        name: name.into(),
        description: format!("{name} (test)"),
        rect: WireRect { x, y, w, h },
        scale: 1.0,
        transform: 0,
        shift: None,
    }
}

/// The desktop of the first-light setup.
pub fn desk_monitors() -> Vec<MonitorInfo> {
    vec![
        mon("HDMI-A-1", 0, 0, 1920, 1080),
        mon("DVI-D-1", 1920, 0, 1920, 1080),
    ]
}

/// Options for a test daemon: an ephemeral identity, loopback on an ephemeral port, no signals.
pub fn options(env: &Env) -> Options {
    let mut o = Options::new(env.paths());
    o.identity = Some(Identity::ephemeral().unwrap());
    o.bind = Some("127.0.0.1:0".parse().unwrap());
    o.name = Some("desk".into());
    o.monitors = MonitorSource::Fixed(desk_monitors());
    o
}

pub fn start(env: &Env) -> Daemon {
    Daemon::start(options(env)).unwrap()
}

/// A daemon whose target input goes to `recorder` instead of a compositor.
pub fn start_recording(env: &Env, recorder: &ferryd::Recorder) -> Daemon {
    let mut o = options(env);
    o.inject = ferryd::InjectBackend::Custom(Box::new(recorder.clone()));
    Daemon::start(o).unwrap()
}

/// A tokio runtime whose one worker keeps driving the fake peer's endpoint while the test
/// blocks on the socket.
pub fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

pub fn fpx(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub hello: Value,
    /// The start of a line that a timed-out read left behind.
    pending: Vec<u8>,
    /// Events that came before a response while [`Client::call_holding`] waited for it: the
    /// next reads return them first, in the order they came.
    held: std::collections::VecDeque<Value>,
}

/// How long `read_line` waits for the daemon.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

impl Client {
    pub fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let hello = serde_json::from_str(&line).unwrap();
        Self {
            reader,
            writer: stream,
            next_id: 1,
            hello,
            pending: Vec::new(),
            held: std::collections::VecDeque::new(),
        }
    }

    /// Writes raw bytes and reads one response line.
    pub fn raw(&mut self, bytes: &[u8]) -> Value {
        self.writer.write_all(bytes).unwrap();
        self.read_line()
    }

    /// The write half, for a test that sends bytes without waiting for an answer.
    pub fn writer_for_test(&mut self) -> &mut UnixStream {
        &mut self.writer
    }

    pub fn read_line(&mut self) -> Value {
        self.read_within(READ_TIMEOUT)
            .expect("the daemon sent no line within 10 s")
    }

    /// The next line, waiting at most `timeout`; `None` when none came. A line cut short by the
    /// timeout is kept for the next call. Panics when the daemon closed the connection.
    pub fn read_within(&mut self, timeout: Duration) -> Option<Value> {
        if let Some(v) = self.held.pop_front() {
            return Some(v);
        }
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .unwrap();
        let line = loop {
            match self.reader.read_until(b'\n', &mut self.pending) {
                Ok(0) => panic!("the daemon closed the connection"),
                Ok(_) if self.pending.last() == Some(&b'\n') => {
                    break std::mem::take(&mut self.pending);
                }
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    self.reader
                        .get_ref()
                        .set_read_timeout(Some(READ_TIMEOUT))
                        .unwrap();
                    return None;
                }
                Err(e) => panic!("{e}"),
            }
        };
        self.reader
            .get_ref()
            .set_read_timeout(Some(READ_TIMEOUT))
            .unwrap();
        Some(
            serde_json::from_slice(&line)
                .unwrap_or_else(|e| panic!("not JSON: {:?}: {e}", String::from_utf8_lossy(&line))),
        )
    }

    /// Every line that comes until `quiet` passes with none.
    pub fn lines_until_quiet(&mut self, quiet: Duration) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = self.read_within(quiet) {
            out.push(v);
        }
        out
    }

    /// The next event of `topic` within `timeout`, skipping the events of other topics.
    pub fn event_within(&mut self, topic: &str, timeout: Duration) -> Option<Value> {
        let until = std::time::Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            let v = self.read_within(left)?;
            if v.get("event").and_then(Value::as_str) == Some(topic) {
                return Some(v);
            }
            if left.is_zero() {
                return None;
            }
        }
    }

    /// Sends a request and returns its response, with the events that came before it.
    pub fn call_collecting(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).unwrap();
        let mut events = Vec::new();
        loop {
            let v = self.read_line();
            if v.get("event").is_some() {
                events.push(v);
            } else {
                assert_eq!(v["id"], id, "{v}");
                return (v, events);
            }
        }
    }

    /// Like [`Client::call_collecting`], but the events that came before the response are not
    /// handed back: they stay in front of the connection, for the reads that follow. A daemon
    /// may write an event before the response to the request that caused it (the dialer's
    /// `code` before the answer to `pair.start`, under load), so a test that waits for that
    /// event afterwards must not lose it here.
    pub fn call_holding(&mut self, method: &str, params: Value) -> Value {
        let (response, events) = self.call_collecting(method, params);
        self.held.extend(events);
        response
    }

    /// `subscribe` to `topics`; returns the topics the daemon says it subscribed to.
    pub fn subscribe(&mut self, topics: &[&str]) -> Value {
        self.result("subscribe", json!({ "topics": topics }))
    }

    /// Sends a request, returns the whole response line (checking the id).
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"id": id, "method": method, "params": params});
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        let resp = self.raw(&line);
        assert_eq!(resp["id"], id, "{resp}");
        resp
    }

    pub fn result(&mut self, method: &str, params: Value) -> Value {
        let resp = self.call(method, params);
        assert!(resp.get("error").is_none(), "{method}: {resp}");
        resp["result"].clone()
    }

    /// The error code of a call that must fail.
    pub fn error(&mut self, method: &str, params: Value) -> (String, String, Option<String>) {
        let resp = self.call(method, params);
        let e = resp
            .get("error")
            .unwrap_or_else(|| panic!("{method} should fail: {resp}"));
        (
            e["code"].as_str().unwrap().to_string(),
            e["message"].as_str().unwrap().to_string(),
            e.get("hint").and_then(Value::as_str).map(str::to_string),
        )
    }
}

/// Polls `f` until it returns `Some` (up to 5 s).
pub fn wait_for<T>(what: &str, f: impl FnMut() -> Option<T>) -> T {
    wait_for_within(what, Duration::from_secs(5), f)
}

/// Polls `f` until it returns `Some`, for at most `within`: for a condition whose time depends
/// on how fast the host drains a backlog, where the bound only tells a hang from a slow host.
pub fn wait_for_within<T>(what: &str, within: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + within;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
