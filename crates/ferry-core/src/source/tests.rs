//! SourceMachine tests (`source::*`).

use proptest::prelude::*;

use super::*;
use crate::geom::{Align, Anchor, Layout, Origin, PeerMonitors, Rel};
use crate::testkit::*;
use crate::time::LockBudget;

const KEYMAP: &str = "xkb_keymap { desk };";

/// The `SetBarriers` of first light with the netbook up: the machine sends
/// its own barrier list (one bottom barrier on HDMI-A-1).
fn set_barriers(refresh_zones: bool) -> PortalCmd {
    PortalCmd::SetBarriers {
        refresh_zones,
        barriers: geom::barriers(&first_light(), &[netbook()]),
    }
}
const BTN_LEFT: u16 = 0x110;

struct Rig {
    m: SourceMachine,
    out: Actions<SourceAction>,
    now: Micros,
    topo: Topology,
    activation: u32,
}

impl Rig {
    fn with(topo: Topology, config: SourceConfig) -> Self {
        let mut r = Rig {
            m: SourceMachine::new(config),
            out: Actions::new(),
            now: Micros::from_millis(10_000),
            topo: topo.clone(),
            activation: 0,
        };
        r.ev(SourceEvent::Topology(topo));
        r.ev(SourceEvent::Keymap {
            text: KEYMAP.into(),
        });
        r.ev(SourceEvent::PeerUp(netbook()));
        r.ev(SourceEvent::Tick);
        r.ev(SourceEvent::LockPolled(LockCheck::Unlocked));
        r.ev(SourceEvent::BarriersSet { failed: vec![] });
        assert_eq!(r.m.status().gate, Gate::Armed);
        r
    }

    /// First light, default config, armed.
    fn armed() -> Self {
        Self::with(first_light(), SourceConfig::default())
    }

    /// Feed one event now; its actions without `Status`.
    fn ev(&mut self, ev: SourceEvent) -> Vec<SourceAction> {
        self.m.handle(ev, self.now, &mut self.out);
        self.out
            .drain()
            .filter(|a| !matches!(a, SourceAction::Status(_)))
            .collect()
    }

    /// Advance to `now + ms`, firing the timer at every deadline on the way.
    fn wait(&mut self, ms: u64) -> Vec<SourceAction> {
        self.wait_us(ms * 1000)
    }

    fn wait_us(&mut self, us: u64) -> Vec<SourceAction> {
        let until = self.now.plus(Micros(us));
        let mut all = Vec::new();
        while let Some(d) = self.m.next_deadline() {
            if d > until {
                break;
            }
            self.now = d.max(self.now);
            all.extend(self.ev(SourceEvent::Tick));
        }
        self.now = until;
        all
    }

    /// `start_emulating` + `Activated` at local (x, y) on barrier 1.
    fn activate(&mut self, x: f64, y: f64) -> Vec<SourceAction> {
        self.activate_with(x, y, LockCheck::Unlocked)
    }

    fn activate_with(&mut self, x: f64, y: f64, lock: LockCheck) -> Vec<SourceAction> {
        self.activation += 5;
        let mut a = self.ev(SourceEvent::StartEmulating);
        a.extend(self.ev(SourceEvent::Activated {
            activation_id: self.activation,
            barrier_id: 1,
            x,
            y,
            lock,
        }));
        a
    }

    /// First light: cross at x = 800 (one motion step past the bottom edge).
    fn remote() -> Self {
        let mut r = Rig::armed();
        r.activate(800.0, 1081.0);
        assert_eq!(r.m.status().role, SourceRole::Remote);
        r
    }

    fn motion(&mut self, dx: f32, dy: f32) -> Vec<SourceAction> {
        self.ev(SourceEvent::Motion { dx, dy })
    }

    fn key(&mut self, code: u16, pressed: bool) -> Vec<SourceAction> {
        let t = self.now;
        self.ev(SourceEvent::Key {
            code,
            pressed,
            t_eis: t,
        })
    }

    fn ack(&mut self, n: u32) -> Vec<SourceAction> {
        let session = self.m.status().session;
        self.ev(SourceEvent::Control {
            peer: netbook(),
            msg: Control::HeldAck { session, n },
        })
    }

    /// Acknowledge the outstanding `Held`, as a prompt target would.
    fn ack_outstanding(&mut self) -> Vec<SourceAction> {
        match &self.m.crossing {
            Crossing::Remote(r) => {
                let n = r.held_n;
                self.ack(n)
            }
            _ => Vec::new(),
        }
    }

    fn model(&self) -> Point {
        match &self.m.crossing {
            Crossing::Remote(r) => r.model,
            other => panic!("not remote: {other:?}"),
        }
    }
}

fn sends(a: &[SourceAction]) -> Vec<&Control> {
    a.iter()
        .filter_map(|a| match a {
            SourceAction::Send { msg, .. } => Some(msg),
            _ => None,
        })
        .collect()
}

fn no_input_forwarded(a: &[SourceAction]) -> bool {
    !a.iter().any(|a| {
        matches!(a, SourceAction::Datagram { .. })
            || matches!(
                a,
                SourceAction::Send {
                    msg: Control::Key { .. }
                        | Control::Button { .. }
                        | Control::Scroll { .. }
                        | Control::Enter { .. },
                    ..
                }
            )
    })
}

fn release(id: u32, x: f64, y: f64) -> SourceAction {
    SourceAction::Portal(PortalCmd::Release {
        activation_id: Some(id),
        pos: Some(Point { x, y }),
    })
}

/// First light's only barrier parked: `SetBarriers` with an empty list.
fn park_all() -> SourceAction {
    SourceAction::Portal(PortalCmd::SetBarriers {
        refresh_zones: false,
        barriers: Vec::new(),
    })
}

fn leave(session: u32, reason: LeaveReason) -> SourceAction {
    SourceAction::Send {
        peer: netbook(),
        msg: Control::Leave {
            session,
            reason: reason.code(),
        },
    }
}

#[test]
fn start_up_arms_after_first_unlock_only() {
    let mut m = SourceMachine::new(SourceConfig::default());
    let mut out = Actions::new();
    let mut ev = |m: &mut SourceMachine, e| {
        m.handle(e, Micros::ZERO, &mut out);
        out.drain()
            .filter(|a| !matches!(a, SourceAction::Status(_)))
            .collect::<Vec<_>>()
    };
    // The first deadline is the first lock poll, at once.
    assert_eq!(m.next_deadline(), Some(Micros::ZERO));
    assert_eq!(ev(&mut m, SourceEvent::Tick), vec![SourceAction::CheckLock]);
    // Topology and a peer before any unlock: nothing is armed.
    assert!(ev(&mut m, SourceEvent::Topology(first_light())).is_empty());
    assert!(ev(&mut m, SourceEvent::PeerUp(netbook())).is_empty());
    assert!(ev(&mut m, SourceEvent::LockPolled(LockCheck::Unknown)).is_empty());
    assert_eq!(m.counters().lock_unknown, 1);
    assert_eq!(
        ev(&mut m, SourceEvent::LockPolled(LockCheck::Unlocked)),
        vec![SourceAction::Portal(set_barriers(true))]
    );
    assert_eq!(m.status().gate, Gate::Idle);
    assert_eq!(
        ev(&mut m, SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(m.status().gate, Gate::Armed);
    // The peer goes down: no barrier left → Disable → Idle.
    assert_eq!(
        ev(&mut m, SourceEvent::PeerDown(netbook())),
        vec![SourceAction::Portal(PortalCmd::Disable)]
    );
    assert_eq!(m.status().gate, Gate::Idle);
    // Back up: barriers again, without a zone refresh this time.
    assert_eq!(
        ev(&mut m, SourceEvent::PeerUp(netbook())),
        vec![SourceAction::Portal(set_barriers(false))]
    );
}

#[test]
fn cross_down_and_back() {
    let mut r = Rig::armed();
    let a = r.activate(800.0, 1081.0);
    assert_eq!(
        sends(&a),
        vec![
            &Control::Enter {
                session: 1,
                x: 523.0,
                y: 0.0,
                keymap_serial: 1,
                motion_seq: 0,
            },
            &Control::Held {
                session: 1,
                n: 1,
                keys: vec![],
                buttons: vec![],
                pos: Pos {
                    motion_seq: 0,
                    x: 523.0,
                    y: 0.0
                },
            }
        ]
    );
    assert!(!a.iter().any(|a| matches!(a, SourceAction::Portal(_))));
    // Down into the netbook: target-local datagrams.
    let a = r.motion(3.0, 40.0);
    assert_eq!(
        a,
        vec![SourceAction::Datagram {
            peer: netbook(),
            motion: Motion {
                session_lo: 1,
                seq: 1,
                x: 526.0,
                y: 40.0,
                dx: 3.0,
                dy: 40.0,
                t_us: r.now.low32(),
            }
        }]
    );
    // Back up past y = 1080 inside the span: home, released at the
    // continuation (the carried point, inside HDMI-A-1).
    let a = r.motion(0.0, -45.0);
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Home),
            release(r.activation, 803.0, 1075.0)
        ]
    );
    assert_eq!(r.m.status().role, SourceRole::Local);
    // A small step back home still lands 2 px inside.
    r.activate(800.0, 1081.0);
    let a = r.motion(0.0, -1.0);
    assert_eq!(
        a,
        vec![
            leave(2, LeaveReason::Home),
            release(r.activation, 800.0, 1077.0)
        ]
    );
}

/// The M1 exit criterion: cross and return on any side.
#[test]
fn cross_and_return_on_every_side() {
    for rel in [Rel::Below, Rel::Above, Rel::LeftOf, Rel::RightOf] {
        let topo = topo_with_netbook(rel);
        let link = topo.links[0];
        assert_eq!(link.to.peer, netbook());
        let side = link.side;
        let home = topo.monitor(link.from).expect("home").rect;
        let target = topo.monitor(link.to).expect("target").rect;
        let along = f64::from(link.span.0 + link.span.1) / 2.0;
        let line = f64::from(link.line);
        let mut r = Rig::with(topo.clone(), SourceConfig::default());
        // One step past the line, outward.
        let step = if matches!(side, Side::Right | Side::Bottom) {
            1.0
        } else {
            -1.0
        };
        let past = side.point(along, line + step);
        let a = r.activate(past.x, past.y);
        // The entry is the same plane point, clamped into the target.
        let entry = target.clamp(side.point(along, line));
        let (ox, oy) = topo.origin(netbook()).expect("origin");
        match sends(&a).first() {
            Some(Control::Enter { x, y, .. }) => {
                assert_eq!(
                    (f64::from(*x), f64::from(*y)),
                    (entry.x - f64::from(ox), entry.y - f64::from(oy)),
                    "{rel:?}"
                );
            }
            other => panic!("{rel:?}: {other:?}"),
        }
        // Further in, then back out through the link.
        let (dx, dy) = match side {
            Side::Left => (-30.0, 0.0),
            Side::Right => (30.0, 0.0),
            Side::Top => (0.0, -30.0),
            Side::Bottom => (0.0, 30.0),
        };
        assert_eq!(r.motion(dx, dy).len(), 1, "{rel:?}");
        assert!(target.contains(r.model()), "{rel:?}");
        let a = r.motion(-2.0 * dx, -2.0 * dy);
        assert_eq!(a.first(), Some(&leave(1, LeaveReason::Home)), "{rel:?}");
        match a.get(1) {
            Some(SourceAction::Portal(PortalCmd::Release { pos: Some(p), .. })) => {
                assert!(home.contains(*p), "{rel:?}: {p:?}");
                // At least 2 px from the link's edge.
                assert!((side.across(*p) - line).abs() >= 2.0, "{rel:?}: {p:?}");
            }
            other => panic!("{rel:?}: {other:?}"),
        }
    }
}

#[test]
fn bounce_outside_link() {
    let mut r = Rig::armed();
    // x = 100 is on HDMI-A-1's bottom edge but outside [277, 1643).
    let a = r.activate(100.0, 1081.0);
    // A wall stretch: the bounce, then its barrier comes down.
    assert_eq!(a, vec![release(r.activation, 100.0, 1077.0), park_all()]);
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.status().role, SourceRole::Local);
    assert_eq!(r.m.counters().bounces, 1);
    assert_eq!(r.m.counters().edge_parks, 1);
    // The portal's Deactivated for our own release is not capture ending.
    let id = r.activation;
    assert!(
        r.ev(SourceEvent::Deactivated { activation_id: id })
            .is_empty()
    );
}

#[test]
fn bounce_dead_corner() {
    // Contacts at x ∈ [285, 1635) cross; the 8 px at each end bounce.
    for (x, crosses) in [
        (277.0, false),
        (284.9, false),
        (285.0, true),
        (1634.9, true),
        (1635.0, false),
        (1642.0, false),
    ] {
        let mut r = Rig::armed();
        let a = r.activate(x, 1080.5);
        assert_eq!(
            r.m.status().role == SourceRole::Remote,
            crosses,
            "x = {x}: {a:?}"
        );
        if !crosses {
            assert_eq!(a, vec![release(r.activation, x, 1077.0), park_all()]);
        }
    }
}

#[test]
fn locked_activation_forwards_nothing() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.key(30, true);
    r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: true,
    });
    let a = r.ev(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Locked,
    });
    assert_eq!(
        a,
        vec![
            release(9, 800.0, 1077.0),
            SourceAction::Portal(PortalCmd::Disable)
        ]
    );
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.status().gate, Gate::Locked);
    assert_eq!(r.m.counters().lock_refusals, 1);
}

#[test]
fn unknown_lock_refuses() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.key(30, true);
    let a = r.ev(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unknown,
    });
    assert_eq!(a, vec![release(9, 800.0, 1077.0)]);
    assert_eq!(r.m.status().gate, Gate::Armed);
    assert_eq!(r.m.counters().lock_refusals, 1);
}

#[test]
fn poll_interval_by_state() {
    let mut r = Rig::armed();
    let polls = |a: &[SourceAction]| a.iter().filter(|a| **a == SourceAction::CheckLock).count();
    // Local, unlocked: every 2 s.
    assert_eq!(polls(&r.wait(1999)), 0);
    assert_eq!(polls(&r.wait(1)), 1);
    assert_eq!(polls(&r.wait(4000)), 2);
    // Remote: within 250 ms of the Enter, then every 250 ms.
    r.activate(800.0, 1081.0);
    let mut seen = 0;
    for _ in 0..10 {
        r.ack(r.m.status().session); // keep the session alive
        let a = r.wait(100);
        seen += polls(&a);
        if let Some(n) = a.iter().find_map(|a| match a {
            SourceAction::Send {
                msg: Control::Held { n, .. },
                ..
            } => Some(*n),
            _ => None,
        }) {
            r.ack(n);
        }
    }
    assert_eq!(seen, 4, "1 s remote: 4 polls at 250 ms");
    // Locked: every 500 ms.
    r.ev(SourceEvent::LockPolled(LockCheck::Locked));
    assert_eq!(r.m.status().gate, Gate::Locked);
    assert_eq!(polls(&r.wait(2000)), 4);
}

#[test]
fn lock_during_capture() {
    let mut r = Rig::remote();
    r.key(42, true);
    let a = r.ev(SourceEvent::LockPolled(LockCheck::Locked));
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Locked),
            release(r.activation, 800.0, 1077.0),
            SourceAction::Portal(PortalCmd::Disable)
        ]
    );
    assert_eq!(r.m.status().gate, Gate::Locked);
    // Locked again: nothing more.
    assert!(r.ev(SourceEvent::LockPolled(LockCheck::Locked)).is_empty());
    // A Disabled echo while locked is ignored.
    assert!(r.ev(SourceEvent::Disabled).is_empty());
    assert_eq!(r.m.status().gate, Gate::Locked);
    // Unlock: barriers with fresh zones, then Enable.
    assert_eq!(
        r.ev(SourceEvent::LockPolled(LockCheck::Unlocked)),
        vec![SourceAction::Portal(set_barriers(true))]
    );
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(r.m.status().gate, Gate::Armed);
}

fn helds(a: &[SourceAction]) -> Vec<u32> {
    a.iter()
        .filter_map(|a| match a {
            SourceAction::Send {
                msg: Control::Held { n, .. },
                ..
            } => Some(*n),
            _ => None,
        })
        .collect()
}

#[test]
fn snapshot_cadence() {
    let mut r = Rig::armed();
    let a = r.activate(800.0, 1081.0);
    assert_eq!(helds(&a), vec![1]);
    r.ack(1);
    let t0 = r.now;
    let mut ns = Vec::new();
    for _ in 0..5 {
        let a = r.wait(100);
        let n = helds(&a);
        assert_eq!(n.len(), 1, "one Held per 100 ms");
        r.ack(n[0]);
        ns.extend(n);
    }
    assert_eq!(ns, vec![2, 3, 4, 5, 6]);
    assert_eq!(r.now, t0.plus(Micros::from_millis(500)));
    // The held set rides along.
    r.key(29, true);
    let a = r.wait(100);
    match sends(&a).as_slice() {
        [Control::Held { n: 7, keys, .. }] => assert_eq!(keys, &vec![29]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn one_way_cut_dates_from_send_time() {
    let mut r = Rig::remote();
    let t0 = r.now;
    r.ack(1);
    // Held #2 leaves at +100; its ack arrives late, at +400. Stop-and-wait:
    // nothing is sent while it is outstanding, and Held #3, due since +200,
    // leaves with the ack.
    assert_eq!(helds(&r.wait(100)), vec![2]);
    assert_eq!(helds(&r.wait(300)), Vec::<u32>::new());
    assert_eq!(helds(&r.ack(2)), vec![3]);
    // Then nothing more is acknowledged (the target→source path is cut):
    // no further Held, and the release is at sent_at(2) + 650 = +750, not
    // at arrival + 650 = +1050.
    assert_eq!(helds(&r.wait_us(349_999)), Vec::<u32>::new());
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(r.now, t0.plus(Micros(749_999)));
    let mut a = r.wait_us(1);
    a.retain(|a| *a != SourceAction::CheckLock);
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Silence),
            release(r.activation, 800.0, 1077.0),
            // The suspect peer's edge is a wall: no barrier.
            SourceAction::Portal(PortalCmd::Disable),
        ]
    );
    assert_eq!(r.m.status().role, SourceRole::Local);
    assert_eq!(r.m.counters().silence_releases, 1);
}

#[test]
fn held_ack_needs_session_and_the_outstanding_n() {
    let mut r = Rig::remote();
    let t0 = r.now;
    r.ack(1);
    r.wait(100); // Held #2
    // Wrong session, an n never sent, an n already acknowledged, another
    // peer: no effect.
    r.ev(SourceEvent::Control {
        peer: netbook(),
        msg: Control::HeldAck { session: 7, n: 2 },
    });
    r.ack(40);
    r.ack(1);
    r.ev(SourceEvent::Control {
        peer: third(),
        msg: Control::HeldAck { session: 1, n: 2 },
    });
    match &r.m.crossing {
        Crossing::Remote(rem) => {
            assert_eq!(rem.acked_at, t0);
            assert!(!rem.held_acked);
        }
        other => panic!("{other:?}"),
    }
    r.ack(2);
    match &r.m.crossing {
        Crossing::Remote(rem) => {
            assert_eq!(rem.acked_at, t0.plus(Micros::from_millis(100)));
            assert!(rem.held_acked);
        }
        other => panic!("{other:?}"),
    }
    // A duplicate ack changes nothing and sends nothing.
    assert!(r.ack(2).is_empty());
}

#[test]
fn stop_and_wait_holds_the_next_held_until_the_ack() {
    let mut r = Rig::remote();
    let t0 = r.now;
    // Held #1 went with Enter and is never acknowledged: no Held #2, however
    // long we wait, until silence ends the crossing at +650.
    let a = r.wait(649);
    assert_eq!(helds(&a), Vec::<u32>::new());
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // The only deadline left is liveness.
    assert_eq!(r.m.next_deadline(), Some(t0.plus(SOURCE_LIVENESS)));
    let a = r.wait(1);
    assert_eq!(a.first(), Some(&leave(1, LeaveReason::Silence)));
    assert_eq!(r.m.status().role, SourceRole::Local);
}

#[test]
fn a_key_while_the_ack_is_outstanding_rides_the_next_held() {
    let mut r = Rig::remote();
    r.ack(1);
    assert_eq!(helds(&r.wait(100)), vec![2]);
    // Keys change while #2 is unacknowledged: Key frames go at once, and
    // Held #3 (sent on the late ack) carries the set as it is then.
    r.wait(50);
    assert_eq!(sends(&r.key(29, true)).len(), 1);
    r.wait(80);
    assert_eq!(sends(&r.key(30, true)).len(), 1);
    // The release waits out the repeat filter's 1 ms, then goes.
    r.key(29, false);
    assert_eq!(sends(&r.wait(1)).len(), 1);
    let a = r.ack(2);
    match sends(&a).as_slice() {
        [Control::Held { n: 3, keys, .. }] => assert_eq!(keys, &vec![30]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn slow_acks_slow_the_cadence() {
    // RTT 150 ms: each Held waits for the previous ack, so one leaves every
    // 150 ms instead of every 100 ms, and the crossing stays up.
    let mut r = Rig::remote();
    let mut ns = Vec::new();
    for _ in 0..8 {
        let mut a = r.wait(150);
        let outstanding = match &r.m.crossing {
            Crossing::Remote(rem) => rem.held_n,
            other => panic!("{other:?}"),
        };
        a.extend(r.ack(outstanding));
        ns.extend(helds(&a));
    }
    assert_eq!(ns, (2..=9).collect::<Vec<u32>>());
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn a_replaced_connection_gets_a_fresh_held() {
    let mut r = Rig::remote();
    r.ack(1);
    assert_eq!(helds(&r.wait(100)), vec![2]);
    // The connection is replaced under the crossing (PeerUp again): Held #2
    // may have died with the old one, so #3 goes out on the new one at once
    // (with the keymap, which a new connection always gets).
    let a = r.ev(SourceEvent::PeerUp(netbook()));
    assert_eq!(helds(&a), vec![3]);
    assert!(
        sends(&a)
            .iter()
            .any(|m| matches!(m, Control::Keymap { serial: 1, .. }))
    );
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // Its ack restarts the cadence; the stale ack of #2 does nothing.
    assert!(r.ack(2).is_empty());
    assert!(r.ack(3).is_empty());
    assert_eq!(helds(&r.wait(100)), vec![4]);
}

#[test]
fn escape_chord_not_forwarded() {
    for (sup, ctrl) in [(125, 29), (126, 97)] {
        let mut r = Rig::remote();
        let mut all = r.key(sup, true);
        all.extend(r.key(ctrl, true));
        assert_eq!(sends(&all).len(), 2);
        let a = r.key(1, true);
        assert_eq!(
            a,
            vec![
                leave(1, LeaveReason::EscapeChord),
                release(r.activation, 800.0, 1077.0)
            ]
        );
        all.extend(a);
        all.extend(r.key(1, false));
        assert!(!all.iter().any(|a| matches!(
            a,
            SourceAction::Send {
                msg: Control::Key { code: 1, .. },
                ..
            }
        )));
        assert_eq!(r.m.status().role, SourceRole::Local);
    }
    // Escape alone is forwarded.
    let mut r = Rig::remote();
    assert_eq!(sends(&r.key(1, true)).len(), 1);
}

#[test]
fn drag_rule() {
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: false,
    });
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Drag),
            release(r.activation, 800.0, 1077.0)
        ]
    );
    assert_eq!(r.m.counters().drag_leaves, 1);
    // A known press and release are forwarded with the position fence.
    let mut r = Rig::remote();
    r.motion(1.0, 1.0);
    let a = r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: true,
    });
    assert_eq!(
        sends(&a),
        vec![&Control::Button {
            session: 1,
            code: BTN_LEFT,
            pressed: true,
            pos: Pos {
                motion_seq: 1,
                x: 524.0,
                y: 1.0
            }
        }]
    );
    assert_eq!(
        sends(&r.ev(SourceEvent::Button {
            code: BTN_LEFT,
            pressed: false
        }))
        .len(),
        1
    );
}

#[test]
fn orphan_key_release_dropped() {
    let mut r = Rig::remote();
    // Super's release after a Super+drag crossed.
    assert!(r.key(125, false).is_empty());
    assert!(r.ev(SourceEvent::BatchEnd).is_empty());
    // Held 1 ms like any release that may start a pair, then dropped.
    assert!(r.wait_us(1000).is_empty());
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(r.m.counters().orphan_key_releases, 1);
}

#[test]
fn repeat_pairs_in_remote() {
    let mut r = Rig::remote();
    assert_eq!(sends(&r.key(30, true)).len(), 1);
    r.ev(SourceEvent::BatchEnd);
    // An S17 pair inside one batch, 15 µs apart.
    let t = r.now;
    assert!(
        r.ev(SourceEvent::Key {
            code: 30,
            pressed: false,
            t_eis: t
        })
        .is_empty()
    );
    assert!(
        r.ev(SourceEvent::Key {
            code: 30,
            pressed: true,
            t_eis: t.plus(Micros(15))
        })
        .is_empty()
    );
    assert!(r.ev(SourceEvent::BatchEnd).is_empty());
    assert_eq!(r.m.counters().repeat_pairs_dropped, 1);
    // A real release goes out at its deadline, 1 ms after its EIS time,
    // not at the end of its batch.
    assert!(r.key(30, false).is_empty());
    assert!(r.ev(SourceEvent::BatchEnd).is_empty());
    assert_eq!(r.m.next_deadline(), Some(r.now.plus(Micros(1000))));
    assert!(r.wait_us(999).is_empty());
    let a = r.wait_us(1);
    assert_eq!(
        sends(&a),
        vec![&Control::Key {
            session: 1,
            code: 30,
            pressed: false
        }]
    );
    // Another key flushes the held release first, in order.
    r.key(31, true);
    r.key(31, false);
    let a = r.key(32, true);
    assert_eq!(
        sends(&a),
        vec![
            &Control::Key {
                session: 1,
                code: 31,
                pressed: false
            },
            &Control::Key {
                session: 1,
                code: 32,
                pressed: true
            }
        ]
    );
}

#[test]
fn repeat_pair_split_across_batches_in_remote() {
    // The VM drill: the press arrives 452 µs later, in the next read, after
    // the machine's own clock passed the deadline. The filter judges by EIS
    // time, so the pair is still dropped.
    let mut r = Rig::remote();
    r.key(30, true);
    for n in 2..8 {
        r.wait(100);
        r.ack(n);
    }
    let t = r.now;
    assert!(
        sends(&r.ev(SourceEvent::Key {
            code: 30,
            pressed: false,
            t_eis: t
        }))
        .is_empty()
    );
    assert!(r.ev(SourceEvent::BatchEnd).is_empty());
    r.now = t.plus(Micros(1200));
    let a = r.ev(SourceEvent::Key {
        code: 30,
        pressed: true,
        t_eis: t.plus(Micros(452)),
    });
    assert!(sends(&a).is_empty(), "{a:?}");
    assert_eq!(r.m.counters().repeat_pairs_dropped, 1);
}

#[test]
fn repeat_pairs_split_by_more_than_1_ms_are_counted_not_dropped() {
    // A diagnostic (exit stream A3): the press comes 3.5 ms after the held key's release, as
    // under VM load. The filter does not change: both edges reach the peer. The counters give
    // the real machines the data for the user's decision on the gap.
    let mut r = Rig::remote();
    r.key(30, true);
    let t = r.now;
    assert!(
        sends(&r.ev(SourceEvent::Key {
            code: 30,
            pressed: false,
            t_eis: t
        }))
        .is_empty()
    );
    let a = r.ev(SourceEvent::Key {
        code: 30,
        pressed: true,
        t_eis: t.plus(Micros(3500)),
    });
    assert_eq!(
        sends(&a),
        vec![
            &Control::Key {
                session: 1,
                code: 30,
                pressed: false
            },
            &Control::Key {
                session: 1,
                code: 30,
                pressed: true
            }
        ]
    );
    let c = r.m.counters();
    assert_eq!(c.repeat_pairs_dropped, 0);
    assert_eq!((c.repeat_split_pairs, c.repeat_split_max_us), (1, 3500));
}

#[test]
fn short_presses_are_not_delayed() {
    let config = SourceConfig {
        repeat_hold_after_ms: 550,
        ..SourceConfig::default()
    };
    let mut r = Rig::with(first_light(), config);
    r.activate(800.0, 1081.0);
    r.key(30, true);
    r.wait(100);
    // Down 100 ms: the release goes out at once.
    assert_eq!(
        sends(&r.key(30, false)),
        vec![&Control::Key {
            session: 1,
            code: 30,
            pressed: false
        }]
    );
    // Down 600 ms: held up to 1 ms.
    r.key(31, true);
    r.ack_outstanding();
    r.wait(90);
    for _ in 0..5 {
        r.ack_outstanding();
        r.wait(100);
    }
    assert!(sends(&r.key(31, false)).is_empty());
    assert_eq!(sends(&r.wait_us(1000)).len(), 1);
}

#[test]
fn session_restarted() {
    // Remote: Leave(capture_ended), no Release, re-zone, re-arm.
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::SessionRestarted);
    assert_eq!(a, vec![leave(1, LeaveReason::CaptureEnded)]);
    assert_eq!(r.m.status().gate, Gate::Rezoning);
    assert_eq!(r.wait(100), vec![SourceAction::Portal(set_barriers(true))]);
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(r.m.status().gate, Gate::Armed);
    // Locked: the gate stays; unlock arms with SetBarriers.
    let mut r = Rig::armed();
    r.ev(SourceEvent::LockPolled(LockCheck::Locked));
    assert!(r.ev(SourceEvent::SessionRestarted).is_empty());
    assert_eq!(r.m.status().gate, Gate::Locked);
    assert_eq!(
        r.ev(SourceEvent::LockPolled(LockCheck::Unlocked)),
        vec![SourceAction::Portal(set_barriers(true))]
    );
    // Suspended: the gate stays; the wake re-zones instead of restoring Armed.
    let mut r = Rig::armed();
    r.ev(SourceEvent::PrepareForSleep { start: true });
    assert!(r.ev(SourceEvent::SessionRestarted).is_empty());
    assert_eq!(r.m.status().gate, Gate::Suspended);
    r.ev(SourceEvent::PrepareForSleep { start: false });
    assert_eq!(r.m.status().gate, Gate::Rezoning);
    let a = r.wait(100);
    assert!(a.contains(&SourceAction::Portal(set_barriers(true))));
}

#[test]
fn activation_without_position_bounces() {
    for (barrier_id, x, y) in [
        (1, f64::NAN, f64::NAN),
        (0, 800.0, 1081.0),
        (1, 800.0, f64::INFINITY),
    ] {
        let mut r = Rig::armed();
        r.ev(SourceEvent::StartEmulating);
        r.key(30, true);
        let a = r.ev(SourceEvent::Activated {
            activation_id: 7,
            barrier_id,
            x,
            y,
            lock: LockCheck::Unlocked,
        });
        assert_eq!(
            a,
            vec![SourceAction::Portal(PortalCmd::Release {
                activation_id: Some(7),
                pos: None
            })]
        );
        assert_eq!(r.m.counters().bounces, 1);
        assert_eq!(r.m.status().role, SourceRole::Local);
    }
}

#[test]
fn own_release_or_disable_gets_no_second_leave() {
    // xdph sends neither Deactivated nor Disabled after ferry's own Release
    // or Disable; only EIS StopEmulating arrives, and it is ferry's.
    let mut r = Rig::remote();
    assert_eq!(r.ev(SourceEvent::ForceRelease).len(), 2);
    assert!(r.ev(SourceEvent::StopEmulating).is_empty());
    let mut r = Rig::remote();
    assert_eq!(r.ev(SourceEvent::LockPolled(LockCheck::Locked)).len(), 3);
    assert!(r.ev(SourceEvent::StopEmulating).is_empty());
    // Nothing is waited for: unlock re-arms without any portal signal.
    assert!(
        !r.ev(SourceEvent::LockPolled(LockCheck::Unlocked))
            .is_empty()
    );
    // A bounce's StopEmulating, then a new crossing, works.
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    assert!(r.ev(SourceEvent::StopEmulating).is_empty());
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn key_wakeup_dropped() {
    let mut r = Rig::remote();
    assert!(r.key(crate::KEY_WAKEUP, true).is_empty());
    assert!(r.key(crate::KEY_WAKEUP, false).is_empty());
    assert!(r.ev(SourceEvent::BatchEnd).is_empty());
    assert!(r.wait(100).iter().all(|a| !matches!(
        a,
        SourceAction::Send {
            msg: Control::Key { .. },
            ..
        }
    )));
}

#[test]
fn return_blocked_while_button_held() {
    let mut r = Rig::remote();
    r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: true,
    });
    // At the entry row (y = 1080), pushing up: clamped, no crossing, and no
    // datagram because the model did not move.
    let a = r.motion(0.0, -10.0);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(
        r.model(),
        Point {
            x: 800.0,
            y: 1080.0
        }
    );
    // Sliding along the edge still moves.
    assert_eq!(r.motion(5.0, -10.0).len(), 1);
    assert_eq!(
        r.model(),
        Point {
            x: 805.0,
            y: 1080.0
        }
    );
    // Released: the next push goes home.
    r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: false,
    });
    let a = r.motion(0.0, -10.0);
    assert_eq!(a.first(), Some(&leave(1, LeaveReason::Home)));
}

#[test]
fn walls_clamp_and_stay_on_target() {
    let mut r = Rig::remote();
    // Down into the netbook, then far left: eDP-1 starts at x = 277.
    r.motion(0.0, 100.0);
    r.wait(1);
    assert_eq!(r.motion(-5000.0, 0.0).len(), 1);
    assert_eq!(
        r.model(),
        Point {
            x: 277.0,
            y: 1180.0
        }
    );
    // Pushing further into the wall: no datagram.
    assert!(r.motion(-10.0, 0.0).is_empty());
    // Up at x = 280 is HDMI-A-1's link span but inside the dead corner: wall.
    r.motion(3.0, -100.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(
        r.model(),
        Point {
            x: 280.0,
            y: 1080.0
        }
    );
    // Down past the bottom: wall at 1847.
    r.motion(0.0, 5000.0);
    assert_eq!(
        r.model(),
        Point {
            x: 280.0,
            y: 1847.0
        }
    );
}

#[test]
fn resistance_counts_from_start_emulating() {
    let config = SourceConfig {
        edge_resistance_px: 60,
        ..SourceConfig::default()
    };
    // Push 25 (before Activated) + 20 + 20 = 65 ≥ 60: cross.
    let mut r = Rig::with(first_light(), config.clone());
    r.ev(SourceEvent::StartEmulating);
    r.motion(0.0, 25.0);
    r.ev(SourceEvent::Activated {
        activation_id: 1,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(r.m.status().role, SourceRole::Arming);
    assert!(r.motion(0.0, 20.0).is_empty());
    let a = r.motion(0.0, 20.0);
    assert!(matches!(sends(&a).first(), Some(Control::Enter { .. })));
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // 25 then −10: abort, released inside.
    let mut r = Rig::with(first_light(), config.clone());
    r.ev(SourceEvent::StartEmulating);
    r.motion(0.0, 25.0);
    r.ev(SourceEvent::Activated {
        activation_id: 2,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(r.motion(0.0, -10.0), vec![release(2, 800.0, 1077.0)]);
    assert_eq!(r.m.status().role, SourceRole::Local);
    // 25 and a wait: timeout at 500 ms after the activation.
    let mut r = Rig::with(first_light(), config.clone());
    r.ev(SourceEvent::StartEmulating);
    r.motion(0.0, 25.0);
    r.ev(SourceEvent::Activated {
        activation_id: 3,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert!(!r.wait(499).contains(&release(3, 800.0, 1077.0)));
    assert!(r.wait(1).contains(&release(3, 800.0, 1077.0)));
    assert_eq!(r.m.status().role, SourceRole::Local);
    // Resistance also applies on the return: 60 px against the home link.
    let mut r = Rig::with(first_light(), config);
    r.ev(SourceEvent::StartEmulating);
    r.motion(0.0, 70.0);
    r.ev(SourceEvent::Activated {
        activation_id: 4,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(r.m.status().role, SourceRole::Remote);
    r.motion(0.0, -30.0);
    r.motion(0.0, -25.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    let a = r.motion(0.0, -10.0);
    assert_eq!(a.first(), Some(&leave(1, LeaveReason::Home)));
}

#[test]
fn dwell_delays_the_crossing() {
    let config = SourceConfig {
        edge_dwell_ms: 200,
        ..SourceConfig::default()
    };
    let mut r = Rig::with(first_light(), config);
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Arming);
    r.wait(100);
    assert!(
        r.motion(0.0, 2.0).is_empty(),
        "a 100 ms touch does not cross"
    );
    r.wait(100);
    assert!(matches!(
        sends(&r.motion(0.0, 2.0)).first(),
        Some(Control::Enter { .. })
    ));
}

#[test]
fn pending_timeout() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.key(30, true);
    assert!(r.wait(99).is_empty());
    let a = r.wait(1);
    assert_eq!(
        a,
        vec![SourceAction::Portal(PortalCmd::Release {
            activation_id: None,
            pos: None
        })]
    );
    assert_eq!(r.m.status().role, SourceRole::Local);
    // xdph rejects that Release: the late Activated is released by its id.
    let a = r.ev(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(a, vec![release(9, 800.0, 1077.0)]);
}

/// `[safety] lock_check_budget_ms = 100`: Pending waits 150 ms, so an
/// `Activated` behind a 100 ms lock check still crosses, and the poll during a
/// crossing runs every 200 ms, so poll + check stays 300 ms.
#[test]
fn a_raised_lock_budget_moves_both_timers() {
    let config = || SourceConfig {
        lock_check_budget: LockBudget::clamped(100).0,
        ..SourceConfig::default()
    };
    // The verdict comes 140 ms after the push: past the old 100 ms, still Pending.
    let mut r = Rig::with(first_light(), config());
    r.ev(SourceEvent::StartEmulating);
    assert!(r.wait(140).is_empty());
    r.ev(SourceEvent::Activated {
        activation_id: 5,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // Within 1 s remote: 5 polls at 200 ms.
    let polls = |a: &[SourceAction]| a.iter().filter(|a| **a == SourceAction::CheckLock).count();
    let mut seen = 0;
    for _ in 0..10 {
        r.ack(r.m.status().session);
        let a = r.wait(100);
        seen += polls(&a);
        if let Some(n) = a.iter().find_map(|a| match a {
            SourceAction::Send {
                msg: Control::Held { n, .. },
                ..
            } => Some(*n),
            _ => None,
        }) {
            r.ack(n);
        }
    }
    assert_eq!(seen, 5, "1 s remote: 5 polls at 200 ms");
    // Pending with no Activated gives up at 150 ms.
    let mut r = Rig::with(first_light(), config());
    r.ev(SourceEvent::StartEmulating);
    assert!(r.wait(149).is_empty());
    assert_eq!(
        r.wait(1),
        vec![SourceAction::Portal(PortalCmd::Release {
            activation_id: None,
            pos: None
        })]
    );
}

#[test]
fn pending_overflow_bounces() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    let mut a = Vec::new();
    for i in 0..PENDING_MAX as u16 + 1 {
        a.extend(r.key(30 + (i % 2), i % 4 < 2));
    }
    assert_eq!(
        a,
        vec![SourceAction::Portal(PortalCmd::Release {
            activation_id: None,
            pos: None
        })]
    );
    assert_eq!(r.m.counters().pending_overflows, 1);
    let a = r.ev(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(a, vec![release(9, 800.0, 1077.0)]);
    assert_eq!(r.m.counters().bounces, 1);
}

#[test]
fn buffered_input_replays_after_enter() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.key(30, true);
    r.ev(SourceEvent::BatchEnd);
    r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: true,
    });
    let a = r.ev(SourceEvent::Activated {
        activation_id: 1,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    let s = sends(&a);
    assert!(matches!(s[0], Control::Enter { .. }));
    assert!(matches!(s[1], Control::Held { n: 1, .. }));
    assert_eq!(
        s[2],
        &Control::Key {
            session: 1,
            code: 30,
            pressed: true
        }
    );
    assert!(matches!(
        s[3],
        Control::Button {
            code: BTN_LEFT,
            pressed: true,
            ..
        }
    ));
    // An orphan button release in the buffer bounces (the drag rule).
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.ev(SourceEvent::Button {
        code: BTN_LEFT,
        pressed: false,
    });
    let a = r.ev(SourceEvent::Activated {
        activation_id: 2,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(a, vec![release(2, 800.0, 1077.0)]);
}

#[test]
fn zones_debounce() {
    let mut r = Rig::remote();
    let mut all = Vec::new();
    all.extend(r.ev(SourceEvent::ZonesChanged));
    all.extend(r.wait(30));
    all.extend(r.ev(SourceEvent::ZonesChanged));
    all.extend(r.wait(30));
    all.extend(r.ev(SourceEvent::ZonesChanged));
    assert_eq!(all, vec![leave(1, LeaveReason::CaptureEnded)]);
    assert_eq!(r.m.status().gate, Gate::Rezoning);
    let a = r.wait(99);
    assert!(
        !a.iter().any(|a| matches!(a, SourceAction::Portal(_))),
        "{a:?}"
    );
    let a = r.wait(1);
    assert_eq!(a, vec![SourceAction::Portal(set_barriers(true))]);
    // Still enabled (ZonesChanged does not disable): no second Enable.
    assert!(r.ev(SourceEvent::BarriersSet { failed: vec![] }).is_empty());
    assert_eq!(r.m.status().gate, Gate::Armed);
}

#[test]
fn escape_bind_rearms_after_100ms() {
    // The escape bind: Deactivated + Disabled.
    let mut r = Rig::remote();
    let id = r.activation;
    let a = r.ev(SourceEvent::Deactivated { activation_id: id });
    assert_eq!(a, vec![leave(1, LeaveReason::CaptureEnded)]);
    assert!(r.ev(SourceEvent::Disabled).is_empty());
    assert_eq!(r.m.status().gate, Gate::Rezoning);
    let a = r.wait(100);
    assert!(a.contains(&SourceAction::Portal(set_barriers(true))));
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(r.m.status().gate, Gate::Armed);
    // The next crossing works.
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn barriers_failed_retries_five_times() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::ZonesChanged);
    r.wait(100);
    let a = r.ev(SourceEvent::BarriersSet { failed: vec![1] });
    assert_eq!(a, vec![SourceAction::Portal(PortalCmd::Disable)]);
    assert_eq!(r.m.status().gate, Gate::BarriersFailed);
    let mut retries = 0;
    for _ in 0..8 {
        let a = r.wait(1000);
        if a.iter()
            .any(|a| matches!(a, SourceAction::Portal(PortalCmd::SetBarriers { .. })))
        {
            retries += 1;
            r.ev(SourceEvent::BarriersSet { failed: vec![1] });
        }
    }
    assert_eq!(retries, 5);
    assert_eq!(r.m.status().gate, Gate::BarriersFailed);
    // An activation while failed bounces.
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Local);
}

#[test]
fn suspend() {
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::PrepareForSleep { start: true });
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Suspend),
            release(r.activation, 800.0, 1077.0)
        ]
    );
    assert_eq!(r.m.status().gate, Gate::Suspended);
    // No Disable: the session survives. An activation bounces.
    let a = r.activate(800.0, 1081.0);
    assert_eq!(a, vec![release(r.activation, 800.0, 1077.0)]);
    // Wake: a lock check now, the previous gate back.
    let a = r.ev(SourceEvent::PrepareForSleep { start: false });
    assert_eq!(a, vec![SourceAction::CheckLock]);
    assert_eq!(r.m.status().gate, Gate::Armed);
    // Woke to a lock screen: the answer disables.
    assert_eq!(
        r.ev(SourceEvent::LockPolled(LockCheck::Locked)),
        vec![SourceAction::Portal(PortalCmd::Disable)]
    );
}

#[test]
fn enter_needs_keymap_and_unsuspect_peer() {
    // No keymap yet: bounce.
    let mut r = Rig {
        m: SourceMachine::new(SourceConfig::default()),
        out: Actions::new(),
        now: Micros::from_millis(10_000),
        topo: first_light(),
        activation: 0,
    };
    let topo = r.topo.clone();
    r.ev(SourceEvent::Topology(topo));
    r.ev(SourceEvent::PeerUp(netbook()));
    r.ev(SourceEvent::Tick);
    r.ev(SourceEvent::LockPolled(LockCheck::Unlocked));
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    let a = r.activate(800.0, 1081.0);
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.status().role, SourceRole::Local);
    // The keymap arrives and is sent to the up peer: crosses.
    let a = r.ev(SourceEvent::Keymap {
        text: KEYMAP.into(),
    });
    assert_eq!(
        sends(&a),
        vec![&Control::Keymap {
            serial: 1,
            format: 1,
            text: KEYMAP.into()
        }]
    );
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // Silence: released, and the peer is suspect → its link is a wall, and
    // its edge's barrier comes down (the only one here: Disable).
    let a = r.wait(650);
    assert!(
        a.contains(&SourceAction::Portal(PortalCmd::Disable)),
        "{a:?}"
    );
    assert_eq!(r.m.status().role, SourceRole::Local);
    assert_eq!(r.m.status().gate, Gate::Idle);
    // An activation already on its way bounces.
    let a = r.activate(800.0, 1081.0);
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.status().role, SourceRole::Local);
    // Heard again (a Ping or Pong): the barrier is back, and crossing works.
    assert_eq!(
        r.ev(SourceEvent::PeerAlive(netbook())),
        vec![SourceAction::Portal(set_barriers(false))]
    );
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn keymap_change_reaches_the_target_before_the_next_key() {
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::Keymap {
        text: "xkb_keymap { de };".into(),
    });
    assert_eq!(
        sends(&a),
        vec![&Control::Keymap {
            serial: 2,
            format: 1,
            text: "xkb_keymap { de };".into()
        }]
    );
    assert_eq!(sends(&r.key(30, true)).len(), 1);
}

#[test]
fn an_identical_keymap_is_not_resent() {
    // Hyprland re-adds the EIS keyboard with the same keymap again and
    // again (first light: 159 swaps for 31 crossings). Same text: no
    // new serial, no frame. A real change (de) is a new serial at once.
    let mut r = Rig::remote();
    let sent = r.m.counters().keymaps_sent;
    for _ in 0..5 {
        let a = r.ev(SourceEvent::Keymap {
            text: KEYMAP.into(),
        });
        assert!(sends(&a).is_empty(), "{a:?}");
    }
    assert_eq!(r.m.counters().keymaps_sent, sent);
    // Crossings carry serial 1 still.
    r.ev(SourceEvent::ForceRelease);
    let a = r.activate(800.0, 1081.0);
    assert!(sends(&a).iter().any(|m| matches!(
        m,
        Control::Enter {
            keymap_serial: 1,
            ..
        }
    )));
    // A different text is a new keymap; going back is one more.
    let a = r.ev(SourceEvent::Keymap {
        text: "xkb_keymap { de };".into(),
    });
    assert!(matches!(
        sends(&a).as_slice(),
        [Control::Keymap { serial: 2, .. }]
    ));
    let a = r.ev(SourceEvent::Keymap {
        text: KEYMAP.into(),
    });
    assert!(matches!(
        sends(&a).as_slice(),
        [Control::Keymap { serial: 3, .. }]
    ));
    // A new connection gets the current one, once.
    let a = r.ev(SourceEvent::PeerUp(netbook()));
    assert_eq!(
        sends(&a)
            .iter()
            .filter(|m| matches!(m, Control::Keymap { serial: 3, .. }))
            .count(),
        1
    );
    assert_eq!(r.m.counters().keymaps_sent, sent + 3);
}

/// The peers that got a `Keymap` frame in `a`.
fn keymap_frames(a: &[SourceAction]) -> Vec<PeerId> {
    a.iter()
        .filter_map(|x| match x {
            SourceAction::Send {
                peer,
                msg: Control::Keymap { .. },
            } => Some(*peer),
            _ => None,
        })
        .collect()
}

#[test]
fn one_keymap_is_one_frame_per_connection() {
    // ferryd makes each EIS text canonical, so the keyboard's text and fcitx5's round trip of
    // it reach the machine as one text (first light: 92 frames for 30 crossings without it).
    // Its re-adds, a reconnect and a second peer: one frame per connection, never two.
    let mut r = Rig::armed();
    let sent = r.m.counters().keymaps_sent;
    let readds = |r: &mut Rig| {
        for _ in 0..4 {
            let a = r.ev(SourceEvent::Keymap {
                text: KEYMAP.into(),
            });
            assert!(keymap_frames(&a).is_empty(), "{a:?}");
        }
    };
    readds(&mut r);
    assert_eq!(
        keymap_frames(&r.ev(SourceEvent::PeerUp(third()))),
        [third()]
    );
    readds(&mut r);
    r.ev(SourceEvent::PeerDown(netbook()));
    readds(&mut r);
    assert_eq!(
        keymap_frames(&r.ev(SourceEvent::PeerUp(netbook()))),
        [netbook()]
    );
    readds(&mut r);
    assert_eq!(r.m.counters().keymaps_sent, sent + 2);
    assert_eq!(r.m.keymap_serial(), Some(1));
}

#[test]
fn target_ends_or_goes_down() {
    // Ended from the target: release at exit, no Leave.
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::Control {
        peer: netbook(),
        msg: Control::Ended {
            session: 1,
            reason: LeaveReason::Watchdog.code(),
        },
    });
    assert_eq!(a, vec![release(r.activation, 800.0, 1077.0)]);
    assert_eq!(r.m.status().last_leave, Some(LeaveReason::Watchdog));
    // An Ended for an old session is ignored.
    r.activate(800.0, 1081.0);
    assert!(
        r.ev(SourceEvent::Control {
            peer: netbook(),
            msg: Control::Ended {
                session: 1,
                reason: 0
            },
        })
        .is_empty()
    );
    // Peer down: release at exit, no Leave, and the barrier goes.
    let a = r.ev(SourceEvent::PeerDown(netbook()));
    assert_eq!(
        a,
        vec![
            release(r.activation, 800.0, 1077.0),
            SourceAction::Portal(PortalCmd::Disable)
        ]
    );
    // Its own reason, not `other` (exit check row 14, kill-nb).
    assert_eq!(r.m.status().last_leave, Some(LeaveReason::LinkLost));
}

#[test]
fn force_and_shutdown() {
    let mut r = Rig::remote();
    assert_eq!(
        r.ev(SourceEvent::ForceRelease),
        vec![
            leave(1, LeaveReason::Forced),
            release(r.activation, 800.0, 1077.0)
        ]
    );
    assert!(r.ev(SourceEvent::ForceRelease).is_empty());
    let mut r = Rig::remote();
    assert_eq!(
        r.ev(SourceEvent::Shutdown),
        vec![
            leave(1, LeaveReason::Shutdown),
            release(r.activation, 800.0, 1077.0)
        ]
    );
}

#[test]
fn a_forced_end_in_pending_refuses_its_late_activation() {
    // xdph 1.4.1 rejects a Release with no activation id: the capture goes
    // on, and its Activated comes after the forced end. The row 11
    // confirmation (2026-10-19) saw it twice on the netbook, behind a
    // 27-45 ms lock check.
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.key(30, true);
    assert_eq!(
        r.ev(SourceEvent::ForceRelease),
        vec![SourceAction::Portal(PortalCmd::Release {
            activation_id: None,
            pos: None
        })]
    );
    r.now = r.now.plus(Micros::from_millis(45));
    let a = r.ev(SourceEvent::Activated {
        activation_id: 9,
        barrier_id: 1,
        x: 800.0,
        y: 1081.0,
        lock: LockCheck::Unlocked,
    });
    assert_eq!(a, vec![release(9, 800.0, 1077.0)]);
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.status().role, SourceRole::Local);
    assert_eq!(r.m.counters().bounces, 1);
    // Its Deactivated is ferry's own release; the next capture crosses.
    assert!(
        r.ev(SourceEvent::Deactivated { activation_id: 9 })
            .is_empty()
    );
    r.ev(SourceEvent::StopEmulating);
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn a_refusal_lapses_at_its_deadline_or_a_new_capture() {
    let activated = |r: &mut Rig| {
        r.ev(SourceEvent::Activated {
            activation_id: 9,
            barrier_id: 1,
            x: 800.0,
            y: 1081.0,
            lock: LockCheck::Unlocked,
        })
    };
    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.ev(SourceEvent::ForceRelease);
    r.now = r.now.plus(REFUSE_LATE_ACTIVATION);
    activated(&mut r);
    assert_eq!(r.m.status().role, SourceRole::Remote);

    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.ev(SourceEvent::ForceRelease);
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);

    let mut r = Rig::armed();
    r.ev(SourceEvent::StartEmulating);
    r.ev(SourceEvent::ForceRelease);
    r.ev(SourceEvent::StopEmulating);
    activated(&mut r);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(r.m.counters().bounces, 0);
}

#[test]
fn topology_change_while_remote() {
    let mut r = Rig::remote();
    r.motion(400.0, 300.0);
    // The netbook's monitor shrinks: the model is clamped onto it.
    let monitors = [
        desk_monitors(),
        PeerMonitors {
            peer: netbook(),
            monitors: vec![mon("eDP-1", 0, 0, 1024, 200)],
        },
    ];
    let layout = layout_with_netbook(Rel::Below);
    let t = geom::topology(desk(), &layout, &monitors, &[netbook()]);
    r.ev(SourceEvent::Topology(t));
    assert_eq!(
        r.model(),
        Point {
            x: 1200.0,
            y: 1279.0
        }
    );
    // The netbook's monitors are gone: Leave(layout) + Release.
    let t = geom::topology(desk(), &layout, &[desk_monitors()], &[netbook()]);
    let a = r.ev(SourceEvent::Topology(t));
    assert_eq!(a[0], leave(1, LeaveReason::Layout));
    assert!(matches!(
        a[1],
        SourceAction::Portal(PortalCmd::Release { .. })
    ));
}

/// A third peer right of the netbook: moving right off eDP-1 hops.
#[test]
fn hop_to_a_third_peer() {
    let mut layout = layout_with_netbook(Rel::Below);
    layout.set_origin(Origin {
        peer: third(),
        x: 1643,
        y: 1080,
    });
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 800, 600)],
        },
    ];
    let topo = geom::topology(desk(), &layout, &monitors, &[netbook(), third()]);
    let mut r = Rig::with(topo, SourceConfig::default());
    r.activate(800.0, 1081.0);
    r.motion(0.0, 100.0);
    // Third is not up: a wall.
    r.motion(2000.0, 0.0);
    assert_eq!(
        r.model(),
        Point {
            x: 1642.0,
            y: 1180.0
        }
    );
    let a = r.ev(SourceEvent::PeerUp(third()));
    assert!(matches!(sends(&a).first(), Some(Control::Keymap { .. })));
    let a = r.motion(10.0, 0.0);
    let s: Vec<_> = a
        .iter()
        .filter_map(|a| match a {
            SourceAction::Send { peer, msg } => Some((*peer, msg)),
            _ => None,
        })
        .collect();
    assert_eq!(
        s[0],
        (
            netbook(),
            &Control::Leave {
                session: 1,
                reason: LeaveReason::Hop.code()
            }
        )
    );
    assert_eq!(
        s[1],
        (
            third(),
            &Control::Enter {
                session: 2,
                x: 0.0,
                y: 100.0,
                keymap_serial: 1,
                motion_seq: 2
            }
        )
    );
    assert!(!a.iter().any(|a| matches!(a, SourceAction::Portal(_))));
    assert_eq!(r.m.status().peer, Some(third()));
    // Leaving the third peer releases at the original exit on HDMI-A-1.
    let a = r.ev(SourceEvent::ForceRelease);
    assert_eq!(a[1], release(r.activation, 800.0, 1077.0));
}

#[test]
fn deadlines_fire_before_late_events() {
    // An ack that arrives after the liveness deadline does not save the
    // session: the deadline fires first.
    let mut r = Rig::remote();
    r.now = r.now.plus(Micros::from_millis(700));
    let a = r.ack(1);
    assert_eq!(a.first(), Some(&leave(1, LeaveReason::Silence)));
    assert_eq!(r.m.status().role, SourceRole::Local);
}

#[test]
fn status_is_published_on_change() {
    let mut r = Rig::armed();
    r.m.handle(SourceEvent::StartEmulating, r.now, &mut r.out);
    let a: Vec<_> = r.out.drain().collect();
    assert_eq!(
        a,
        vec![SourceAction::Status(SourceStatus {
            role: SourceRole::Pending,
            peer: None,
            session: 0,
            last_leave: None,
            gate: Gate::Armed,
            suspect: vec![],
            controlled: false,
            locked_here: false,
            locked_peers: vec![],
            edge: None,
        })]
    );
    assert_eq!(Gate::BarriersFailed.as_str(), "barriers_failed");
}

/// While a crossing lasts, the status names the link it went over (the edge glow), and
/// home again it names none.
#[test]
fn the_status_names_the_link_of_the_crossing_in_progress() {
    let mut r = Rig::armed();
    assert_eq!(r.m.status().edge, None);
    r.activate(800.0, 1081.0);
    let edge = r.m.status().edge.expect("remote names its link");
    assert_eq!(edge.side, Side::Bottom);
    assert!(edge.span.0 <= 800 && 800 < edge.span.1, "{edge:?}");
    r.m.handle(SourceEvent::ForceRelease, r.now, &mut r.out);
    assert_eq!(r.m.status().edge, None);
}

// ---- property tests --------------------------------------------------------

#[derive(Debug, Clone)]
enum Ev {
    Wait(u64),
    Activate(f64, LockCheck),
    Motion(f32, f32),
    Key(u16, bool),
    Button(bool),
    BatchEnd,
    Ack(u32),
    AckSession(u32, u32),
    Ended,
    Alive,
    Lock(LockCheck),
    Zones,
    Disabled,
    Deactivated,
    Sleep(bool),
    PeerDown,
    PeerUp,
    Force,
    Restart,
    NoPosition,
}

fn arb_ev() -> impl Strategy<Value = Ev> {
    let lock = prop_oneof![
        8 => Just(LockCheck::Unlocked),
        1 => Just(LockCheck::Locked),
        1 => Just(LockCheck::Unknown),
    ];
    prop_oneof![
        6 => prop_oneof![0u64..120_000, 0u64..900_000].prop_map(Ev::Wait),
        3 => (200.0f64..1800.0, lock.clone()).prop_map(|(x, l)| Ev::Activate(x, l)),
        6 => (-300.0f32..300.0, -300.0f32..300.0).prop_map(|(x, y)| Ev::Motion(x, y)),
        3 => (prop_oneof![Just(1u16), 28u16..32, Just(125), Just(143)], any::<bool>())
            .prop_map(|(c, p)| Ev::Key(c, p)),
        1 => any::<bool>().prop_map(Ev::Button),
        2 => Just(Ev::BatchEnd),
        4 => (0u32..40).prop_map(Ev::Ack),
        1 => (0u32..4, 0u32..40).prop_map(|(s, n)| Ev::AckSession(s, n)),
        1 => Just(Ev::Ended),
        1 => Just(Ev::Alive),
        1 => lock.prop_map(Ev::Lock),
        1 => Just(Ev::Zones),
        1 => Just(Ev::Disabled),
        1 => Just(Ev::Deactivated),
        1 => any::<bool>().prop_map(Ev::Sleep),
        1 => Just(Ev::PeerDown),
        1 => Just(Ev::PeerUp),
        1 => Just(Ev::Force),
        1 => Just(Ev::Restart),
        1 => Just(Ev::NoPosition),
    ]
}

/// Run a sequence through an armed first-light machine, with the timer fired
/// at `next_deadline` (late by `late` µs, but never past the next event),
/// calling `check` after every event.
fn run(
    evs: Vec<Ev>,
    late: u64,
    config: SourceConfig,
    mut check: impl FnMut(&SourceMachine, Micros) -> Result<(), TestCaseError>,
) -> Result<(), TestCaseError> {
    let mut r = Rig::with(first_light(), config);
    let mut id = 100;
    for e in evs {
        let ev = match e {
            Ev::Wait(us) => {
                let until = r.now.plus(Micros(us));
                while let Some(d) = r.m.next_deadline() {
                    if d > until {
                        break;
                    }
                    r.now = d.plus(Micros(late)).min(until).max(r.now);
                    r.ev(SourceEvent::Tick);
                    check(&r.m, r.now)?;
                }
                r.now = until;
                continue;
            }
            Ev::Activate(x, lock) => {
                id += 1;
                r.ev(SourceEvent::StartEmulating);
                SourceEvent::Activated {
                    activation_id: id,
                    barrier_id: 1,
                    x,
                    y: 1081.0,
                    lock,
                }
            }
            Ev::Motion(dx, dy) => SourceEvent::Motion { dx, dy },
            Ev::Key(code, pressed) => SourceEvent::Key {
                code,
                pressed,
                t_eis: r.now,
            },
            Ev::Button(pressed) => SourceEvent::Button {
                code: BTN_LEFT,
                pressed,
            },
            Ev::BatchEnd => SourceEvent::BatchEnd,
            Ev::Ack(n) => SourceEvent::Control {
                peer: netbook(),
                msg: Control::HeldAck {
                    session: r.m.status().session,
                    n,
                },
            },
            Ev::AckSession(session, n) => SourceEvent::Control {
                peer: netbook(),
                msg: Control::HeldAck { session, n },
            },
            Ev::Ended => SourceEvent::Control {
                peer: netbook(),
                msg: Control::Ended {
                    session: r.m.status().session,
                    reason: 12,
                },
            },
            Ev::Alive => SourceEvent::PeerAlive(netbook()),
            Ev::Lock(l) => SourceEvent::LockPolled(l),
            Ev::Zones => SourceEvent::ZonesChanged,
            Ev::Disabled => SourceEvent::Disabled,
            Ev::Deactivated => SourceEvent::Deactivated { activation_id: id },
            Ev::Sleep(start) => SourceEvent::PrepareForSleep { start },
            Ev::PeerDown => SourceEvent::PeerDown(netbook()),
            Ev::PeerUp => SourceEvent::PeerUp(netbook()),
            Ev::Force => SourceEvent::ForceRelease,
            Ev::Restart => SourceEvent::SessionRestarted,
            Ev::NoPosition => {
                id += 1;
                r.ev(SourceEvent::StartEmulating);
                SourceEvent::Activated {
                    activation_id: id,
                    barrier_id: 0,
                    x: f64::NAN,
                    y: f64::NAN,
                    lock: LockCheck::Unlocked,
                }
            }
        };
        r.ev(ev);
        // Re-arm after a re-zone or unlock, as ctl would answer.
        if r.m.awaiting_barriers {
            r.ev(SourceEvent::BarriersSet { failed: vec![] });
        }
        check(&r.m, r.now)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Arbitrary events and times: never Remote at or after
    /// `acked_at + 650 ms`, whatever the acks, cuts and late timers do.
    #[test]
    fn never_remote_past_liveness(
        evs in proptest::collection::vec(arb_ev(), 1..150),
        late in prop_oneof![Just(0u64), 0u64..80_000],
    ) {
        run(evs, late, SourceConfig::default(), |m, now| {
            if let Crossing::Remote(r) = &m.crossing {
                prop_assert!(
                    now < r.acked_at.plus(SOURCE_LIVENESS),
                    "remote at {:?}, acked_at {:?}", now, r.acked_at
                );
                prop_assert!(r.acked_at <= now);
            }
            Ok(())
        })?;
    }

    /// The cursor model is always inside a target monitor while Remote.
    #[test]
    fn model_stays_on_target(
        evs in proptest::collection::vec(arb_ev(), 1..150),
        resistance in prop_oneof![Just(0u16), 0u16..100],
        corner in 0u16..50,
    ) {
        let config = SourceConfig {
            edge_resistance_px: resistance,
            corner_dead_zone_px: corner,
            ..SourceConfig::default()
        };
        let topo = first_light();
        run(evs, 0, config, |m, _| {
            if let Crossing::Remote(r) = &m.crossing {
                prop_assert!(
                    topo.monitors.iter().any(|mon| mon.id.peer == r.peer && mon.rect.contains(r.model)),
                    "model {:?} off target", r.model
                );
            }
            Ok(())
        })?;
    }

    /// Exit and entry are the same plane point within 1 px,
    /// on every side, anywhere on the link.
    #[test]
    fn crossing_is_identity(
        rel in prop_oneof![Just(Rel::Below), Just(Rel::Above), Just(Rel::LeftOf), Just(Rel::RightOf)],
        t in 0.0f64..1.0,
        overshoot in 0.0f64..40.0,
    ) {
        let topo = topo_with_netbook(rel);
        let link = topo.links[0];
        let c = 8.0;
        let along = f64::from(link.span.0) + c + t * f64::from(link.span.1 - link.span.0 - 2 * 8 - 1);
        let line = f64::from(link.line);
        let out = if matches!(link.side, Side::Right | Side::Bottom) { overshoot } else { -overshoot };
        let p = link.side.point(along, line + out);
        let mut r = Rig::with(topo.clone(), SourceConfig::default());
        let a = r.activate(p.x, p.y);
        let (ox, oy) = topo.origin(netbook()).expect("origin");
        let entry = match sends(&a).first() {
            Some(Control::Enter { x, y, .. }) => Point { x: f64::from(*x) + f64::from(ox), y: f64::from(*y) + f64::from(oy) },
            other => return Err(TestCaseError::fail(format!("{rel:?}: {other:?}"))),
        };
        let exit = link.side.point(along, line);
        prop_assert!((entry.x - exit.x).abs() <= 1.0 && (entry.y - exit.y).abs() <= 1.0,
            "{:?}: exit {:?} entry {:?}", rel, exit, entry);
    }
}

// ---- stop-and-wait snapshots (amended 2026-10-16) ----------------

#[derive(Debug, Clone)]
enum Sw {
    Wait(u64),
    /// Acknowledge the outstanding Held (a prompt or late ack).
    AckOutstanding,
    /// An ack for an arbitrary n (stale, duplicate or bogus).
    Ack(u32),
    Key(u16, bool),
    Motion(f32),
    Activate,
}

fn arb_sw() -> impl Strategy<Value = Sw> {
    prop_oneof![
        6 => prop_oneof![0u64..60_000, 0u64..400_000].prop_map(Sw::Wait),
        5 => Just(Sw::AckOutstanding),
        2 => (0u32..12).prop_map(Sw::Ack),
        3 => (28u16..32, any::<bool>()).prop_map(|(c, p)| Sw::Key(c, p)),
        2 => (-40.0f32..40.0).prop_map(Sw::Motion),
        1 => Just(Sw::Activate),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Whatever the acks do: a session's Helds are numbered 1, 2, 3, …, and
    /// Held n + 1 is sent only after Held n was acknowledged. So at most one
    /// is ever unacknowledged, and after a target → source cut at most one
    /// more reaches the target.
    #[test]
    fn held_is_stop_and_wait(evs in proptest::collection::vec(arb_sw(), 1..200)) {
        let mut r = Rig::armed();
        // Per session: the newest Held n sent, and whether it was acked.
        let mut last: Option<(u32, u32, bool)> = None;
        let mut check = |a: &[SourceAction], session: u32, acked: Option<u32>|
            -> Result<(), TestCaseError>
        {
            if let (Some(n), Some((s, m, done))) = (acked, last.as_mut())
                && *s == session
                && *m == n
            {
                *done = true;
            }
            for n in helds(a) {
                match last {
                    Some((s, m, done)) if s == session => {
                        prop_assert_eq!(n, m + 1, "Helds of a session are consecutive");
                        prop_assert!(done, "Held {} sent before Held {} was acked", n, m);
                    }
                    _ => prop_assert_eq!(n, 1, "a session starts at Held 1"),
                }
                last = Some((session, n, false));
            }
            Ok(())
        };
        for e in evs {
            let session = r.m.status().session;
            let (a, acked) = match e {
                Sw::Wait(us) => (r.wait_us(us), None),
                Sw::AckOutstanding => {
                    let n = match &r.m.crossing {
                        Crossing::Remote(rem) => Some(rem.held_n),
                        _ => None,
                    };
                    (r.ack_outstanding(), n)
                }
                Sw::Ack(n) => {
                    // Counts as acked only if it is the outstanding one.
                    let hit = matches!(&r.m.crossing,
                        Crossing::Remote(rem) if rem.held_n == n && !rem.held_acked);
                    (r.ack(n), hit.then_some(n))
                }
                Sw::Key(c, p) => (r.key(c, p), None),
                Sw::Motion(dx) => (r.motion(dx, 0.0), None),
                Sw::Activate => {
                    if r.m.status().role == SourceRole::Local {
                        r.ev(SourceEvent::PeerAlive(netbook()));
                        (r.activate(800.0, 1081.0), None)
                    } else {
                        (Vec::new(), None)
                    }
                }
            };
            // An Enter starts a new session; its Held 1 is checked against it.
            let session = if a.iter().any(|x| matches!(x,
                SourceAction::Send { msg: Control::Enter { .. }, .. }))
            {
                r.m.status().session
            } else {
                session
            };
            check(&a, session, acked)?;
        }
    }

    /// Source and target machines over a link with small one-way delays,
    /// cut target → source at a random moment of a crossing (drill D3).
    /// With stop-and-wait, at most one Held lands after the cut, so the
    /// target releases within 100 ms + its 650 ms watchdog of the cut even
    /// when the source's `Leave(silence)` never arrives; when it arrives, the
    /// release comes with it, ≤ 650 ms after the cut plus the delay.
    #[test]
    fn one_way_cut_releases_the_target_within_750_ms(
        cut_ms in 0u64..1500,
        cut_us in 0u64..1000,
        to_b in 50u64..5000,
        to_a in 50u64..5000,
        leave_lost in any::<bool>(),
    ) {
        let (released, local_at, cut) = one_way_cut(
            Micros::from_millis(cut_ms).plus(Micros(cut_us)),
            Micros(to_b),
            Micros(to_a),
            leave_lost,
        );
        let released = released.expect("the target released");
        let after = released.since(cut);
        if leave_lost {
            prop_assert!(after <= SNAPSHOT_PERIOD.plus(crate::time::TARGET_WATCHDOG),
                "target released {} µs after the cut", after.0);
        } else {
            prop_assert!(after <= SOURCE_LIVENESS.plus(Micros(to_b)),
                "target released {} µs after the cut", after.0);
        }
        // The source's own release: ≤ 650 ms after the cut.
        let local_at = local_at.expect("the source released");
        prop_assert!(local_at.since(cut) <= SOURCE_LIVENESS,
            "source released {} µs after the cut", local_at.since(cut).0);
    }
}

/// Run one crossing first light → netbook through a simulated link: frames
/// a → b arrive after `to_b`, b → a after `to_a`, and every b → a frame sent
/// at or after `cut` (relative to the Enter) is lost. With `leave_lost`, the
/// source's `Leave` is lost too (its control stream stalled behind the cut,
/// as in drill D3). Returns when the target released (`ReleaseAll`) and when
/// the source went local, and the absolute cut time.
fn one_way_cut(
    cut: Micros,
    to_b: Micros,
    to_a: Micros,
    leave_lost: bool,
) -> (Option<Micros>, Option<Micros>, Micros) {
    use crate::target::{InjectCmd, TargetAction, TargetEvent, TargetMachine};

    let mut r = Rig::armed();
    let mut t = TargetMachine::new();
    let mut tout = Actions::new();
    let start = r.now;
    t.handle(TargetEvent::Start, start, &mut tout);
    t.handle(TargetEvent::PeerUp(desk()), start, &mut tout);
    t.handle(
        TargetEvent::Control {
            peer: desk(),
            msg: Control::Keymap {
                serial: 1,
                format: 1,
                text: KEYMAP.into(),
            },
        },
        start,
        &mut tout,
    );
    tout.drain().for_each(drop);

    // In flight: (arrival, to the target?, frame).
    let mut wire: Vec<(Micros, bool, Control)> = Vec::new();
    let enter = r.activate(800.0, 1081.0);
    let cut = r.now.plus(cut);
    let send_a = |wire: &mut Vec<(Micros, bool, Control)>, now: Micros, a: Vec<SourceAction>| {
        for x in a {
            if let SourceAction::Send { msg, .. } = x {
                if leave_lost && matches!(msg, Control::Leave { .. }) {
                    continue;
                }
                wire.push((now.plus(to_b), true, msg));
            }
        }
    };
    send_a(&mut wire, r.now, enter);
    let mut released = None;
    let mut local_at = None;
    let end = cut.plus(Micros::from_millis(3000));
    loop {
        let next_wire = wire.iter().map(|w| w.0).min();
        let next = [r.m.next_deadline(), t.next_deadline(), next_wire]
            .into_iter()
            .flatten()
            .min();
        let Some(now) = next.filter(|n| *n <= end) else {
            break;
        };
        r.now = now.max(r.now);
        let now = r.now;
        // Deadlines first, as the threads do.
        if r.m.next_deadline().is_some_and(|d| d <= now) {
            let a = r.ev(SourceEvent::Tick);
            send_a(&mut wire, now, a);
        }
        if t.next_deadline().is_some_and(|d| d <= now) {
            t.handle(TargetEvent::Tick, now, &mut tout);
        }
        let due: Vec<_> = {
            let (due, rest): (Vec<_>, Vec<_>) = wire.drain(..).partition(|w| w.0 <= now);
            wire = rest;
            due
        };
        for (_, to_target, msg) in due {
            if to_target {
                t.handle(TargetEvent::Control { peer: desk(), msg }, now, &mut tout);
            } else {
                let a = r.ev(SourceEvent::Control {
                    peer: netbook(),
                    msg,
                });
                send_a(&mut wire, now, a);
            }
        }
        for x in tout.drain() {
            match x {
                TargetAction::Send { msg, .. } if now < cut => {
                    wire.push((now.plus(to_a), false, msg));
                }
                TargetAction::Inject(InjectCmd::ReleaseAll) if now >= cut => {
                    released.get_or_insert(now);
                }
                _ => {}
            }
        }
        if local_at.is_none() && r.m.status().role == SourceRole::Local {
            local_at = Some(now);
        }
        if released.is_some() && local_at.is_some() {
            break;
        }
    }
    (released, local_at, cut)
}

#[test]
fn d3_one_way_cut_worst_case() {
    // The cut lands just after an ack left the target: the next Held is
    // already due at +100 and is the one that lands. Without its Leave, the
    // target releases at that Held + 650 ms, inside 750 ms of the cut.
    for cut_us in [0u64, 100, 600, 99_400, 99_900, 100_100, 450_000] {
        let (released, local_at, cut) = one_way_cut(Micros(cut_us), Micros(300), Micros(300), true);
        let after = released.unwrap().since(cut);
        assert!(after <= Micros::from_millis(750), "{cut_us}: {after:?}");
        assert!(local_at.unwrap().since(cut) <= SOURCE_LIVENESS);
    }
}

// ---- both peers capture, lock to this machine ----

fn portal(a: &[SourceAction]) -> Vec<&PortalCmd> {
    a.iter()
        .filter_map(|a| match a {
            SourceAction::Portal(c) => Some(c),
            _ => None,
        })
        .collect()
}

#[test]
fn controlled_disables_and_restores() {
    let mut r = Rig::armed();
    // Another peer entered this one: the empty-barrier row.
    assert_eq!(
        r.ev(SourceEvent::Controlled(true)),
        vec![SourceAction::Portal(PortalCmd::Disable)]
    );
    assert_eq!(r.m.status().gate, Gate::Idle);
    assert!(r.m.status().controlled);
    // An activation that was already on its way bounces (and is counted).
    let before = r.m.counters().activations;
    let a = r.activate(800.0, 1081.0);
    assert!(no_input_forwarded(&a));
    assert_eq!(r.m.counters().activations, before + 1);
    assert_eq!(r.m.status().role, SourceRole::Local);
    // A topology change while controlled arms nothing.
    assert!(portal(&r.ev(SourceEvent::Topology(first_light()))).is_empty());
    // The session ended (devices gone): barriers again, then Enable → Armed.
    assert_eq!(
        r.ev(SourceEvent::Controlled(false)),
        vec![SourceAction::Portal(set_barriers(false))]
    );
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(r.m.status().gate, Gate::Armed);
    assert!(!r.m.status().controlled);
    // And crossing works again.
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
}

#[test]
fn controlled_while_remote_preempts() {
    let mut r = Rig::remote();
    let a = r.ev(SourceEvent::Controlled(true));
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Preempted),
            release(r.activation, 800.0, 1077.0),
            SourceAction::Portal(PortalCmd::Disable),
        ]
    );
    assert_eq!(r.m.status().last_leave, Some(LeaveReason::Preempted));
    assert_eq!(r.m.status().gate, Gate::Idle);
    // Pending and Arming give up too, releasing inside.
    for config in [
        SourceConfig::default(),
        SourceConfig {
            edge_dwell_ms: 300,
            ..SourceConfig::default()
        },
    ] {
        let pending = config.edge_dwell_ms == 0;
        let mut r = Rig::with(first_light(), config);
        if pending {
            r.ev(SourceEvent::StartEmulating);
            assert_eq!(r.m.status().role, SourceRole::Pending);
        } else {
            r.activate(800.0, 1081.0);
            assert_eq!(r.m.status().role, SourceRole::Arming);
        }
        let a = r.ev(SourceEvent::Controlled(true));
        assert!(no_input_forwarded(&a));
        assert!(matches!(
            portal(&a)[..],
            [PortalCmd::Release { .. }, PortalCmd::Disable]
        ));
        assert_eq!(r.m.status().role, SourceRole::Local);
    }
}

#[test]
fn locked_here_removes_barriers() {
    let mut r = Rig::armed();
    assert_eq!(
        r.ev(SourceEvent::LockedHere(true)),
        vec![SourceAction::Portal(PortalCmd::Disable)]
    );
    assert_eq!(r.m.status().gate, Gate::Idle);
    assert!(r.m.status().locked_here);
    // A screen lock and unlock while locked here arm nothing (at most a
    // second Disable).
    r.ev(SourceEvent::LockPolled(LockCheck::Locked));
    assert!(
        portal(&r.ev(SourceEvent::LockPolled(LockCheck::Unlocked)))
            .iter()
            .all(|c| **c == PortalCmd::Disable)
    );
    assert_eq!(r.m.status().gate, Gate::Idle);
    // Unlock: barriers, Enable.
    assert_eq!(
        r.ev(SourceEvent::LockedHere(false)),
        vec![SourceAction::Portal(set_barriers(false))]
    );
    assert_eq!(
        r.ev(SourceEvent::BarriersSet { failed: vec![] }),
        vec![SourceAction::Portal(PortalCmd::Enable)]
    );
    assert_eq!(r.m.status().gate, Gate::Armed);

    // Locked during a crossing: nothing moves, the session goes on, and the
    // pointer can still come home (that crossing starts from the target).
    let mut r = Rig::remote();
    assert!(r.ev(SourceEvent::LockedHere(true)).is_empty());
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(sends(&r.motion(0.0, 50.0)).len(), 0);
    let a = r.motion(0.0, -100.0);
    assert_eq!(a[0], leave(1, LeaveReason::Home));
    // Home: now its links are walls.
    assert_eq!(a.last(), Some(&SourceAction::Portal(PortalCmd::Disable)));
    assert_eq!(r.m.status().gate, Gate::Idle);

    // Locked while arming: about to cross out of a locked peer, so it bounces.
    let mut r = Rig::with(
        first_light(),
        SourceConfig {
            edge_dwell_ms: 300,
            ..SourceConfig::default()
        },
    );
    r.activate(800.0, 1081.0);
    let a = r.ev(SourceEvent::LockedHere(true));
    assert!(matches!(
        portal(&a)[..],
        [PortalCmd::Release { .. }, PortalCmd::Disable]
    ));
    assert!(no_input_forwarded(&a));
}

#[test]
fn locked_target_walls_its_links() {
    let mut r = Rig::remote();
    r.motion(0.0, 100.0);
    r.ev(SourceEvent::PeerLocked {
        peer: netbook(),
        locked: true,
    });
    assert_eq!(r.m.status().locked_peers, vec![netbook()]);
    // Up into the link home: the model clamps at the netbook's top edge.
    r.motion(0.0, -500.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert_eq!(
        r.model(),
        Point {
            x: 800.0,
            y: 1080.0
        }
    );
    assert!(r.motion(0.0, -50.0).is_empty());
    // The escape chord still releases.
    r.key(125, true);
    r.key(29, true);
    let a = r.key(1, true);
    assert_eq!(a[0], leave(1, LeaveReason::EscapeChord));
    assert_eq!(r.m.status().role, SourceRole::Local);
    // Entering a locked peer is allowed, and unlocking opens the way home.
    r.activate(800.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    r.ev(SourceEvent::PeerLocked {
        peer: netbook(),
        locked: false,
    });
    let a = r.motion(0.0, -10.0);
    assert_eq!(a[0], leave(2, LeaveReason::Home));
    // A peer that goes down is no longer locked.
    r.ev(SourceEvent::PeerLocked {
        peer: netbook(),
        locked: true,
    });
    r.ev(SourceEvent::PeerDown(netbook()));
    assert!(r.m.status().locked_peers.is_empty());
}

#[test]
fn locked_third_peer_walls_the_hop_out_of_it_only() {
    let mut layout = layout_with_netbook(Rel::Below);
    layout.set_origin(Origin {
        peer: third(),
        x: 1643,
        y: 1080,
    });
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 800, 600)],
        },
    ];
    let topo = geom::topology(desk(), &layout, &monitors, &[netbook(), third()]);
    let mut r = Rig::with(topo, SourceConfig::default());
    r.ev(SourceEvent::PeerUp(third()));
    // The third peer is locked: hopping into it is still allowed.
    r.ev(SourceEvent::PeerLocked {
        peer: third(),
        locked: true,
    });
    r.activate(800.0, 1081.0);
    r.motion(0.0, 100.0);
    r.motion(2000.0, 0.0);
    assert_eq!(r.m.status().peer, Some(third()));
    // Out of it, back to the netbook: a wall.
    r.motion(-500.0, 0.0);
    assert_eq!(r.m.status().peer, Some(third()));
    assert_eq!(r.model().x, 1643.0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Any lock toggles on either side, at any point of a crossing: every
    /// release path still ends it at once.
    #[test]
    fn lock_never_blocks_a_release(
        toggles in proptest::collection::vec((any::<bool>(), any::<bool>(), 0u8..4), 0..6),
        path in 0u8..9,
    ) {
        let mut r = Rig::remote();
        for (here, on, when) in toggles {
            match when {
                0 => { r.motion(0.0, 30.0); }
                1 => { r.ack_outstanding(); }
                2 => { r.key(30, true); }
                _ => {}
            }
            let ev = if here {
                SourceEvent::LockedHere(on)
            } else {
                SourceEvent::PeerLocked { peer: netbook(), locked: on }
            };
            r.ev(ev);
            prop_assert_eq!(r.m.status().role, SourceRole::Remote);
        }
        match path {
            0 => {
                r.key(125, true);
                r.key(29, true);
                r.key(1, true);
            }
            1 => { r.ev(SourceEvent::ForceRelease); }
            2 => { r.wait(700); }
            3 => { r.ev(SourceEvent::LockPolled(LockCheck::Locked)); }
            4 => { r.ev(SourceEvent::PrepareForSleep { start: true }); }
            5 => {
                let session = r.m.status().session;
                r.ev(SourceEvent::Control {
                    peer: netbook(),
                    msg: Control::Ended { session, reason: LeaveReason::Takeover.code() },
                });
            }
            6 => { r.ev(SourceEvent::Deactivated { activation_id: r.activation }); }
            7 => { r.ev(SourceEvent::Shutdown); }
            _ => { r.ev(SourceEvent::Controlled(true)); }
        }
        prop_assert_eq!(r.m.status().role, SourceRole::Local);
    }
}

// ---- the cost of a wall (M1 exit findings F3 and F4) ---------------

/// Hyprland and xdph as far as barriers go. A motion that crosses an armed,
/// enabled barrier's line activates, and `Activated` carries the unclamped
/// next position (the netbook logged x = -38 at its corner, F3). The
/// activating motion is captured. `Release` warps the cursor; with no barrier
/// the cursor stops at the screen edge. `SetBarriers` and cursor reads are
/// answered at once.
struct Sim {
    r: Rig,
    armed: Vec<Barrier>,
    enabled: bool,
    cursor: Point,
    captured: bool,
    activations: u32,
}

impl Sim {
    fn new(r: Rig, x: f64, y: f64) -> Self {
        let armed = r.m.barriers.clone();
        Sim {
            r,
            armed,
            enabled: true,
            cursor: Point { x, y },
            captured: false,
            activations: 0,
        }
    }

    fn apply(&mut self, actions: Vec<SourceAction>) {
        let mut queue: std::collections::VecDeque<_> = actions.into();
        while let Some(a) = queue.pop_front() {
            match a {
                SourceAction::Portal(PortalCmd::Release { pos, .. }) => {
                    self.captured = false;
                    if let Some(p) = pos {
                        self.cursor = p;
                    }
                }
                SourceAction::Portal(PortalCmd::SetBarriers { barriers, .. }) => {
                    self.armed = barriers;
                    queue.extend(self.r.ev(SourceEvent::BarriersSet { failed: vec![] }));
                }
                SourceAction::Portal(PortalCmd::Enable) => self.enabled = true,
                SourceAction::Portal(PortalCmd::Disable) => self.enabled = false,
                SourceAction::CheckCursor => {
                    let p = self.cursor;
                    queue.extend(self.r.ev(SourceEvent::CursorPolled(Some(p))));
                }
                _ => {}
            }
        }
    }

    /// One motion of the hand, then `ms` of time.
    fn step(&mut self, dx: f64, dy: f64, ms: u64) {
        if self.captured {
            let a = self.r.motion(dx as f32, dy as f32);
            self.apply(a);
        } else {
            let next = Point {
                x: self.cursor.x + dx,
                y: self.cursor.y + dy,
            };
            let hit = self
                .armed
                .iter()
                .copied()
                .find(|b| self.enabled && crosses_line(b, self.cursor, next));
            match hit {
                Some(b) => {
                    self.activations += 1;
                    self.captured = true;
                    self.r.activation += 5;
                    let mut a = self.r.ev(SourceEvent::StartEmulating);
                    a.extend(self.r.motion(dx as f32, dy as f32));
                    a.extend(self.r.ev(SourceEvent::Activated {
                        activation_id: self.r.activation,
                        barrier_id: b.id,
                        x: next.x,
                        y: next.y,
                        lock: LockCheck::Unlocked,
                    }));
                    self.apply(a);
                }
                // The desk's two monitors span [0, 3840) × [0, 1080).
                None => {
                    self.cursor = Point {
                        x: next.x.clamp(0.0, 3839.0),
                        y: next.y.clamp(0.0, 1079.0),
                    }
                }
            }
        }
        let a = self.r.wait(ms);
        self.apply(a);
    }

    fn remote(&self) -> bool {
        self.r.m.status().role == SourceRole::Remote
    }
}

/// The motion `from → to` crosses `b`'s line from a cursor on its monitor
/// (where along the line does not matter: the netbook's corner activated
/// with x = -38).
fn crosses_line(b: &Barrier, from: Point, to: Point) -> bool {
    let (x1, y1, x2, y2) = b.pos;
    let (x, y) = (f64::from(x1), f64::from(y1));
    let on = |v: f64, lo: i32, hi: i32| v >= f64::from(lo) && v <= f64::from(hi);
    match b.side {
        Side::Bottom => on(from.x, x1, x2) && from.y < y && to.y >= y,
        Side::Top => on(from.x, x1, x2) && from.y >= y && to.y < y,
        Side::Right => on(from.y, y1, y2) && from.x < x && to.x >= x,
        Side::Left => on(from.y, y1, y2) && from.x >= x && to.x < x,
    }
}

/// The desk with the netbook below HDMI-A-1 at `align`.
fn netbook_below(align: Align) -> Topology {
    let mut layout = Layout::empty(desk());
    layout.set_origin(Origin {
        peer: desk(),
        x: 0,
        y: 0,
    });
    let monitors = [desk_monitors(), netbook_monitors()];
    let o = geom::place(
        &layout,
        &monitors,
        netbook(),
        Rel::Below,
        Anchor::Monitor(hdmi()),
        align,
    )
    .unwrap();
    layout.set_origin(o);
    geom::topology(desk(), &layout, &monitors, &[netbook()])
}

/// F3's trace, mirrored onto the desk: the link starts at the screen's
/// corner (align start, link [0, 1366)), the hand pushes down and left into
/// the corner, then right along the edge, still pushing down, until it
/// crosses at x = 8. Before BL-009 every step on the dead corner was an
/// activation and a bounce (the netbook logged 16 in 0.3 s).
fn f3_trace(sim: &mut Sim) -> u32 {
    for _ in 0..8 {
        sim.step(-5.0, 8.0, 8);
    }
    let mut steps = 0;
    while !sim.remote() && steps < 60 {
        sim.step(1.0, 8.0, 8);
        steps += 1;
    }
    steps
}

#[test]
fn the_f3_corner_costs_one_bounce_then_crosses() {
    let r = Rig::with(netbook_below(Align::Start), SourceConfig::default());
    let mut sim = Sim::new(r, 12.0, 1060.0);
    let steps = f3_trace(&mut sim);
    assert!(sim.remote(), "never crossed");
    // One bounce in the corner, then the crossing: 2 activations, not 17.
    assert_eq!(sim.activations, 2, "{:?}", sim.r.m.counters());
    assert_eq!(sim.r.m.counters().bounces, 1);
    assert_eq!(sim.r.m.counters().edge_parks, 1);
    // The barrier came back within one cursor poll of the cursor reaching
    // x = 8, the first contact that crosses: the slide took at most
    // 8 steps plus 40 ms of polling (5 steps of 8 ms).
    assert!(steps <= 8 + 5 + 1, "{steps} steps");
    // The crossing holds: no home while the push goes on into the netbook.
    for _ in 0..12 {
        sim.step(1.0, 8.0, 8);
        assert!(sim.remote());
    }
    // And the barrier list is whole again once home.
    sim.step(0.0, -200.0, 8);
    assert!(!sim.remote());
    assert_eq!(sim.armed, geom::barriers(&sim.r.topo, &[netbook()]));
}

#[test]
fn a_two_second_push_on_a_stretch_with_no_link_is_one_activation() {
    // First light: no link on x < 277. 250 motions of 8 ms at x = 100.
    let mut sim = Sim::new(Rig::armed(), 100.0, 1075.0);
    for _ in 0..250 {
        sim.step(0.0, 8.0, 8);
    }
    // The park ends after PARK_MAX (2 s) with the cursor still there: one
    // more bounce, and parked again.
    assert!(sim.activations <= 2, "{} activations", sim.activations);
    assert!(!sim.remote());
    // Along the edge into the link: it crosses.
    for _ in 0..30 {
        sim.step(20.0, 8.0, 8);
        if sim.remote() {
            break;
        }
    }
    assert!(sim.remote());
}

#[test]
fn a_two_second_push_at_a_suspect_peer_costs_nothing() {
    let mut sim = Sim::new(Rig::remote(), 800.0, 1077.0);
    sim.captured = true;
    // The target goes silent: released, suspect, and its edge a wall.
    let a = sim.r.wait(650);
    sim.apply(a);
    assert!(!sim.remote());
    assert_eq!(sim.r.m.status().suspect, vec![netbook()]);
    for _ in 0..250 {
        sim.step(0.0, 8.0, 8);
    }
    assert_eq!(sim.activations, 0);
    assert_eq!(sim.r.m.counters().bounces, 0);
    // Heard again: the barrier is back, and the next push crosses.
    let a = sim.r.ev(SourceEvent::PeerAlive(netbook()));
    sim.apply(a);
    assert!(sim.enabled);
    sim.step(0.0, 8.0, 8);
    assert!(sim.remote());
    assert_eq!(sim.activations, 1);
}

#[test]
fn a_suspect_peer_walls_only_its_own_stretch() {
    // HDMI-A-1's bottom edge links to the netbook on [277, 1643) and to a
    // third peer on [1643, 1920); DVI-D-1's to the third peer only.
    let mut layout = layout_with_netbook(Rel::Below);
    layout.set_origin(Origin {
        peer: third(),
        x: 1643,
        y: 1080,
    });
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 800, 600)],
        },
    ];
    let topo = geom::topology(desk(), &layout, &monitors, &[netbook(), third()]);
    let mut r = Rig::with(topo.clone(), SourceConfig::default());
    r.ev(SourceEvent::PeerUp(third()));
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    let all = geom::barriers(&topo, &[netbook(), third()]);
    assert_eq!(r.m.barriers, all);
    r.activate(800.0, 1081.0);
    r.wait(650);
    assert_eq!(r.m.status().suspect, vec![netbook()]);
    // HDMI-A-1's edge still crosses to the third peer: its barrier stays,
    // with its id. The netbook's stretch is a wall stretch: a bounce there
    // parks the barrier like a dead corner.
    assert_eq!(r.m.barriers, all);
    let a = r.activate(800.0, 1081.0);
    assert!(no_input_forwarded(&a));
    let hdmi_barrier = all[0];
    assert_eq!(hdmi_barrier.monitor, hdmi());
    assert_eq!(
        portal(&a)[1],
        &PortalCmd::SetBarriers {
            refresh_zones: false,
            barriers: all[1..].to_vec()
        }
    );
    // The cursor moves along to the third peer's stretch: back up.
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    let a = r.wait(40);
    assert_eq!(a, vec![SourceAction::CheckCursor]);
    let a = r.ev(SourceEvent::CursorPolled(Some(Point {
        x: 1700.0,
        y: 1079.0,
    })));
    assert_eq!(
        portal(&a),
        vec![&PortalCmd::SetBarriers {
            refresh_zones: false,
            barriers: all.clone()
        }]
    );
}

#[test]
fn a_parked_barrier_returns_when_the_cursor_leaves_the_wall() {
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    assert_eq!(r.m.status().gate, Gate::Armed);
    // Polled every 40 ms while the cursor stays on the wall stretch.
    for _ in 0..3 {
        assert_eq!(r.wait(40), vec![SourceAction::CheckCursor]);
        let a = r.ev(SourceEvent::CursorPolled(Some(Point {
            x: 150.0,
            y: 1079.0,
        })));
        assert!(a.is_empty(), "{a:?}");
    }
    // Still on the dead corner of the link (x < 285): parked.
    r.wait(40);
    let a = r.ev(SourceEvent::CursorPolled(Some(Point {
        x: 284.0,
        y: 1079.0,
    })));
    assert!(a.is_empty(), "{a:?}");
    // On the link: back up at once.
    r.wait(40);
    let a = r.ev(SourceEvent::CursorPolled(Some(Point {
        x: 285.0,
        y: 1079.0,
    })));
    assert_eq!(a, vec![SourceAction::Portal(set_barriers(false))]);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    // No more polls.
    assert!(!r.wait(500).contains(&SourceAction::CheckCursor));

    // Away from the edge (more than 8 px): back up too.
    r.activate(100.0, 1081.0);
    r.wait(40);
    let a = r.ev(SourceEvent::CursorPolled(Some(Point {
        x: 100.0,
        y: 1072.0,
    })));
    assert!(portal(&a).is_empty());
    r.wait(40);
    let a = r.ev(SourceEvent::CursorPolled(Some(Point {
        x: 100.0,
        y: 1071.0,
    })));
    assert_eq!(a, vec![SourceAction::Portal(set_barriers(false))]);

    // Two failed reads in a row put it back; one is forgiven (a busy
    // compositor can time a read out).
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    r.activate(100.0, 1081.0);
    for read in [
        None,
        Some(Point {
            x: 100.0,
            y: 1079.0,
        }),
        None,
    ] {
        r.wait(40);
        assert!(r.ev(SourceEvent::CursorPolled(read)).is_empty());
    }
    r.wait(40);
    let a = r.ev(SourceEvent::CursorPolled(None));
    assert_eq!(a, vec![SourceAction::Portal(set_barriers(false))]);
}

#[test]
fn a_park_ends_after_park_max() {
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    // Nobody answers the reads: the park still ends at PARK_MAX.
    let a = r.wait(1999);
    assert!(!a.iter().any(|a| matches!(a, SourceAction::Portal(_))));
    let a = r.wait(1);
    assert_eq!(portal(&a), vec![&set_barriers(false)]);
    assert!(!r.wait(500).contains(&SourceAction::CheckCursor));
}

#[test]
fn every_arm_puts_parked_barriers_back() {
    // Lock and unlock.
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    r.ev(SourceEvent::LockPolled(LockCheck::Locked));
    assert_eq!(
        portal(&r.ev(SourceEvent::LockPolled(LockCheck::Unlocked))),
        vec![&set_barriers(true)]
    );
    // A re-zone (the escape bind's Disabled).
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    r.ev(SourceEvent::Disabled);
    let a = r.wait(100);
    assert!(portal(&a).contains(&&set_barriers(true)), "{a:?}");
    // Controlled, then not.
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    r.ev(SourceEvent::BarriersSet { failed: vec![] });
    assert_eq!(
        portal(&r.ev(SourceEvent::Controlled(true))),
        vec![&PortalCmd::Disable]
    );
    assert_eq!(
        portal(&r.ev(SourceEvent::Controlled(false))),
        vec![&set_barriers(false)]
    );
}

#[test]
fn a_late_activation_on_a_parked_barrier_still_maps_to_its_edge() {
    let mut r = Rig::armed();
    r.activate(100.0, 1081.0);
    // The next motion's Activated was already on its way.
    let a = r.activate(110.0, 1081.0);
    assert_eq!(a, vec![release(r.activation, 110.0, 1077.0)]);
    assert_eq!(r.m.counters().edge_parks, 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any hand motion at the bottom edge of first light: never more than
    /// one bounce per visit to a wall stretch, where a visit ends when the
    /// cursor leaves it (onto the link, or 8 px away) or after PARK_MAX; and
    /// whenever the cursor pushes down on the link with the barrier up, it
    /// crosses.
    #[test]
    fn walls_cost_one_bounce_per_visit(
        steps in proptest::collection::vec((-30.0f64..30.0, -6.0f64..14.0, 1u64..20), 1..300),
        x0 in 0.0f64..1920.0,
    ) {
        let mut sim = Sim::new(Rig::armed(), x0, 1070.0);
        let mut visits = 0u32;
        let mut on_wall = false;
        let mut since = Micros::ZERO;
        for (dx, dy, ms) in steps {
            let bounces = sim.r.m.counters().bounces;
            let from = sim.cursor;
            let next = Point { x: from.x + dx, y: from.y + dy };
            let must_cross = sim.enabled
                && sim.armed.iter().any(|b| crosses_line(b, from, next))
                && (285.0..1635.0).contains(&next.x);
            sim.step(dx, dy, ms);
            prop_assert!(!must_cross || sim.remote(), "a push on the link at {next:?} did not cross");
            if sim.remote() {
                // Home again, to keep exercising the edge.
                sim.step(0.0, -2000.0, 8);
                prop_assert!(!sim.remote());
                on_wall = false;
                continue;
            }
            if sim.r.m.counters().bounces > bounces {
                if !on_wall || sim.r.now.since(since) >= PARK_MAX {
                    visits += 1;
                    since = sim.r.now;
                }
                on_wall = true;
            }
            // Off the wall: on the link, on DVI-D-1, or more than 8 px from
            // the edge (with a hair of slack for the machine's float compare).
            let c = sim.cursor;
            if 1080.0 - c.y > PARK_NEAR_PX - 0.01
                || (284.99..1635.01).contains(&c.x)
                || c.x >= 1919.99
            {
                on_wall = false;
            }
        }
        prop_assert!(
            sim.r.m.counters().bounces <= u64::from(visits),
            "{} bounces for {} visits", sim.r.m.counters().bounces, visits
        );
    }
}

// ---- non-finite EIS values (A-01) ------------------------------

/// A NaN delta would make the model NaN: no monitor contains it, so every
/// later motion is dropped and the crossing can never come home.
#[test]
fn a_non_finite_motion_is_dropped_and_the_crossing_still_goes_home() {
    let mut r = Rig::remote();
    let before = r.model();
    for (dx, dy) in [
        (f32::NAN, 0.0),
        (0.0, f32::INFINITY),
        (f32::NEG_INFINITY, f32::NAN),
    ] {
        assert_eq!(r.motion(dx, dy), vec![]);
    }
    assert_eq!(r.model(), before);
    assert_eq!(r.m.counters().nonfinite_dropped, 3);
    for _ in 0..50 {
        r.motion(0.0, -2000.0);
        r.ack_outstanding();
        if r.m.status().role == SourceRole::Local {
            break;
        }
    }
    assert_eq!(r.m.status().role, SourceRole::Local);
}

#[test]
fn a_non_finite_scroll_is_dropped() {
    let mut r = Rig::remote();
    let scroll = |value| {
        SourceEvent::Scroll(ScrollIn {
            axis: 0,
            source: 0,
            value,
            value120: Some(120),
            stop: false,
        })
    };
    assert_eq!(r.ev(scroll(f32::NAN)), vec![]);
    assert_eq!(r.ev(scroll(f32::INFINITY)), vec![]);
    assert_eq!(r.m.counters().nonfinite_dropped, 2);
    assert!(matches!(
        sends(&r.ev(scroll(15.0)))[..],
        [Control::Scroll { .. }]
    ));
}

#[derive(Debug, Clone)]
enum Raw {
    Wait(u64),
    Activate(f64),
    Motion(u32, u32),
    Scroll(u32),
    Button(bool),
    Ack,
}

fn arb_raw() -> impl Strategy<Value = Raw> {
    // Half the deltas are small finite ones, so crossings happen.
    let bits = prop_oneof![
        any::<u32>(),
        (-300.0f32..300.0).prop_map(f32::to_bits),
        Just(f32::NAN.to_bits()),
        Just(f32::INFINITY.to_bits()),
    ];
    prop_oneof![
        2 => (0u64..200_000).prop_map(Raw::Wait),
        2 => (200.0f64..1800.0).prop_map(Raw::Activate),
        6 => (bits.clone(), bits.clone()).prop_map(|(x, y)| Raw::Motion(x, y)),
        2 => bits.prop_map(Raw::Scroll),
        1 => any::<bool>().prop_map(Raw::Button),
        3 => Just(Raw::Ack),
    ]
}

/// Every float in an action the machine emits.
fn action_floats(a: &SourceAction) -> Vec<f64> {
    let pos = |p: &Pos| vec![f64::from(p.x), f64::from(p.y)];
    match a {
        SourceAction::Datagram { motion: m, .. } => {
            [m.x, m.y, m.dx, m.dy].into_iter().map(f64::from).collect()
        }
        SourceAction::Send { msg, .. } => match msg {
            Control::Enter { x, y, .. } => vec![f64::from(*x), f64::from(*y)],
            Control::Button { pos: p, .. } | Control::Held { pos: p, .. } => pos(p),
            Control::Scroll { value, pos: p, .. } => {
                let mut v = pos(p);
                v.push(f64::from(*value));
                v
            }
            _ => vec![],
        },
        SourceAction::Portal(PortalCmd::Release { pos: Some(p), .. }) => vec![p.x, p.y],
        _ => vec![],
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Arbitrary `f32` bit patterns from EIS: nothing non-finite leaves the
    /// machine, and the model stays finite while Remote.
    #[test]
    fn non_finite_eis_values_never_leave_the_source(
        evs in proptest::collection::vec(arb_raw(), 1..120),
    ) {
        let mut r = Rig::armed();
        for e in evs {
            let a = match e {
                Raw::Wait(us) => r.wait_us(us),
                Raw::Activate(x) => {
                    if r.m.status().role == SourceRole::Local {
                        r.activate(x, 1081.0)
                    } else {
                        vec![]
                    }
                }
                Raw::Motion(x, y) => r.motion(f32::from_bits(x), f32::from_bits(y)),
                Raw::Scroll(v) => r.ev(SourceEvent::Scroll(ScrollIn {
                    axis: 0,
                    source: 1,
                    value: f32::from_bits(v),
                    value120: None,
                    stop: false,
                })),
                Raw::Button(pressed) => r.ev(SourceEvent::Button { code: BTN_LEFT, pressed }),
                Raw::Ack => r.ack_outstanding(),
            };
            for act in &a {
                prop_assert!(
                    action_floats(act).iter().all(|v| v.is_finite()),
                    "non-finite value in {:?}", act
                );
            }
            if let Crossing::Remote(rem) = &r.m.crossing {
                prop_assert!(rem.model.x.is_finite() && rem.model.y.is_finite());
            }
        }
    }
}

fn datagrams(a: &[SourceAction]) -> Vec<Motion> {
    a.iter()
        .filter_map(|a| match a {
            SourceAction::Datagram { motion, .. } => Some(*motion),
            _ => None,
        })
        .collect()
}

/// A-04: the next motion takes the newest `HeldAck` to be older than [`MOTION_ACK_WINDOW`].
fn stall(r: &mut Rig) {
    r.ack_outstanding();
    // The snapshot due 100 ms later leaves and is never acknowledged.
    r.wait(crate::time::MOTION_ACK_WINDOW.0 / 1000);
    r.wait_us(1);
}

/// A-04: with no `HeldAck` for two snapshot periods, motion sends no datagram, and
/// every key and `Held` still carries the position, so the target is where the model is.
#[test]
fn no_datagram_follows_a_stale_ack() {
    let mut r = Rig::remote();
    assert_eq!(datagrams(&r.motion(0.0, 100.0)).len(), 1);
    stall(&mut r);
    for _ in 0..50 {
        let a = r.motion(2.0, 1.0);
        assert!(datagrams(&a).is_empty(), "{a:?}");
        r.wait_us(125);
    }
    assert_eq!(r.m.counters().motion_suppressed, 50);
    assert_eq!(
        r.model(),
        Point {
            x: 900.0,
            y: 1230.0
        }
    );
    // A key still leaves, fenced at the model's position.
    let pos = r.m.pos();
    assert_eq!((pos.x, pos.y), (900.0 - 277.0, 1230.0 - 1080.0));
    let a = r.key(30, true);
    assert!(
        matches!(sends(&a)[..], [Control::Key { pressed: true, .. }]),
        "{a:?}"
    );
    // The stall ends: the Held that waited for the ack leaves at once with the position, with
    // a seq newer than the last datagram's, so the target warps there first.
    let a = r.ack_outstanding();
    match sends(&a)[..] {
        [Control::Held { pos: p, .. }] => assert_eq!(*p, pos),
        _ => panic!("{a:?}"),
    }
    // And motion resumes.
    let a = r.motion(1.0, 0.0);
    match datagrams(&a)[..] {
        [m] => {
            assert_eq!((m.x, m.y), (901.0 - 277.0, 150.0));
            assert!(crate::wire::datagram::seq_newer(m.seq, pos.motion_seq));
        }
        _ => panic!("{a:?}"),
    }
}

/// A-04: an 8 kHz mouse sends one datagram a millisecond, and the last of a burst still
/// leaves within that millisecond, at the model's position and with the burst's sum.
#[test]
fn motion_is_one_datagram_a_millisecond_and_the_last_one_is_not_lost() {
    let mut r = Rig::remote();
    r.ack_outstanding();
    let mut sent = Vec::new();
    for _ in 0..80 {
        sent.extend(datagrams(&r.motion(1.0, 1.0)));
        sent.extend(datagrams(&r.wait_us(125)));
    }
    sent.extend(datagrams(&r.wait(1)));
    assert!((10..=11).contains(&sent.len()), "{}", sent.len());
    for w in sent.windows(2) {
        assert!(w[1].t_us.wrapping_sub(w[0].t_us) >= 1000, "{w:?}");
    }
    let summed: f32 = sent.iter().map(|m| m.dx).sum();
    assert_eq!(summed, 80.0);
    let last = sent.last().unwrap();
    assert_eq!((last.x, last.y), (880.0 - 277.0, 80.0));
    // Seven of every eight wait for the next datagram.
    assert!(
        r.m.counters().motion_coalesced >= 70,
        "{:?}",
        r.m.counters()
    );
    // Nothing owed, so no timer left for it.
    let Crossing::Remote(rem) = &r.m.crossing else {
        panic!()
    };
    assert_eq!(rem.pace.deadline(), None);
}

/// A-04: suppression changes what reaches the target, never the model, so the crossing home
/// still works in a stall and after it.
#[test]
fn the_crossing_home_works_in_a_stall_and_after_it() {
    let mut r = Rig::remote();
    r.motion(3.0, 40.0);
    stall(&mut r);
    assert!(datagrams(&r.motion(0.0, 5.0)).is_empty());
    let a = r.motion(0.0, -50.0);
    assert_eq!(
        a,
        vec![
            leave(1, LeaveReason::Home),
            release(r.activation, 803.0, 1075.0)
        ]
    );

    // Cross again; a stall that ends: motion is back, then home.
    r.activate(800.0, 1081.0);
    stall(&mut r);
    assert!(datagrams(&r.motion(0.0, 10.0)).is_empty());
    r.ack_outstanding();
    assert_eq!(datagrams(&r.motion(0.0, 10.0)).len(), 1);
    let a = r.motion(0.0, -30.0);
    assert_eq!(a.first(), Some(&leave(2, LeaveReason::Home)), "{a:?}");
    assert_eq!(r.m.status().role, SourceRole::Local);
}

// ---- key crossings ---------------------------------------------------------------

const SUPER: u16 = 125;
const CTRL: u16 = 29;
const ALT: u16 = 56;
const UP: u16 = 103;
const DOWN: u16 = 108;
const RIGHT: u16 = 106;

fn go(r: &mut Rig, side: Side, x: f64, y: f64) -> Vec<SourceAction> {
    r.ev(SourceEvent::Go {
        side,
        cursor: Some(Point { x, y }),
    })
}

fn refused(a: &[SourceAction]) -> bool {
    a.iter().any(|a| matches!(a, SourceAction::GoRefused(_)))
}

/// Press the go chord's modifiers, then `arrow`: the actions of the arrow.
fn chord(r: &mut Rig, arrow: u16) -> Vec<SourceAction> {
    for m in [SUPER, CTRL, ALT] {
        r.key(m, true);
    }
    r.key(arrow, true)
}

/// From home: a warp next to the middle of the link and a nudge across it; the nudge's
/// activation crosses at once, whatever the resistance and dwell.
#[test]
fn go_from_home_warps_nudges_and_crosses_at_once() {
    let config = SourceConfig {
        edge_resistance_px: 100,
        edge_dwell_ms: 300,
        ..SourceConfig::default()
    };
    let mut r = Rig::with(first_light(), config);
    let a = go(&mut r, Side::Bottom, 100.0, 100.0);
    assert_eq!(
        a,
        [SourceAction::Go {
            x: 960.0,
            y: 1077.0,
            dx: 0.0,
            dy: 4.0
        }]
    );
    let a = r.activate(960.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Remote);
    assert!(matches!(
        sends(&a).first(),
        Some(Control::Enter { x, y, .. }) if *x == 683.0 && *y == 0.0
    ));
    assert_eq!(r.m.counters().go_crossings, 1);
}

/// The go chord toward home ends the crossing where the cursor was before the key crossing;
/// the arrow is never forwarded.
#[test]
fn the_go_chord_goes_home_to_where_the_cursor_was() {
    let mut r = Rig::armed();
    go(&mut r, Side::Bottom, 100.0, 100.0);
    r.activate(960.0, 1081.0);
    let a = chord(&mut r, UP);
    assert!(
        !sends(&a)
            .iter()
            .any(|m| matches!(m, Control::Key { code: UP, .. }))
    );
    assert_eq!(a[0], leave(1, LeaveReason::Home));
    assert_eq!(a[1], release(r.activation, 100.0, 100.0));
    assert_eq!(r.m.status().role, SourceRole::Local);
}

/// After a barrier crossing, the go chord home lands mid-link, inside the home monitor.
#[test]
fn the_go_chord_home_after_a_barrier_crossing_lands_mid_link() {
    let mut r = Rig::remote();
    let a = chord(&mut r, UP);
    assert_eq!(a[1], release(r.activation, 960.0, 1077.0));
}

/// A side with no machine: nothing happens but the refusal, at home and remote.
#[test]
fn go_toward_no_machine_does_nothing() {
    let mut r = Rig::armed();
    let a = go(&mut r, Side::Left, 100.0, 100.0);
    assert!(refused(&a) && a.len() == 1);
    let mut r = Rig::remote();
    let a = chord(&mut r, DOWN);
    assert!(refused(&a));
    assert!(sends(&a).is_empty());
    assert_eq!(r.m.status().role, SourceRole::Remote);
    // Releasing the arrow is an orphan, not a key edge.
    assert!(sends(&r.key(DOWN, false)).is_empty());
}

/// A nudge whose activation never comes lapses; a later contact arms as usual.
#[test]
fn a_nudge_that_never_activates_lapses() {
    let config = SourceConfig {
        edge_resistance_px: 100,
        ..SourceConfig::default()
    };
    let mut r = Rig::with(first_light(), config);
    go(&mut r, Side::Bottom, 100.0, 100.0);
    assert!(refused(&r.wait(600)));
    r.activate(960.0, 1081.0);
    assert_eq!(r.m.status().role, SourceRole::Arming);
}

/// Capture off (locked to this machine): no warp.
#[test]
fn go_while_locked_here_does_nothing() {
    let mut r = Rig::armed();
    r.ev(SourceEvent::LockedHere(true));
    let a = go(&mut r, Side::Bottom, 100.0, 100.0);
    assert!(refused(&a));
    assert!(!a.iter().any(|a| matches!(a, SourceAction::Go { .. })));
}

/// The go chord toward another peer hops there, mid-link.
#[test]
fn the_go_chord_hops_to_a_third_peer() {
    let mut layout = layout_with_netbook(Rel::Below);
    layout.set_origin(Origin {
        peer: third(),
        x: 1643,
        y: 1080,
    });
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 800, 600)],
        },
    ];
    let topo = geom::topology(desk(), &layout, &monitors, &[netbook(), third()]);
    let mut r = Rig::with(topo, SourceConfig::default());
    r.ev(SourceEvent::PeerUp(third()));
    r.activate(800.0, 1081.0);
    let a = chord(&mut r, RIGHT);
    let s: Vec<_> = a
        .iter()
        .filter_map(|a| match a {
            SourceAction::Send { peer, msg } => Some((*peer, msg)),
            _ => None,
        })
        .collect();
    assert_eq!(
        s[0],
        (
            netbook(),
            &Control::Leave {
                session: 1,
                reason: LeaveReason::Hop.code()
            }
        )
    );
    assert!(matches!(
        s[1],
        (p, Control::Enter { session: 2, x, y, .. }) if p == third() && *x == 0.0 && *y == 300.0
    ));
    assert_eq!(r.m.status().peer, Some(third()));
}
