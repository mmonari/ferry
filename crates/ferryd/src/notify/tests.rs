//! The notify task against a [`Recorder`]: what the hooks send, what reaches the desktop, and
//! what comes back. The policy has its own tests (`policy/tests.rs`); the hooks in `ctl` are
//! driven through two daemons in `tests/notify.rs`.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferry_core::Micros;
use ferry_core::api::Attention;

use super::*;

/// Runs `f` on a current-thread runtime with a `LocalSet`, as the `ctl` thread does.
fn run<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(f))
}

/// Waits (up to 10 s) until `cond` holds, polling every 5 ms.
async fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

struct Harness {
    notifier: Notifier,
    recorder: Recorder,
    /// Every toast `on_toast_shown` would have heard.
    shown: Rc<RefCell<Vec<Toast>>>,
    /// Every view `on_notification_action` would have heard.
    acted: Rc<RefCell<Vec<UiView>>>,
    /// What `status.attention` says now; the test edits it.
    attention: Rc<RefCell<Vec<Attention>>>,
    /// How many times the task has read it.
    looks: Rc<Cell<usize>>,
    live: SharedLive,
    task: tokio::task::JoinHandle<()>,
}

/// The task as `spawn` builds it, without a `Ctl`: closures keep what the two hooks hear, and
/// stand in for `status.attention`.
fn harness(limits: Limits, enabled: bool, desktop: bool) -> Harness {
    harness_with(limits, enabled, desktop, Vec::new())
}

/// [`harness`] with `status.attention` holding `attention` when the task starts.
fn harness_with(
    limits: Limits,
    enabled: bool,
    desktop: bool,
    attention: Vec<Attention>,
) -> Harness {
    let list = Rc::new(RefCell::new(attention));
    let read = list.clone();
    let mut h = harness_reading(limits, enabled, desktop, move || {
        Some(read.borrow().clone())
    });
    h.attention = list;
    h
}

/// [`harness`] with `read` standing in for `status.attention`.
fn harness_reading(
    limits: Limits,
    enabled: bool,
    desktop: bool,
    read: impl Fn() -> Option<Vec<Attention>> + 'static,
) -> Harness {
    let live = SharedLive::default();
    let (notifier, rx) = channel(live.clone());
    let recorder = Recorder::new();
    let shown = Rc::new(RefCell::new(Vec::new()));
    let acted = Rc::new(RefCell::new(Vec::new()));
    let looks = Rc::new(Cell::new(0));
    let (s, a, n) = (shown.clone(), acted.clone(), looks.clone());
    let actor = Actor {
        policy: Policy::new(limits),
        sink: desktop.then(|| recorder.clone()),
        enabled,
        slot: None,
        reported: false,
        seen: Vec::new(),
        live: live.clone(),
        on_shown: move |t: &Toast| s.borrow_mut().push(t.clone()),
        on_action: move |v: UiView| a.borrow_mut().push(v),
        attention: move || {
            n.set(n.get() + 1);
            read()
        },
    };
    let task = tokio::task::spawn_local(actor.run(rx));
    Harness {
        notifier,
        recorder,
        shown,
        acted,
        attention: Rc::default(),
        looks,
        live,
        task,
    }
}

fn entry(code: AttentionCode, peer: Option<&str>) -> Attention {
    Attention {
        code,
        message: match peer {
            Some(p) => format!("{p} cannot connect: key changed"),
            None => "3 of 4 pointer barriers failed to set".into(),
        },
        hint: Some("`ferry doctor` has the details".into()),
        peer: peer.map(str::to_string),
    }
}

impl Harness {
    /// Pokes the task, as `ctl` does after a message, and waits until it has read the list.
    async fn look(&self) {
        let before = self.looks.get();
        self.notifier.poke();
        wait_for("a look at attention", || self.looks.get() > before).await;
    }

    fn counter(&self, name: &str) -> u64 {
        self.live.lock().counters.get(name).copied().unwrap_or(0)
    }

    fn kinds(&self) -> Vec<Kind> {
        self.shown.borrow().iter().map(|t| t.kind).collect()
    }
}

fn fp(n: u8) -> Fingerprint {
    Fingerprint([n; 32])
}

fn quick() -> Limits {
    Limits {
        connection: Micros::from_millis(60),
        attention: Micros::from_millis(60),
    }
}

fn code(h: &Harness) {
    h.notifier
        .pair_code(PairingRole::Acceptor, "lap", &PairCode(418_207), 100_000);
}

#[test]
fn the_layout_hook_reaches_the_desktop_in_the_frds_words() {
    run(async {
        let h = harness(Limits::default(), true, true);
        h.notifier.layout_replaced("lap");
        h.notifier.paired("tower", None);
        wait_for("two toasts", || h.recorder.summaries().len() == 2).await;
        assert_eq!(
            h.recorder.summaries(),
            ["Layout replaced by lap", "Paired with tower"]
        );
        assert_eq!(h.kinds(), [Kind::LayoutReplaced, Kind::Paired]);
        assert_eq!(h.counter("toasts_shown"), 2);
    });
}

#[test]
fn a_new_attention_entry_is_one_critical_toast_and_one_that_stays_is_not_another() {
    run(async {
        let h = harness(Limits::default(), true, true);
        // nothing at the first look
        h.look().await;
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::PeerRefused, Some("lap")));
        h.look().await;
        wait_for("the toast", || h.recorder.summaries().len() == 1).await;
        let Call::Show {
            summary,
            body,
            urgency,
            action,
            ..
        } = h.recorder.calls()[0].clone()
        else {
            panic!()
        };
        assert_eq!(summary, "lap cannot connect: key changed");
        assert_eq!(body, "`ferry doctor` has the details");
        assert_eq!((urgency, action), (Urgency::Critical, None));

        // it is still there at the next looks: not new
        h.look().await;
        h.look().await;
        // another peer's entry is another problem, and the sentinel behind which "not new" shows
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::PeerRefused, Some("tower")));
        h.look().await;
        wait_for("tower's", || h.recorder.summaries().len() == 2).await;
        assert_eq!(
            h.recorder.summaries()[1],
            "tower cannot connect: key changed"
        );

        // lap's goes and comes back at once: the same problem inside the five minutes
        h.attention.borrow_mut().remove(0);
        h.look().await;
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::PeerRefused, Some("lap")));
        h.look().await;
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::BarriersFailed, None));
        h.look().await;
        wait_for("the barriers", || h.recorder.summaries().len() == 3).await;
        assert_eq!(
            h.recorder.summaries()[2],
            "3 of 4 pointer barriers failed to set",
            "lap's came back inside five minutes: no toast"
        );
        assert_eq!(
            h.kinds(),
            [Kind::Attention, Kind::Attention, Kind::Attention]
        );
    });
}

#[test]
fn an_attention_entry_that_comes_back_after_the_window_is_a_new_toast() {
    run(async {
        let h = harness(quick(), true, true);
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::CaptureDenied, None));
        h.look().await;
        wait_for("the first", || h.recorder.summaries().len() == 1).await;
        h.attention.borrow_mut().clear();
        h.look().await;
        tokio::time::sleep(Duration::from_millis(80)).await; // the window is 60 ms
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::CaptureDenied, None));
        h.look().await;
        wait_for("the second", || h.recorder.summaries().len() == 2).await;
    });
}

#[test]
fn a_look_that_could_not_read_the_state_changes_nothing() {
    run(async {
        let readable = Rc::new(Cell::new(true));
        let r = readable.clone();
        // the window is 60 ms: an entry the task forgot would be a new toast after it
        let h = harness_reading(quick(), true, true, move || {
            r.get()
                .then(|| vec![entry(AttentionCode::CaptureDenied, None)])
        });
        wait_for("the first look", || h.recorder.summaries().len() == 1).await;
        readable.set(false);
        h.look().await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        readable.set(true);
        h.look().await;
        h.look().await;
        // the sentinel: another kind of toast, after which nothing else came
        h.notifier.layout_replaced("lap");
        wait_for("the sentinel", || h.recorder.summaries().len() == 2).await;
        assert_eq!(h.recorder.summaries()[1], "Layout replaced by lap");
    });
}

#[test]
fn what_is_in_attention_when_the_task_starts_is_toasted_and_a_thread_can_wake_the_task() {
    run(async {
        // the state ferryd starts with: a config file it could not read in full
        let h = harness_with(
            Limits::default(),
            true,
            true,
            vec![entry(AttentionCode::ConfigInvalid, None)],
        );
        wait_for("the first look", || h.recorder.summaries().len() == 1).await;
        // a thread that changes what `status` shows (the capture thread's gate) wakes it
        // without a poke from ctl
        h.attention
            .borrow_mut()
            .push(entry(AttentionCode::BarriersFailed, None));
        std::thread::scope(|scope| {
            scope.spawn(|| h.live.changed());
        });
        wait_for("the thread's change", || h.recorder.summaries().len() == 2).await;
    });
}

#[test]
fn a_pair_code_is_closed_when_the_pairing_moves_on_and_replaced_by_a_result() {
    run(async {
        let h = harness(Limits::default(), true, true);
        code(&h);
        wait_for("the code", || h.recorder.calls().len() == 1).await;
        let Call::Show {
            id: first,
            summary,
            urgency,
            action,
            replaces,
            expire_ms,
            ..
        } = h.recorder.calls()[0].clone()
        else {
            panic!()
        };
        assert_eq!(summary, "Pairing code 418 207");
        assert_eq!(urgency, Urgency::Critical);
        assert_eq!(action.as_deref(), Some("open"));
        assert_eq!(replaces, None);
        assert_eq!(expire_ms, Some(100_000));

        // the user answered here: the code is no use any more
        h.notifier.pairing_moved();
        wait_for("the close", || h.recorder.calls().len() == 2).await;
        assert_eq!(h.recorder.calls()[1], Call::Close { id: first });

        // a result while the code is still shown takes its place
        code(&h);
        wait_for("the second code", || h.recorder.calls().len() == 3).await;
        let Call::Show { id: second, .. } = h.recorder.calls()[2] else {
            panic!()
        };
        h.notifier.paired(
            "lap",
            Some("pinned as lap-2: the name lap belongs to another peer"),
        );
        wait_for("the result", || h.recorder.calls().len() == 4).await;
        let Call::Show {
            summary,
            body,
            urgency,
            action,
            replaces,
            ..
        } = h.recorder.calls()[3].clone()
        else {
            panic!()
        };
        assert_eq!(summary, "Paired with lap");
        assert_eq!(
            body,
            "Pinned as lap-2: the name lap belongs to another peer"
        );
        assert_eq!(
            (urgency, action.as_deref(), replaces),
            (Urgency::Normal, None, Some(second))
        );

        // it replaced the code toast, so there is nothing to close; sentinel behind it
        h.notifier.pairing_moved();
        h.notifier.layout_replaced("tower");
        wait_for("the sentinel", || h.recorder.calls().len() == 5).await;
        assert!(matches!(h.recorder.calls()[4], Call::Show { .. }));
    });
}

#[test]
fn a_dismissed_or_forgotten_code_toast_is_never_replaced_or_closed_by_its_old_id() {
    run(async {
        let h = harness(Limits::default(), true, true);
        // the user dismisses the code toast: the result is a notification of its own
        code(&h);
        wait_for("a code", || h.recorder.calls().len() == 1).await;
        let Call::Show { id, .. } = h.recorder.calls()[0] else {
            panic!()
        };
        h.recorder.end(id);
        h.notifier
            .pair_failed("rejected", "cancelled: codes differ");
        wait_for("the result", || h.recorder.calls().len() == 2).await;
        let Call::Show {
            summary,
            body,
            replaces,
            ..
        } = h.recorder.calls()[1].clone()
        else {
            panic!()
        };
        assert_eq!(
            (summary.as_str(), body.as_str()),
            ("Pairing cancelled", "Codes differ")
        );
        assert_eq!(replaces, None);

        // the notification daemon restarted: its ids start again, and the old one is another
        // application's now
        code(&h);
        wait_for("a second code", || h.recorder.calls().len() == 3).await;
        h.recorder.reset();
        h.notifier.pair_failed("timeout", "timed out");
        wait_for("the result", || h.recorder.calls().len() == 4).await;
        let Call::Show { replaces, .. } = h.recorder.calls()[3] else {
            panic!()
        };
        assert_eq!(replaces, None);
    });
}

#[test]
fn pressing_open_on_the_code_toast_asks_for_the_pair_view_once_per_press() {
    run(async {
        let h = harness(Limits::default(), true, true);
        code(&h);
        wait_for("the code", || h.recorder.calls().len() == 1).await;
        let Call::Show { id, .. } = h.recorder.calls()[0] else {
            panic!()
        };
        // not this notification, not this key: ignored
        h.recorder.press(id + 7, "open");
        h.recorder.press(id, "default");
        h.recorder.press(id, "open");
        wait_for("the view", || h.acted.borrow().len() == 1).await;
        h.recorder.press(id, "open");
        wait_for("the second press", || h.acted.borrow().len() == 2).await;
        assert_eq!(*h.acted.borrow(), [UiView::Pair, UiView::Pair]);
        assert_eq!(h.counter("notification_actions"), 2);
    });
}

#[test]
fn with_notify_off_every_toast_is_still_decided_and_heard_but_nothing_is_sent() {
    run(async {
        let h = harness(Limits::default(), false, true);
        h.notifier.layout_replaced("lap");
        wait_for("the toast", || h.shown.borrow().len() == 1).await;
        assert!(h.recorder.calls().is_empty());
        assert_eq!(h.counter("toasts_shown"), 1);
        // turned on live (`config.set notify.enabled`)
        h.notifier.set_enabled(true);
        h.notifier.layout_replaced("tower");
        wait_for("the second toast", || h.recorder.calls().len() == 1).await;
        assert_eq!(h.recorder.summaries(), ["Layout replaced by tower"]);
        assert_eq!(h.shown.borrow().len(), 2);
    });
}

#[test]
fn without_a_desktop_every_toast_is_still_heard() {
    run(async {
        let h = harness(Limits::default(), true, false);
        h.notifier.paired("lap", None);
        wait_for("the toast", || h.shown.borrow().len() == 1).await;
        assert!(h.recorder.calls().is_empty());
        assert_eq!(h.counter("toasts_undelivered"), 0);
    });
}

#[test]
fn a_notification_daemon_that_does_not_answer_costs_a_counter_and_is_found_again() {
    run(async {
        let h = harness(Limits::default(), true, true);
        h.recorder.set_unavailable(true);
        code(&h);
        h.notifier.layout_replaced("lap");
        wait_for("both heard", || h.shown.borrow().len() == 2).await;
        wait_for("both counted", || h.counter("toasts_undelivered") == 2).await;
        assert!(h.recorder.calls().is_empty());
        assert_eq!(h.counter("toasts_shown"), 2);

        // it comes back: the next toast is delivered, and the code toast that never was has
        // nothing to close
        h.recorder.set_unavailable(false);
        h.notifier.pairing_moved();
        h.notifier.paired("lap", None);
        wait_for("the delivery", || h.recorder.calls().len() == 1).await;
        let Call::Show {
            summary, replaces, ..
        } = h.recorder.calls()[0].clone()
        else {
            panic!()
        };
        assert_eq!((summary.as_str(), replaces), ("Paired with lap", None));
    });
}

#[test]
fn a_link_that_flaps_is_one_toast_and_the_settled_state_follows_the_window() {
    run(async {
        let h = harness(quick(), true, true);
        let (peer, started) = (fp(1), Instant::now());
        h.notifier.link(peer, "lap", &LinkState::Connected);
        h.notifier.link(peer, "lap", &LinkState::Offline);
        for _ in 0..4 {
            h.notifier.link(peer, "lap", &LinkState::Connected);
            h.notifier.link(peer, "lap", &LinkState::Offline);
        }
        h.notifier.link(peer, "lap", &LinkState::Connected);
        wait_for("the first toast", || !h.recorder.summaries().is_empty()).await;
        // the rest of the flap is inside the window: only "Lost lap" so far, and the settled
        // state comes when the window ends (60 ms after the first toast), by the task's timer
        wait_for("the settled toast", || h.recorder.summaries().len() == 2).await;
        assert_eq!(h.recorder.summaries(), ["Lost lap", "lap is back"]);
        assert!(
            started.elapsed() >= Duration::from_millis(55),
            "{:?}",
            started.elapsed()
        );
    });
}

#[test]
fn shutdown_takes_down_an_open_code_toast_and_stops_the_task() {
    run(async {
        let h = harness(Limits::default(), true, true);
        code(&h);
        wait_for("the code", || h.recorder.calls().len() == 1).await;
        let Call::Show { id, .. } = h.recorder.calls()[0] else {
            panic!()
        };
        h.notifier.shutdown().await;
        assert_eq!(h.recorder.calls()[1], Call::Close { id });
        h.task.await.unwrap();
    });
}

#[test]
fn a_full_queue_drops_the_toast_and_counts_it() {
    let live = SharedLive::default();
    let (notifier, rx) = channel(live.clone());
    // nobody reads: the task is stuck on a notification daemon that hangs
    for _ in 0..QUEUE_CAP + 8 {
        notifier.layout_replaced("lap");
    }
    assert_eq!(live.lock().counters["toast_queue_full"], 8);
    drop(rx);
    // a stopped task is not a full queue
    notifier.layout_replaced("lap");
    assert_eq!(live.lock().counters["toast_queue_full"], 8);
}

#[test]
fn only_a_change_of_a_link_is_an_event() {
    let (notifier, mut rx) = channel(SharedLive::default());
    let lap = fp(1);
    let sent = |rx: &mut mpsc::Receiver<Cmd>| {
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    };
    notifier.link(lap, "lap", &LinkState::Connecting);
    assert_eq!(sent(&mut rx), 0, "connecting is on the way, not a state");
    notifier.link(lap, "lap", &LinkState::Connected);
    assert_eq!(sent(&mut rx), 1);
    // net refreshes the RTT every 2 s
    notifier.link(lap, "lap", &LinkState::Connected);
    notifier.link(lap, "lap", &LinkState::Connecting);
    notifier.link(lap, "lap", &LinkState::Connected);
    assert_eq!(sent(&mut rx), 0);
    notifier.link(lap, "lap", &LinkState::Offline);
    notifier.link(lap, "lap", &LinkState::Refused);
    assert_eq!(sent(&mut rx), 1, "down is down, whatever the reason");
    notifier.link(lap, "lap", &LinkState::Connected);
    assert_eq!(sent(&mut rx), 1);
    // an unpair forgets it: the same state is news again
    notifier.forget_peer(lap);
    assert_eq!(sent(&mut rx), 1);
    notifier.link(lap, "lap", &LinkState::Connected);
    assert_eq!(sent(&mut rx), 1);
}

#[test]
fn the_debug_of_everything_that_carries_the_code_hides_it() {
    let (notifier, mut rx) = channel(SharedLive::default());
    notifier.pair_code(PairingRole::Acceptor, "lap", &PairCode(418_207), 1_000);
    let queued = format!("{:?}", rx.try_recv().unwrap());
    assert!(
        !queued.contains("418") && !queued.contains("207"),
        "{queued}"
    );
    assert!(queued.contains("Secret(..)"), "{queued}");
}

#[test]
fn nothing_reaches_a_bus_unless_the_caller_chose_one() {
    // RG-SAFE-03's rule for the desktop: a library user and every test get `Off`
    assert!(matches!(Options::default().mode, Mode::Off));
    let tmp = tempfile::tempdir().unwrap();
    let opts = crate::Options::new(crate::Paths::under_home(tmp.path(), tmp.path()));
    assert!(matches!(opts.toasts.mode, Mode::Off));
}

// --- attention, from a real State -----------------------------------------------------------------

/// A State as stream B's tests build one: two pinned peers, `a` and `b`, both connected.
fn a_state(capture_on: bool) -> crate::state::State {
    use crate::chan::LinkUpdate;
    use crate::state::{Me, State};
    let me = Fingerprint([0xd0; 32]);
    let mut trust = crate::trust_store::TrustStore::default();
    for (fp, name) in [(fp(0x0a), "a"), (fp(0x0b), "b")] {
        trust.upsert(fp, Some(name), None).unwrap();
    }
    let mut s = State {
        me: Me {
            name: "desk".into(),
            fp: me,
            id: crate::layout_store::peer_id(me),
        },
        port: 4243,
        config: crate::config::Config::default(),
        saved: crate::config::Config::default(),
        config_warnings: Vec::new(),
        trust,
        layout: crate::layout_store::LayoutStore::empty(crate::layout_store::peer_id(me)),
        edits: crate::layout_sync::EditLog::default(),
        own_monitors: Vec::new(),
        links: Default::default(),
        refusals: Vec::new(),
        refusals_warned: std::collections::HashSet::new(),
        place_on_up: std::collections::HashSet::new(),
        counters: Default::default(),
        last_release: None,
        live: SharedLive::default(),
        capture_on,
        locked: false,
    };
    for peer in [fp(0x0a), fp(0x0b)] {
        s.apply_link(LinkUpdate {
            peer,
            state: LinkState::Connected,
            remote: None,
            rtt_us: None,
            proto: None,
            detail: None,
        });
    }
    s
}

/// Every problem `status` can name is a critical toast, and the same words are the `notice`:
/// each of the five codes, from the conditions that make `State::status` build it.
#[test]
fn every_attention_code_that_status_builds_is_a_toast_and_a_notice() {
    use crate::chan::LinkUpdate;
    use ferry_core::api::NoticeKind;
    use ferry_core::source::{Gate, SourceRole, SourceStatus};

    run(async {
        let state = Rc::new(RefCell::new(a_state(true)));
        let read = state.clone();
        let h = harness_reading(Limits::default(), true, true, move || {
            Some(read.borrow().status().attention)
        });
        h.look().await;
        assert!(h.shown.borrow().is_empty(), "a healthy machine has nothing");

        // peer_refused: a pinned peer's link is refused
        state.borrow_mut().apply_link(LinkUpdate {
            peer: fp(0x0a),
            state: LinkState::Refused,
            remote: None,
            rtt_us: None,
            proto: Some("2.0".into()),
            detail: Some("version mismatch: 1.0 vs 2.0".into()),
        });
        h.look().await;
        wait_for("peer_refused", || h.shown.borrow().len() == 1).await;

        // barriers_failed: the portal said so, and the machine's gate agrees
        {
            let s = state.borrow();
            {
                let mut l = s.live.lock();
                l.barriers = 3;
                l.failed_barriers = vec![2];
            }
            s.live.source_status(SourceStatus {
                role: SourceRole::Local,
                peer: None,
                session: 0,
                last_leave: None,
                gate: Gate::BarriersFailed,
                suspect: vec![],
                controlled: false,
                locked_here: false,
                locked_peers: vec![],
                edge: None,
            });
        }
        h.look().await;
        wait_for("barriers_failed", || h.shown.borrow().len() == 2).await;

        // capture_denied: Hyprland will ask at the first crossing
        state.borrow().live.lock().enforce_permissions = Some(true);
        h.look().await;
        wait_for("capture_denied", || h.shown.borrow().len() == 3).await;

        // layout_unplaced: b has told its monitors and nobody placed it
        state.borrow_mut().layout.set_known(
            crate::layout_store::peer_id(fp(0x0b)),
            vec![ferry_core::geom::MonitorInfo {
                name: "eDP-1".into(),
                description: String::new(),
                rect: ferry_core::geom::WireRect {
                    x: 0,
                    y: 0,
                    w: 1366,
                    h: 768,
                },
                scale: 1.0,
                transform: 0,
                shift: None,
            }],
        );
        h.look().await;
        wait_for("layout_unplaced", || h.shown.borrow().len() == 4).await;

        // config_invalid: what the loader said about the file
        state.borrow_mut().config_warnings = vec!["unknown key peer.nmae".into()];
        h.look().await;
        wait_for("config_invalid", || h.shown.borrow().len() == 5).await;

        // all five, critical, in the order they came, with the words `status` gives the entry
        let status = state.borrow().status().attention;
        let said = |code: AttentionCode| {
            status
                .iter()
                .find(|a| a.code == code)
                .map(|a| (a.message.clone(), a.hint.clone().unwrap_or_default()))
                .unwrap()
        };
        let shown = h.shown.borrow().clone();
        let got: Vec<_> = shown
            .iter()
            .map(|t| (t.title.clone(), t.body.clone()))
            .collect();
        assert_eq!(
            got,
            [
                said(AttentionCode::PeerRefused),
                said(AttentionCode::BarriersFailed),
                said(AttentionCode::CaptureDenied),
                said(AttentionCode::LayoutUnplaced),
                said(AttentionCode::ConfigInvalid),
            ]
        );
        assert_eq!(
            shown[0].title,
            "a cannot connect: version mismatch: 1.0 vs 2.0"
        );
        assert_eq!(shown[3].title, "b is not placed in the layout");
        for toast in &shown {
            assert_eq!(toast.kind, Kind::Attention);
            assert_eq!(toast.urgency, Urgency::Critical);
            let notice = toast.notice();
            assert_eq!(notice.kind, NoticeKind::Attention);
            assert_eq!(notice.title, toast.title);
            assert_eq!(notice.body, toast.body);
        }
        assert_eq!(h.recorder.summaries().len(), 5);
    });
}
