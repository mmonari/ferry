//! End to end, on the host, without touching input: two ferryd instances, each with its own
//! temp HOME and XDG_RUNTIME_DIR and its own loopback port, driven only through the real `ferry`
//! binary. Pins, TLS handshakes and the named-fingerprint failure are all real.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use serde_json::Value;
use support::*;

fn peer(status: &Value, fp: &str) -> Option<Value> {
    status["peers"]
        .as_array()?
        .iter()
        .find(|p| p["fingerprint"] == fp)
        .cloned()
}

#[test]
fn two_daemons_pair_through_the_cli() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (ea.daemon("alpha"), eb.daemon("beta"));
    assert_ne!(da.net_addr().port(), db.net_addr().port());

    // `ferry id` on each
    let ida = ea.ferry_json(&["id", "--json"]);
    let idb = eb.ferry_json(&["id", "--json"]);
    let (fa, fb) = (
        ida["fingerprint"].as_str().unwrap().to_string(),
        idb["fingerprint"].as_str().unwrap().to_string(),
    );
    assert_eq!(fa.len(), 64);
    assert_ne!(fa, fb);
    assert_eq!(ida["port"], da.net_addr().port());

    // The human `ferry id` prints the line to paste on the other machine. Run exactly that line
    // on beta, with the LAN address swapped for loopback (the test endpoints listen there).
    let human = text(&ea.ferry(&["id"]).stdout);
    let line = human
        .lines()
        .find(|l| l.trim_start().starts_with("ferry trust"))
        .unwrap()
        .trim()
        .to_string();
    assert!(
        line.starts_with(&format!("ferry trust {fa} --name alpha")),
        "{line}"
    );
    let mut args: Vec<String> = line
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect();
    let addr = format!("127.0.0.1:{}", da.net_addr().port());
    match args.iter().position(|a| a == "--addr") {
        Some(i) => args[i + 1] = addr,
        None => args.extend(["--addr".to_string(), addr]),
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    // beta pins alpha and dials it; alpha has not pinned beta yet
    let out = eb.ferry(&args);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("Trusted alpha"));

    // ...so the handshake fails, and alpha names the fingerprint that was turned away
    let st_a = wait_for("alpha to refuse beta by fingerprint", || {
        let st = ea.ferry_json(&["status", "--json"]);
        peer(&st, &fb)
            .filter(|p| p["state"] == "refused")
            .map(|_| st)
    });
    let refusal = peer(&st_a, &fb).unwrap();
    assert!(
        refusal["detail"].as_str().unwrap().contains(&fb),
        "the fingerprint is named: {refusal}"
    );
    let human = text(&ea.ferry(&["status"]).stdout);
    assert!(human.contains("refused") && human.contains(&fb), "{human}");
    // beta sees that alpha does not trust it
    let st_b = wait_for("beta to see the refusal", || {
        let st = eb.ferry_json(&["status", "--json"]);
        peer(&st, &fa)
            .filter(|p| p["state"] == "refused")
            .map(|_| st)
    });
    assert!(
        peer(&st_b, &fa).unwrap()["detail"]
            .as_str()
            .unwrap()
            .contains("does not trust")
    );

    // alpha pins beta, pasting the fingerprint in the shape a person might: upper case with colons
    let pasted: Vec<String> = fb.to_uppercase().as_bytes().chunks(2).map(text).collect();
    let out = ea.ferry(&["trust", &pasted.join(":"), "--name", "beta"]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    // now both pins are in: the handshake succeeds from beta's next retry
    wait_for("both sides connected", || {
        let a = ea.ferry_json(&["status", "--json"]);
        let b = eb.ferry_json(&["status", "--json"]);
        (peer(&a, &fb)?["state"] == "connected" && peer(&b, &fa)?["state"] == "connected")
            .then_some(())
    });
    let a = ea.ferry_json(&["status", "--json"]);
    let p = peer(&a, &fb).unwrap();
    assert_eq!(p["name"], "beta", "the refused stub became the named peer");
    assert_eq!(a["peers"].as_array().unwrap().len(), 1);
    assert!(p["rtt_us"].is_u64());
    // the layout knows the peer is online
    let layout = ea.ferry_json(&["layout", "show", "--json"]);
    assert_eq!(layout["peers"][1]["name"], "beta");
    assert_eq!(layout["peers"][1]["online"], true);

    // persisted with private modes
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(ea.paths().trusted_file()), 0o600);
    assert_eq!(mode(ea.paths().data_dir.join("key.der")), 0o600);
    assert_eq!(mode(ea.paths().data_dir.join("cert.der")), 0o600);
    assert_eq!(mode(ea.paths().data_dir), 0o700);

    // alpha stops; beta says so, and the CLI against alpha now says ferryd is not running
    da.shutdown();
    let out = ea.ferry(&["status"]);
    assert_eq!(out.status.code(), Some(3));
    wait_for("beta to see alpha leave", || {
        let b = eb.ferry_json(&["status", "--json"]);
        (peer(&b, &fa)?["state"] != "connected").then_some(())
    });
    db.shutdown();
}

#[test]
fn a_restarted_daemon_redials_its_pinned_peers_from_the_store() {
    let (ea, eb) = (Env::new(), Env::new());
    let da = ea.daemon("alpha");
    let mut db = eb.daemon("beta");
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());

    // beta dials alpha (it is given the address); alpha only accepts
    ea.ferry(&["trust", &fb, "--name", "beta"]);
    let addr = format!("127.0.0.1:{}", da.net_addr().port());
    assert!(
        eb.ferry(&["trust", &fa, "--name", "alpha", "--addr", &addr])
            .status
            .success()
    );
    wait_for("the first connection", || {
        let b = eb.ferry_json(&["status", "--json"]);
        (peer(&b, &fa)?["state"] == "connected").then_some(())
    });

    // beta restarts: same identity, new port; trusted.toml still has alpha's address
    db.shutdown();
    wait_for("alpha to see beta leave", || {
        let a = ea.ferry_json(&["status", "--json"]);
        (peer(&a, &fb)?["state"] != "connected").then_some(())
    });
    db = eb.daemon("beta");
    assert_eq!(
        db.fingerprint().to_string(),
        fb,
        "the identity is persisted"
    );
    wait_for("beta to redial alpha by itself", || {
        let b = eb.ferry_json(&["status", "--json"]);
        let a = ea.ferry_json(&["status", "--json"]);
        (peer(&b, &fa)?["state"] == "connected" && peer(&a, &fb)?["state"] == "connected")
            .then_some(())
    });
    da.shutdown();
    db.shutdown();
}

/// A peer with another major version is refused, and `ferry status` says so with
/// both versions. The other peer is a fake on loopback that sends a v2.0 `Hello`.
#[test]
fn status_names_a_version_mismatch() {
    use ferryd::fake_peer::{FakePeer, hello};
    let env = Env::new();
    let d = env.daemon("desk");
    let id = ferry_net::Identity::ephemeral().unwrap();
    let fp = id.fingerprint().to_string();
    let out = env.ferry(&["trust", &fp, "--name", "future"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut p = rt
        .block_on(FakePeer::dial(id, d.net_addr(), d.fingerprint()))
        .unwrap();
    p.send(&hello("future", 2, 0, vec![])).unwrap();
    let human = wait_for("the refusal", || {
        let human = text(&env.ferry(&["status"]).stdout);
        human
            .contains("version mismatch: 1.0 vs 2.0")
            .then_some(human)
    });
    assert!(
        human.contains("future") && human.contains("refused"),
        "{human}"
    );
}
