//! `ferry pair`, `ferry peers` and `ferry unpair` against two in-process daemons on loopback,
//! with the answers scripted. Nothing captures or injects input.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::Duration;

use ferry_cli::pair::Input;
use ferry_cli::run_with;
use support::*;

/// Scripted answers. Each line comes a little after it is asked for, as a person types: the
/// question drops what was typed before it. With a gate, each line waits until the gate holds.
struct Script {
    lines: Vec<String>,
    terminal: bool,
    gate: Option<Box<dyn Fn() -> bool + Send>>,
}

impl Script {
    fn answers(lines: &[&str]) -> Box<dyn Input> {
        Box::new(Self {
            lines: lines.iter().rev().map(|s| s.to_string()).collect(),
            terminal: false,
            gate: None,
        })
    }

    /// Claims to be a terminal, so the commands that need one run.
    fn tty(lines: &[&str]) -> Box<dyn Input> {
        Box::new(Self {
            lines: lines.iter().rev().map(|s| s.to_string()).collect(),
            terminal: true,
            gate: None,
        })
    }

    /// [`Script::tty`] whose answers wait until `gate` holds (checked every 20 ms, up to 30 s).
    fn tty_after(lines: &[&str], gate: impl Fn() -> bool + Send + 'static) -> Box<dyn Input> {
        Box::new(Self {
            lines: lines.iter().rev().map(|s| s.to_string()).collect(),
            terminal: true,
            gate: Some(Box::new(gate)),
        })
    }
}

/// The pairing stage of the daemon at `socket`, from `ferry --json status`.
fn pairing_stage(socket: &std::path::Path) -> Option<String> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(
        ["ferry", "--json", "status"],
        socket,
        &mut out,
        &mut err,
        Script::answers(&[]),
    );
    if code != 0 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
    v["pairing"]["stage"].as_str().map(str::to_string)
}

impl Input for Script {
    fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// The transcript shows the answer, as a terminal's echo would.
    fn echoes(&self) -> bool {
        false
    }

    fn read_line(&mut self) -> Option<String> {
        std::thread::sleep(Duration::from_millis(300));
        if let Some(gate) = &self.gate {
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while !gate() {
                assert!(
                    std::time::Instant::now() < until,
                    "the script's gate never held"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let l = self.lines.pop();
        if l.is_none() {
            // the end of the script: block like a terminal nobody types into
            std::thread::sleep(Duration::from_secs(3600));
        }
        l
    }
}

fn ferry(env: &Env, args: &[&str], input: Box<dyn Input>) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(
        std::iter::once("ferry").chain(args.iter().copied()),
        &env.socket(),
        &mut out,
        &mut err,
        input,
    );
    (code, text(&out), text(&err))
}

/// The transcript with what changes from run to run replaced: the code, keys, ports.
fn masked(s: &str, keys: &[(&str, &str)], ports: &[u16]) -> String {
    let mut s = s.to_string();
    for (key, name) in keys {
        s = s.replace(key, name);
    }
    for p in ports {
        s = s.replace(&format!(":{p}"), ":<port>");
        s = s.replace(&format!("UDP {p}."), "UDP <port>.");
    }
    // "    Pairing code   418 207"
    let re_code = |line: &str| {
        if let Some(rest) = line.strip_prefix("    Pairing code   ") {
            assert!(rest.len() == 7 && rest.as_bytes()[3] == b' ', "{line}");
            "    Pairing code   NNN NNN".to_string()
        } else {
            line.to_string()
        }
    };
    s.lines().map(re_code).collect::<Vec<_>>().join("\n") + "\n"
}

fn code_in(transcript: &str) -> String {
    transcript
        .lines()
        .find_map(|l| l.strip_prefix("    Pairing code   "))
        .expect("a code")
        .to_string()
}

fn short(fp: &str) -> String {
    ferry_core::PeerId::parse(fp).unwrap().short()
}

#[test]
fn pair_transcripts_peers_and_unpair() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (ea.daemon("desktop"), eb.daemon("laptop"));
    let (fa, fb) = (da.fingerprint().to_string(), db.fingerprint().to_string());
    let keys = [
        (short(&fa), "<key-a>".to_string()),
        (short(&fb), "<key-b>".to_string()),
    ];
    let keys: Vec<(&str, &str)> = keys.iter().map(|(k, n)| (k.as_str(), n.as_str())).collect();
    let ports = [da.net_addr().port(), db.net_addr().port()];

    // B accepts, A asks; both say y, A first: B answers only once A waits for it, so A's
    // transcript has "Waiting for laptop..." and B's goes straight to "Paired" every time
    let b = std::thread::spawn({
        let eb_socket = eb.socket();
        let ea_socket = ea.socket();
        move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with(
                ["ferry", "pair", "--accept"],
                &eb_socket,
                &mut out,
                &mut err,
                Script::tty_after(&["y"], move || {
                    pairing_stage(&ea_socket).as_deref() == Some("waiting_peer")
                }),
            );
            (code, text(&out), text(&err))
        }
    });
    wait_for("B's window", || {
        let st = eb.ferry_json(&["status", "--json"]);
        st.get("pairing").is_some().then_some(())
    });
    let b_addr = format!("127.0.0.1:{}", db.net_addr().port());
    let (code_a, out_a, err_a) = ferry(
        &ea,
        &["pair", &b_addr, "--name", "laptop"],
        Script::tty(&["y"]),
    );
    let (code_b, out_b, err_b) = b.join().unwrap();
    assert_eq!((code_a, err_a.as_str()), (0, ""), "{out_a}");
    assert_eq!((code_b, err_b.as_str()), (0, ""), "{out_b}");
    assert_eq!(code_in(&out_a), code_in(&out_b), "the same code on both");
    insta::assert_snapshot!("pair_dialer", masked(&out_a, &keys, &ports));
    insta::assert_snapshot!("pair_acceptor", masked(&out_b, &keys, &ports));

    // ferry peers on both
    let (code, peers_a, _) = ferry(&ea, &["peers"], Script::answers(&[]));
    assert_eq!(code, 0);
    assert!(
        peers_a.contains("laptop") && peers_a.contains("code"),
        "{peers_a}"
    );
    let (_, peers_b, _) = ferry(&eb, &["peers"], Script::answers(&[]));
    assert!(peers_b.contains("desktop"), "{peers_b}");
    let json = ea.ferry_json(&["peers", "--json"]);
    assert_eq!(json["peers"][0]["paired"], "sas");

    // unpair asks; n keeps the pin, y removes it
    let (code, out, err) = ferry(&ea, &["unpair", "laptop"], Script::tty(&["n"]));
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("laptop is still paired"), "{err}");
    let (code, out, _) = ferry(&ea, &["unpair", "laptop"], Script::tty(&["y"]));
    assert_eq!(code, 0);
    assert!(
        out.contains(&format!("Unpaired laptop ({}).", short(&fb))),
        "{out}"
    );
    let (_, peers_a, _) = ferry(&ea, &["peers"], Script::answers(&[]));
    assert!(peers_a.starts_with("No peers."), "{peers_a}");
    da.shutdown();
    db.shutdown();
}

/// A peer name with `CR` and cursor escapes once moved up two rows and rewrote the code
/// line on both screens. Now neither transcript holds a control byte but `\n`, each shows the
/// other under its default name, and the code line is the daemon's.
#[test]
fn a_peer_name_with_escapes_is_printed_escaped() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (
        ea.daemon("A\u{1b}[1;31m\u{7}"),
        eb.daemon("B\r\u{1b}[2A\u{1b}[2K    Pairing code   123 456\u{1b}[2B"),
    );
    let name_a = ferry_core::PeerId(da.fingerprint().0).default_name();
    let name_b = ferry_core::PeerId(db.fingerprint().0).default_name();
    let b = std::thread::spawn({
        let eb_socket = eb.socket();
        let ea_socket = ea.socket();
        move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with(
                ["ferry", "pair", "--accept"],
                &eb_socket,
                &mut out,
                &mut err,
                Script::tty_after(&["y"], move || {
                    pairing_stage(&ea_socket).as_deref() == Some("waiting_peer")
                }),
            );
            (code, text(&out), text(&err))
        }
    });
    wait_for("B's window", || {
        let st = eb.ferry_json(&["status", "--json"]);
        st.get("pairing").is_some().then_some(())
    });
    let b_addr = format!("127.0.0.1:{}", db.net_addr().port());
    let (code_a, out_a, err_a) = ferry(&ea, &["pair", &b_addr], Script::tty(&["y"]));
    let (code_b, out_b, err_b) = b.join().unwrap();
    assert_eq!((code_a, err_a.as_str()), (0, ""), "{out_a}");
    assert_eq!((code_b, err_b.as_str()), (0, ""), "{out_b}");
    for t in [&out_a, &out_b] {
        assert!(
            t.bytes().all(|c| c == b'\n' || (c >= 0x20 && c != 0x7f)),
            "{t:?}"
        );
        assert!(!t.chars().any(|c| c != '\n' && c.is_control()), "{t:?}");
        assert_eq!(
            t.lines().filter(|l| l.contains("Pairing code")).count(),
            1,
            "{t}"
        );
    }
    assert_eq!(code_in(&out_a), code_in(&out_b), "the same code on both");
    assert!(out_a.contains(&format!("{name_b} answered")), "{out_a}");
    assert!(
        out_a.contains(&format!("Confirm only if {name_b} shows")),
        "{out_a}"
    );
    assert!(
        out_b.contains(&format!("Request from {name_a} at")),
        "{out_b}"
    );
    assert!(
        out_b.contains(&format!(
            "Note: pinned as {name_a}: the name it sent is not allowed; \
             a peer name cannot contain '\\u{{1b}}'"
        )),
        "{out_b}"
    );
    da.shutdown();
    db.shutdown();
}

#[test]
fn codes_differ_cancels_both() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (ea.daemon("desktop"), eb.daemon("laptop"));
    // A says y first, B says n once A waits for it: both end with "codes differ"
    let b = std::thread::spawn({
        let s = eb.socket();
        let ea_socket = ea.socket();
        move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with(
                ["ferry", "pair", "--accept"],
                &s,
                &mut out,
                &mut err,
                Script::tty_after(&["n"], move || {
                    pairing_stage(&ea_socket).as_deref() == Some("waiting_peer")
                }),
            );
            (code, text(&out), text(&err))
        }
    });
    wait_for("B's window", || {
        eb.ferry_json(&["status", "--json"])
            .get("pairing")
            .is_some()
            .then_some(())
    });
    let (code_a, out_a, err_a) = ferry(
        &ea,
        &["pair", &format!("127.0.0.1:{}", db.net_addr().port())],
        Script::tty(&["y"]),
    );
    assert_eq!(code_a, 1, "{out_a}");
    assert!(
        err_a.contains("Pairing cancelled: codes differ."),
        "{err_a}"
    );
    // B said no; its window stays open for another try until ferryd goes away
    wait_for("B's failure", || {
        let st = eb.ferry_json(&["status", "--json"]);
        (st["counters"]["pair_failures"] == 1).then_some(())
    });
    db.shutdown();
    let (code_b, out_b, _) = b.join().unwrap();
    assert_eq!(code_b, 1);
    assert!(
        out_b.contains("Pairing cancelled: codes differ."),
        "{out_b}"
    );
    assert!(out_b.contains("Still accepting for"), "{out_b}");
    let (_, peers, _) = ferry(&ea, &["peers"], Script::answers(&[]));
    assert!(peers.starts_with("No peers."));
    da.shutdown();
}

#[test]
fn without_a_terminal_pair_and_unpair_are_usage_errors() {
    let env = Env::new();
    let d = env.daemon("desk");
    let (code, _, err) = ferry(&env, &["pair", "--accept"], Script::answers(&["y"]));
    assert_eq!(code, 2);
    assert!(err.contains("needs a terminal"), "{err}");
    assert!(err.contains("pair.accept"), "{err}");
    let (code, _, err) = ferry(&env, &["unpair", "x"], Script::answers(&["y"]));
    assert_eq!(code, 2);
    assert!(err.contains("--yes"), "{err}");
    // --yes needs no terminal; an unknown peer is a daemon error (exit 1)
    let (code, _, err) = ferry(&env, &["unpair", "nobody", "--yes"], Script::answers(&[]));
    assert_eq!(code, 1);
    assert!(err.contains("nobody"), "{err}");
    // not accepting: exit 1 with the hint of §2.12
    let other = Env::new();
    let o = other.daemon("other");
    let (code, out, err) = ferry(
        &env,
        &["pair", &format!("127.0.0.1:{}", o.net_addr().port())],
        Script::tty(&[]),
    );
    assert_eq!(code, 1, "{out}");
    assert!(err.contains("is not accepting pairing"), "{err}");
    assert!(err.contains("hint: run ferry pair --accept there"), "{err}");
    o.shutdown();
    d.shutdown();
}

// --- the answer race: the user answers after the attempt has ended ----------------------------

/// A scripted ferryd on a Unix socket for one connection: the hello, then for each request line
/// the lines `reply` gives, in order. It plays the peer's side of the race exactly: the user's
/// answer reaches it only after the attempt has ended.
fn fake_ferryd(
    dir: &std::path::Path,
    mut reply: impl FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send + 'static,
) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader, Write};
    let path = dir.join("fake-ferryd.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let t = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut w = stream.try_clone().unwrap();
        let hello = serde_json::json!({"event": "hello", "data": {"api": 1, "version": "0.1.0",
            "peer": "desk", "fingerprint": "d0".repeat(32)}});
        writeln!(w, "{hello}").unwrap();
        let mut methods = Vec::new();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            methods.push(req["method"].as_str().unwrap_or_default().to_string());
            for out in reply(&req) {
                if writeln!(w, "{out}").is_err() {
                    return methods;
                }
            }
        }
        methods
    });
    (path, t)
}

fn ok_line(req: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id": req["id"], "result": result})
}

fn err_line(req: &serde_json::Value, code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"id": req["id"], "error": {"code": code, "message": message}})
}

fn event(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"event": "pairing", "data": data})
}

fn peer_json() -> serde_json::Value {
    serde_json::json!({"name": "laptop", "fingerprint": "9c2e41aa0b7d5512".repeat(4),
        "addr": "192.0.2.11:4243"})
}

/// `failed` with this reason and message, from the end of the attempt.
fn failed(reason: &str, message: &str) -> serde_json::Value {
    event(
        serde_json::json!({"stage": "failed", "peer": peer_json(), "expires_ms": 90_000,
        "reason": reason, "message": message}),
    )
}

/// Runs `ferry pair` (dialer or acceptor) against a fake ferryd whose `pair.answer` comes too
/// late: it answers with `answer_error` and the attempt's `failed` event (in that order, or the
/// event first). Returns the exit code, stdout, stderr and the methods ferryd saw.
fn late_answer(
    accept: bool,
    answer_error: (&'static str, &'static str),
    reason: &'static str,
    message: &'static str,
    event_first: bool,
) -> (u8, String, String, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let (sock, server) = fake_ferryd(dir.path(), move |req| {
        match req["method"].as_str().unwrap() {
            "id" => vec![ok_line(
                req,
                serde_json::json!({"name": "desk", "fingerprint": "d0".repeat(32), "port": 4243,
                    "addrs": []}),
            )],
            "pair.accept" => vec![
                ok_line(req, serde_json::json!({"expires_ms": 120_000})),
                event(
                    serde_json::json!({"stage": "code", "peer": peer_json(), "code": "418207",
                    "expires_ms": 118_000}),
                ),
            ],
            "pair.start" => vec![
                event(serde_json::json!({"stage": "connecting", "expires_ms": 120_000})),
                ok_line(req, serde_json::json!({})),
                event(
                    serde_json::json!({"stage": "code", "peer": peer_json(), "code": "418207",
                    "expires_ms": 118_000}),
                ),
            ],
            "pair.answer" => {
                // the peer's no (or cancel, or timeout) ended the attempt before this answer
                let err = err_line(req, answer_error.0, answer_error.1);
                let mut v = if event_first {
                    vec![failed(reason, message), err]
                } else {
                    vec![err, failed(reason, message)]
                };
                if accept {
                    // the acceptor's window then ends too (a cancel closes it)
                    v.push(event(
                        serde_json::json!({"stage": "closed", "expires_ms": 0,
                        "reason": "cancelled", "message": "pairing window closed: cancelled"}),
                    ));
                }
                v
            }
            other => vec![err_line(req, "unknown_method", other)],
        }
    });
    let args: Vec<&str> = if accept {
        vec!["ferry", "pair", "--accept"]
    } else {
        vec!["ferry", "pair", "192.0.2.11"]
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(args, &sock, &mut out, &mut err, Script::tty(&["y"]));
    drop(dir);
    let methods = server.join().unwrap();
    (code, text(&out), text(&err), methods)
}

/// Exit-check row 4: y here, N there, and the N ends the attempt first. The late y must not be
/// an error of its own: the transcript shows why the pairing ended, with a cancel's exit code.
#[test]
fn a_yes_after_the_peer_said_no_shows_the_cancel() {
    for event_first in [true, false] {
        let (code, out, err, methods) = late_answer(
            false,
            ("no_pairing", "the attempt has ended"),
            "rejected",
            "cancelled: codes differ",
            event_first,
        );
        assert_eq!(code, 1, "{out}{err}");
        assert!(methods.contains(&"pair.answer".to_string()), "{methods:?}");
        assert!(out.contains("Same code? [y/N] y\n"), "{out}");
        assert_eq!(err, "error: Pairing cancelled: codes differ.\n", "{out}");
        assert!(!err.contains("the attempt has ended") && !out.contains("the attempt has ended"));
    }
    // a normal cancel ends the same way: exit 1, the same line
    let (code, _, err, _) = late_answer(
        false,
        ("no_pairing", "no pairing runs on this connection"),
        "cancelled",
        "cancelled",
        true,
    );
    assert_eq!((code, err.as_str()), (1, "error: Pairing cancelled.\n"));
}

/// The dialer answers just as the acceptor's window times out: "timed out", not an error.
#[test]
fn a_yes_after_the_window_timed_out_shows_the_timeout() {
    for event_first in [true, false] {
        let (code, out, err, _) = late_answer(
            false,
            ("pairing_expired", "the pairing has timed out"),
            "timeout",
            "timed out",
            event_first,
        );
        assert_eq!(code, 1, "{out}{err}");
        assert_eq!(err, "error: Pairing timed out.\n", "{out}");
    }
}

/// The acceptor's own late answer: the reason is printed, then the window's end, exit 1.
#[test]
fn the_acceptor_shows_the_reason_of_a_late_answer_too() {
    for event_first in [true, false] {
        let (code, out, err, _) = late_answer(
            true,
            ("no_pairing", "the attempt has ended"),
            "cancelled",
            "cancelled",
            event_first,
        );
        assert_eq!(code, 1, "{out}{err}");
        assert!(out.contains("Pairing cancelled.\n"), "{out}");
        assert_eq!(err, "error: pairing window closed: cancelled\n", "{out}");
        assert!(!err.contains("the attempt has ended"));
    }
}

// --- C-09, C-10, C-11: the CLI reads typed fields, not text ------------------------------------

/// The wording of a failed pairing follows the event's `reason`. A message that is worded
/// otherwise (or that begins like another reason's) changes nothing.
#[test]
fn a_pairing_failure_is_worded_by_its_reason_not_by_its_message() {
    let cases = [
        (
            "rejected",
            "the other machine said the codes differ",
            "Pairing cancelled: codes differ.",
        ),
        (
            "cancelled",
            "stopped on the other side",
            "Pairing cancelled.",
        ),
        ("timeout", "nobody answered", "Pairing timed out."),
        (
            "protocol",
            "garbled",
            "Pairing cancelled: the peer broke the protocol.",
        ),
        // a failure that merely reads like a cancel stays a failure
        (
            "lost",
            "cancelled: the link went away",
            "Pairing failed: cancelled: the link went away.",
        ),
        (
            "version",
            "timed out? no: 1.0 vs 2.0",
            "Pairing failed: timed out? no: 1.0 vs 2.0.",
        ),
    ];
    for (reason, message, want) in cases {
        let (code, out, err, _) = late_answer(
            false,
            ("no_pairing", "the attempt has ended"),
            reason,
            message,
            true,
        );
        assert_eq!(code, 1, "{reason}: {out}{err}");
        assert_eq!(err, format!("error: {want}\n"), "{reason}");
    }
}

/// A fake ferryd for one command: `reply` answers each request, and `ferry <args>` runs against it.
fn run_fake(
    args: &[&str],
    input: Box<dyn Input>,
    reply: impl FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send + 'static,
) -> (u8, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let (sock, server) = fake_ferryd(dir.path(), reply);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(
        std::iter::once("ferry").chain(args.iter().copied()),
        &sock,
        &mut out,
        &mut err,
        input,
    );
    drop(dir);
    let _ = server.join();
    (code, text(&out), text(&err))
}

/// A result whose fields were renamed is an error, not ` not yet: ...` for every pairing.
#[test]
fn a_renamed_status_field_fails_the_pairing_wait_with_the_unexpected_shape_error() {
    let (code, out, err) = run_fake(&["pair", "192.0.2.11"], Script::tty(&["y"]), |req| {
        match req["method"].as_str().unwrap() {
            "pair.start" => vec![
                event(serde_json::json!({"stage": "connecting", "expires_ms": 120_000})),
                ok_line(req, serde_json::json!({})),
                event(
                    serde_json::json!({"stage": "code", "peer": peer_json(), "code": "418207",
                    "expires_ms": 118_000}),
                ),
            ],
            "pair.answer" => vec![
                ok_line(req, serde_json::json!({})),
                event(serde_json::json!({"stage": "paired", "peer": peer_json(), "expires_ms": 0})),
            ],
            // `peers` is `links` now, and `me` is gone
            "status" => vec![ok_line(
                req,
                serde_json::json!({"api": 1, "links": [], "port": 4243, "role": "idle"}),
            )],
            other => vec![err_line(req, "unknown_method", other)],
        }
    });
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("unexpected shape"), "{err}");
    assert!(!out.contains("not yet"), "{out}");
}

/// The same for `trust` and `unpair`, which printed `?` for a name they could not find.
#[test]
fn a_renamed_trust_or_unpair_field_is_the_unexpected_shape_error() {
    let renamed = |req: &serde_json::Value| {
        vec![ok_line(
            req,
            serde_json::json!({"peer": {"label": "laptop", "fingerprint": "ab".repeat(32)}}),
        )]
    };
    let fp = "ab".repeat(32);
    let (code, out, err) = run_fake(&["trust", &fp], Script::answers(&[]), renamed);
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("unexpected shape"), "{err}");
    assert!(!out.contains("Trusted"), "{out}");

    let (code, out, err) = run_fake(
        &["unpair", "laptop", "--yes"],
        Script::answers(&[]),
        renamed,
    );
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("unexpected shape"), "{err}");
    assert!(!out.contains("Unpaired"), "{out}");
}

/// The acceptor's window and port come from ferryd's answers, not from numbers in the CLI.
#[test]
fn the_acceptor_names_the_window_and_the_port_ferryd_reports() {
    let (code, out, err) =
        run_fake(
            &["pair", "--accept"],
            Script::tty(&[]),
            |req| match req["method"].as_str().unwrap() {
                "id" => vec![ok_line(
                    req,
                    serde_json::json!({"name": "desk", "fingerprint": "d0".repeat(32), "port": 5000,
                    "addrs": []}),
                )],
                "pair.accept" => vec![
                    ok_line(req, serde_json::json!({"expires_ms": 90_000})),
                    event(
                        serde_json::json!({"stage": "closed", "expires_ms": 0, "reason": "timeout",
                    "message": "pairing window closed: timed out"}),
                    ),
                ],
                other => vec![err_line(req, "unknown_method", other)],
            },
        );
    assert_eq!(code, 1, "{err}");
    assert!(
        out.starts_with("Accepting one pairing request for 90 s on UDP 5000. Ctrl+C stops.\n"),
        "{out}"
    );
}

/// A code answered in the panel (here a second connection) counts for this peer; the
/// terminal that asked drops its question and goes on waiting for the other side.
#[test]
fn an_answer_from_another_connection_settles_the_question_in_the_terminal() {
    let (ea, eb) = (Env::new(), Env::new());
    let (da, db) = (ea.daemon("desktop"), eb.daemon("laptop"));
    // B's terminal asks and nobody types; B's panel answers once the code is shown
    let b = std::thread::spawn({
        let eb_socket = eb.socket();
        move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with(
                ["ferry", "pair", "--accept"],
                &eb_socket,
                &mut out,
                &mut err,
                Script::tty(&[]),
            );
            (code, text(&out), text(&err))
        }
    });
    let panel = std::thread::spawn({
        let eb_socket = eb.socket();
        move || {
            wait_for("B shows the code", || {
                (pairing_stage(&eb_socket).as_deref() == Some("code")).then_some(())
            });
            let mut c = ferry_cli::client::Client::connect(&eb_socket).unwrap();
            c.call(ferry_core::api::Request::PairAnswer(
                ferry_core::api::PairAnswerParams { matches: true },
            ))
            .unwrap();
        }
    });
    wait_for("B's window", || {
        eb.ferry_json(&["status", "--json"])
            .get("pairing")
            .is_some()
            .then_some(())
    });
    // A answers only after B's side did, so A's transcript waits for B and B's does not
    let eb_socket = eb.socket();
    let (code_a, out_a, err_a) = ferry(
        &ea,
        &["pair", &format!("127.0.0.1:{}", db.net_addr().port())],
        Script::tty_after(&["y"], move || {
            pairing_stage(&eb_socket).as_deref() == Some("waiting_peer")
        }),
    );
    panel.join().unwrap();
    let (code_b, out_b, err_b) = b.join().unwrap();
    assert_eq!((code_a, err_a.as_str()), (0, ""), "{out_a}");
    assert_eq!((code_b, err_b.as_str()), (0, ""), "{out_b}");
    // B's question was left open, closed with a newline, and B went on to wait and to pair
    assert!(
        out_b.contains("Same code? [y/N] \nWaiting for desktop...\nPaired with desktop"),
        "{out_b}"
    );
    assert!(out_a.contains("Paired with laptop"), "{out_a}");
    da.shutdown();
    db.shutdown();
}

/// pair-polish: the panel or another terminal cancels the pairing `ferry pair` started. That is
/// the user's own decision, so the command says so and exits 0. A cancel by the other peer is
/// still a failure.
#[test]
fn a_cancel_from_elsewhere_ends_ferry_pair_cleanly() {
    let cancel = |by: &'static str| {
        move |req: &serde_json::Value| match req["method"].as_str() {
            Some("pair.start") => {
                let mut ev = failed("cancelled", "cancelled");
                ev["data"]["cancelled_by"] = serde_json::json!(by);
                vec![
                    event(serde_json::json!({"stage": "connecting", "expires_ms": 120_000})),
                    ok_line(req, serde_json::json!({})),
                    ev,
                ]
            }
            _ => vec![ok_line(req, serde_json::json!({}))],
        }
    };
    let (code, out, err) = run_fake(&["pair", "192.0.2.11"], Script::tty(&[]), cancel("local"));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.contains("Pairing cancelled elsewhere."), "{out}");
    let (code, _, err) = run_fake(&["pair", "192.0.2.11"], Script::tty(&[]), cancel("peer"));
    assert_ne!(code, 0);
    assert!(err.contains("Pairing cancelled."), "{err}");
}
