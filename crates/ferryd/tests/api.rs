//! The socket API against an in-process daemon: every method, every error code that can be
//! triggered without ferry-core's bodies, and the socket's own rules.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ferryd::{Daemon, MonitorSource, StartError};
use serde_json::{Value, json};
use support::*;

fn mask(v: &Value, pairs: &[(&str, &str)]) -> Value {
    let mut text = v.to_string();
    for (from, to) in pairs {
        text = text.replace(from, to);
    }
    serde_json::from_str(&text).unwrap()
}

#[test]
fn hello_is_the_first_line() {
    let env = Env::new();
    let d = start(&env);
    let c = Client::connect(d.socket_path());
    assert_eq!(c.hello["event"], "hello");
    assert_eq!(c.hello["data"]["api"], 1);
    assert_eq!(c.hello["data"]["peer"], "desk");
    assert_eq!(c.hello["data"]["fingerprint"], d.fingerprint().to_string());
    assert_eq!(c.hello["data"]["version"], env!("CARGO_PKG_VERSION"));
    d.shutdown();
}

#[test]
fn the_socket_directory_is_0700_and_the_socket_0600() {
    let env = Env::new();
    let d = start(&env);
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&env.paths().socket_dir()), 0o700);
    assert_eq!(mode(d.socket_path()), 0o600);
    d.shutdown();
}

#[test]
fn id_reports_name_fingerprint_and_the_bound_port() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result("id", json!(null));
    assert_eq!(r["name"], "desk");
    assert_eq!(r["fingerprint"], d.fingerprint().to_string());
    assert_eq!(r["port"], d.net_addr().port());
    assert!(r["addrs"].is_array());
    d.shutdown();
}

#[test]
fn status_of_a_fresh_daemon() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result("status", json!({}));
    let mut masked = mask(&r, &[(&d.fingerprint().to_string(), "<fp>")]);
    assert_eq!(masked["port"], d.net_addr().port());
    masked["port"] = json!("<port>");
    insta::assert_json_snapshot!(masked);
    d.shutdown();
}

#[test]
fn status_reports_capture_off_for_a_target_only_peer() {
    let env = Env::new();
    let cfg = env.paths().config_file;
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[input]\ncapture = \"off\"\n").unwrap();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result("status", json!(null));
    assert_eq!(r["capture"]["backend"], "off");
    assert_eq!(r["capture"]["gate"], "off");
    d.shutdown();
}

#[test]
fn doctor_answers_with_checks_and_never_reaches_for_the_hosts_compositor() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result("doctor", json!(null));
    let checks = r["checks"].as_array().expect("a list of checks");
    let get = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no check {name}: {checks:?}"))
    };
    // A daemon without input has no Hyprland to ask: named, FAIL, not a crash.
    assert_eq!(get("hyprland")["level"], "fail");
    assert!(
        get("hyprland")["detail"]
            .as_str()
            .unwrap()
            .contains("no Hyprland IPC")
    );
    assert_eq!(get("globals")["level"], "fail");
    assert_eq!(get("capture")["level"], "pass", "capture is off here");
    assert_eq!(get("port")["level"], "pass");
    assert_eq!(get("peers")["detail"], "none paired yet");
    for c in checks {
        assert!(c["level"].is_string() && c["detail"].is_string(), "{c}");
    }
    // the answer carries only the documented keys
    assert_eq!(r.as_object().unwrap().len(), 1);
    d.shutdown();
}

#[test]
fn release_answers_empty_and_is_recorded() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    assert_eq!(c.result("release", json!(null)), json!({}));
    let r = c.result("status", json!(null));
    assert_eq!(r["counters"]["releases"], 1);
    assert_eq!(r["last_release"]["reason"], "forced");
    assert!(r["last_release"]["ago_ms"].as_u64().unwrap() < 5000);
    d.shutdown();
}

#[test]
fn trust_pins_persists_and_lists_the_peer() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result(
        "trust",
        json!({"fingerprint": fpx(0x9c), "name": "laptop", "addr": "192.0.2.11"}),
    );
    assert_eq!(
        r,
        json!({"peer": {"name": "laptop", "fingerprint": fpx(0x9c)}})
    );

    let file = env.paths().trusted_file();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(&fpx(0x9c)) && text.contains("laptop") && text.contains("192.0.2.11"));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );

    let st = c.result("status", json!(null));
    let peer = &st["peers"][0];
    assert_eq!(peer["name"], "laptop");
    assert_eq!(peer["fingerprint"], fpx(0x9c));
    // an address was given, so ferryd dials; nothing listens there, so it is not connected
    assert_ne!(peer["state"], "connected");
    // typed without a port, shown with the one it is dialled on, like a learned one
    assert_eq!(peer["addr"], "192.0.2.11:4243");
    let peers = c.result("peers", json!(null));
    assert_eq!(peers["peers"][0]["addr"], "192.0.2.11:4243");
    d.shutdown();

    // it survives a restart
    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let st = c.result("status", json!(null));
    assert_eq!(st["peers"][0]["name"], "laptop");
    d.shutdown();
}

/// `trust` with an address awaits net twice between inserting the link and reading it
/// back, and an `unpair` of the same peer can run on ctl in between. It used to panic ctl at
/// `expect("inserted above")` (416 tries in the review's scratch test).
#[test]
fn trust_racing_unpair_never_panics_ctl() {
    let env = Env::new();
    let d = start(&env);
    let path = d.socket_path().to_path_buf();
    let fp = fpx(0x9c);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut threads = Vec::new();
    for unpair in [false, true] {
        let (path, fp, stop) = (path.clone(), fp.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            let mut c = Client::connect(&path);
            let mut tries = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                tries += 1;
                // either answer is fine; a dead socket is not (the client panics on it)
                if unpair {
                    c.call("unpair", json!({"peer": fp}));
                } else {
                    let r = c.call(
                        "trust",
                        json!({"fingerprint": fp, "name": "laptop", "addr": "127.0.0.1:9"}),
                    );
                    if let Some(e) = r.get("error") {
                        assert_eq!(e["code"], "unknown_peer", "{r}");
                    }
                }
            }
            tries
        }));
    }
    std::thread::sleep(Duration::from_secs(3));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for t in threads {
        assert!(t.join().expect("a client saw the socket die") > 0);
    }
    // ctl is alive
    let mut c = Client::connect(&path);
    c.result("status", json!(null));
    d.shutdown();
}

/// A client that pipelines requests and never reads used to grow the connection's reply
/// queue without bound. The queue is bounded now, and the connection is closed when it fills.
#[test]
fn a_client_that_never_reads_is_closed() {
    use std::io::BufRead;

    let env = Env::new();
    let d = start(&env);
    let stream = UnixStream::connect(d.socket_path()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    // 10,000 `status` requests, in one burst on another thread: once ferryd closes the
    // connection the writes fail, which is fine. The test reads nothing until they are done.
    let burst = std::thread::spawn(move || {
        let request = b"{\"id\":1,\"method\":\"status\",\"params\":{}}\n";
        for _ in 0..10_000 {
            if writer.write_all(request).is_err() {
                return false;
            }
        }
        true
    });
    let all_written = burst.join().unwrap();
    // Read the connection to its end: it must end, and with far fewer than 10,000 replies.
    let mut replies = 0usize;
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => replies += 1,
        }
    }
    assert!(
        replies < 10_000,
        "all {replies} replies were queued for a client that did not read (all written: {all_written})"
    );
    // ferryd serves the next client as before
    let mut c = Client::connect(d.socket_path());
    c.result("id", json!(null));
    d.shutdown();
}

#[test]
fn trust_is_idempotent_and_updates_name_and_address() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let first = c.result("trust", json!({"fingerprint": fpx(1), "name": "a"}));
    let again = c.result("trust", json!({"fingerprint": fpx(1), "name": "a"}));
    assert_eq!(first, again);
    // an omitted name keeps the stored one
    let kept = c.result("trust", json!({"fingerprint": fpx(1), "addr": "10.0.0.9"}));
    assert_eq!(kept["peer"]["name"], "a");
    let renamed = c.result("trust", json!({"fingerprint": fpx(1), "name": "b"}));
    assert_eq!(renamed["peer"]["name"], "b");
    let st = c.result("status", json!(null));
    assert_eq!(st["peers"].as_array().unwrap().len(), 1);
    let text = std::fs::read_to_string(env.paths().trusted_file()).unwrap();
    assert_eq!(text.matches("[[peer]]").count(), 1);
    assert!(text.contains("10.0.0.9"));
    d.shutdown();
}

#[test]
fn trust_accepts_the_pasted_forms_of_a_fingerprint() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let fp = fpx(0xab);
    let upper = fp.to_uppercase();
    let colons = upper
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(":");
    for form in [
        upper.clone(),
        colons,
        format!("SHA256:{upper}"),
        format!("  {fp}  "),
    ] {
        let r = c.result("trust", json!({"fingerprint": form}));
        assert_eq!(r["peer"]["fingerprint"], fp);
    }
    let st = c.result("status", json!(null));
    assert_eq!(st["peers"].as_array().unwrap().len(), 1);
    assert_eq!(st["peers"][0]["name"], "peer-abababab");
    d.shutdown();
}

#[test]
fn trust_errors() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());

    let (code, _, hint) = c.error("trust", json!({"fingerprint": "nonsense"}));
    assert_eq!(code, "bad_fingerprint");
    assert!(hint.is_some());
    assert_eq!(
        c.error("trust", json!({"fingerprint": "ab"})).0,
        "bad_fingerprint"
    );
    assert_eq!(
        c.error("trust", json!({"fingerprint": "g".repeat(64)})).0,
        "bad_fingerprint"
    );

    let own = d.fingerprint().to_string();
    assert_eq!(
        c.error("trust", json!({"fingerprint": own})).0,
        "self_fingerprint"
    );

    assert_eq!(c.error("trust", json!({})).0, "bad_params");
    assert_eq!(c.error("trust", json!(null)).0, "bad_params");
    assert_eq!(c.error("trust", json!({"fingerprint": 5})).0, "bad_params");
    assert_eq!(
        c.error("trust", json!({"fingerprint": fpx(1), "name": "a b"}))
            .0,
        "bad_params"
    );
    assert_eq!(
        c.error("trust", json!({"fingerprint": fpx(1), "name": ""}))
            .0,
        "bad_params"
    );
    assert_eq!(
        c.error(
            "trust",
            json!({"fingerprint": fpx(1), "addr": "10.0.0.1:99999"})
        )
        .0,
        "bad_params"
    );
    assert_eq!(
        c.error(
            "trust",
            json!({"fingerprint": fpx(1), "addr": "not an addr"})
        )
        .0,
        "bad_params"
    );

    c.result("trust", json!({"fingerprint": fpx(1), "name": "a"}));
    let (code, msg, _) = c.error("trust", json!({"fingerprint": fpx(2), "name": "a"}));
    assert_eq!(code, "bad_params");
    assert!(msg.contains(&fpx(1)), "{msg}");

    // none of the failures pinned anything
    let st = c.result("status", json!(null));
    assert_eq!(st["peers"].as_array().unwrap().len(), 1);
    d.shutdown();
}

#[test]
fn a_failed_write_is_an_internal_error_and_pins_nothing() {
    if rustix::process::getuid().is_root() {
        return; // root writes anywhere
    }
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let dir = env.paths().data_dir;
    // an ephemeral identity creates no data dir; make a private one, then take away write access
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (code, _, hint) = c.error("trust", json!({"fingerprint": fpx(1), "name": "a"}));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, "internal");
    assert!(hint.is_some());
    let st = c.result("status", json!(null));
    assert!(st["peers"].as_array().unwrap().is_empty());
    d.shutdown();
}

#[test]
fn layout_get_of_a_fresh_daemon_lists_my_monitors_unplaced() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let r = c.result("layout.get", json!(null));
    let masked = mask(&r, &[(&d.fingerprint().to_string(), "<fp>")]);
    insta::assert_json_snapshot!(masked);
    d.shutdown();
}

/// Writes a `layout.toml` that already knows two peers' monitors, as if they had connected.
fn seed_known_peers(env: &Env) {
    let path = env.paths().layout_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let monitor = |name: &str, w: u32, h: u32| {
        format!(
            "[[known.monitor]]\nname = \"{name}\"\ndescription = \"\"\nscale = 1.0\ntransform = 0\n\
             [known.monitor.rect]\nx = 0\ny = 0\nw = {w}\nh = {h}\n"
        )
    };
    let text = format!(
        "[[known]]\npeer = \"{}\"\n{}\n[[known]]\npeer = \"{}\"\n{}\n",
        fpx(0x9c),
        monitor("eDP-1", 1366, 768),
        fpx(0x9d),
        monitor("HDMI-A-1", 1280, 720),
    );
    std::fs::write(path, text).unwrap();
}

#[test]
fn layout_get_shows_known_monitors_of_trusted_peers() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "laptop"}));
    let r = c.result("layout.get", json!(null));
    let peers = r["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[1]["name"], "laptop");
    assert_eq!(peers[1]["origin"], Value::Null);
    assert_eq!(peers[1]["online"], false);
    assert_eq!(peers[1]["monitors"][0]["name"], "eDP-1");
    assert_eq!(peers[1]["monitors"][0]["w"], 1366);
    assert_eq!(r["links"], json!([]));
    d.shutdown();
}

#[test]
fn layout_place_errors_that_come_before_the_geometry() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "laptop"}));
    // same eight-digit prefix as 9c9c9c9c…, so a prefix of 8 is ambiguous
    let sibling = format!("9c9c9c9c{}", "ab".repeat(28));
    c.result("trust", json!({"fingerprint": sibling, "name": "sibling"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "other"}));
    c.result("trust", json!({"fingerprint": fpx(0x42), "name": "silent"}));

    let place = |peer: &str, anchor: &str| json!({"peer": peer, "rel": "below", "anchor": anchor, "align": "center"});

    assert_eq!(
        c.error("layout.place", place("nobody", "HDMI-A-1")).0,
        "unknown_peer"
    );
    assert_eq!(
        c.error("layout.place", place("9c", "HDMI-A-1")).0,
        "unknown_peer"
    ); // under 8 digits
    let (code, msg, _) = c.error("layout.place", place("9c9c9c9c", "HDMI-A-1"));
    assert_eq!(code, "ambiguous_peer");
    assert!(msg.contains("laptop") && msg.contains("sibling"), "{msg}");

    assert_eq!(
        c.error("layout.place", place("laptop", "nowhere")).0,
        "unknown_anchor"
    );
    assert_eq!(
        c.error("layout.place", place("laptop", "desk:DP-9")).0,
        "unknown_anchor"
    );
    assert_eq!(
        c.error("layout.place", place("laptop", "ghost:HDMI-A-1")).0,
        "unknown_anchor"
    );
    // an ambiguous fingerprint prefix as the anchor
    let (code, msg, _) = c.error("layout.place", place("silent", "9c9c9c9c"));
    assert_eq!(code, "ambiguous_anchor");
    assert!(msg.contains("laptop") && msg.contains("sibling"), "{msg}");

    // a peer that never connected has no monitors
    let (code, msg, hint) = c.error("layout.place", place("silent", "HDMI-A-1"));
    assert_eq!(code, "peer_monitors_unknown");
    assert!(msg.contains("silent"), "{msg}");
    assert!(hint.is_some());

    // this machine is the reference of the layout
    assert_eq!(
        c.error("layout.place", place("desk", "laptop")).0,
        "bad_params"
    );

    // bad shapes
    assert_eq!(
        c.error("layout.place", json!({"peer": "laptop"})).0,
        "bad_params"
    );
    assert_eq!(
        c.error(
            "layout.place",
            json!({"peer": "laptop", "rel": "diagonal", "anchor": "HDMI-A-1"})
        )
        .0,
        "bad_params"
    );
    assert_eq!(
        c.error(
            "layout.place",
            json!({"peer": "laptop", "rel": "below", "anchor": "HDMI-A-1", "align": "middle"})
        )
        .0,
        "bad_params"
    );

    // no layout was saved: no revision was spent and nothing is placed (the file may exist, since
    // the daemon saves its monitor memory at start)
    assert_eq!(
        c.result("layout.get", json!(null))["revision"]["counter"],
        0
    );
    assert!(
        !std::fs::read_to_string(env.paths().layout_file())
            .unwrap()
            .contains("[[origin]]")
    );
    d.shutdown();
}

#[test]
fn a_bare_monitor_name_on_two_other_peers_is_ambiguous() {
    let env = Env::new();
    seed_known_peers(&env);
    // both seeded peers get a monitor called DP-7; this peer has none
    let path = env.paths().layout_file();
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("eDP-1", "DP-7")
        .replace("HDMI-A-1", "DP-7");
    std::fs::write(&path, text).unwrap();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "p1"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "p2"}));
    let (code, msg, hint) = c.error(
        "layout.place",
        json!({"peer": "p1", "rel": "below", "anchor": "DP-7"}),
    );
    assert_eq!(code, "ambiguous_anchor");
    assert!(msg.contains("p1") && msg.contains("p2"), "{msg}");
    assert_eq!(hint.as_deref(), Some("write <peer>:<monitor>"));
    d.shutdown();
}

#[test]
fn layout_place_below_center_matches_the_first_light_numbers() {
    // Desk HDMI-A-1 1920x1080 at 0,0, netbook eDP-1 1366x768, --below HDMI-A-1
    // --align center puts the netbook at (277, 1080), revision 1, link span [277, 1643).
    let env = Env::new();
    let path = env.paths().layout_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "[[known]]\npeer = \"{}\"\n[[known.monitor]]\nname = \"eDP-1\"\ndescription = \"\"\nscale = 1.0\ntransform = 0\n[known.monitor.rect]\nx = 0\ny = 0\nw = 1366\nh = 768\n",
            fpx(0x9c)
        ),
    )
    .unwrap();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "laptop"}));
    let r = c.result(
        "layout.place",
        json!({"peer": "laptop", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    assert_eq!(r["revision"]["counter"], 1);
    assert_eq!(r["peers"][1]["origin"], json!([277, 1080]));
    assert_eq!(r["links"][0]["span"], json!([277, 1643]));
    // persisted, and read back after a restart
    let again = c.result("layout.get", json!(null));
    assert_eq!(again, r);
    d.shutdown();
    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    assert_eq!(
        c.result("layout.get", json!(null))["revision"]["counter"],
        1
    );
    d.shutdown();
}

#[test]
fn layout_place_overlap_is_refused_and_leaves_the_layout() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "b"}));
    c.result(
        "layout.place",
        json!({"peer": "a", "rel": "below", "anchor": "HDMI-A-1"}),
    );
    let before = c.result("layout.get", json!(null));
    // b below the same edge at the same place overlaps a
    let (code, _, _) = c.error(
        "layout.place",
        json!({"peer": "b", "rel": "below", "anchor": "HDMI-A-1"}),
    );
    assert_eq!(code, "overlap");
    assert_eq!(c.result("layout.get", json!(null)), before);
    d.shutdown();
}

#[test]
fn layout_place_with_an_unplaced_anchor_peer_is_unknown_anchor() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "b"}));
    let (code, _, _) = c.error(
        "layout.place",
        json!({"peer": "a", "rel": "left_of", "anchor": "b"}),
    );
    assert_eq!(code, "unknown_anchor");
    d.shutdown();
}

#[test]
fn layout_get_derives_links_from_the_placement() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    let r = c.result(
        "layout.place",
        json!({"peer": "a", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    let links = r["links"].as_array().unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0]["from"], "desk:HDMI-A-1");
    assert_eq!(links[0]["side"], "bottom");
    assert_eq!(links[0]["to"], "a:eDP-1");
    assert_eq!(links[0]["live"], false); // a is not connected
    d.shutdown();
}

/// The error of a call that must fail, whole: `{code, message, hint?}` as the socket sends it.
fn error_body(c: &mut Client, method: &str, params: Value) -> Value {
    let resp = c.call(method, params);
    resp.get("error")
        .unwrap_or_else(|| panic!("{method} should fail: {resp}"))
        .clone()
}

/// `pin` two peers whose monitors the daemon knows (`seed_known_peers`): `a` has one 1366x768
/// monitor, `b` one 1280x720 monitor.
fn pinned_a_and_b(env: &Env) -> (ferryd::Daemon, Client) {
    seed_known_peers(env);
    let d = start(env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "b"}));
    (d, c)
}

#[test]
fn layout_set_moves_several_groups_in_one_revision() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    let r = c.result(
        "layout.set",
        json!({"origins": [
            {"peer": "a", "x": 0, "y": 1080},
            {"peer": "b", "x": 1400, "y": 1080},
        ]}),
    );
    // one revision for the batch, and this machine took the origin (0, 0)
    assert_eq!(r["revision"]["counter"], 1);
    assert_eq!(r["revision"]["peer"], d.fingerprint().to_string());
    let origins: Vec<&Value> = r["peers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["origin"])
        .collect();
    assert_eq!(
        origins,
        [&json!([0, 0]), &json!([0, 1080]), &json!([1400, 1080])]
    );
    // the links follow: desk's two monitors sit over the two groups
    let links = r["links"].as_array().unwrap();
    assert!(
        links
            .iter()
            .any(|l| l["from"] == "desk:HDMI-A-1" && l["to"] == "a:eDP-1")
    );
    assert!(
        links
            .iter()
            .any(|l| l["from"] == "desk:DVI-D-1" && l["to"] == "b:HDMI-A-1")
    );
    // one group as a frontend gets it, with the fields M3 added
    insta::assert_json_snapshot!(
        "layout_set_result",
        mask(
            &json!({"revision": r["revision"], "peer": r["peers"][1]}),
            &[(&d.fingerprint().to_string(), "<fp>")]
        )
    );

    // the answer is what layout.get says, and it survives a restart
    assert_eq!(c.result("layout.get", json!(null)), r);
    d.shutdown();
    let d = Daemon::start(options(&env)).unwrap();
    let mut c = Client::connect(d.socket_path());
    let again = c.result("layout.get", json!(null));
    assert_eq!(again["revision"]["counter"], 1);
    assert_eq!(again["peers"][2]["origin"], json!([1400, 1080]));

    // peers that are not named keep their origins; the next edit is the next revision
    let r = c.result(
        "layout.set",
        json!({"origins": [{"peer": "b", "x": 1366, "y": 1080}]}),
    );
    assert_eq!(r["revision"]["counter"], 2);
    assert_eq!(r["peers"][1]["origin"], json!([0, 1080]));
    assert_eq!(r["peers"][2]["origin"], json!([1366, 1080]));
    d.shutdown();
}

#[test]
fn layout_set_moves_this_machine_too() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    let r = c.result(
        "layout.set",
        json!({"origins": [
            {"peer": "desk", "x": -3840, "y": 0},
            {"peer": "a", "x": 0, "y": 0},
        ]}),
    );
    assert_eq!(r["peers"][0]["origin"], json!([-3840, 0]));
    // desk's right edge (x = 0) touches a's left edge: a link both ways
    let links = r["links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[0]["from"], "desk:DVI-D-1");
    assert_eq!(links[0]["side"], "right");
    d.shutdown();
}

#[test]
fn layout_set_refuses_an_overlap_and_names_both_peers() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    // Onto desk's HDMI-A-1
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 100, "y": 100}]}),
    );
    assert_eq!(e["code"], "overlap");
    assert_eq!(e["message"], "a overlaps desk");
    assert!(e["hint"].is_string());
    insta::assert_json_snapshot!("layout_set_overlap_error", e);
    // two groups of one batch that only overlap each other at their new places
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 6000, "y": 0}, {"peer": "b", "x": 6500, "y": 100}]}),
    );
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str()),
        (Some("overlap"), Some("a overlaps b"))
    );
    // nothing was spent or saved
    let v = c.result("layout.get", json!(null));
    assert_eq!(v["revision"]["counter"], 0);
    assert!(
        v["peers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["name"] == "desk" || p["origin"].is_null())
    );
    d.shutdown();
}

#[test]
fn layout_set_answers_stale_layout_when_the_base_revision_is_not_current() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    let first = c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 1080}]}),
    );
    let rev1 = first["revision"].clone();
    // another edit lands (as a push or a CLI call would): revision 2
    c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 10, "y": 1080}]}),
    );

    // a draft from revision 1 is stale; nothing changes
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "b", "x": 1400, "y": 1080}], "base_rev": rev1}),
    );
    assert_eq!(e["code"], "stale_layout");
    assert_eq!(e["message"], "the layout changed (rev 2 by desk)");
    assert!(e["hint"].is_string());
    insta::assert_json_snapshot!("layout_set_stale_error", e);
    let now = c.result("layout.get", json!(null));
    assert_eq!(now["revision"]["counter"], 2);
    assert_eq!(now["peers"][2]["origin"], Value::Null, "b was not placed");

    // the same revision with a different peer is stale too
    let other = json!({"counter": 2, "peer": fpx(0x9c)});
    assert_eq!(
        error_body(
            &mut c,
            "layout.set",
            json!({"origins": [{"peer": "b", "x": 0, "y": 5000}], "base_rev": other})
        )["code"],
        "stale_layout"
    );

    // "Keep mine": resent with the current revision it applies
    let r = c.result(
        "layout.set",
        json!({"origins": [{"peer": "b", "x": 1400, "y": 1080}], "base_rev": now["revision"]}),
    );
    assert_eq!(r["revision"]["counter"], 3);
    assert_eq!(r["peers"][2]["origin"], json!([1400, 1080]));

    // staleness is judged before geometry: a stale overlapping draft is stale
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 0}], "base_rev": rev1}),
    );
    assert_eq!(e["code"], "stale_layout");
    // and a bad base revision is a bad param
    assert_eq!(
        error_body(
            &mut c,
            "layout.set",
            json!({"origins": [{"peer": "a", "x": 0, "y": 1080}], "base_rev": {"counter": 1, "peer": "nope"}})
        )["code"],
        "bad_params"
    );
    d.shutdown();
}

#[test]
fn layout_set_refuses_bad_params_before_it_looks_at_the_geometry() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    c.result("trust", json!({"fingerprint": fpx(0x42), "name": "silent"}));
    let code = |c: &mut Client, params: Value| {
        let e = error_body(c, "layout.set", params);
        (
            e["code"].as_str().unwrap().to_string(),
            e["message"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(code(&mut c, json!({"origins": []})).0, "bad_params");
    assert_eq!(code(&mut c, json!({})).0, "bad_params");
    assert_eq!(
        code(
            &mut c,
            json!({"origins": [{"peer": "nobody", "x": 0, "y": 0}]})
        )
        .0,
        "unknown_peer"
    );
    // a peer twice, by name and by a fingerprint prefix
    let (c1, m1) = code(
        &mut c,
        json!({"origins": [{"peer": "a", "x": 0, "y": 1080}, {"peer": "9c9c9c9c", "x": 5, "y": 1080}]}),
    );
    assert_eq!(c1, "bad_params");
    assert!(m1.contains("a is named twice"), "{m1}");
    // far past the cap
    let (c2, m2) = code(
        &mut c,
        json!({"origins": [{"peer": "a", "x": 16_777_217, "y": 0}]}),
    );
    assert_eq!(c2, "bad_params");
    assert!(m2.contains("16777216"), "{m2}");
    // a peer that never connected has no monitors to place
    let (c3, m3) = code(
        &mut c,
        json!({"origins": [{"peer": "silent", "x": 0, "y": 1080}]}),
    );
    assert_eq!(c3, "peer_monitors_unknown");
    assert!(m3.contains("silent"), "{m3}");
    // a coordinate that is not a whole number is a malformed request
    assert_eq!(
        code(
            &mut c,
            json!({"origins": [{"peer": "a", "x": 1.5, "y": 0}]})
        )
        .0,
        "bad_params"
    );
    // none of it spent a revision
    assert_eq!(
        c.result("layout.get", json!(null))["revision"]["counter"],
        0
    );
    d.shutdown();
}

#[test]
fn layout_set_with_nothing_to_change_spends_no_revision() {
    let env = Env::new();
    let (d, mut c) = pinned_a_and_b(&env);
    let first = c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 1080}]}),
    );
    assert_eq!(first["revision"]["counter"], 1);
    // the same origin again: the same layout, the same revision
    let again = c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 1080}]}),
    );
    assert_eq!(again, first);
    // desk's own origin is (0, 0) already
    let me = c.result(
        "layout.set",
        json!({"origins": [{"peer": "desk", "x": 0, "y": 0}]}),
    );
    assert_eq!(me["revision"]["counter"], 1);
    d.shutdown();
}

#[test]
fn layout_set_refuses_a_fifth_peer_and_place_does_too() {
    let env = Env::new();
    seed_known_peers(&env);
    // two more peers that are already placed, as another peer's layout would have them
    let path = env.paths().layout_file();
    let mut text = std::fs::read_to_string(&path).unwrap();
    for n in [0x91u8, 0x92, 0x93] {
        text.push_str(&format!(
            "[[origin]]\npeer = \"{}\"\nx = {}\ny = 9000\n",
            fpx(n),
            i32::from(n) * 5000
        ));
    }
    std::fs::write(&path, text).unwrap();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    // three origins already; desk and `a` would make five
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 1080}]}),
    );
    assert_eq!(e["code"], "bad_params");
    assert!(
        e["message"].as_str().unwrap().contains("at most 4 peers"),
        "{e}"
    );
    let e = error_body(
        &mut c,
        "layout.place",
        json!({"peer": "a", "rel": "below", "anchor": "HDMI-A-1"}),
    );
    assert_eq!(e["code"], "bad_params");
    d.shutdown();
}

/// A third peer another peer placed: listed as not trusted, counted in the overlap
/// check, and movable by its fingerprint.
#[test]
fn a_placed_peer_that_is_not_paired_here_is_listed_untrusted_and_can_be_moved() {
    let env = Env::new();
    seed_known_peers(&env); // 0x9d is known but is never pinned in this test
    let path = env.paths().layout_file();
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!(
        "[revision]\ncounter = 4\npeer = \"{}\"\n\n[[origin]]\npeer = \"{}\"\nx = 3840\ny = 0\n",
        fpx(0x9d),
        fpx(0x9d)
    ));
    std::fs::write(&path, text).unwrap();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    let v = c.result("layout.get", json!(null));
    let peers = v["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 3);
    let flags = |i: usize| (peers[i]["trusted"].clone(), peers[i]["self"].clone());
    assert_eq!(flags(0), (json!(true), json!(true)));
    assert_eq!(flags(1), (json!(true), json!(false)));
    assert_eq!(flags(2), (json!(false), json!(false)));
    assert_eq!(peers[2]["fingerprint"], fpx(0x9d));
    assert_eq!(peers[2]["name"], "9d9d9d9d");
    assert_eq!(peers[2]["origin"], json!([3840, 0]));
    assert_eq!(peers[2]["online"], false);
    assert_eq!(
        peers[2]["monitors"][0]["name"], "HDMI-A-1",
        "its monitors were reported once"
    );
    assert_eq!(
        peers[2]["bounds"],
        json!({"x": 3840, "y": 0, "w": 1280, "h": 720})
    );
    assert_eq!(
        v["links"],
        json!([]),
        "no edge link to a peer that is not trusted"
    );

    // `a` cannot go where the untrusted peer is
    let e = error_body(
        &mut c,
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 3900, "y": 100}]}),
    );
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str()),
        (Some("overlap"), Some("a overlaps 9d9d9d9d"))
    );
    // and the untrusted peer can be moved, by its fingerprint
    let r = c.result(
        "layout.set",
        json!({"origins": [{"peer": fpx(0x9d), "x": 0, "y": 1080}], "base_rev": v["revision"]}),
    );
    assert_eq!(r["revision"]["counter"], 5);
    assert_eq!(r["peers"][2]["origin"], json!([0, 1080]));
    // a prefix of 8 digits finds it too, and an unknown prefix is still unknown
    c.result(
        "layout.set",
        json!({"origins": [{"peer": "9d9d9d9d", "x": 0, "y": 1100}]}),
    );
    assert_eq!(
        error_body(
            &mut c,
            "layout.set",
            json!({"origins": [{"peer": "abababab", "x": 0, "y": 0}]})
        )["code"],
        "unknown_peer"
    );
    d.shutdown();
}

#[test]
fn unknown_methods_and_malformed_lines() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());

    let (code, msg, _) = c.error("frobnicate", json!(null));
    assert_eq!(code, "unknown_method");
    assert!(msg.contains("frobnicate"));
    // a method of a later api version
    assert_eq!(c.error("discover.list", json!(null)).0, "unknown_method");

    let r = c.raw(b"this is not json\n");
    assert_eq!(r["error"]["code"], "bad_params");
    assert_eq!(r["id"], 0);
    let r = c.raw(b"{\"method\":\"status\"}\n");
    assert_eq!(r["error"]["code"], "bad_params");
    let r = c.raw(b"{\"id\":9}\n");
    assert_eq!(r["error"]["code"], "bad_params");
    assert_eq!(r["id"], 9);
    // blank lines are skipped, and the connection still works
    let r = c.raw(b"\n\n{\"id\":10,\"method\":\"id\"}\n");
    assert_eq!(r["id"], 10);
    assert!(r["result"].is_object());
    d.shutdown();
}

#[test]
fn unknown_params_are_ignored() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("status", json!({"future": [1, 2, 3]}));
    c.result("id", json!({"future": true}));
    let r = c.result(
        "trust",
        json!({"fingerprint": fpx(1), "name": "a", "future_field": "x"}),
    );
    assert_eq!(r["peer"]["name"], "a");
    d.shutdown();
}

#[test]
fn requests_are_answered_in_order_and_clients_are_independent() {
    let env = Env::new();
    let d = start(&env);
    let mut a = Client::connect(d.socket_path());
    let mut b = Client::connect(d.socket_path());
    let r = a.raw(b"{\"id\":1,\"method\":\"id\"}\n{\"id\":2,\"method\":\"status\"}\n");
    assert_eq!(r["id"], 1);
    assert_eq!(a.read_line()["id"], 2);
    a.result("trust", json!({"fingerprint": fpx(3), "name": "c"}));
    // the other client sees the same daemon state
    let st = b.result("status", json!(null));
    assert_eq!(st["peers"][0]["name"], "c");
    d.shutdown();
}

#[test]
fn an_oversized_line_closes_the_connection() {
    let env = Env::new();
    let d = start(&env);
    let mut s = UnixStream::connect(d.socket_path()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let big = vec![b'x'; ferryd::server::MAX_LINE + 10];
    // the daemon may close while we are still writing
    let _ = s.write_all(&big);
    let mut text = String::new();
    let _ = s.read_to_string(&mut text);
    assert!(text.contains("request line too long"), "{text:?}");
    // the daemon is still fine
    let mut c = Client::connect(d.socket_path());
    c.result("id", json!(null));
    d.shutdown();
}

/// The request line reader: whatever bytes a local client writes, every non-blank
/// line gets exactly one answer, in order, and the connection still serves the next request.
#[test]
fn arbitrary_bytes_on_the_socket_get_one_answer_per_line() {
    use proptest::prelude::*;

    let env = Env::new();
    let d = start(&env);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(64));
    runner
        .run(
            &proptest::collection::vec(any::<u8>(), 0..300),
            |mut bytes| {
                // a `\n` inside would make it two lines; count the lines as the daemon will
                bytes.push(b'\n');
                let answers = bytes
                    .split(|&b| b == b'\n')
                    .filter(|l| !l.trim_ascii().is_empty())
                    .count();
                let mut c = Client::connect(d.socket_path());
                c.writer_for_test().write_all(&bytes).unwrap();
                c.writer_for_test()
                    .write_all(b"{\"id\":7,\"method\":\"id\"}\n")
                    .unwrap();
                for _ in 0..answers {
                    let r = c.read_line();
                    prop_assert!(r.get("id").is_some(), "{r}");
                }
                let last = c.read_line();
                prop_assert_eq!(&last["id"], &json!(7));
                prop_assert!(last["result"].is_object(), "{last}");
                Ok(())
            },
        )
        .unwrap();
    d.shutdown();
}

#[test]
fn a_client_of_another_uid_gets_no_hello_and_is_dropped() {
    let env = Env::new();
    let mut o = options(&env);
    // pretend the owner is someone else, which makes this (the only possible) client a stranger
    o.socket_uid = Some(rustix::process::getuid().as_raw().wrapping_add(1));
    let d = Daemon::start(o).unwrap();
    let mut s = UnixStream::connect(d.socket_path()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = Vec::new();
    let n = s.read_to_end(&mut buf).unwrap();
    assert_eq!(n, 0, "a stranger must not even get the hello line: {buf:?}");
    d.shutdown();
}

#[test]
fn the_uid_rule() {
    assert!(ferryd::server::uid_allowed(1000, 1000));
    assert!(!ferryd::server::uid_allowed(0, 1000));
    assert!(!ferryd::server::uid_allowed(1001, 1000));
}

#[test]
fn a_second_daemon_refuses_while_the_socket_is_live() {
    let env = Env::new();
    let d = start(&env);
    let e = Daemon::start(options(&env)).err().expect("must refuse");
    assert!(matches!(e, StartError::AlreadyRunning(_)), "{e}");
    assert!(e.to_string().contains("already running"));
    // the first one is untouched
    let mut c = Client::connect(d.socket_path());
    c.result("id", json!(null));
    d.shutdown();
}

/// A second ferryd on the state of a running one is refused before it writes anything: its
/// monitor memory (it sees other monitors here) must not land over the layout file of the first.
#[test]
fn a_refused_second_daemon_leaves_the_layout_file_alone() {
    let env = Env::new();
    let d = start(&env);
    let file = env.paths().layout_file();
    let first = std::fs::read(&file).expect("the first daemon saved its monitor memory");
    let mut o = options(&env);
    o.monitors = MonitorSource::Fixed(vec![mon("DP-9", 0, 0, 800, 600)]);
    let e = Daemon::start(o).err().expect("must refuse");
    assert!(matches!(e, StartError::AlreadyRunning(_)), "{e}");
    assert_eq!(std::fs::read(&file).unwrap(), first);
    d.shutdown();
}

#[test]
fn a_stale_socket_is_replaced() {
    let env = Env::new();
    let dir = env.paths().socket_dir();
    std::fs::create_dir_all(&dir).unwrap();
    // bind and drop: the file stays, nothing answers
    drop(std::os::unix::net::UnixListener::bind(env.socket()).unwrap());
    assert!(env.socket().exists());
    let d = start(&env);
    Client::connect(d.socket_path()).result("id", json!(null));
    d.shutdown();
}

#[test]
fn something_that_is_not_a_socket_is_never_deleted() {
    let env = Env::new();
    std::fs::create_dir_all(env.paths().socket_dir()).unwrap();
    std::fs::write(env.socket(), "precious").unwrap();
    let e = Daemon::start(options(&env)).err().expect("must refuse");
    assert!(matches!(e, StartError::Socket { .. }), "{e}");
    assert_eq!(std::fs::read_to_string(env.socket()).unwrap(), "precious");
}

#[test]
fn shutdown_removes_the_socket_and_closes_clients() {
    let env = Env::new();
    let d = start(&env);
    let stream = UnixStream::connect(d.socket_path()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut hello = String::new();
    std::io::BufRead::read_line(&mut reader, &mut hello).unwrap();
    let path = d.socket_path().to_path_buf();
    d.shutdown();
    assert!(!path.exists());
    // the daemon is gone, so the client's next read ends
    let mut rest = String::new();
    assert_eq!(
        std::io::BufRead::read_line(&mut reader, &mut rest).unwrap(),
        0
    );
    assert!(UnixStream::connect(&path).is_err());
}

#[test]
fn start_creates_the_identity_with_private_modes() {
    let env = Env::new();
    let mut o = options(&env);
    o.identity = None; // load_or_create under HOME/.local/share/ferry
    let d = Daemon::start(o).unwrap();
    let data = env.paths().data_dir;
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(data.clone()), 0o700);
    assert_eq!(mode(data.join("key.der")), 0o600);
    assert_eq!(mode(data.join("cert.der")), 0o600);
    let fp = d.fingerprint();
    d.shutdown();
    // the same identity after a restart
    let mut o = options(&env);
    o.identity = None;
    let d = Daemon::start(o).unwrap();
    assert_eq!(d.fingerprint(), fp);
    d.shutdown();
}

#[test]
fn corrupted_stores_stop_start_up_with_a_message_naming_the_file() {
    for (file, content) in [
        ("trusted", "[[peer\n"),
        ("layout", "[revision\n"),
        ("config", "[peer\nport =\n"),
    ] {
        let env = Env::new();
        let p = env.paths();
        let path = match file {
            "trusted" => p.trusted_file(),
            "layout" => p.layout_file(),
            _ => p.config_file.clone(),
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if file == "trusted" {
            // keep the data dir private so the identity check is not what fails
            std::fs::set_permissions(
                path.parent().unwrap(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        std::fs::write(&path, content).unwrap();
        let e = Daemon::start(options(&env))
            .err()
            .unwrap_or_else(|| panic!("{file} should stop start-up"));
        let text = e.to_string();
        assert!(text.contains(path.to_str().unwrap()), "{text}");
        assert!(text.contains("corrupted"), "{text}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            content,
            "{file} must not be overwritten"
        );
        assert!(!env.socket().exists(), "no socket is left behind");
    }
}
