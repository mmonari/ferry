//! The hub with a stand-in for ctl and synthetic time: no sleeping, no socket.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use ferry_core::api::{
    EventLine, Notice, NoticeKind, PairingEvent, PairingStage, PeersResult, UiEvent, Urgency,
};
use serde_json::{Value, json};

use super::*;

/// What the stand-in reports; the tests change it between flushes.
#[derive(Default)]
struct Fake {
    /// The news of `state`.
    rev: Cell<u32>,
    /// The noise of `state`.
    noise: Cell<u32>,
    /// `peers`, `layout`, `config`.
    other: Cell<u32>,
    pairing: RefCell<Option<PairingEvent>>,
    built: Cell<[u32; 4]>,
}

impl Fake {
    fn built(&self, snap: Snap) -> u32 {
        self.built.get()[snap.index()]
    }
}

impl Source for Fake {
    fn snapshot(&self, snap: Snap) -> Snapshot {
        let mut built = self.built.get();
        built[snap.index()] += 1;
        self.built.set(built);
        match snap {
            Snap::State => Snapshot::with_noise(
                snap,
                &json!({"rev": self.rev.get(), "noise": self.noise.get()}),
                &json!({"rev": self.rev.get()}),
            ),
            _ => Snapshot::new(snap, &json!({"v": self.other.get()})),
        }
    }

    fn pairing(&self) -> Option<PairingEvent> {
        self.pairing.borrow().clone()
    }
}

struct Rig {
    hub: Hub,
    src: Fake,
    t0: Instant,
}

impl Rig {
    fn new() -> Self {
        Self {
            hub: Hub::new(Arc::new(Changed::default())),
            src: Fake::default(),
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn sub(&self, conn: u64, topics: &[Topic], ms: u64) -> Rc<EventQueue> {
        let q = Rc::new(EventQueue::default());
        self.hub.subscribe(&self.src, conn, &q, topics, self.at(ms));
        q
    }

    /// A poke and a flush at `ms`; returns when the hub wants to run next.
    fn tick(&self, ms: u64) -> Option<Instant> {
        self.hub.poke();
        self.hub.flush(&self.src, self.at(ms))
    }
}

/// Everything waiting, as parsed JSON; empties the queue.
fn drain(q: &EventQueue) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(line) = q.pop() {
        assert_eq!(line.last(), Some(&b'\n'), "one line, ended");
        out.push(serde_json::from_slice(&line).unwrap());
    }
    out
}

/// `[("state", 3), ("peers", 1)]`: topic and `rev` or `v`.
fn summary(events: &[Value]) -> Vec<(String, u64)> {
    events
        .iter()
        .map(|e| {
            let d = &e["data"];
            (
                e["event"].as_str().unwrap().to_string(),
                d["rev"].as_u64().or_else(|| d["v"].as_u64()).unwrap_or(999),
            )
        })
        .collect()
}

fn s(topic: &str, n: u64) -> (String, u64) {
    (topic.to_string(), n)
}

#[test]
fn nothing_is_built_while_nobody_subscribes() {
    let r = Rig::new();
    r.hub.poke();
    r.hub.poke();
    assert_eq!(r.hub.flush(&r.src, r.at(5)), None);
    assert_eq!(r.src.built.get(), [0; 4]);
    assert!(!r.hub.wants(Topic::State));
}

#[test]
fn subscribing_sends_one_snapshot_of_each_asked_topic_in_list_order() {
    let r = Rig::new();
    r.src.rev.set(7);
    r.src.other.set(3);
    // asked out of order, with a topic that has no snapshot
    let q = r.sub(
        1,
        &[Topic::Config, Topic::Notice, Topic::Layout, Topic::State],
        0,
    );
    assert_eq!(
        summary(&drain(&q)),
        [s("state", 7), s("layout", 3), s("config", 3)]
    );
    // nothing was built for the topics nobody asked for
    assert_eq!(r.src.built.get(), [1, 0, 1, 1]);
    // and nothing more is due until something happens
    assert!(q.is_empty());
}

#[test]
fn a_change_goes_out_at_once_and_the_rest_of_a_burst_at_the_end_of_the_window() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State], 0);
    drain(&q);

    // the first change: leading edge
    r.src.rev.set(1);
    r.tick(1);
    assert_eq!(summary(&drain(&q)), [s("state", 1)]);

    // changes inside the window wait, and the hub says when the window ends
    r.src.rev.set(2);
    let next = r.tick(10).unwrap();
    assert_eq!(next, r.at(1) + COALESCE);
    r.src.rev.set(3);
    r.tick(30);
    assert!(q.is_empty(), "at most one event per window");

    // the window ends: the newest state goes, the one in between never does
    r.hub.flush(&r.src, r.at(51));
    assert_eq!(summary(&drain(&q)), [s("state", 3)]);

    // and a flush with nothing new sends nothing
    r.hub.flush(&r.src, r.at(60));
    r.tick(200);
    assert!(q.is_empty());
}

#[test]
fn no_topic_sends_more_than_once_per_window_whatever_the_pokes() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State, Topic::Peers], 0);
    drain(&q);
    let mut sent = 0;
    for ms in 1..=1000 {
        r.src.rev.set(u32::try_from(ms).unwrap());
        r.tick(ms);
        sent += drain(&q).len();
    }
    // 1000 ms of changes every millisecond: at most one state event per 50 ms (+1 for the edge);
    // peers never changed
    assert!(sent <= 21, "{sent} events in 1 s");
    assert!(
        sent >= 18,
        "{sent} events in 1 s: the window must not starve the topic"
    );
}

#[test]
fn noise_alone_sends_nothing_but_the_refresh_does() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State], 0);
    drain(&q);

    r.src.noise.set(1);
    r.tick(100);
    r.src.noise.set(2);
    r.tick(900);
    assert!(q.is_empty(), "counters and round trips are not news");

    // the refresh carries the noise: 2 s after the snapshot
    r.hub.flush(&r.src, r.at(1999));
    assert!(q.is_empty());
    let next = r.hub.flush(&r.src, r.at(2000));
    let events = drain(&q);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["data"], json!({"rev": 0, "noise": 2}));
    // the next refresh is 2 s on, and a refresh with nothing different sends nothing
    assert_eq!(next, Some(r.at(4000)));
    r.hub.flush(&r.src, r.at(4000));
    assert!(q.is_empty());
    r.src.noise.set(3);
    r.hub.flush(&r.src, r.at(6000));
    assert_eq!(drain(&q).len(), 1);
}

#[test]
fn news_carries_the_noise_of_the_moment_and_the_refresh_keeps_its_beat() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State], 0);
    drain(&q);
    r.src.noise.set(5);
    r.src.rev.set(1);
    let next = r.tick(1500);
    let events = drain(&q);
    assert_eq!(events[0]["data"], json!({"rev": 1, "noise": 5}));
    // looking for news does not push the refresh out: a link whose round trip changes every
    // second would otherwise never refresh
    assert_eq!(next, Some(r.at(2000)));
    for ms in (1600..2000).step_by(100) {
        r.tick(ms);
    }
    assert!(q.is_empty());
}

#[test]
fn only_state_has_noise_and_a_refresh() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::Peers, Topic::Layout, Topic::Config], 0);
    drain(&q);
    // no timer at all while nothing is pending: a poke is all that can wake the hub
    assert_eq!(r.hub.flush(&r.src, r.at(10_000)), None);
    r.src.other.set(1);
    r.tick(10_001);
    assert_eq!(
        summary(&drain(&q)),
        [s("peers", 1), s("layout", 1), s("config", 1)]
    );
}

#[test]
fn one_snapshot_is_built_per_look_however_many_connections_watch() {
    let r = Rig::new();
    let qs: Vec<_> = (1..=4).map(|c| r.sub(c, &[Topic::State], 0)).collect();
    assert_eq!(r.src.built(Snap::State), 4, "one per subscribe");
    r.src.rev.set(1);
    r.tick(5);
    assert_eq!(r.src.built(Snap::State), 5, "one look, one snapshot");
    for q in &qs {
        assert_eq!(summary(&drain(q)).last(), Some(&s("state", 1)));
    }
}

#[test]
fn a_connection_that_subscribes_late_is_not_sent_what_it_already_has() {
    let r = Rig::new();
    let early = r.sub(1, &[Topic::State], 0);
    drain(&early);
    r.src.rev.set(1);
    r.tick(60);
    drain(&early);
    // subscribes between two looks: its snapshot is the current one
    let late = r.sub(2, &[Topic::State], 70);
    assert_eq!(summary(&drain(&late)), [s("state", 1)]);
    r.src.rev.set(2);
    r.tick(130);
    assert_eq!(summary(&drain(&early)), [s("state", 2)]);
    assert_eq!(summary(&drain(&late)), [s("state", 2)]);
}

#[test]
fn a_second_subscribe_replaces_the_topics_and_sends_the_snapshots_again() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State, Topic::Peers], 0);
    assert_eq!(drain(&q).len(), 2);
    // events that waited for the old set are forgotten
    r.src.rev.set(1);
    r.src.other.set(1);
    r.tick(100);
    assert_eq!(q.len(), 2);
    r.hub
        .subscribe(&r.src, 1, &q, &[Topic::Layout, Topic::State], r.at(110));
    assert_eq!(summary(&drain(&q)), [s("state", 1), s("layout", 1)]);
    // peers no longer reaches it
    r.src.other.set(2);
    r.tick(200);
    assert_eq!(summary(&drain(&q)), [s("layout", 2)]);
    assert!(!r.hub.wants(Topic::Peers));
}

#[test]
fn a_reader_that_is_behind_finds_the_newest_snapshot_only() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State, Topic::Peers], 0);
    drain(&q);
    for i in 1..=10u32 {
        r.src.rev.set(i);
        r.src.other.set(i);
        r.tick(u64::from(i) * 60);
    }
    // ten changes of each, nobody read: one event of each topic waits, the newest
    assert_eq!(q.len(), 2);
    assert_eq!(summary(&drain(&q)), [s("state", 10), s("peers", 10)]);
    // the counter says nine of each were replaced
    assert_eq!(r.hub.superseded(), 18);
}

#[test]
fn a_slow_reader_does_not_slow_another() {
    let r = Rig::new();
    let slow = r.sub(1, &[Topic::State], 0);
    let quick = r.sub(2, &[Topic::State], 0);
    drain(&slow);
    drain(&quick);
    let mut seen = Vec::new();
    for i in 1..=5u32 {
        r.src.rev.set(i);
        r.tick(u64::from(i) * 60);
        seen.extend(summary(&drain(&quick)));
    }
    assert_eq!(
        seen,
        [
            s("state", 1),
            s("state", 2),
            s("state", 3),
            s("state", 4),
            s("state", 5)
        ]
    );
    assert_eq!(summary(&drain(&slow)), [s("state", 5)]);
}

fn code_event(stage: PairingStage, code: Option<&str>) -> PairingEvent {
    PairingEvent {
        stage,
        role: Some(ferry_core::api::PairingRole::Acceptor),
        peer: None,
        code: code.map(str::to_string),
        expires_ms: 100_000,
        reason: None,
        message: None,
        cancelled_by: None,
    }
}

#[test]
fn a_pairing_event_skips_the_connection_that_started_the_pairing() {
    let r = Rig::new();
    let owner = r.sub(1, &[Topic::Pairing], 0);
    let other = r.sub(2, &[Topic::Pairing, Topic::Ui], 0);
    let not_pairing = r.sub(3, &[Topic::State], 0);
    drain(&not_pairing);
    r.hub
        .publish_pairing(&code_event(PairingStage::Code, Some("418207")), Some(1));
    assert!(owner.is_empty(), "the owner gets its events another way");
    let got = drain(&other);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["event"], "pairing");
    assert_eq!(got[0]["data"]["code"], "418207");
    assert!(not_pairing.is_empty());
    // without an owner everybody gets it
    r.hub
        .publish_pairing(&code_event(PairingStage::WaitingPeer, None), None);
    assert_eq!(drain(&owner).len(), 1);
    assert_eq!(drain(&other).len(), 1);
}

#[test]
fn a_pairing_that_runs_is_sent_to_a_connection_that_subscribes_in_the_middle() {
    let r = Rig::new();
    let none = r.sub(1, &[Topic::Pairing], 0);
    assert!(none.is_empty(), "no pairing, no event");
    *r.src.pairing.borrow_mut() = Some(code_event(PairingStage::Code, Some("007003")));
    let late = r.sub(2, &[Topic::Pairing], 10);
    let got = drain(&late);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["data"]["stage"], "code");
    assert_eq!(got[0]["data"]["code"], "007003");
    // not asked for, not sent
    let q = r.sub(3, &[Topic::State], 10);
    assert_eq!(summary(&drain(&q)), [s("state", 0)]);
}

#[test]
fn notices_and_view_requests_reach_only_their_subscribers_in_order() {
    let r = Rig::new();
    let notices = r.sub(1, &[Topic::Notice], 0);
    let views = r.sub(2, &[Topic::Ui], 0);
    let both = r.sub(3, &[Topic::Notice, Topic::Ui], 0);
    let notice = |n: u32| Notice {
        kind: NoticeKind::PeerBack,
        title: format!("n{n}"),
        body: String::new(),
        peer: Some("lap".into()),
        urgency: Urgency::Normal,
    };
    r.hub.publish_notice(&notice(1));
    r.hub.publish_ui(UiView::Pair);
    r.hub.publish_notice(&notice(2));
    let titles = |q: &EventQueue| -> Vec<String> {
        drain(q)
            .iter()
            .map(|e| match e["event"].as_str().unwrap() {
                "notice" => e["data"]["title"].as_str().unwrap().to_string(),
                other => format!("{other}:{}", e["data"]["open"].as_str().unwrap()),
            })
            .collect()
    };
    assert_eq!(titles(&notices), ["n1", "n2"]);
    assert_eq!(titles(&views), ["ui:pair"]);
    assert_eq!(titles(&both), ["n1", "ui:pair", "n2"]);
}

#[test]
fn events_that_are_not_snapshots_wait_in_order_up_to_a_bound() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::Notice], 0);
    for i in 0..(EVENT_BACKLOG + 5) {
        r.hub.publish_notice(&Notice {
            kind: NoticeKind::Paired,
            title: format!("n{i}"),
            body: String::new(),
            peer: None,
            urgency: Urgency::Low,
        });
    }
    let got = drain(&q);
    assert_eq!(got.len(), EVENT_BACKLOG);
    // the oldest went
    assert_eq!(got[0]["data"]["title"], "n5");
    assert_eq!(
        got.last().unwrap()["data"]["title"],
        format!("n{}", EVENT_BACKLOG + 4)
    );
    assert_eq!(r.hub.superseded(), 5);
}

#[test]
fn unsubscribing_forgets_the_events_and_puts_the_hub_to_sleep() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State, Topic::Notice], 0);
    assert!(!q.is_empty());
    r.hub.unsubscribe(1);
    assert!(q.is_empty(), "what waited is forgotten");
    assert!(!r.hub.wants(Topic::State));
    r.hub.poke();
    assert_eq!(r.hub.flush(&r.src, r.at(10)), None);
    let built = r.src.built.get();
    r.hub.publish_notice(&Notice {
        kind: NoticeKind::Paired,
        title: String::new(),
        body: String::new(),
        peer: None,
        urgency: Urgency::Low,
    });
    assert!(q.is_empty());
    assert_eq!(r.src.built.get(), built);
    // unsubscribing twice, or a connection that never subscribed, is nothing
    r.hub.unsubscribe(1);
    r.hub.unsubscribe(99);
}

#[test]
fn the_hub_names_the_next_time_it_has_work() {
    let r = Rig::new();
    let q = r.sub(1, &[Topic::State, Topic::Peers], 0);
    drain(&q);
    // idle: the refresh of the noise is the only thing due
    assert_eq!(
        r.hub.flush(&r.src, r.at(10)),
        Some(r.at(REFRESH.as_millis() as u64))
    );
    // a change inside a window is due at the end of it
    r.src.other.set(1);
    r.tick(20);
    assert_eq!(drain(&q).len(), 1);
    r.src.other.set(2);
    assert_eq!(r.tick(30), Some(r.at(20) + COALESCE));
}

#[test]
fn an_event_line_is_what_serde_writes_for_the_event() {
    // the hub builds lines by hand to share one buffer between connections: they must be the
    // lines `EventLine` writes, or a client that parses with it would fail
    let ui = UiEvent { open: UiView::Pair };
    let by_hand = event_line(Topic::Ui, &serde_json::to_string(&ui).unwrap());
    let by_serde = serde_json::to_string(&EventLine::Ui(ui)).unwrap() + "\n";
    assert_eq!(&*by_hand, by_serde.as_bytes());

    let peers = PeersResult { peers: vec![] };
    let by_hand = event_line(Topic::Peers, &serde_json::to_string(&peers).unwrap());
    let by_serde = serde_json::to_string(&EventLine::Peers(peers)).unwrap() + "\n";
    assert_eq!(&*by_hand, by_serde.as_bytes());

    // and every topic's line parses back as the event of its name
    for topic in Topic::ALL {
        let line = event_line(topic, "null");
        let v: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["event"], topic.as_str());
    }
}

#[test]
fn the_queue_hands_events_out_in_order_and_waits_for_the_next() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let q = Rc::new(EventQueue::default());
        let line = |s: &str| Rc::<[u8]>::from(s.as_bytes());
        q.push(Topic::Notice, line("a\n"));
        q.push(Topic::Notice, line("b\n"));
        assert_eq!(&*q.next().await, b"a\n");
        assert_eq!(&*q.next().await, b"b\n");
    });
}

#[test]
fn an_empty_queue_makes_the_writer_wait_for_the_next_event() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let q = Rc::new(EventQueue::default());
        let q2 = q.clone();
        let writer = tokio::task::spawn_local(async move { q2.next().await });
        // the writer parks on the empty queue
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        assert!(!writer.is_finished());
        q.push(Topic::Ui, Rc::from(&b"x\n"[..]));
        assert_eq!(&*writer.await.unwrap(), b"x\n");
    });
}

#[test]
fn a_change_made_while_nobody_watches_does_not_wake_the_hub() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let signal = Arc::new(Changed::default());
        let woken = |signal: Arc<Changed>| async move {
            tokio::time::timeout(Duration::from_millis(40), signal.woken())
                .await
                .is_ok()
        };
        signal.changed();
        assert!(!woken(signal.clone()).await, "nobody watches: no wake-up");
        signal.watch(true);
        signal.changed();
        assert!(woken(signal.clone()).await, "a subscriber is woken");
        // one wake-up per beat, however many changes came in it
        signal.changed();
        signal.changed();
        assert!(woken(signal.clone()).await);
        assert!(!woken(signal.clone()).await);
        signal.watch(false);
        signal.changed();
        assert!(!woken(signal).await);
    });
}
