//! The event stream of the socket API (m3 design §2.1–2.3): `hello`, `subscribe`,
//! snapshots, coalescing, the noise rule, a reader that stops reading, `notice` and `ui`.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

mod support;

use std::time::{Duration, Instant};

use ferry_core::api::{Notice, NoticeKind, Request, Topic, UiView, Urgency};
use serde_json::{Value, json};
use support::*;

const ALL: [&str; 8] = [
    "state", "peers", "layout", "pairing", "config", "notice", "ui", "edge",
];
const QUIET: Duration = Duration::from_millis(250);

/// The `event` names of `lines`.
fn names(lines: &[Value]) -> Vec<&str> {
    lines.iter().map(|l| l["event"].as_str().unwrap()).collect()
}

/// A second connection that waits until the daemon has looked at the change, so a connection that
/// should have heard of it has by now: a negative assertion after a sentinel, not after a sleep.
fn sentinel(d: &ferryd::Daemon, topic: &str) -> Client {
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&[topic]);
    c
}

#[test]
fn the_hello_lists_the_methods_and_topics_of_the_daemon() {
    let env = Env::new();
    let d = start(&env);
    let c = Client::connect(d.socket_path());
    let data = &c.hello["data"];
    let methods: Vec<&str> = data["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(methods, Request::METHODS);
    // what the plugin feature-detects with
    for m in [
        "subscribe",
        "unsubscribe",
        "config.schema",
        "config.get",
        "config.set",
        "layout.set",
        "peer.rename",
    ] {
        assert!(methods.contains(&m), "{m} is not advertised: {methods:?}");
    }
    assert_eq!(data["topics"], json!(ALL));
    // and every listed topic subscribes
    let mut c = Client::connect(d.socket_path());
    assert_eq!(c.subscribe(&ALL)["topics"], json!(ALL));
    d.shutdown();
}

#[test]
fn subscribe_answers_first_and_then_sends_one_snapshot_of_each_topic() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let (response, before) = c.call_collecting("subscribe", json!({}));
    assert!(
        before.is_empty(),
        "no event precedes the answer: {before:?}"
    );
    assert_eq!(response["result"]["topics"], json!(ALL));

    // the order of the topic list, the topics that have a snapshot; no pairing runs
    let events = c.lines_until_quiet(QUIET);
    assert_eq!(names(&events), ["state", "peers", "layout", "config"]);

    // each is what the method of the same name answers now
    let mut other = Client::connect(d.socket_path());
    assert_eq!(events[0]["data"], other.result("status", json!(null)));
    assert_eq!(events[1]["data"], other.result("peers", json!(null)));
    assert_eq!(events[2]["data"], other.result("layout.get", json!(null)));
    assert_eq!(events[3]["data"], other.result("config.get", json!(null)));
    d.shutdown();
}

#[test]
fn params_are_optional_and_a_list_picks_the_topics() {
    let env = Env::new();
    let d = start(&env);
    for (params, expect) in [
        (json!(null), ALL.to_vec()),
        (
            json!({"topics": ["layout", "state", "state"]}),
            vec!["state", "layout"],
        ),
        (json!({"topics": ["config"]}), vec!["config"]),
    ] {
        let mut c = Client::connect(d.socket_path());
        let r = c.result("subscribe", params);
        assert_eq!(r["topics"], json!(expect));
        let got = c.lines_until_quiet(QUIET);
        let want: Vec<_> = expect
            .iter()
            .filter(|t| ["state", "peers", "layout", "config"].contains(t))
            .copied()
            .collect();
        assert_eq!(names(&got), want);
    }
    // a bare request line with no params at all
    let mut c = Client::connect(d.socket_path());
    let r = c.raw(b"{\"id\":5,\"method\":\"subscribe\"}\n");
    assert_eq!(r["result"]["topics"], json!(ALL));
    d.shutdown();
}

#[test]
fn an_unknown_topic_is_bad_params_and_subscribes_to_nothing() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    let (code, message, _) = c.error("subscribe", json!({"topics": ["state", "fax"]}));
    assert_eq!(code, "bad_params");
    assert!(
        message.contains("fax") && message.contains("pairing"),
        "{message}"
    );
    assert!(c.lines_until_quiet(QUIET).is_empty());
    // a topic list of the wrong shape too
    assert_eq!(
        c.error("subscribe", json!({"topics": "state"})).0,
        "bad_params"
    );
    d.shutdown();
}

#[test]
fn a_second_subscribe_replaces_the_topics_and_sends_the_snapshots_again() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&["state", "peers"]);
    c.lines_until_quiet(QUIET);
    let r = c.subscribe(&["layout"]);
    assert_eq!(r["topics"], json!(["layout"]));
    assert_eq!(names(&c.lines_until_quiet(QUIET)), ["layout"]);

    // peers no longer reaches it: a pin changes `peers`, the sentinel proves the daemon saw it
    let mut other = Client::connect(d.socket_path());
    let mut watcher = sentinel(&d, "peers");
    watcher.lines_until_quiet(QUIET);
    other.result("trust", json!({"fingerprint": fpx(1), "name": "a"}));
    assert!(
        watcher
            .event_within("peers", Duration::from_secs(5))
            .is_some()
    );
    let got = c.lines_until_quiet(QUIET);
    // `layout` lists pins too, so it may speak; `peers` and `state` must not
    assert!(
        names(&got).iter().all(|n| *n == "layout"),
        "an unsubscribed topic was sent: {got:?}"
    );
    d.shutdown();
}

#[test]
fn unsubscribe_ends_the_events_and_an_empty_list_does_too() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&ALL);
    c.lines_until_quiet(QUIET);
    let (response, events) = c.call_collecting("unsubscribe", json!(null));
    assert_eq!(response["result"], json!({}));
    assert!(events.is_empty());

    let mut other = Client::connect(d.socket_path());
    let mut watcher = sentinel(&d, "state");
    watcher.lines_until_quiet(QUIET);
    other.result("lock", json!({"on": true}));
    assert!(
        watcher
            .event_within("state", Duration::from_secs(5))
            .is_some()
    );
    assert!(
        c.lines_until_quiet(QUIET).is_empty(),
        "events after unsubscribe"
    );

    // an empty list is the same
    c.subscribe(&["state"]);
    c.lines_until_quiet(QUIET);
    let r = c.subscribe(&[]);
    assert_eq!(r["topics"], json!([]));
    other.result("lock", json!({"on": false}));
    assert!(
        watcher
            .event_within("state", Duration::from_secs(5))
            .is_some()
    );
    assert!(c.lines_until_quiet(QUIET).is_empty());
    // the connection still works
    assert_eq!(c.result("lock", json!({"on": false}))["locked"], false);
    d.shutdown();
}

#[test]
fn a_change_reaches_a_subscriber_with_the_summary_ferryd_computed() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&["state"]);
    let first = c.event_within("state", Duration::from_secs(5)).unwrap();
    assert_eq!(first["data"]["summary"], json!({"state": "idle"}));
    assert_eq!(first["data"]["attention"], json!([]));
    assert_eq!(first["data"]["escape_chord"], "Super+Ctrl+Escape");

    let mut other = Client::connect(d.socket_path());
    let t = Instant::now();
    other.result("lock", json!({"on": true}));
    let ev = c.event_within("state", Duration::from_secs(5)).unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(ev["data"]["locked"], true);
    assert_eq!(ev["data"]["summary"], json!({"state": "locked"}));
    other.result("lock", json!({"on": false}));
    let ev = c.event_within("state", Duration::from_secs(5)).unwrap();
    assert_eq!(ev["data"]["summary"], json!({"state": "idle"}));
    d.shutdown();
}

/// The widget follows within 100 ms. The first change after a quiet moment goes out
/// at once, not at the end of a window.
#[test]
fn the_first_change_after_a_quiet_moment_arrives_within_the_budget() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&["state"]);
    c.lines_until_quiet(QUIET);
    let mut other = Client::connect(d.socket_path());
    for on in [true, false, true] {
        std::thread::sleep(Duration::from_millis(120));
        let t = Instant::now();
        other.result("lock", json!({"on": on}));
        let ev = c.event_within("state", Duration::from_secs(5)).unwrap();
        assert_eq!(ev["data"]["locked"], on);
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?} from the change to the event",
            t.elapsed()
        );
    }
    d.shutdown();
}

#[test]
fn a_burst_of_changes_is_coalesced_and_the_last_state_arrives() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&["state"]);
    c.lines_until_quiet(QUIET);

    // 81 toggles over about 400 ms, the last one `on`
    let mut other = Client::connect(d.socket_path());
    for i in 0..81 {
        other.result("lock", json!({"on": i % 2 == 0}));
        std::thread::sleep(Duration::from_millis(5));
    }
    let events = c.lines_until_quiet(Duration::from_millis(400));
    // at most one per 50 ms window (+ the edge and the end): never one per change
    assert!(
        !events.is_empty() && events.len() <= 14,
        "{} events for 81 changes",
        events.len()
    );
    assert_eq!(
        events.last().unwrap()["data"]["locked"],
        true,
        "the last change always arrives"
    );
    d.shutdown();
}

/// The noise rule: a change in the counters alone is not an event, and the refresh brings it.
#[test]
fn counters_alone_are_not_an_event_but_the_refresh_carries_them() {
    let env = Env::new();
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.subscribe(&["state"]);
    let first = c.event_within("state", Duration::from_secs(5)).unwrap();
    let t0 = Instant::now();
    assert!(first["data"]["counters"].get("inbound_abandoned").is_none());

    let n = d.fill_ctl_queue();
    assert!(n > 0);
    // nothing for a while: these messages only count
    assert!(
        c.read_within(Duration::from_millis(1200)).is_none(),
        "a counter is not news"
    );
    // the refresh is due 2 s after the snapshot
    let ev = c.event_within("state", Duration::from_secs(3)).unwrap();
    let waited = t0.elapsed();
    assert!(
        waited >= Duration::from_millis(1500) && waited < Duration::from_millis(3500),
        "refresh after {waited:?}"
    );
    assert_eq!(ev["data"]["counters"]["inbound_abandoned"], n);
    d.shutdown();
}

/// The fingerprint of the `i`th peer `trust_many` pins.
fn many_fp(i: u32) -> String {
    format!("{:02x}{:062x}", i % 200 + 1, i + 1)
}

fn trust_many(c: &mut Client, n: u32) {
    for i in 0..n {
        c.result(
            "trust",
            json!({"fingerprint": many_fp(i), "name": format!("peer{i:04}")}),
        );
    }
}

/// The kernel's send buffer for a Unix socket, which the daemon's writes fill first.
fn socket_buffer() -> usize {
    std::fs::read_to_string("/proc/sys/net/core/wmem_default")
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(212_992)
}

/// A subscriber that stops reading never stalls the daemon, costs a bounded amount, and
/// finds the newest snapshot when it reads again.
#[test]
fn a_subscriber_that_stops_reading_never_stalls_the_daemon_and_catches_up() {
    let env = Env::new();
    let d = start(&env);
    let mut driver = Client::connect(d.socket_path());
    // a `peers` snapshot of tens of kilobytes, so a few of them fill the socket
    trust_many(&mut driver, 300);

    let mut slow = Client::connect(d.socket_path());
    slow.subscribe(&["peers"]);
    let first = slow.event_within("peers", Duration::from_secs(5)).unwrap();
    let size = first.to_string().len();
    assert!(size > 20_000, "{size}");

    // The slow one reads nothing from here on. Rename the same peer until more than three
    // socket buffers of snapshots have been produced; each rename is an event of its own.
    let want = socket_buffer() * 3;
    let mut produced = 0;
    let mut renames = 0u32;
    let started = Instant::now();
    while produced < want && renames < 120 {
        renames += 1;
        let t = Instant::now();
        driver.result(
            "peer.rename",
            json!({"peer": many_fp(0), "name": format!("renamed{renames}")}),
        );
        // the daemon answers at once, whatever the slow subscriber does
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "a request waited for the slow reader"
        );
        produced += size;
        std::thread::sleep(Duration::from_millis(55));
    }
    let last_name = format!("renamed{renames}");
    assert!(
        produced >= want,
        "not enough produced in {:?}",
        started.elapsed()
    );

    // the other connections are served as before, and the drops were counted
    let status = driver.result("status", json!(null));
    assert!(
        status["counters"]["events_superseded"].as_u64().unwrap() > 0,
        "nothing was replaced: {status}"
    );

    // it reads again: the newest snapshot is the last one, and far fewer arrive than were made
    let mut received = Vec::new();
    while let Some(v) = slow.read_within(Duration::from_millis(700)) {
        received.push(v);
    }
    let peers: Vec<&Value> = received.iter().filter(|v| v["event"] == "peers").collect();
    assert!(!peers.is_empty());
    assert!(
        peers.len() < usize::try_from(renames).unwrap() - 3,
        "{} events for {renames} changes: none was dropped",
        peers.len()
    );
    let last = peers.last().unwrap();
    assert_eq!(
        last["data"]["peers"][0]["name"], last_name,
        "the last snapshot is the newest"
    );

    // and its connection is alive: a response is never dropped
    assert_eq!(slow.result("lock", json!({"on": false}))["locked"], false);
    d.shutdown();
}

#[test]
fn notices_and_view_requests_reach_the_subscribers_of_their_topic() {
    let env = Env::new();
    let d = start(&env);
    let mut notices = Client::connect(d.socket_path());
    notices.subscribe(&["notice"]);
    let mut views = Client::connect(d.socket_path());
    views.subscribe(&["ui"]);
    let mut both = Client::connect(d.socket_path());
    both.subscribe(&["notice", "ui"]);
    // none of them has a snapshot to read
    for c in [&mut notices, &mut views, &mut both] {
        assert!(c.lines_until_quiet(QUIET).is_empty());
    }

    d.publish_notice(Notice {
        kind: NoticeKind::PeerBack,
        title: "lap is back".into(),
        body: String::new(),
        peer: Some("lap".into()),
        urgency: Urgency::Normal,
    });
    d.publish_ui(UiView::Pair);

    let n = notices
        .event_within("notice", Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        n["data"],
        json!({"kind":"peer_back","title":"lap is back","body":"","peer":"lap","urgency":"normal"})
    );
    let u = views.event_within("ui", Duration::from_secs(5)).unwrap();
    assert_eq!(u["data"], json!({"open": "pair"}));
    let got = both.lines_until_quiet(QUIET);
    assert_eq!(names(&got), ["notice", "ui"], "in the order they were made");
    // each got only its own
    assert!(notices.lines_until_quiet(QUIET).is_empty());
    assert!(views.lines_until_quiet(QUIET).is_empty());
    // the typed events parse with the client library's own type
    let parsed: ferry_core::api::EventLine = serde_json::from_value(got[1].clone()).unwrap();
    assert_eq!(
        parsed,
        ferry_core::api::EventLine::Ui(ferry_core::api::UiEvent { open: UiView::Pair })
    );
    d.shutdown();
}

#[test]
fn a_connection_that_closes_is_forgotten_and_the_daemon_goes_on() {
    let env = Env::new();
    let d = start(&env);
    {
        let mut c = Client::connect(d.socket_path());
        c.subscribe(&ALL);
        c.lines_until_quiet(QUIET);
    }
    // the daemon keeps serving, and a change finds no one to send to
    let mut other = Client::connect(d.socket_path());
    other.result("lock", json!({"on": true}));
    let mut again = Client::connect(d.socket_path());
    assert_eq!(again.subscribe(&["state"])["topics"], json!(["state"]));
    let ev = again.event_within("state", Duration::from_secs(5)).unwrap();
    assert_eq!(ev["data"]["locked"], true);
    d.shutdown();
}

#[test]
fn the_topics_are_the_enum_of_the_client_library() {
    // the names the daemon advertises are the names `ferry-core` parses
    let env = Env::new();
    let d = start(&env);
    let c = Client::connect(d.socket_path());
    for t in c.hello["data"]["topics"].as_array().unwrap() {
        assert!(t.as_str().unwrap().parse::<Topic>().is_ok(), "{t}");
    }
    d.shutdown();
}

/// Two peers whose monitors ferryd knows, as if they had connected before.
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

/// The `layout` topic is `layout.get`, whatever fields the layout stream gives that result, and
/// every edit (`layout.place`, `layout.set`) is an event with the new revision.
#[test]
fn a_layout_edit_reaches_a_layout_subscriber_as_the_layout_the_method_answers() {
    let env = Env::new();
    seed_known_peers(&env);
    let d = start(&env);
    let mut c = Client::connect(d.socket_path());
    c.result("trust", json!({"fingerprint": fpx(0x9c), "name": "a"}));
    c.result("trust", json!({"fingerprint": fpx(0x9d), "name": "b"}));
    let mut w = Client::connect(d.socket_path());
    w.subscribe(&["layout"]);
    let first = w.event_within("layout", Duration::from_secs(5)).unwrap();
    assert_eq!(first["data"]["revision"]["counter"], 0);
    assert_eq!(first["data"], c.result("layout.get", json!(null)));
    // the fields of the layout view are carried as they are
    let desk = &first["data"]["peers"][0];
    assert_eq!(desk["self"], true);
    assert_eq!(desk["trusted"], true);

    c.result(
        "layout.place",
        json!({"peer": "a", "rel": "below", "anchor": "HDMI-A-1", "align": "center"}),
    );
    let placed = w.event_within("layout", Duration::from_secs(5)).unwrap();
    assert_eq!(placed["data"]["revision"]["counter"], 1);
    assert_eq!(placed["data"], c.result("layout.get", json!(null)));

    // one edit of two groups is one revision, and one event
    let moved = c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 4000, "y": 0}, {"peer": "b", "x": 4000, "y": 1000}]}),
    );
    let ev = w.event_within("layout", Duration::from_secs(5)).unwrap();
    assert_eq!(ev["data"]["revision"]["counter"], 2);
    assert_eq!(ev["data"], moved);

    // a refused edit changes nothing, so it is no event: the next real one is the next event
    let (code, _, _) = c.error(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 0, "y": 0}], "base_rev": {"counter": 1, "peer": first["data"]["revision"]["peer"]}}),
    );
    assert_eq!(code, "stale_layout");
    c.result(
        "layout.set",
        json!({"origins": [{"peer": "a", "x": 4100, "y": 0}]}),
    );
    let next = w.event_within("layout", Duration::from_secs(5)).unwrap();
    assert_eq!(next["data"]["revision"]["counter"], 3);
    d.shutdown();
}
