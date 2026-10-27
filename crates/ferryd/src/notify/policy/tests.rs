//! The policy under a synthetic clock: every rate limit of FR-UI-09 and M3 design §3.

use proptest::prelude::*;

use super::*;
use crate::notify::toast::Kind;

fn at(ms: u64) -> Micros {
    Micros::from_millis(ms)
}

fn fp(n: u8) -> Fingerprint {
    Fingerprint([n; 32])
}

fn link(p: &mut Policy, ms: u64, peer: u8, up: bool) -> Vec<Action> {
    p.handle(
        at(ms),
        Event::Link {
            peer: fp(peer),
            name: if peer == 1 {
                "lap".into()
            } else {
                "tower".into()
            },
            up,
        },
    )
}

/// `lap` is peer 1.
fn lap(p: &mut Policy, ms: u64, up: bool) -> Vec<Action> {
    link(p, ms, 1, up)
}

fn shown(actions: &[Action]) -> Vec<&'static str> {
    actions
        .iter()
        .map(|a| match a {
            Action::Show(t) => t.kind.as_str(),
            Action::ClosePairing => "close",
        })
        .collect()
}

fn titles(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Show(t) => Some(t.title.clone()),
            Action::ClosePairing => None,
        })
        .collect()
}

fn code(role: PairingRole) -> Event {
    Event::PairCode {
        role,
        peer: "lap".into(),
        code: Secret::new("418 207"),
        window_ms: 100_000,
    }
}

// --- connection toasts ------------------------------------------------------------------------

#[test]
fn the_first_connect_of_a_peer_is_silent() {
    let mut p = Policy::new(Limits::default());
    assert!(lap(&mut p, 0, true).is_empty());
    assert_eq!(p.next_deadline(), None);
}

#[test]
fn a_peer_that_never_came_up_has_nothing_to_lose() {
    let mut p = Policy::new(Limits::default());
    // down at start-up (the first dial failed), down again, then the first connect
    assert!(lap(&mut p, 0, false).is_empty());
    assert!(lap(&mut p, 1_000, false).is_empty());
    assert!(lap(&mut p, 2_000, true).is_empty());
    assert_eq!(p.next_deadline(), None);
    // from then on it counts
    assert_eq!(titles(&lap(&mut p, 3_000, false)), ["Lost lap"]);
}

#[test]
fn losing_a_connected_peer_is_one_toast_and_getting_it_back_after_the_window_is_one_more() {
    let mut p = Policy::new(Limits::default());
    lap(&mut p, 0, true);
    assert_eq!(titles(&lap(&mut p, 5_000, false)), ["Lost lap"]);
    // past 30 s from the first toast the next one is not held back
    assert_eq!(titles(&lap(&mut p, 36_000, true)), ["lap is back"]);
}

/// FR-UI-09's test: "Flap lap's link 5× in 30 s: one toast".
#[test]
fn flapping_a_link_five_times_in_thirty_seconds_shows_one_toast() {
    let mut p = Policy::new(Limits::default());
    lap(&mut p, 0, true);
    let mut toasts = Vec::new();
    for flap in 0..5 {
        let t = 1_000 + flap * 5_000; // 1 s, 6 s, 11 s, 16 s, 21 s
        toasts.extend(shown(&lap(&mut p, t, false)));
        toasts.extend(shown(&lap(&mut p, t + 2_500, true)));
        toasts.extend(shown(&p.tick(at(t + 2_500))));
    }
    // the last up came at 24.5 s: still inside the window that the first toast opened at 1 s
    assert_eq!(toasts, ["peer_lost"]);
    // The window ends at 31 s. The link is up and the last toast says lost, so the state the
    // window swallowed is announced then, once.
    assert_eq!(p.next_deadline(), Some(at(31_000)));
    assert!(p.tick(at(30_999)).is_empty());
    assert_eq!(titles(&p.tick(at(31_000))), ["lap is back"]);
    assert_eq!(p.next_deadline(), None);
    assert!(p.tick(at(500_000)).is_empty());
}

#[test]
fn a_flap_that_ends_where_the_last_toast_stands_adds_nothing() {
    let mut p = Policy::new(Limits::default());
    lap(&mut p, 0, true);
    assert_eq!(shown(&lap(&mut p, 1_000, false)), ["peer_lost"]);
    assert!(lap(&mut p, 2_000, true).is_empty());
    assert_eq!(p.next_deadline(), Some(at(31_000)));
    assert!(lap(&mut p, 3_000, false).is_empty());
    // down again: the toast that stands ("Lost lap") is right, so nothing is due
    assert_eq!(p.next_deadline(), None);
    assert!(p.tick(at(60_000)).is_empty());
}

#[test]
fn each_peer_has_its_own_window_and_the_swallowed_states_come_in_a_fixed_order() {
    let mut p = Policy::new(Limits::default());
    link(&mut p, 0, 2, true);
    lap(&mut p, 0, true);
    assert_eq!(titles(&link(&mut p, 1_000, 2, false)), ["Lost tower"]);
    assert_eq!(titles(&lap(&mut p, 2_000, false)), ["Lost lap"]);
    assert!(link(&mut p, 3_000, 2, true).is_empty());
    assert!(lap(&mut p, 4_000, true).is_empty());
    // tower's window ends first (31 s), then lap's (32 s)
    assert_eq!(p.next_deadline(), Some(at(31_000)));
    assert_eq!(titles(&p.tick(at(31_000))), ["tower is back"]);
    assert_eq!(p.next_deadline(), Some(at(32_000)));
    assert_eq!(titles(&p.tick(at(40_000))), ["lap is back"]);
    // two due at once come out in fingerprint order, not in hash order
    let mut p = Policy::new(Limits::default());
    for peer in [3, 1, 2] {
        link(&mut p, 0, peer, true);
        link(&mut p, 100, peer, false);
        link(&mut p, 200, peer, true);
    }
    assert_eq!(
        titles(&p.tick(at(100_000))),
        ["lap is back", "tower is back", "tower is back"]
    );
}

#[test]
fn a_peer_that_is_paired_again_connects_silently() {
    let mut p = Policy::new(Limits::default());
    lap(&mut p, 0, true);
    // unpaired here (or paired again over a live connection): the memory goes
    p.handle(at(1_000), Event::Forget { peer: fp(1) });
    assert_eq!(p.next_deadline(), None);
    assert!(
        lap(&mut p, 2_000, false).is_empty(),
        "the Offline that follows an unpair"
    );
    assert!(
        lap(&mut p, 3_000, true).is_empty(),
        "the first connect after the pairing"
    );
    assert_eq!(titles(&lap(&mut p, 4_000, false)), ["Lost lap"]);
}

#[test]
fn a_revoke_toasts_once_and_the_offline_that_follows_it_is_silent() {
    let mut p = Policy::new(Limits::default());
    lap(&mut p, 0, true);
    let out = p.handle(
        at(1_000),
        Event::Unpaired {
            peer: fp(1),
            name: "lap".into(),
        },
    );
    assert_eq!(titles(&out), ["lap unpaired this machine"]);
    assert!(lap(&mut p, 1_001, false).is_empty());
    assert_eq!(p.next_deadline(), None);
}

// --- attention --------------------------------------------------------------------------------

fn attention(p: &mut Policy, ms: u64, code: &str, peer: Option<&str>) -> Vec<Action> {
    p.handle(
        at(ms),
        Event::Attention {
            code: code.into(),
            message: "lap's fingerprint changed".into(),
            hint: Some("ferry unpair lap, then pair again".into()),
            peer: peer.map(Into::into),
        },
    )
}

#[test]
fn an_attention_code_toasts_once_per_five_minutes() {
    let mut p = Policy::new(Limits::default());
    let out = attention(&mut p, 0, "peer_refused", Some("lap"));
    assert_eq!(shown(&out), ["attention"]);
    let Action::Show(t) = &out[0] else { panic!() };
    assert_eq!(t.title, "lap's fingerprint changed");
    assert_eq!(t.body, "ferry unpair lap, then pair again");
    assert_eq!(t.urgency, crate::notify::toast::Urgency::Critical);
    assert!(attention(&mut p, 1_000, "peer_refused", Some("lap")).is_empty());
    assert!(attention(&mut p, 299_999, "peer_refused", Some("lap")).is_empty());
    assert_eq!(
        shown(&attention(&mut p, 300_000, "peer_refused", Some("lap"))),
        ["attention"]
    );
}

#[test]
fn another_code_or_another_peer_is_another_problem() {
    let mut p = Policy::new(Limits::default());
    assert_eq!(
        shown(&attention(&mut p, 0, "peer_refused", Some("lap"))),
        ["attention"]
    );
    assert_eq!(
        shown(&attention(&mut p, 1, "peer_refused", Some("tower"))),
        ["attention"]
    );
    assert_eq!(
        shown(&attention(&mut p, 2, "barriers_failed", None)),
        ["attention"]
    );
    assert!(attention(&mut p, 3, "barriers_failed", None).is_empty());
}

#[test]
fn the_attention_limiter_stays_bounded() {
    let mut p = Policy::new(Limits::default());
    for i in 0..1_000u64 {
        attention(&mut p, i, &format!("code{i}"), None);
        assert!(p.attention.len() <= ATTENTION_KEYS_MAX);
    }
}

// --- pairing ----------------------------------------------------------------------------------

#[test]
fn only_the_acceptor_of_a_pairing_gets_the_code_toast() {
    let mut p = Policy::new(Limits::default());
    assert!(p.handle(at(0), code(PairingRole::Dialer)).is_empty());
    // nothing was opened, so nothing closes
    assert!(p.handle(at(1), Event::PairMoved).is_empty());
    let out = p.handle(at(2), code(PairingRole::Acceptor));
    assert_eq!(titles(&out), ["Pairing code 418 207"]);
}

#[test]
fn the_code_toast_closes_once_when_the_pairing_moves_on() {
    let mut p = Policy::new(Limits::default());
    p.handle(at(0), code(PairingRole::Acceptor));
    assert_eq!(shown(&p.handle(at(1), Event::PairMoved)), ["close"]);
    assert!(p.handle(at(2), Event::PairMoved).is_empty());
}

#[test]
fn a_result_takes_the_place_of_the_code_and_needs_no_close() {
    for result in [
        Event::Paired {
            peer: "lap".into(),
            note: None,
        },
        Event::PairFailed {
            reason: "rejected".into(),
            message: "cancelled: codes differ".into(),
        },
    ] {
        let mut p = Policy::new(Limits::default());
        p.handle(at(0), code(PairingRole::Acceptor));
        let out = p.handle(at(1), result);
        assert!(
            matches!(shown(&out).as_slice(), ["paired"] | ["pair_failed"]),
            "{out:?}"
        );
        assert!(p.handle(at(2), Event::PairMoved).is_empty());
    }
}

#[test]
fn the_dialer_is_told_the_result_though_it_had_no_code_toast() {
    let mut p = Policy::new(Limits::default());
    assert!(p.handle(at(0), code(PairingRole::Dialer)).is_empty());
    let out = p.handle(
        at(1),
        Event::Paired {
            peer: "tower".into(),
            note: None,
        },
    );
    assert_eq!(titles(&out), ["Paired with tower"]);
}

#[test]
fn a_replaced_layout_always_toasts() {
    let mut p = Policy::new(Limits::default());
    for i in 0..3 {
        let out = p.handle(
            at(i),
            Event::LayoutReplaced {
                winner: "lap".into(),
            },
        );
        assert_eq!(titles(&out), ["Layout replaced by lap"]);
    }
}

#[test]
fn the_events_never_show_a_pairing_code_in_their_debug_output() {
    let shown = format!("{:?}", code(PairingRole::Acceptor));
    assert!(!shown.contains("418") && !shown.contains("207"), "{shown}");
}

// --- the connection rule, for any sequence ----------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// For any flapping: a peer's connection toasts are 30 s apart at least, they alternate
    /// from "lost", nothing shows before the peer has been up, and once the windows have run
    /// out the last toast says how the link is.
    #[test]
    fn connection_toasts_keep_the_window_alternate_and_end_on_the_truth(
        steps in proptest::collection::vec((0u64..45_000, any::<bool>()), 1..40),
    ) {
        let mut p = Policy::new(Limits::default());
        let mut now = 0u64;
        let mut toasts: Vec<(u64, Kind)> = Vec::new();
        let collect = |at_ms: u64, actions: Vec<Action>, out: &mut Vec<(u64, Kind)>| {
            for a in actions {
                if let Action::Show(t) = a {
                    out.push((at_ms, t.kind));
                }
            }
        };
        let mut up = false;
        let mut ever_up = false;
        for (delta, now_up) in steps {
            now += delta;
            // the actor runs a due deadline before it takes the next event
            collect(now, p.tick(at(now)), &mut toasts);
            collect(now, lap(&mut p, now, now_up), &mut toasts);
            up = now_up;
            ever_up |= now_up;
        }
        // let every window run out
        while let Some(d) = p.next_deadline() {
            now = now.max(d.0 / 1000);
            collect(now, p.tick(at(now)), &mut toasts);
        }
        for pair in toasts.windows(2) {
            prop_assert!(pair[1].0 - pair[0].0 >= 30_000, "{toasts:?}");
        }
        for (i, (_, kind)) in toasts.iter().enumerate() {
            let want = if i % 2 == 0 { Kind::PeerLost } else { Kind::PeerBack };
            prop_assert_eq!(*kind, want, "{:?}", toasts);
        }
        if ever_up {
            match toasts.last() {
                Some((_, kind)) => prop_assert_eq!(*kind == Kind::PeerBack, up, "{:?}", toasts),
                None => prop_assert!(up, "{toasts:?}"),
            }
        } else {
            prop_assert!(toasts.is_empty());
        }
    }
}
