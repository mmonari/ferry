//! The panic hook asks for the shutdown at once, even when Hyprland's IPC is
//! wedged. One test in its own binary, because it installs the process-wide panic hook. IPC goes
//! to a fake Hyprland under the temp runtime dir and the portal is the fake one: nothing here
//! captures or injects.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::time::{Duration, Instant};

use ferryd::{FakePortal, MonitorSource};
use serde_json::json;
use support::*;

/// A Hyprland whose command socket never accepts and whose listen queue is full: every connect
/// would block. Keep the returned values alive.
fn wedged_hyprland(env: &Env) -> (UnixListener, UnixStream) {
    let dir = env.run.join("hypr/wedged_1_1");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".socket.sock");
    let listener = UnixListener::bind(&path).unwrap();
    nix::sys::socket::listen(&listener, nix::sys::socket::Backlog::new(0).unwrap()).unwrap();
    let waiting = UnixStream::connect(&path).unwrap();
    (listener, waiting)
}

/// The panic hook used to run the blocking `release_input_capture()` before it asked for the
/// shutdown, and its connect to a wedged Hyprland never returned: the daemon stayed half-dead
/// until systemd's watchdog. Now the IPC release gets 250 ms at most, and the shutdown (its
/// `STOPPING=1`) follows within 300 ms of the panic.
#[test]
fn a_panic_asks_for_the_shutdown_even_when_hyprland_is_wedged() {
    let env = Env::new();
    let _hyprland = wedged_hyprland(&env);
    let notify = env.tmp.path().join("notify.sock");
    let sock = UnixDatagram::bind(&notify).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut o = options(&env);
    o.monitors = MonitorSource::HyprlandIn(env.run.clone());
    // `capture` only turns IPC on, to the wedged fake; the portal is the fake one.
    o.capture = true;
    o.fake_portal = Some(FakePortal::new());
    o.panic_hook = true;
    o.notify_socket = Some(notify);
    let d = ferryd::Daemon::start(o).unwrap();
    let mut buf = [0u8; 128];
    let n = sock.recv(&mut buf).unwrap();
    assert_eq!(std::str::from_utf8(&buf[..n]).unwrap().trim(), "READY=1");

    let mut c = Client::connect(d.socket_path());
    d.panic_net();
    let t0 = Instant::now();
    // a release request wakes the net thread, which panics at once
    let _ = c.call("release", json!(null));
    let stopping = loop {
        let n = sock.recv(&mut buf).expect("STOPPING=1 within 3 s");
        if std::str::from_utf8(&buf[..n])
            .unwrap()
            .contains("STOPPING=1")
        {
            break t0.elapsed();
        }
    };
    assert!(
        stopping < Duration::from_millis(300),
        "STOPPING=1 after {stopping:?}"
    );
    d.wait();
    assert!(ferryd::panicked());
}
