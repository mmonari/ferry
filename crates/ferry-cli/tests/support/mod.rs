//! A daemon per temp HOME and XDG_RUNTIME_DIR, and the real `ferry` binary run against it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use ferry_core::geom::{MonitorInfo, WireRect};
use ferryd::{Daemon, MonitorSource, Options, Paths};
use serde_json::Value;

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub run: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("fc").tempdir().unwrap();
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

    /// An in-process daemon with a real on-disk identity, on loopback with an ephemeral port.
    pub fn daemon(&self, name: &str) -> Daemon {
        let mut o = Options::new(self.paths());
        o.bind = Some("127.0.0.1:0".parse().unwrap());
        o.name = Some(name.into());
        o.monitors = MonitorSource::Fixed(vec![MonitorInfo {
            name: "HDMI-A-1".into(),
            description: String::new(),
            rect: WireRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            scale: 1.0,
            transform: 0,
            shift: None,
        }]);
        Daemon::start(o).unwrap()
    }

    /// Runs the `ferry` binary with this env's HOME and XDG_RUNTIME_DIR and nothing else.
    pub fn ferry(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ferry"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.run)
            .output()
            .unwrap()
    }

    pub fn ferry_json(&self, args: &[&str]) -> Value {
        let out = self.ferry(args);
        assert!(
            out.status.success(),
            "ferry {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

/// Writes an executable script. `install(1)` writes it in its own process: while this process holds
/// a file open for writing, a child that a parallel test forks inherits the descriptor until it
/// execs, and running the script in that window fails with `Text file busy` (ETXTBSY).
pub fn script(path: &Path, body: &str) {
    let src = path.with_extension("src");
    std::fs::write(&src, format!("#!/bin/sh\n{body}\n")).unwrap();
    let ok = std::process::Command::new("install")
        .args(["-m", "755"])
        .arg(&src)
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "install {}", path.display());
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}
