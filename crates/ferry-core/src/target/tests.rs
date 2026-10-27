//! TargetMachine tests (`target::*`).

use proptest::prelude::*;

use super::*;
use crate::testkit::{desk, netbook, third};
use crate::wire::Motion;

const KEYMAP: &str = "xkb_keymap { desk };";

struct Rig {
    m: TargetMachine,
    out: Actions<TargetAction>,
    now: Micros,
}

impl Rig {
    fn new() -> Self {
        let mut r = Rig {
            m: TargetMachine::new(),
            out: Actions::new(),
            now: Micros::from_millis(1000),
        };
        r.ev(TargetEvent::Start);
        r
    }

    /// Feed one event at the current time; return its actions without
    /// `Status` and `Controlled` (the bracket has its own tests).
    fn ev(&mut self, ev: TargetEvent) -> Vec<TargetAction> {
        self.ev_all(ev)
            .into_iter()
            .filter(|a| !matches!(a, TargetAction::Controlled(_)))
            .collect()
    }

    /// Like [`Rig::ev`], with `Controlled`.
    fn ev_all(&mut self, ev: TargetEvent) -> Vec<TargetAction> {
        self.m.handle(ev, self.now, &mut self.out);
        self.out
            .drain()
            .filter(|a| !matches!(a, TargetAction::Status(_)))
            .collect()
    }

    fn at_ms(&mut self, ms: u64) -> &mut Self {
        self.now = Micros::from_millis(1000 + ms);
        self
    }

    fn ctl(&mut self, peer: PeerId, msg: Control) -> Vec<TargetAction> {
        self.ev(TargetEvent::Control { peer, msg })
    }

    fn keymap(&mut self, peer: PeerId, serial: u32) -> Vec<TargetAction> {
        self.ctl(
            peer,
            Control::Keymap {
                serial,
                format: 1,
                text: format!("{KEYMAP} {serial}"),
            },
        )
    }

    fn enter(&mut self, peer: PeerId, session: u32) -> Vec<TargetAction> {
        self.ctl(
            peer,
            Control::Enter {
                session,
                x: 523.0,
                y: 0.0,
                keymap_serial: 1,
                motion_seq: 10,
            },
        )
    }

    /// Keymap 1 from `peer`, then `Enter` session `session`.
    fn entered(peer: PeerId, session: u32) -> Self {
        let mut r = Rig::new();
        r.ev(TargetEvent::PeerUp(peer));
        r.keymap(peer, 1);
        r.enter(peer, session);
        r
    }
}

fn pos(motion_seq: u32, x: f32, y: f32) -> Pos {
    Pos { motion_seq, x, y }
}

fn key(session: u32, code: u16, pressed: bool) -> Control {
    Control::Key {
        session,
        code,
        pressed,
    }
}

fn held(session: u32, n: u32, keys: Vec<u16>, buttons: Vec<u16>) -> Control {
    Control::Held {
        session,
        n,
        keys,
        buttons,
        pos: pos(0, 0.0, 0.0),
    }
}

fn inject(c: InjectCmd) -> TargetAction {
    TargetAction::Inject(c)
}

fn ended(peer: PeerId, session: u32, reason: LeaveReason) -> TargetAction {
    TargetAction::Send {
        peer,
        msg: Control::Ended {
            session,
            reason: reason.code(),
        },
    }
}

#[test]
fn start_resets_modifiers() {
    let mut m = TargetMachine::new();
    let mut out = Actions::new();
    m.handle(TargetEvent::Start, Micros::ZERO, &mut out);
    assert_eq!(
        out.as_slice().first(),
        Some(&inject(InjectCmd::ModifierReset))
    );
    assert_eq!(m.next_deadline(), None);
}

#[test]
fn enter_applies_keymap_and_position() {
    let mut r = Rig::new();
    assert_eq!(
        r.keymap(desk(), 1),
        vec![inject(InjectCmd::SetKeymap(format!("{KEYMAP} 1")))]
    );
    // Already applied while idle: the Enter only moves the pointer.
    assert_eq!(
        r.enter(desk(), 1),
        vec![inject(InjectCmd::Motion { x: 523.0, y: 0.0 })]
    );
    assert_eq!(r.m.status().entered_by, Some(desk()));
    assert_eq!(
        r.m.next_deadline(),
        Some(r.now.plus(Micros::from_millis(650)))
    );
}

#[test]
fn enter_refused_without_keymap_or_injection() {
    let mut r = Rig::new();
    assert_eq!(
        r.enter(desk(), 1),
        vec![ended(desk(), 1, LeaveReason::Refused)]
    );
    r.keymap(desk(), 1);
    // A serial that was never sent.
    assert_eq!(
        r.ctl(
            desk(),
            Control::Enter {
                session: 2,
                x: 0.0,
                y: 0.0,
                keymap_serial: 7,
                motion_seq: 0
            }
        ),
        vec![ended(desk(), 2, LeaveReason::Refused)]
    );
    // A peer without injection (ferryd's `AcceptEnters(false)`) refuses
    // every Enter.
    r.ev(TargetEvent::AcceptEnters(false));
    assert_eq!(
        r.enter(desk(), 3),
        vec![ended(desk(), 3, LeaveReason::Refused)]
    );
    assert_eq!(r.m.counters().refused, 3);
    r.ev(TargetEvent::AcceptEnters(true));
    assert!(
        r.enter(desk(), 4)
            .contains(&inject(InjectCmd::Motion { x: 523.0, y: 0.0 }))
    );
    // Turning it off while entered releases and refuses the session.
    assert_eq!(
        r.ev_all(TargetEvent::AcceptEnters(false)),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 4, LeaveReason::Refused),
            TargetAction::Controlled(false),
        ]
    );
}

#[test]
fn watchdog_650() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(desk(), key(1, 42, true));
    let t0 = r.now;
    r.now = t0.plus(Micros(649_999));
    assert!(r.ev(TargetEvent::Tick).is_empty());
    r.now = t0.plus(Micros::from_millis(650));
    assert_eq!(r.m.next_deadline(), Some(r.now));
    assert_eq!(
        r.ev(TargetEvent::Tick),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 1, LeaveReason::Watchdog)
        ]
    );
    assert_eq!(r.m.status().entered_by, None);
    assert_eq!(r.m.counters().watchdog_releases, 1);
    // A snapshot postpones it.
    let mut r = Rig::entered(desk(), 1);
    r.at_ms(600).ctl(desk(), held(1, 2, vec![], vec![]));
    r.at_ms(1249);
    assert!(r.ev(TargetEvent::Tick).is_empty());
    r.at_ms(1250);
    assert!(
        r.ev(TargetEvent::Tick)
            .contains(&inject(InjectCmd::ReleaseAll))
    );
}

#[test]
fn snapshot_releases_missing() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(desk(), key(1, 125, true));
    r.ctl(desk(), key(1, 42, true));
    r.ctl(
        desk(),
        Control::Button {
            session: 1,
            code: 272,
            pressed: true,
            pos: pos(10, 523.0, 0.0),
        },
    );
    // The snapshot lost Shift and the button and names a key never pressed
    // here: release only, never press.
    let a = r.ctl(desk(), held(1, 2, vec![30, 125], vec![]));
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::Key {
                code: 42,
                pressed: false
            }),
            inject(InjectCmd::Button {
                code: 272,
                pressed: false
            }),
            TargetAction::Send {
                peer: desk(),
                msg: Control::HeldAck { session: 1, n: 2 }
            },
        ]
    );
    assert_eq!(r.m.counters().snapshot_releases, 2);
}

#[test]
fn orphan_release_dropped() {
    let mut r = Rig::entered(desk(), 1);
    // Super's release after a drag crossing, never seen pressed.
    assert!(r.ctl(desk(), key(1, 125, false)).is_empty());
    assert_eq!(r.m.counters().orphan_releases, 1);
    // A double press injects once.
    assert_eq!(r.ctl(desk(), key(1, 30, true)).len(), 1);
    assert!(r.ctl(desk(), key(1, 30, true)).is_empty());
}

fn motion(session_lo: u16, seq: u32, x: f32) -> TargetEvent {
    TargetEvent::Datagram {
        peer: desk(),
        dgram: Datagram::Motion(Motion {
            session_lo,
            seq,
            x,
            y: 5.0,
            dx: 1.0,
            dy: 0.0,
            t_us: 0,
        }),
    }
}

#[test]
fn latest_wins() {
    let mut r = Rig::entered(desk(), 1);
    // The Enter fenced at seq 10.
    assert!(r.ev(motion(1, 10, 1.0)).is_empty());
    assert_eq!(
        r.ev(motion(1, 12, 2.0)),
        vec![inject(InjectCmd::Motion { x: 2.0, y: 5.0 })]
    );
    // Older seq, wrong session, other peer: dropped.
    assert!(r.ev(motion(1, 11, 3.0)).is_empty());
    assert!(r.ev(motion(2, 13, 4.0)).is_empty());
    assert!(
        r.ev(TargetEvent::Datagram {
            peer: third(),
            dgram: Datagram::Motion(Motion {
                session_lo: 1,
                seq: 14,
                x: 0.0,
                y: 0.0,
                dx: 0.0,
                dy: 0.0,
                t_us: 0
            })
        })
        .is_empty()
    );
    assert_eq!(r.m.counters().stale_datagrams, 4);
    // Ping and Pong never reach the machine; if they do, nothing happens.
    assert!(
        r.ev(TargetEvent::Datagram {
            peer: desk(),
            dgram: Datagram::Ping { id: 1, t_us: 0 }
        })
        .is_empty()
    );
}

#[test]
fn fence_before_click() {
    let mut r = Rig::entered(desk(), 1);
    r.ev(motion(1, 11, 100.0));
    // Seq 14 never arrived; the click carries it.
    let a = r.ctl(
        desk(),
        Control::Button {
            session: 1,
            code: 272,
            pressed: true,
            pos: pos(14, 300.0, 200.0),
        },
    );
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::Motion { x: 300.0, y: 200.0 }),
            inject(InjectCmd::Button {
                code: 272,
                pressed: true
            }),
        ]
    );
    // The late datagrams 12–14 are now stale.
    assert!(r.ev(motion(1, 13, 250.0)).is_empty());
    assert!(r.ev(motion(1, 14, 300.0)).is_empty());
    // A fence that is not newer does not move the pointer.
    let a = r.ctl(
        desk(),
        Control::Scroll {
            session: 1,
            axis: 0,
            source: 0,
            value: 15.0,
            value120: Some(120),
            stop: false,
            pos: pos(14, 300.0, 200.0),
        },
    );
    assert_eq!(
        a,
        vec![inject(InjectCmd::Scroll {
            axis: 0,
            source: 0,
            value: 15.0,
            value120: Some(120)
        })]
    );
    let a = r.ctl(
        desk(),
        Control::Scroll {
            session: 1,
            axis: 1,
            source: 1,
            value: 0.0,
            value120: None,
            stop: true,
            pos: pos(15, 1.0, 2.0),
        },
    );
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::Motion { x: 1.0, y: 2.0 }),
            inject(InjectCmd::ScrollStop { axis: 1, source: 1 })
        ]
    );
}

#[test]
fn leave_any_reason_release_all() {
    for code in 0..=20u8 {
        let mut r = Rig::entered(desk(), 1);
        r.ctl(desk(), key(1, 29, true));
        let a = r.ctl(
            desk(),
            Control::Leave {
                session: 1,
                reason: code,
            },
        );
        assert_eq!(a, vec![inject(InjectCmd::ReleaseAll)], "reason {code}");
        assert_eq!(r.m.status().entered_by, None);
        assert_eq!(r.m.status().last_end, Some(LeaveReason::from_code(code)));
    }
    // A Leave for another session is ignored.
    let mut r = Rig::entered(desk(), 2);
    assert!(
        r.ctl(
            desk(),
            Control::Leave {
                session: 1,
                reason: 1
            }
        )
        .is_empty()
    );
    assert_eq!(r.m.status().entered_by, Some(desk()));
}

#[test]
fn preempt() {
    let mut r = Rig::entered(desk(), 5);
    r.ctl(desk(), key(5, 42, true));
    r.ev(TargetEvent::PeerUp(third()));
    r.ctl(
        third(),
        Control::Keymap {
            serial: 1,
            format: 1,
            text: "xkb_keymap { third };".into(),
        },
    );
    let a = r.enter(third(), 1);
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 5, LeaveReason::Preempted),
            inject(InjectCmd::SetKeymap("xkb_keymap { third };".into())),
            inject(InjectCmd::Motion { x: 523.0, y: 0.0 }),
        ]
    );
    assert_eq!(r.m.status().entered_by, Some(third()));
    // The preempted source's in-flight frames get no second answer.
    assert!(r.ctl(desk(), key(5, 42, false)).is_empty());
    // The same source re-entering in a newer session releases first.
    let a = r.ctl(
        third(),
        Control::Enter {
            session: 2,
            x: 1.0,
            y: 1.0,
            keymap_serial: 1,
            motion_seq: 0,
        },
    );
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::ReleaseAll),
            inject(InjectCmd::Motion { x: 1.0, y: 1.0 })
        ]
    );
}

#[test]
fn stale_answered_once() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(
        desk(),
        Control::Leave {
            session: 1,
            reason: 1,
        },
    );
    // A source that kept running with session 2 after our watchdog.
    let a = r.ctl(desk(), key(2, 30, true));
    assert_eq!(a, vec![ended(desk(), 2, LeaveReason::Stale)]);
    for _ in 0..10 {
        assert!(r.ctl(desk(), key(2, 30, false)).is_empty());
        assert!(r.ctl(desk(), held(2, 3, vec![], vec![])).is_empty());
    }
    assert_eq!(r.m.counters().stale_frames, 21);
    // A new session that is also stale gets its own answer.
    assert_eq!(
        r.ctl(desk(), key(3, 30, true)),
        vec![ended(desk(), 3, LeaveReason::Stale)]
    );
    // After the watchdog, the timed-out session is not answered twice.
    let mut r = Rig::entered(desk(), 1);
    r.at_ms(700);
    assert!(
        r.ev(TargetEvent::Tick)
            .contains(&ended(desk(), 1, LeaveReason::Watchdog))
    );
    assert!(r.ctl(desk(), held(1, 2, vec![], vec![])).is_empty());
}

#[test]
fn keymap_swap() {
    let mut r = Rig::entered(desk(), 1);
    // A new serial while entered → SetKeymap.
    assert_eq!(
        r.keymap(desk(), 2),
        vec![inject(InjectCmd::SetKeymap(format!("{KEYMAP} 2")))]
    );
    // Unchanged → nothing.
    assert!(r.keymap(desk(), 2).is_empty());
    // Another peer's keymap while entered by desk: stored, not applied.
    r.ev(TargetEvent::PeerUp(third()));
    assert!(r.keymap(third(), 1).is_empty());
    // Format other than 1: dropped.
    assert!(
        r.ctl(
            desk(),
            Control::Keymap {
                serial: 3,
                format: 2,
                text: "x".into()
            }
        )
        .is_empty()
    );
    assert_eq!(r.m.counters().keymap_swaps, 2);
}

#[test]
fn an_identical_keymap_is_not_a_swap() {
    let mut r = Rig::entered(desk(), 1);
    let swaps = r.m.counters().keymap_swaps;
    // The same text under a new serial (a source that re-sent it), or from
    // another peer, or again after the peer reconnected: the keyboard
    // already has it, so nothing goes to ferry-inject and nothing counts.
    let same = |serial| Control::Keymap {
        serial,
        format: 1,
        text: format!("{KEYMAP} 1"),
    };
    assert!(r.ctl(desk(), same(2)).is_empty());
    assert!(r.ctl(desk(), same(3)).is_empty());
    r.ctl(
        desk(),
        Control::Leave {
            session: 1,
            reason: 1,
        },
    );
    r.ev(TargetEvent::PeerUp(third()));
    assert!(r.ctl(third(), same(9)).is_empty());
    r.ev(TargetEvent::PeerDown(desk()));
    r.ev(TargetEvent::PeerUp(desk()));
    assert!(r.ctl(desk(), same(1)).is_empty());
    // Enter with it applies nothing either.
    let a = r.ctl(
        desk(),
        Control::Enter {
            session: 2,
            x: 1.0,
            y: 1.0,
            keymap_serial: 1,
            motion_seq: 0,
        },
    );
    assert_eq!(a, vec![inject(InjectCmd::Motion { x: 1.0, y: 1.0 })]);
    assert_eq!(r.m.counters().keymap_swaps, swaps);
    // A real change still swaps.
    assert_eq!(
        r.keymap(desk(), 4),
        vec![inject(InjectCmd::SetKeymap(format!("{KEYMAP} 4")))]
    );
    assert_eq!(r.m.counters().keymap_swaps, swaps + 1);
}

#[test]
fn peer_down_releases_and_forgets_keymap() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(desk(), key(1, 30, true));
    assert_eq!(
        r.ev(TargetEvent::PeerDown(desk())),
        vec![inject(InjectCmd::ReleaseAll)]
    );
    // A restarted source reuses serial 1 with other text: re-applied.
    r.ev(TargetEvent::PeerUp(desk()));
    let other = Control::Keymap {
        serial: 1,
        format: 1,
        text: "xkb_keymap { de };".into(),
    };
    assert_eq!(
        r.ctl(desk(), other),
        vec![inject(InjectCmd::SetKeymap("xkb_keymap { de };".into()))]
    );
    // Without a new keymap, Enter is refused.
    r.ev(TargetEvent::PeerDown(desk()));
    assert_eq!(
        r.enter(desk(), 1),
        vec![ended(desk(), 1, LeaveReason::Refused)]
    );
}

#[test]
fn peer_down_ends_with_link_lost() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(
        desk(),
        Control::Leave {
            session: 1,
            reason: LeaveReason::Home.code(),
        },
    );
    assert_eq!(r.m.status().last_end, Some(LeaveReason::Home));
    r.enter(desk(), 2);
    // Another peer going down leaves the session alone.
    r.ev(TargetEvent::PeerUp(third()));
    assert!(r.ev(TargetEvent::PeerDown(third())).is_empty());
    assert_eq!(r.m.status().entered_by, Some(desk()));
    // The driving source's link is lost: release, no `Ended`, and `last_end`
    // says why (exit check rows 13–14: it kept the previous `home`).
    let a = r.ev_all(TargetEvent::PeerDown(desk()));
    assert_eq!(
        a,
        vec![
            inject(InjectCmd::ReleaseAll),
            TargetAction::Controlled(false)
        ]
    );
    let s = r.m.status();
    assert_eq!((s.entered_by, s.session), (None, 2));
    assert_eq!(s.last_end, Some(LeaveReason::LinkLost));
    // Idle: a later PeerDown changes nothing.
    r.ev(TargetEvent::PeerUp(desk()));
    r.ev(TargetEvent::PeerDown(desk()));
    assert_eq!(r.m.status().last_end, Some(LeaveReason::LinkLost));
}

#[test]
fn force_and_shutdown() {
    let mut r = Rig::entered(desk(), 1);
    assert_eq!(
        r.ev(TargetEvent::ForceRelease),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 1, LeaveReason::Forced)
        ]
    );
    assert_eq!(
        r.ev(TargetEvent::ForceRelease),
        vec![inject(InjectCmd::ReleaseAll)]
    );
    let mut r = Rig::entered(desk(), 1);
    assert_eq!(
        r.ev(TargetEvent::Shutdown),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 1, LeaveReason::Shutdown)
        ]
    );
}

#[test]
fn inject_reset_clears_held() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(desk(), key(1, 30, true));
    r.ev(TargetEvent::InjectReset);
    // ferry-inject released 30 already: a press injects again.
    assert_eq!(r.ctl(desk(), key(1, 30, true)).len(), 1);
}

#[test]
fn status_is_published_on_change() {
    let mut m = TargetMachine::new();
    let mut out = Actions::new();
    m.handle(TargetEvent::Start, Micros::ZERO, &mut out);
    out.drain();
    m.handle(
        TargetEvent::Control {
            peer: netbook(),
            msg: Control::Keymap {
                serial: 1,
                format: 1,
                text: "k".into(),
            },
        },
        Micros::ZERO,
        &mut out,
    );
    out.drain();
    m.handle(
        TargetEvent::Control {
            peer: netbook(),
            msg: Control::Enter {
                session: 9,
                x: 0.0,
                y: 0.0,
                keymap_serial: 1,
                motion_seq: 0,
            },
        },
        Micros::ZERO,
        &mut out,
    );
    assert!(out.as_slice().contains(&TargetAction::Status(TargetStatus {
        entered_by: Some(netbook()),
        entry: Some((0.0, 0.0)),
        session: 9,
        last_end: None
    })));
}

// ---- property: never_holds_past_watchdog -----------------------------------

#[derive(Debug, Clone)]
enum Step {
    Wait(u64),
    Enter(u32),
    Leave(u32),
    Key(u32, u16, bool),
    Button(u32, u16, bool),
    Held(u32, Vec<u16>, Vec<u16>),
    Motion(u16, u32),
    Keymap(u32),
    PeerDown,
    Force,
    Reset,
}

fn arb_step() -> impl Strategy<Value = Step> {
    let s = 0u32..4;
    prop_oneof![
        4 => (0u64..400_000).prop_map(Step::Wait),
        1 => s.clone().prop_map(Step::Enter),
        1 => s.clone().prop_map(Step::Leave),
        4 => (s.clone(), 28u16..34, any::<bool>()).prop_map(|(a, b, c)| Step::Key(a, b, c)),
        2 => (s.clone(), 272u16..275, any::<bool>()).prop_map(|(a, b, c)| Step::Button(a, b, c)),
        3 => (
            s.clone(),
            proptest::collection::vec(28u16..34, 0..4),
            proptest::collection::vec(272u16..275, 0..2)
        )
            .prop_map(|(a, b, c)| Step::Held(a, b, c)),
        2 => (0u16..4, 0u32..64).prop_map(|(a, b)| Step::Motion(a, b)),
        1 => (0u32..3).prop_map(Step::Keymap),
        1 => Just(Step::PeerDown),
        1 => Just(Step::Force),
        1 => Just(Step::Reset),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Arbitrary events and times, with the timer fired at `next_deadline`
    /// (or late): nothing injected stays held past `last_snapshot + 650 ms`,
    /// and nothing is held while idle.
    #[test]
    fn never_holds_past_watchdog(steps in proptest::collection::vec(arb_step(), 1..120), late in 0u64..50_000) {
        let mut m = TargetMachine::new();
        let mut out = Actions::new();
        let mut now = Micros::ZERO;
        // What the virtual devices hold, from the injected commands.
        let mut down: Vec<(bool, u16)> = Vec::new();
        let p = desk();
        let mut feed = |m: &mut TargetMachine, ev: TargetEvent, now: Micros, down: &mut Vec<(bool, u16)>| {
            m.handle(ev, now, &mut out);
            for a in out.drain() {
                if let TargetAction::Inject(c) = a {
                    match c {
                        InjectCmd::Key { code, pressed: true } => down.push((false, code)),
                        InjectCmd::Key { code, pressed: false } => down.retain(|&d| d != (false, code)),
                        InjectCmd::Button { code, pressed: true } => down.push((true, code)),
                        InjectCmd::Button { code, pressed: false } => down.retain(|&d| d != (true, code)),
                        InjectCmd::ReleaseAll | InjectCmd::ModifierReset => down.clear(),
                        _ => {}
                    }
                }
            }
        };
        feed(&mut m, TargetEvent::Start, now, &mut down);
        feed(&mut m, TargetEvent::PeerUp(p), now, &mut down);
        // Start entered, so most cases exercise a live session.
        feed(&mut m, TargetEvent::Control { peer: p, msg: Control::Keymap { serial: 1, format: 1, text: "k".into() } }, now, &mut down);
        feed(&mut m, TargetEvent::Control { peer: p, msg: Control::Enter { session: 0, x: 0.0, y: 0.0, keymap_serial: 1, motion_seq: 0 } }, now, &mut down);
        for step in steps {
            if matches!(step, Step::Reset) {
                // ferry-inject has released everything before it reports.
                down.clear();
            }
            let ev = match step {
                Step::Wait(us) => {
                    let until = now.plus(Micros(us));
                    // The caller's timer: fires at the deadline, maybe late.
                    while let Some(d) = m.next_deadline() {
                        if d > until { break; }
                        let fire = d.plus(Micros(late)).min(until);
                        feed(&mut m, TargetEvent::Tick, fire, &mut down);
                        prop_assert!(m.next_deadline().is_none());
                        prop_assert!(down.is_empty());
                    }
                    now = until;
                    continue;
                }
                Step::Enter(s) => TargetEvent::Control { peer: p, msg: Control::Enter { session: s, x: 0.0, y: 0.0, keymap_serial: 1, motion_seq: 0 } },
                Step::Leave(s) => TargetEvent::Control { peer: p, msg: Control::Leave { session: s, reason: 1 } },
                Step::Key(s, code, pressed) => TargetEvent::Control { peer: p, msg: key(s, code, pressed) },
                Step::Button(s, code, pressed) => TargetEvent::Control { peer: p, msg: Control::Button { session: s, code, pressed, pos: pos(0, 0.0, 0.0) } },
                Step::Held(s, k, b) => TargetEvent::Control { peer: p, msg: held(s, 1, k, b) },
                Step::Motion(lo, seq) => motion(lo, seq, 1.0),
                Step::Keymap(serial) => TargetEvent::Control { peer: p, msg: Control::Keymap { serial, format: 1, text: "k".into() } },
                Step::PeerDown => TargetEvent::PeerDown(p),
                Step::Force => TargetEvent::ForceRelease,
                Step::Reset => TargetEvent::InjectReset,
            };
            feed(&mut m, ev, now, &mut down);
            match m.next_deadline() {
                // Entered: the deadline is still ahead.
                Some(d) => prop_assert!(now < d, "entered at {:?} past {:?}", now, d),
                // Idle: the virtual devices hold nothing.
                None => prop_assert!(down.is_empty(), "idle with {:?} held", down),
            }
            // The machine's own set never exceeds what was injected.
            for k in m.held.keys() { prop_assert!(down.contains(&(false, *k))); }
            for b in m.held.buttons() { prop_assert!(down.contains(&(true, *b))); }
        }
    }
}

// ---- both peers capture, lock, takeover -------

#[test]
fn controlled_brackets_a_session() {
    let mut r = Rig::new();
    r.ev(TargetEvent::PeerUp(desk()));
    r.keymap(desk(), 1);
    let a = r.ev_all(TargetEvent::Control {
        peer: desk(),
        msg: Control::Enter {
            session: 1,
            x: 523.0,
            y: 0.0,
            keymap_serial: 1,
            motion_seq: 10,
        },
    });
    // Control starts before anything is injected.
    assert_eq!(a.first(), Some(&TargetAction::Controlled(true)));
    // A preemption keeps control: no bracket.
    r.ev(TargetEvent::PeerUp(netbook()));
    r.keymap(netbook(), 1);
    let a = r.ev_all(TargetEvent::Control {
        peer: netbook(),
        msg: Control::Enter {
            session: 7,
            x: 1.0,
            y: 1.0,
            keymap_serial: 1,
            motion_seq: 0,
        },
    });
    assert!(!a.iter().any(|a| matches!(a, TargetAction::Controlled(_))));
    // The end: release, Ended, then Controlled(false).
    assert_eq!(
        r.ev_all(TargetEvent::ForceRelease),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(netbook(), 7, LeaveReason::Forced),
            TargetAction::Controlled(false),
        ]
    );
    // A Leave ends control too, after its release.
    let mut r = Rig::entered(desk(), 1);
    assert_eq!(
        r.ev_all(TargetEvent::Control {
            peer: desk(),
            msg: Control::Leave {
                session: 1,
                reason: LeaveReason::Home.code()
            }
        }),
        vec![
            inject(InjectCmd::ReleaseAll),
            TargetAction::Controlled(false)
        ]
    );
}

#[test]
fn source_busy_refuses_enter() {
    let mut r = Rig::new();
    r.ev(TargetEvent::PeerUp(desk()));
    r.keymap(desk(), 1);
    r.ev(TargetEvent::SourceBusy(true));
    assert_eq!(
        r.ev_all(TargetEvent::Control {
            peer: desk(),
            msg: Control::Enter {
                session: 1,
                x: 0.0,
                y: 0.0,
                keymap_serial: 1,
                motion_seq: 0
            }
        }),
        vec![ended(desk(), 1, LeaveReason::Refused)]
    );
    assert_eq!(r.m.status().entered_by, None);
    // Local again: the next Enter is taken.
    r.ev(TargetEvent::SourceBusy(false));
    assert!(
        r.enter(desk(), 2)
            .contains(&inject(InjectCmd::Motion { x: 523.0, y: 0.0 }))
    );
}

/// Regression for the m1-rest §5.1 correction. The built machine ended the
/// session (`Ended{refused}`, which sends the pointer home) and refused every
/// `Enter` on `LockToThisMachine(true)`; J5 step 2 says the pointer stays.
/// Lock to this machine no longer reaches the TargetMachine at all: what
/// ferryd's lock path feeds it leaves a session running, and a locked peer
/// may still be entered.
#[test]
fn lock_does_not_end_the_session() {
    let mut r = Rig::entered(desk(), 1);
    r.ctl(desk(), key(1, 42, true));
    for ev in [
        TargetEvent::AcceptEnters(true),
        TargetEvent::SourceBusy(false),
    ] {
        assert!(r.ev_all(ev).is_empty());
    }
    assert_eq!(r.m.status().entered_by, Some(desk()));
    // Input keeps flowing into the session.
    assert_eq!(
        r.ctl(desk(), key(1, 30, true)),
        vec![inject(InjectCmd::Key {
            code: 30,
            pressed: true
        })]
    );
    // Another source may enter (preempting), as on any peer.
    r.ev(TargetEvent::PeerUp(netbook()));
    r.keymap(netbook(), 1);
    let a = r.enter(netbook(), 5);
    assert!(a.contains(&ended(desk(), 1, LeaveReason::Preempted)));
    assert_eq!(r.m.status().entered_by, Some(netbook()));
}

/// Entered by the desktop at (523, 0) with the cursor poll on, past the
/// Enter's 250 ms grace, a snapshot just in (the watchdog is 650 ms away).
fn polling() -> Rig {
    let mut r = Rig::entered(desk(), 1);
    r.ev(TargetEvent::CursorPoll(true));
    r.now = r.now.plus(Micros::from_millis(250));
    r.ctl(desk(), held(1, 2, vec![], vec![]));
    r
}

/// Fire every deadline up to `ms` from now; count the cursor reads asked for.
fn advance(r: &mut Rig, ms: u64) -> usize {
    let until = r.now.plus(Micros::from_millis(ms));
    let mut reads = 0;
    while let Some(d) = r.m.next_deadline() {
        if d > until {
            break;
        }
        r.now = d.max(r.now);
        reads += r
            .ev(TargetEvent::Tick)
            .iter()
            .filter(|a| **a == TargetAction::CheckCursor)
            .count();
    }
    r.now = until;
    reads
}

/// Answer a cursor read; the actions without the next `CheckCursor` (a read
/// that falls due on the way is asked for on any event).
fn polled(r: &mut Rig, x: f64, y: f64) -> Vec<TargetAction> {
    r.ev_all(TargetEvent::CursorPolled(Some((x, y))))
        .into_iter()
        .filter(|a| *a != TargetAction::CheckCursor)
        .collect()
}

#[test]
fn takeover_needs_two_moving_polls() {
    let mut r = polling();
    // One read every 40 ms while entered.
    assert_eq!(advance(&mut r, 120), 3);
    // On the injected position: nothing.
    assert!(polled(&mut r, 523.0, 0.0).is_empty());
    // Off by 1.5 px (pixel-centre rounding): not suspect.
    assert!(polled(&mut r, 524.5, 1.5).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 0);
    // A warp: off once, then still → no takeover.
    assert!(polled(&mut r, 900.0, 400.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 1);
    // The recheck is asked for 16 ms later.
    assert_eq!(
        r.m.next_deadline(),
        Some(r.now.plus(Micros::from_millis(16)))
    );
    r.now = r.now.plus(Micros::from_millis(16));
    assert_eq!(r.ev(TargetEvent::Tick), vec![TargetAction::CheckCursor]);
    assert!(polled(&mut r, 900.0, 400.0).is_empty());
    assert_eq!(r.m.status().entered_by, Some(desk()));
    // Back on the injected position: the episode is over.
    assert!(polled(&mut r, 523.0, 0.0).is_empty());
    // A hand: two suspect reads that moved.
    r.now = r.now.plus(Micros::from_millis(40));
    assert!(polled(&mut r, 600.0, 10.0).is_empty());
    r.now = r.now.plus(Micros::from_millis(16));
    assert_eq!(
        polled(&mut r, 603.0, 12.0),
        vec![
            inject(InjectCmd::ReleaseAll),
            ended(desk(), 1, LeaveReason::Takeover),
            TargetAction::Controlled(false),
        ]
    );
    assert_eq!(r.m.counters().takeovers, 1);
    assert_eq!(r.m.counters().takeover_suspects, 2);
    assert_eq!(r.m.status().last_end, Some(LeaveReason::Takeover));
    // Idle: no deadline, no more reads.
    assert_eq!(r.m.next_deadline(), None);
}

#[test]
fn no_takeover_right_after_enter() {
    // The session's first motion can land late (devices created on demand).
    let mut r = Rig::entered(desk(), 1);
    r.ev(TargetEvent::CursorPoll(true));
    r.now = r.now.plus(Micros::from_millis(60));
    assert!(polled(&mut r, 100.0, 700.0).is_empty());
    r.now = r.now.plus(Micros::from_millis(16));
    assert!(polled(&mut r, 300.0, 400.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 0);
    // 250 ms after the Enter, the same reads would be suspect.
    r.now = r.now.plus(Micros::from_millis(174));
    assert!(polled(&mut r, 100.0, 700.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 1);
}

#[test]
fn no_takeover_after_injected_key() {
    let mut r = polling();
    r.now = r.now.plus(Micros::from_millis(100));
    // Super+Right moves focus to another monitor; Hyprland warps the cursor.
    r.ctl(desk(), key(1, 125, true));
    r.ctl(desk(), key(1, 106, true));
    r.now = r.now.plus(Micros::from_millis(100));
    assert!(polled(&mut r, 1500.0, 300.0).is_empty());
    r.now = r.now.plus(Micros::from_millis(16));
    assert!(polled(&mut r, 1510.0, 300.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 0);
    assert_eq!(r.m.status().entered_by, Some(desk()));
    // A button counts as a key (a snapshot keeps the watchdog away).
    r.now = r.now.plus(Micros::from_millis(300));
    r.ctl(desk(), held(1, 2, vec![125, 106], vec![]));
    r.ctl(
        desk(),
        Control::Button {
            session: 1,
            code: 0x110,
            pressed: true,
            pos: pos(0, 523.0, 0.0),
        },
    );
    r.now = r.now.plus(Micros::from_millis(249));
    assert!(polled(&mut r, 1500.0, 300.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 0);
    // 250 ms after the last edge, a jump is suspect again.
    r.now = r.now.plus(Micros::from_millis(1));
    assert!(polled(&mut r, 1500.0, 300.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 1);
}

#[test]
fn no_takeover_while_injecting() {
    let mut r = polling();
    // Motion in flight: the injected position moves every 8 ms, and the read
    // lags it.
    let mut seq = 11;
    for i in 0..10u32 {
        r.now = r.now.plus(Micros::from_millis(8));
        let x = 523.0 + 20.0 * f64::from(i);
        r.ev(TargetEvent::Datagram {
            peer: desk(),
            dgram: Datagram::Motion(Motion {
                session_lo: 1,
                seq,
                x: x as f32,
                y: 100.0,
                dx: 20.0,
                dy: 0.0,
                t_us: 0,
            }),
        });
        seq += 1;
        assert!(polled(&mut r, x - 40.0, 100.0).is_empty());
    }
    assert_eq!(r.m.counters().takeover_suspects, 0);
    // 30 ms after the last motion, a read off the injected position counts.
    r.now = r.now.plus(Micros::from_millis(30));
    assert!(polled(&mut r, 800.0, 100.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 1);
    // A position fence on a frame counts as injected motion too.
    r.ctl(
        desk(),
        Control::Scroll {
            session: 1,
            axis: 0,
            source: 0,
            value: 15.0,
            value120: Some(120),
            stop: false,
            pos: pos(seq + 5, 700.0, 50.0),
        },
    );
    r.now = r.now.plus(Micros::from_millis(10));
    assert!(polled(&mut r, 702.0, 52.0).is_empty());
    assert!(polled(&mut r, 760.0, 52.0).is_empty());
    assert_eq!(r.m.status().entered_by, Some(desk()));
}

#[test]
fn cursor_poll_is_off_until_ferryd_can_read_the_cursor() {
    let mut r = Rig::entered(desk(), 1);
    // Without CursorPoll(true) the only deadline is the watchdog.
    assert_eq!(r.m.next_deadline(), r.m.watchdog_deadline());
    assert_eq!(advance(&mut r, 600), 0);
    // An answer that arrives anyway changes nothing.
    assert!(polled(&mut r, 900.0, 400.0).is_empty());
    assert!(polled(&mut r, 950.0, 400.0).is_empty());
    assert_eq!(r.m.counters().takeover_suspects, 0);
}

// ---- properties: the bracket and the poll ----------------------------------

#[derive(Debug, Clone)]
enum Tk {
    Wait(u64),
    Enter(bool, u32),
    Leave(u32),
    Held(u32),
    Key(u32, bool),
    Motion(u32),
    PeerDown(bool),
    Force,
    Busy(bool),
    Accept(bool),
    Poll(bool),
    Polled(Option<(i16, i16)>),
    Start,
}

fn arb_tk() -> impl Strategy<Value = Tk> {
    let s = 0u32..3;
    prop_oneof![
        5 => (0u64..300_000).prop_map(Tk::Wait),
        3 => (any::<bool>(), s.clone()).prop_map(|(p, s)| Tk::Enter(p, s)),
        1 => s.clone().prop_map(Tk::Leave),
        3 => s.clone().prop_map(Tk::Held),
        2 => (s.clone(), any::<bool>()).prop_map(|(s, p)| Tk::Key(s, p)),
        3 => (0u32..64).prop_map(Tk::Motion),
        1 => any::<bool>().prop_map(Tk::PeerDown),
        1 => Just(Tk::Force),
        1 => any::<bool>().prop_map(Tk::Busy),
        1 => any::<bool>().prop_map(Tk::Accept),
        1 => any::<bool>().prop_map(Tk::Poll),
        4 => proptest::option::weighted(0.9, (-20i16..20, -20i16..20)).prop_map(Tk::Polled),
        1 => Just(Tk::Start),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Any events: `Controlled(true)` and `Controlled(false)` alternate,
    /// starting with `true`; control is on exactly while entered, so every
    /// session that started ends with `false`. A `CheckCursor` only ever
    /// leaves while entered, and an entered machine is always ahead of its
    /// watchdog, polls or not.
    #[test]
    fn controlled_brackets_every_session_and_polls_only_while_entered(
        steps in proptest::collection::vec(arb_tk(), 1..150),
    ) {
        let mut m = TargetMachine::new();
        let mut out = Actions::new();
        let mut now = Micros::ZERO;
        let mut controlled = false;
        let peers = [desk(), netbook()];
        let mut feed = |m: &mut TargetMachine, ev: TargetEvent, now: Micros, controlled: &mut bool| -> Result<(), TestCaseError> {
            m.handle(ev, now, &mut out);
            for a in out.drain() {
                match a {
                    TargetAction::Controlled(c) => {
                        prop_assert_ne!(c, *controlled, "Controlled({}) twice in a row", c);
                        *controlled = c;
                    }
                    TargetAction::CheckCursor => {
                        prop_assert!(m.status().entered_by.is_some(), "CheckCursor while idle");
                    }
                    _ => {}
                }
            }
            prop_assert_eq!(*controlled, m.status().entered_by.is_some());
            if let Some(w) = m.watchdog_deadline() {
                prop_assert!(now < w);
            }
            Ok(())
        };
        feed(&mut m, TargetEvent::Start, now, &mut controlled)?;
        for p in peers {
            feed(&mut m, TargetEvent::PeerUp(p), now, &mut controlled)?;
            feed(&mut m, TargetEvent::Control { peer: p, msg: Control::Keymap { serial: 1, format: 1, text: "k".into() } }, now, &mut controlled)?;
        }
        let peer = |second: bool| if second { netbook() } else { desk() };
        for step in steps {
            let ev = match step {
                Tk::Wait(us) => {
                    let until = now.plus(Micros(us));
                    while let Some(d) = m.next_deadline() {
                        if d > until { break; }
                        now = d.max(now);
                        feed(&mut m, TargetEvent::Tick, now, &mut controlled)?;
                    }
                    now = until;
                    continue;
                }
                Tk::Enter(p, s) => TargetEvent::Control { peer: peer(p), msg: Control::Enter { session: s, x: 0.0, y: 0.0, keymap_serial: 1, motion_seq: 0 } },
                Tk::Leave(s) => TargetEvent::Control { peer: desk(), msg: Control::Leave { session: s, reason: 1 } },
                Tk::Held(s) => TargetEvent::Control { peer: desk(), msg: held(s, 1, vec![], vec![]) },
                Tk::Key(s, p) => TargetEvent::Control { peer: desk(), msg: key(s, 30, p) },
                Tk::Motion(seq) => motion(0, seq, 5.0),
                Tk::PeerDown(p) => TargetEvent::PeerDown(peer(p)),
                Tk::Force => TargetEvent::ForceRelease,
                Tk::Busy(b) => TargetEvent::SourceBusy(b),
                Tk::Accept(a) => TargetEvent::AcceptEnters(a),
                Tk::Poll(on) => TargetEvent::CursorPoll(on),
                Tk::Polled(p) => TargetEvent::CursorPolled(p.map(|(x, y)| (f64::from(x), f64::from(y)))),
                Tk::Start => TargetEvent::Start,
            };
            feed(&mut m, ev, now, &mut controlled)?;
            if let Tk::PeerDown(p) = step {
                feed(&mut m, TargetEvent::PeerUp(peer(p)), now, &mut controlled)?;
                feed(&mut m, TargetEvent::Control { peer: peer(p), msg: Control::Keymap { serial: 1, format: 1, text: "k".into() } }, now, &mut controlled)?;
            }
        }
    }
}

// ---- non-finite peer floats (A-02) -----------------------------

fn injects_finite(a: &[TargetAction]) -> bool {
    a.iter().all(|a| match a {
        TargetAction::Inject(InjectCmd::Motion { x, y }) => x.is_finite() && y.is_finite(),
        TargetAction::Inject(InjectCmd::Scroll { value, .. }) => value.is_finite(),
        _ => true,
    })
}

#[test]
fn a_non_finite_datagram_is_dropped_and_counted() {
    let mut r = Rig::entered(desk(), 1);
    assert_eq!(r.ev(motion(1, 11, f32::NAN)), vec![]);
    assert_eq!(r.ev(motion(1, 12, f32::INFINITY)), vec![]);
    assert_eq!(r.m.counters().nonfinite_dropped, 2);
    // Takeover still compares against the last finite position.
    assert_eq!(
        r.ev(motion(1, 13, 7.0)),
        vec![inject(InjectCmd::Motion { x: 7.0, y: 5.0 })]
    );
}

#[test]
fn a_non_finite_enter_is_refused() {
    let mut r = Rig::new();
    r.ev(TargetEvent::PeerUp(desk()));
    r.keymap(desk(), 1);
    let a = r.ctl(
        desk(),
        Control::Enter {
            session: 1,
            x: f32::NAN,
            y: 0.0,
            keymap_serial: 1,
            motion_seq: 10,
        },
    );
    assert_eq!(a, vec![ended(desk(), 1, LeaveReason::Refused)]);
    assert_eq!(r.m.status().entered_by, None);
    assert_eq!(r.m.counters().nonfinite_dropped, 1);
}

#[test]
fn a_non_finite_fence_or_scroll_is_dropped_but_the_frame_applies() {
    let mut r = Rig::entered(desk(), 1);
    // The button edge still applies; only the position is dropped.
    let a = r.ctl(
        desk(),
        Control::Button {
            session: 1,
            code: 0x110,
            pressed: true,
            pos: pos(11, f32::NAN, 1.0),
        },
    );
    assert_eq!(
        a,
        vec![inject(InjectCmd::Button {
            code: 0x110,
            pressed: true
        })]
    );
    let a = r.ctl(
        desk(),
        Control::Scroll {
            session: 1,
            axis: 0,
            source: 1,
            value: f32::NEG_INFINITY,
            value120: None,
            stop: false,
            pos: pos(0, 0.0, 0.0),
        },
    );
    assert_eq!(a, vec![]);
    // A Held with a bad position still feeds the watchdog and is acked.
    let mut h = held(1, 2, vec![], vec![0x110]);
    if let Control::Held { pos: p, .. } = &mut h {
        *p = pos(12, 1.0, f32::INFINITY);
    }
    assert_eq!(
        r.ctl(desk(), h),
        vec![TargetAction::Send {
            peer: desk(),
            msg: Control::HeldAck { session: 1, n: 2 },
        }]
    );
    assert_eq!(r.m.counters().nonfinite_dropped, 3);
}

fn arb_bits() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<u32>().prop_map(f32::from_bits),
        -2000.0f32..2000.0,
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
    ]
}

#[derive(Debug, Clone)]
enum Bad {
    Enter(f32, f32),
    Motion(u32, f32, f32),
    Button(u32, f32, f32),
    Scroll(f32, u32, f32, f32),
    Held(u32, f32, f32),
}

fn arb_bad() -> impl Strategy<Value = Bad> {
    let seq = 0u32..64;
    prop_oneof![
        (arb_bits(), arb_bits()).prop_map(|(x, y)| Bad::Enter(x, y)),
        (seq.clone(), arb_bits(), arb_bits()).prop_map(|(s, x, y)| Bad::Motion(s, x, y)),
        (seq.clone(), arb_bits(), arb_bits()).prop_map(|(s, x, y)| Bad::Button(s, x, y)),
        (arb_bits(), seq.clone(), arb_bits(), arb_bits())
            .prop_map(|(v, s, x, y)| Bad::Scroll(v, s, x, y)),
        (seq, arb_bits(), arb_bits()).prop_map(|(s, x, y)| Bad::Held(s, x, y)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Arbitrary `f32` bit patterns in every peer float the machine reads:
    /// nothing non-finite reaches ferry-inject.
    #[test]
    fn non_finite_positions_never_reach_inject(steps in proptest::collection::vec(arb_bad(), 1..60)) {
        let mut r = Rig::new();
        r.ev(TargetEvent::PeerUp(desk()));
        r.keymap(desk(), 1);
        let mut n = 0;
        for s in steps {
            let session = r.m.status().session;
            let a = match s {
                Bad::Enter(x, y) => r.ctl(desk(), Control::Enter {
                    session: session + 1, x, y, keymap_serial: 1, motion_seq: 0,
                }),
                Bad::Motion(seq, x, y) => r.ev(TargetEvent::Datagram {
                    peer: desk(),
                    dgram: Datagram::Motion(Motion {
                        session_lo: session as u16, seq, x, y, dx: x, dy: y, t_us: 0,
                    }),
                }),
                Bad::Button(seq, x, y) => r.ctl(desk(), Control::Button {
                    session, code: 0x110, pressed: seq % 2 == 0, pos: pos(seq, x, y),
                }),
                Bad::Scroll(value, seq, x, y) => r.ctl(desk(), Control::Scroll {
                    session, axis: 0, source: 1, value, value120: None, stop: false,
                    pos: pos(seq, x, y),
                }),
                Bad::Held(seq, x, y) => {
                    n += 1;
                    r.ctl(desk(), Control::Held {
                        session, n, keys: vec![], buttons: vec![], pos: pos(seq, x, y),
                    })
                }
            };
            prop_assert!(injects_finite(&a), "{:?}", a);
        }
    }
}
