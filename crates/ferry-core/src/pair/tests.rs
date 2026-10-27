//! The pairing machine and its formulas (design `m1-rest-design.md` §2.14).

use proptest::prelude::*;

use super::*;
use crate::wire::Control;

const T0: Micros = Micros(1_000_000);
const UNTIL: Micros = Micros(121_000_000);

fn fp_a() -> PeerId {
    PeerId([0xd0; 32])
}

fn fp_b() -> PeerId {
    PeerId([0x0b; 32])
}

fn hello(name: &str) -> Control {
    hello_v(name, PROTO_MAJOR)
}

fn hello_v(name: &str, major: u16) -> Control {
    Control::Hello {
        proto_major: major,
        proto_minor: 0,
        name: name.into(),
        features: Vec::new(),
        build: "test".into(),
        monitors: Vec::new(),
    }
}

fn start(role: Role, me: PeerId, peer: PeerId, nonce: Nonce, name: &str) -> PairEvent {
    PairEvent::Start(Start {
        role,
        me,
        peer,
        nonce,
        until: UNTIL,
        hello: hello(name),
    })
}

fn run(m: &mut PairMachine, ev: PairEvent, now: Micros) -> Vec<PairAction> {
    let mut out = Actions::new();
    m.handle(ev, now, &mut out);
    out.drain().collect()
}

fn sends(actions: &[PairAction]) -> Vec<PairFrame> {
    actions
        .iter()
        .filter_map(|a| match a {
            PairAction::Send(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// Every frame survives the wire: encode, then `PairFrame::decode`.
fn wire(f: &PairFrame) -> PairFrame {
    let mut b = Vec::new();
    f.encode(&mut b);
    PairFrame::decode(&b).expect("a frame the machine sent decodes")
}

/// Two machines joined by an in-memory link. `pump` delivers every frame in
/// both directions until nothing moves, and collects the other actions.
struct Link {
    a: PairMachine,
    b: PairMachine,
    to_a: Vec<PairFrame>,
    to_b: Vec<PairFrame>,
    a_out: Vec<PairAction>,
    b_out: Vec<PairAction>,
}

impl Link {
    fn new(n_a: Nonce, n_b: Nonce) -> Self {
        Self::with(PairMachine::new(), PairMachine::new(), n_a, n_b)
    }

    fn with(a: PairMachine, b: PairMachine, n_a: Nonce, n_b: Nonce) -> Self {
        Self::named(a, b, n_a, n_b, ("a", "b"))
    }

    /// A sends `names.0` in its `Hello`, B `names.1`.
    fn named(
        mut a: PairMachine,
        mut b: PairMachine,
        n_a: Nonce,
        n_b: Nonce,
        names: (&str, &str),
    ) -> Self {
        let b_out = run(
            &mut b,
            start(Role::Acceptor, fp_b(), fp_a(), n_b, names.1),
            T0,
        );
        let a_out = run(
            &mut a,
            start(Role::Dialer, fp_a(), fp_b(), n_a, names.0),
            T0,
        );
        let mut l = Self {
            a,
            b,
            to_a: sends(&b_out),
            to_b: sends(&a_out),
            a_out,
            b_out,
        };
        l.pump();
        l
    }

    fn pump(&mut self) {
        while !self.to_a.is_empty() || !self.to_b.is_empty() {
            for f in std::mem::take(&mut self.to_b) {
                let out = run(&mut self.b, PairEvent::Frame(wire(&f)), T0);
                self.to_a.extend(sends(&out));
                self.b_out.extend(out);
            }
            for f in std::mem::take(&mut self.to_a) {
                let out = run(&mut self.a, PairEvent::Frame(wire(&f)), T0);
                self.to_b.extend(sends(&out));
                self.a_out.extend(out);
            }
        }
    }

    fn answer_a(&mut self, ok: bool) {
        let out = run(&mut self.a, PairEvent::Answer(ok), T0);
        self.to_b.extend(sends(&out));
        self.a_out.extend(out);
        self.pump();
    }

    fn answer_b(&mut self, ok: bool) {
        let out = run(&mut self.b, PairEvent::Answer(ok), T0);
        self.to_a.extend(sends(&out));
        self.b_out.extend(out);
        self.pump();
    }
}

fn shown(actions: &[PairAction]) -> Option<PairCode> {
    actions.iter().find_map(|a| match a {
        PairAction::ShowCode { code, .. } => Some(*code),
        _ => None,
    })
}

fn agreed(actions: &[PairAction]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, PairAction::Agreed { .. }))
}

fn failed(actions: &[PairAction]) -> Option<PairFailure> {
    actions.iter().find_map(|a| match a {
        PairAction::Failed(f) => Some(f.clone()),
        _ => None,
    })
}

fn nonce(n: u8) -> Nonce {
    [n; NONCE_LEN]
}

// --- formulas -------------------------------------------------------------------------------

/// Freezes §2.5 and the commitment of §2.4 step 3: fixed fingerprints and
/// nonces give fixed values.
#[test]
fn code_vectors() {
    let mut s = String::new();
    let cases = [
        (fp_a(), fp_b(), nonce(1), nonce(2)),
        (fp_b(), fp_a(), nonce(1), nonce(2)),
        (fp_a(), fp_b(), nonce(2), nonce(1)),
        (PeerId([0; 32]), PeerId([0xff; 32]), nonce(0), nonce(0)),
        (
            PeerId::of_spki(b"a"),
            PeerId::of_spki(b"b"),
            nonce(0x5a),
            nonce(0xa5),
        ),
    ];
    for (a, b, na, nb) in cases {
        let c = commitment(&a, &b, &na);
        let hex: String = c.iter().map(|b| format!("{b:02x}")).collect();
        s.push_str(&format!(
            "A {} B {} n_A {:02x} n_B {:02x}\n  code {}\n  commit {hex}\n",
            a.short(),
            b.short(),
            na[0],
            nb[0],
            code(&a, &b, &na, &nb),
        ));
    }
    insta::assert_snapshot!(s);
}

#[test]
fn the_code_shows_as_two_zero_padded_groups_and_debug_hides_it() {
    assert_eq!(PairCode(418_207).to_string(), "418 207");
    assert_eq!(PairCode(7).to_string(), "000 007");
    assert_eq!(PairCode(7).digits(), "000007");
    assert_eq!(PairCode(999_999).to_string(), "999 999");
    assert_eq!(format!("{:?}", PairCode(418_207)), "PairCode(..)");
}

#[test]
fn commitment_binds_keys_and_nonce() {
    let base = commitment(&fp_a(), &fp_b(), &nonce(1));
    assert_ne!(base, commitment(&fp_b(), &fp_a(), &nonce(1)), "role order");
    assert_ne!(base, commitment(&PeerId([0xd1; 32]), &fp_b(), &nonce(1)));
    assert_ne!(base, commitment(&fp_a(), &PeerId([0x0c; 32]), &nonce(1)));
    let mut n = nonce(1);
    n[31] ^= 1;
    assert_ne!(base, commitment(&fp_a(), &fp_b(), &n));
    // and it is not the bare H(n_A) of the original ADR 0006
    let bare: [u8; 32] = Sha256::digest(nonce(1)).into();
    assert_ne!(base, bare);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any keys and nonces: the dialer's and the acceptor's code agree, and
    /// it is below 10^6.
    #[test]
    fn both_roles_compute_one_code(
        a in any::<[u8; 32]>(), b in any::<[u8; 32]>(),
        na in any::<[u8; 32]>(), nb in any::<[u8; 32]>(),
    ) {
        let (a, b) = (PeerId(a), PeerId(b));
        prop_assert_eq!(code(&a, &b, &na, &nb), code(&b, &a, &na, &nb));
        prop_assert!(code(&a, &b, &na, &nb).0 < CODE_MODULUS);
    }
}

// --- frames ---------------------------------------------------------------------------------

#[test]
fn pair_frames_decode_by_variant() {
    let enc = |f: &PairFrame| {
        let mut b = Vec::new();
        f.encode(&mut b);
        b
    };
    for f in [
        PairFrame::Control(hello("x")),
        PairFrame::Control(Control::Error {
            code: 1,
            message: "m".into(),
        }),
        PairFrame::Pair(Pair::Commit {
            commitment: [1; 32],
        }),
        PairFrame::Pair(Pair::Nonce { nonce: [2; 32] }),
        PairFrame::Pair(Pair::Reveal { nonce: [3; 32] }),
        PairFrame::Pair(Pair::Confirm {
            ok: false,
            reason: Some(PairReason::Rejected.code()),
        }),
        PairFrame::Pair(Pair::Confirm {
            ok: true,
            reason: None,
        }),
    ] {
        assert_eq!(PairFrame::decode(&enc(&f)).unwrap(), f);
    }
    // Any other ferry/1 message is not valid on ferry-pair/1.
    let mut b = Vec::new();
    Control::HeldAck { session: 1, n: 1 }.encode(&mut b);
    assert!(matches!(
        PairFrame::decode(&b),
        Err(PairDecodeError::Variant(12))
    ));
    // Unknown variants, too: [99, [1]]
    assert!(matches!(
        PairFrame::decode(&[0x82, 0x18, 0x63, 0x81, 0x01]),
        Err(PairDecodeError::Variant(99))
    ));
    assert!(PairFrame::decode(&[0xff]).is_err());
}

fn arb_pair_frame() -> impl Strategy<Value = PairFrame> {
    let text = proptest::collection::vec(any::<char>(), 0..=16)
        .prop_map(|v| v.into_iter().collect::<String>());
    prop_oneof![
        (any::<u16>(), any::<u16>(), text.clone(), text.clone()).prop_map(
            |(proto_major, proto_minor, name, build)| {
                PairFrame::Control(Control::Hello {
                    proto_major,
                    proto_minor,
                    name,
                    features: Vec::new(),
                    build,
                    monitors: Vec::new(),
                })
            }
        ),
        (any::<u16>(), text)
            .prop_map(|(code, message)| PairFrame::Control(Control::Error { code, message })),
        any::<[u8; 32]>().prop_map(|commitment| PairFrame::Pair(Pair::Commit { commitment })),
        any::<[u8; 32]>().prop_map(|nonce| PairFrame::Pair(Pair::Nonce { nonce })),
        any::<[u8; 32]>().prop_map(|nonce| PairFrame::Pair(Pair::Reveal { nonce })),
        (any::<bool>(), proptest::option::of(any::<u8>()))
            .prop_map(|(ok, reason)| PairFrame::Pair(Pair::Confirm { ok, reason })),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// What `encode` writes, `decode` reads back, for every frame valid on `ferry-pair/1`.
    #[test]
    fn a_pair_frame_decodes_to_what_was_encoded(f in arb_pair_frame()) {
        let mut b = Vec::new();
        f.encode(&mut b);
        prop_assert_eq!(PairFrame::decode(&b).ok(), Some(f));
    }

    /// `PairFrame::decode` is what a host with no pin reaches: raw bytes, flipped bytes and
    /// every strict prefix of a valid frame end in an `Err` or a frame, never a panic.
    #[test]
    fn pair_frame_decode_never_panics(
        f in arb_pair_frame(),
        flips in proptest::collection::vec((any::<usize>(), 1u8..=255), 0..8),
        cut in any::<usize>(),
        raw in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut b = Vec::new();
        f.encode(&mut b);
        // A strict prefix of a definite-length item never decodes.
        prop_assert!(PairFrame::decode(&b[..cut % b.len()]).is_err());
        for (i, x) in flips {
            let n = b.len();
            b[i % n] ^= x;
        }
        let _ = PairFrame::decode(&b);
        let _ = PairFrame::decode(&raw);
    }
}

#[test]
fn a_confirm_without_field_1_decodes_as_none() {
    // [33, [true]] as an older peer would send it
    let old = [0x82, 0x18, 0x21, 0x81, 0xf5];
    assert_eq!(
        PairFrame::decode(&old).unwrap(),
        PairFrame::Pair(Pair::Confirm {
            ok: true,
            reason: None
        })
    );
}

#[test]
fn debug_never_prints_a_nonce_or_the_commitment() {
    let f = PairFrame::Pair(Pair::Reveal { nonce: [0xab; 32] });
    let text = format!("{f:?}");
    assert!(!text.contains("171") && !text.contains("ab"), "{text}");
    let f = PairFrame::Pair(Pair::Commit {
        commitment: [0xcd; 32],
    });
    assert!(!format!("{f:?}").contains("205"));
    let mut l = Link::new(nonce(0xab), nonce(0xcd));
    l.answer_a(true);
    for m in [format!("{:?}", l.a), format!("{:?}", l.b)] {
        assert!(!m.contains("171") && !m.contains("205"), "{m}");
    }
    let ev = start(Role::Dialer, fp_a(), fp_b(), nonce(0xab), "a");
    assert!(!format!("{ev:?}").contains("171"));
}

// --- the machine ----------------------------------------------------------------------------

/// §2.4's step table for both machines over an in-memory link.
#[test]
fn happy_path() {
    let mut l = Link::new(nonce(1), nonce(2));
    // Steps 1–5 ran: both show one code, and nobody has agreed.
    let (ca, cb) = (shown(&l.a_out).unwrap(), shown(&l.b_out).unwrap());
    assert_eq!(ca, cb);
    assert_eq!(ca, code(&fp_a(), &fp_b(), &nonce(1), &nonce(2)));
    assert_eq!(l.a.stage(), PairStage::Comparing);
    assert_eq!(l.b.stage(), PairStage::Comparing);
    assert!(
        l.a_out
            .iter()
            .any(|a| matches!(a, PairAction::ShowCode { peer_name, .. } if peer_name == "b"))
    );
    assert!(
        l.b_out
            .iter()
            .any(|a| matches!(a, PairAction::ShowCode { peer_name, .. } if peer_name == "a"))
    );
    // The frames in order: A Hello, B Hello, Commit, Nonce, Reveal.
    // Step 7: A answers first and waits; B answers and both agree.
    l.answer_a(true);
    assert_eq!(l.a.stage(), PairStage::WaitingPeer);
    assert!(!agreed(&l.a_out) && !agreed(&l.b_out));
    l.answer_b(true);
    assert!(agreed(&l.a_out) && agreed(&l.b_out));
    assert_eq!(l.a.stage(), PairStage::Agreed);
    assert_eq!(l.b.stage(), PairStage::Agreed);
    assert!(l.a_out.contains(&PairAction::Agreed {
        peer: fp_b(),
        name: "b".into()
    }));
    assert!(l.b_out.contains(&PairAction::Agreed {
        peer: fp_a(),
        name: "a".into()
    }));
    assert!(failed(&l.a_out).is_none() && failed(&l.b_out).is_none());
    // nothing is pending once both agreed
    assert_eq!(l.a.next_deadline(), None);
    assert_eq!(l.b.next_deadline(), None);
}

#[test]
fn the_frames_go_in_the_order_of_the_step_table() {
    let mut a = PairMachine::new();
    let mut b = PairMachine::new();
    let mut log = Vec::new();
    let mut to_b = sends(&run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    ));
    assert!(
        sends(&run(
            &mut b,
            start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
            T0
        ))
        .is_empty()
    );
    while !to_b.is_empty() {
        let mut to_a = Vec::new();
        for f in to_b.drain(..) {
            log.push(format!("A→B {}", f.variant()));
            to_a.extend(sends(&run(&mut b, PairEvent::Frame(f), T0)));
        }
        for f in to_a {
            log.push(format!("B→A {}", f.variant()));
            to_b.extend(sends(&run(&mut a, PairEvent::Frame(f), T0)));
        }
    }
    assert_eq!(
        log,
        ["A→B 0", "B→A 0", "A→B 30", "B→A 31", "A→B 32"],
        "Hello, Hello, Commit, Nonce, Reveal"
    );
}

#[test]
fn bad_reveal_fails() {
    let mut a = PairMachine::new();
    let mut b = PairMachine::new();
    let out_a = run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    );
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    // Hello, Hello, Commit
    let mut to_b = sends(&out_a);
    let out = run(&mut b, PairEvent::Frame(to_b.remove(0)), T0);
    let out = run(&mut a, PairEvent::Frame(sends(&out).remove(0)), T0);
    let commit = sends(&out).remove(0);
    let out = run(&mut b, PairEvent::Frame(commit), T0);
    assert!(matches!(
        sends(&out)[0],
        PairFrame::Pair(Pair::Nonce { .. })
    ));
    // A reveals another nonce than the one it committed to
    let out = run(
        &mut b,
        PairEvent::Frame(PairFrame::Pair(Pair::Reveal { nonce: nonce(9) })),
        T0,
    );
    assert_eq!(
        failed(&out),
        Some(PairFailure::Reason(PairReason::BadReveal))
    );
    assert!(shown(&out).is_none() && !agreed(&out));
    assert!(
        out.contains(&PairAction::Send(PairFrame::Pair(Pair::Confirm {
            ok: false,
            reason: Some(PairReason::BadReveal.code())
        })))
    );
    assert!(out.contains(&PairAction::Close {
        code: close::PROTOCOL
    }));
    // and later answers change nothing
    assert!(run(&mut b, PairEvent::Answer(true), T0).is_empty());
    assert_eq!(b.stage(), PairStage::Failed);
}

/// Fails with `protocol`, tells the peer, closes with code 2, and never agrees.
fn assert_protocol(out: &[PairAction]) {
    assert_eq!(
        failed(out),
        Some(PairFailure::Reason(PairReason::Protocol)),
        "{out:?}"
    );
    assert!(out.contains(&PairAction::Close {
        code: close::PROTOCOL
    }));
    assert!(!agreed(out));
}

#[test]
fn order_rules() {
    let reveal = PairFrame::Pair(Pair::Reveal { nonce: nonce(1) });
    let commit = PairFrame::Pair(Pair::Commit {
        commitment: commitment(&fp_a(), &fp_b(), &nonce(1)),
    });
    let yes = PairFrame::Pair(Pair::Confirm {
        ok: true,
        reason: None,
    });

    // B: a frame before A's Hello
    let mut b = PairMachine::new();
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    assert_protocol(&run(&mut b, PairEvent::Frame(commit.clone()), T0));

    // B: an early Reveal (before Commit)
    let mut b = PairMachine::new();
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    run(&mut b, PairEvent::Frame(PairFrame::Control(hello("a"))), T0);
    assert_protocol(&run(&mut b, PairEvent::Frame(reveal.clone()), T0));

    // B: a second Commit, and B never sends a second Nonce
    let mut b = PairMachine::new();
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    run(&mut b, PairEvent::Frame(PairFrame::Control(hello("a"))), T0);
    let out = run(&mut b, PairEvent::Frame(commit.clone()), T0);
    assert_eq!(sends(&out).len(), 1);
    let out = run(&mut b, PairEvent::Frame(commit.clone()), T0);
    assert_protocol(&out);
    assert!(
        !sends(&out)
            .iter()
            .any(|f| matches!(f, PairFrame::Pair(Pair::Nonce { .. })))
    );

    // B: an early Confirm (before B computed its code)
    let mut b = PairMachine::new();
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    run(&mut b, PairEvent::Frame(PairFrame::Control(hello("a"))), T0);
    run(&mut b, PairEvent::Frame(commit.clone()), T0);
    assert_protocol(&run(&mut b, PairEvent::Frame(yes.clone()), T0));

    // A: an early Confirm (before its code), a Commit, a Reveal: all wrong
    for wrong in [yes.clone(), commit.clone(), reveal.clone()] {
        let mut a = PairMachine::new();
        run(
            &mut a,
            start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
            T0,
        );
        run(&mut a, PairEvent::Frame(PairFrame::Control(hello("b"))), T0);
        assert_protocol(&run(&mut a, PairEvent::Frame(wrong), T0));
    }

    // A sends Reveal only after Nonce: nothing but Hello and Commit before it
    let mut a = PairMachine::new();
    let mut all = sends(&run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    ));
    all.extend(sends(&run(
        &mut a,
        PairEvent::Frame(PairFrame::Control(hello("b"))),
        T0,
    )));
    assert!(
        !all.iter()
            .any(|f| matches!(f, PairFrame::Pair(Pair::Reveal { .. })))
    );

    // A second Hello is a protocol fault too
    let mut a = PairMachine::new();
    run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    );
    run(&mut a, PairEvent::Frame(PairFrame::Control(hello("b"))), T0);
    assert_protocol(&run(
        &mut a,
        PairEvent::Frame(PairFrame::Control(hello("b"))),
        T0,
    ));

    // A duplicate Confirm{true} after the code
    let mut l = Link::new(nonce(1), nonce(2));
    let out = run(&mut l.a, PairEvent::Frame(yes.clone()), T0);
    assert!(out.is_empty(), "the first one is the answer: {out:?}");
    assert_protocol(&run(&mut l.a, PairEvent::Frame(yes), T0));
}

#[test]
fn a_no_on_either_side_cancels_both_with_codes_differ() {
    for a_says_no in [true, false] {
        let mut l = Link::new(nonce(1), nonce(2));
        if a_says_no {
            l.answer_b(true);
            l.answer_a(false);
        } else {
            l.answer_a(true);
            l.answer_b(false);
        }
        for out in [&l.a_out, &l.b_out] {
            assert_eq!(failed(out), Some(PairFailure::Reason(PairReason::Rejected)));
            assert!(!agreed(out));
        }
        assert_eq!(
            failed(&l.a_out).unwrap().message(),
            "cancelled: codes differ"
        );
    }
}

#[test]
fn cancel_timeout_and_close_pin_nothing() {
    // cancel on either side at each stage
    for stage in 0..3 {
        for cancel_a in [true, false] {
            let mut l = Link::new(nonce(1), nonce(2));
            if stage >= 1 {
                l.answer_a(true);
            }
            if stage >= 2 {
                l.answer_b(true);
            }
            let m = if cancel_a { &mut l.a } else { &mut l.b };
            let out = run(m, PairEvent::Cancel, T0);
            if stage == 2 {
                // both had agreed: the yes stands, the cancel comes too late
                assert!(out.is_empty());
                continue;
            }
            let peer_frames = sends(&out);
            assert_eq!(
                failed(&out),
                Some(PairFailure::Reason(PairReason::Cancelled))
            );
            let other = if cancel_a { &mut l.b } else { &mut l.a };
            let mut seen = Vec::new();
            for f in peer_frames {
                seen.extend(run(other, PairEvent::Frame(f), T0));
            }
            assert_eq!(
                failed(&seen),
                Some(PairFailure::Reason(PairReason::Cancelled))
            );
            assert!(!agreed(&seen));
        }
    }

    // a machine step that takes over 5 s
    let mut a = PairMachine::new();
    run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    );
    assert_eq!(a.next_deadline(), Some(T0.plus(STEP_TIMEOUT)));
    assert!(run(&mut a, PairEvent::Tick, T0.plus(Micros::from_millis(4999))).is_empty());
    let out = run(&mut a, PairEvent::Tick, T0.plus(STEP_TIMEOUT));
    assert_eq!(failed(&out), Some(PairFailure::Reason(PairReason::Timeout)));
    assert_eq!(failed(&out).unwrap().message(), "timed out");

    // the window ends while the person looks
    let mut l = Link::new(nonce(1), nonce(2));
    assert_eq!(l.b.next_deadline(), Some(UNTIL));
    l.answer_a(true);
    let out = run(&mut l.b, PairEvent::Tick, UNTIL);
    assert_eq!(failed(&out), Some(PairFailure::Reason(PairReason::Timeout)));
    let mut seen = Vec::new();
    for f in sends(&out) {
        seen.extend(run(&mut l.a, PairEvent::Frame(f), UNTIL));
    }
    assert_eq!(
        failed(&seen),
        Some(PairFailure::Reason(PairReason::Timeout))
    );

    // the link drops before the end
    let mut l = Link::new(nonce(1), nonce(2));
    l.answer_a(true);
    let out = run(&mut l.a, PairEvent::Closed, T0);
    assert_eq!(failed(&out), Some(PairFailure::Lost));
    assert!(!agreed(&out));
    assert!(
        run(
            &mut l.a,
            PairEvent::Frame(PairFrame::Pair(Pair::Confirm {
                ok: true,
                reason: None
            })),
            T0
        )
        .is_empty()
    );
}

#[test]
fn an_answer_after_the_deadline_times_out() {
    // A-07: the window ended at `UNTIL`; the answer reaches the machine before its `Tick` does.
    let late = UNTIL.plus(Micros(1));

    let mut l = Link::new(nonce(1), nonce(2));
    l.answer_a(true);
    let out = run(&mut l.b, PairEvent::Answer(true), late);
    assert!(!agreed(&out));
    assert_eq!(failed(&out), Some(PairFailure::Reason(PairReason::Timeout)));
    assert_eq!(l.b.stage(), PairStage::Failed);

    // the peer's yes arrives late, after this side said yes in time
    let mut l = Link::new(nonce(1), nonce(2));
    l.answer_b(true);
    let out = run(
        &mut l.b,
        PairEvent::Frame(PairFrame::Pair(Pair::Confirm {
            ok: true,
            reason: None,
        })),
        late,
    );
    assert!(!agreed(&out));
    assert_eq!(failed(&out), Some(PairFailure::Reason(PairReason::Timeout)));

    // a frame past a 5 s step is refused as well
    let mut a = PairMachine::new();
    run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    );
    let out = run(
        &mut a,
        PairEvent::Frame(PairFrame::Control(hello("b"))),
        T0.plus(STEP_TIMEOUT),
    );
    assert_eq!(failed(&out), Some(PairFailure::Reason(PairReason::Timeout)));
    assert!(sends(&out).iter().all(|f| matches!(f, PairFrame::Pair(_))));
}

#[test]
fn a_no_after_the_agreement_still_fails() {
    // B agreed on A's yes, then a `false` comes after it (A cancelled at once): B must not pin.
    let mut l = Link::new(nonce(1), nonce(2));
    l.answer_a(true);
    l.answer_b(true);
    let out = run(
        &mut l.b,
        PairEvent::Frame(PairFrame::Pair(Pair::Confirm {
            ok: false,
            reason: Some(PairReason::Cancelled.code()),
        })),
        T0,
    );
    assert_eq!(
        failed(&out),
        Some(PairFailure::Reason(PairReason::Cancelled))
    );
    assert_eq!(l.b.stage(), PairStage::Failed);
    // A closing after the agreement is the normal end: no failure
    let mut l = Link::new(nonce(1), nonce(2));
    l.answer_a(true);
    l.answer_b(true);
    assert!(run(&mut l.a, PairEvent::Closed, T0).is_empty());
    assert_eq!(l.a.stage(), PairStage::Agreed);
}

#[test]
fn a_major_mismatch_ends_both_with_both_versions() {
    let mut b = PairMachine::new();
    run(
        &mut b,
        start(Role::Acceptor, fp_b(), fp_a(), nonce(2), "b"),
        T0,
    );
    let out = run(
        &mut b,
        PairEvent::Frame(PairFrame::Control(hello_v("a", 2))),
        T0,
    );
    let sent = sends(&out);
    // its own Hello first, then the Error, then the close with code 1
    assert!(matches!(sent[0], PairFrame::Control(Control::Hello { .. })));
    assert!(matches!(
        &sent[1],
        PairFrame::Control(Control::Error { code: 1, message }) if message == "version mismatch: 1.0 vs 2.0"
    ));
    assert_eq!(
        failed(&out).unwrap().message(),
        "version mismatch: 1.0 vs 2.0"
    );
    assert!(out.contains(&PairAction::Close {
        code: close::VERSION
    }));

    // the dialer sees the other side's Hello and fails the same way
    let mut a = PairMachine::new();
    run(
        &mut a,
        start(Role::Dialer, fp_a(), fp_b(), nonce(1), "a"),
        T0,
    );
    let out = run(
        &mut a,
        PairEvent::Frame(PairFrame::Control(hello_v("b", 2))),
        T0,
    );
    assert_eq!(
        failed(&out).unwrap().message(),
        "version mismatch: 1.0 vs 2.0"
    );
    assert!(!sends(&out).iter().any(|f| matches!(f, PairFrame::Pair(_))));
}

// --- properties -----------------------------------------------------------------------------

/// What an arbitrary step feeds the machine under test.
#[derive(Debug, Clone)]
enum Step {
    /// The frame an honest peer would send next.
    Honest,
    /// A specific frame.
    Frame(u8, bool, Option<u8>),
    Answer(bool),
    Cancel,
    Closed,
    Tick(u64),
}

fn arb_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => Just(Step::Honest),
        2 => (0u8..6, any::<bool>(), proptest::option::of(0u8..8)).prop_map(|(k, ok, r)| Step::Frame(k, ok, r)),
        3 => any::<bool>().prop_map(Step::Answer),
        1 => Just(Step::Cancel),
        1 => Just(Step::Closed),
        1 => (0u64..8_000_000).prop_map(Step::Tick),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Any interleaving of frames, answers, cancels, ticks and closes, on
    /// either role: `Agreed` comes at most once, only after the code was shown,
    /// a local yes and a remote `Confirm{true}`, and never after a failure,
    /// a local no, or a remote no.
    #[test]
    fn never_agrees_without_both_yes(
        acceptor in any::<bool>(),
        steps in proptest::collection::vec(arb_step(), 1..40),
    ) {
        let (role, me, peer) = if acceptor {
            (Role::Acceptor, fp_b(), fp_a())
        } else {
            (Role::Dialer, fp_a(), fp_b())
        };
        // the honest peer's script, in order, for this role
        let n_peer = nonce(7);
        let honest: Vec<PairFrame> = if acceptor {
            vec![
                PairFrame::Control(hello("a")),
                PairFrame::Pair(Pair::Commit { commitment: commitment(&fp_a(), &fp_b(), &n_peer) }),
                PairFrame::Pair(Pair::Reveal { nonce: n_peer }),
                PairFrame::Pair(Pair::Confirm { ok: true, reason: None }),
            ]
        } else {
            vec![
                PairFrame::Control(hello("b")),
                PairFrame::Pair(Pair::Nonce { nonce: n_peer }),
                PairFrame::Pair(Pair::Confirm { ok: true, reason: None }),
            ]
        };
        let mut next_honest = 0usize;
        let mut m = PairMachine::new();
        let mut now = T0;
        let mut out = Actions::new();
        m.handle(start(role, me, peer, nonce(3), "me"), now, &mut out);
        out.drain().for_each(drop);

        let (mut shown, mut local_yes, mut local_no, mut remote_yes, mut remote_no, mut failed) =
            (false, false, false, false, false, false);
        let mut agreements = 0;
        for step in steps {
            let ev = match step {
                Step::Honest => {
                    let Some(f) = honest.get(next_honest) else { continue };
                    next_honest += 1;
                    PairEvent::Frame(f.clone())
                }
                Step::Frame(k, ok, r) => PairEvent::Frame(match k {
                    0 => PairFrame::Control(hello("x")),
                    1 => PairFrame::Pair(Pair::Commit { commitment: [k; 32] }),
                    2 => PairFrame::Pair(Pair::Nonce { nonce: [k; 32] }),
                    3 => PairFrame::Pair(Pair::Reveal { nonce: [k; 32] }),
                    4 => PairFrame::Control(Control::Error { code: 4, message: String::new() }),
                    _ => PairFrame::Pair(Pair::Confirm { ok, reason: r }),
                }),
                Step::Answer(ok) => PairEvent::Answer(ok),
                Step::Cancel => PairEvent::Cancel,
                Step::Closed => PairEvent::Closed,
                Step::Tick(dt) => {
                    now = now.plus(Micros(dt));
                    PairEvent::Tick
                }
            };
            // what this event means, judged before the machine sees it
            let stage_before = m.stage();
            match &ev {
                PairEvent::Answer(true) if stage_before == PairStage::Comparing => local_yes = true,
                PairEvent::Answer(false) if stage_before == PairStage::Comparing => local_no = true,
                PairEvent::Frame(PairFrame::Pair(Pair::Confirm { ok: true, .. })) if shown => remote_yes = true,
                PairEvent::Frame(PairFrame::Pair(Pair::Confirm { ok: false, .. })) => remote_no = true,
                _ => {}
            }
            m.handle(ev, now, &mut out);
            for a in out.drain() {
                match a {
                    PairAction::ShowCode { .. } => {
                        prop_assert!(!shown, "the code is shown once");
                        shown = true;
                    }
                    PairAction::Agreed { peer: p, .. } => {
                        agreements += 1;
                        prop_assert_eq!(p, peer);
                        prop_assert!(shown && local_yes && remote_yes);
                        prop_assert!(!failed && !local_no && !remote_no);
                    }
                    PairAction::Failed(_) => failed = true,
                    _ => {}
                }
            }
            prop_assert!(agreements <= 1);
            if failed {
                prop_assert_eq!(m.stage(), PairStage::Failed);
            }
        }
    }
}

// --- the man in the middle, with a toy modulus ----------------------------------------------

/// splitmix64: a seeded generator, so the run is the same every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn nonce(&mut self) -> Nonce {
        let mut n = [0u8; 32];
        for chunk in n.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        n
    }
}

/// Feeds `f` to `m` and returns what it sends back.
fn step(m: &mut PairMachine, f: PairFrame) -> Vec<PairFrame> {
    sends(&run(m, PairEvent::Frame(f), T0))
}

/// M sits between A and B with its own keys M1 (posing as B to A) and M2
/// (posing as A to B). With the code reduced mod 100, M plays every order
/// the protocol allows and picks each of its values adaptively, as well as
/// it can from what it has seen. The two codes match in 1 % of 20 000 runs,
/// ± 3σ: the commitment leaves M one guess. Without the commitment (A's nonce
/// in the clear before M chooses), the same M matches almost always.
#[test]
fn mitm_toy_modulus() {
    const RUNS: u32 = 20_000;
    const MODULUS: u32 = 100;
    let (m1, m2) = (PeerId([0x31; 32]), PeerId([0x32; 32]));
    let mut rng = Rng(0x5eed_f00d);
    let mut matches = 0u32;
    for run_no in 0..RUNS {
        let mut a = PairMachine::with_modulus(MODULUS);
        let mut b = PairMachine::with_modulus(MODULUS);
        let (n_a, n_b) = (rng.nonce(), rng.nonce());
        // A dials M (thinking it is B); M dials B (posing as A).
        let a_hello = sends(&run(&mut a, start(Role::Dialer, fp_a(), m1, n_a, "a"), T0));
        run(&mut b, start(Role::Acceptor, fp_b(), m2, n_b, "b"), T0);
        assert_eq!(a_hello.len(), 1);
        // M answers A's Hello and gets A's commitment.
        let commit_a = step(&mut a, PairFrame::Control(hello("b")));
        let [PairFrame::Pair(Pair::Commit { commitment: c_a })] = commit_a.as_slice() else {
            panic!("{commit_a:?}")
        };
        // M greets B.
        step(&mut b, PairFrame::Control(hello("a")));

        let (code_a, code_b) = if run_no % 2 == 0 {
            // Order 1: finish A–M first, so M knows code_A, then try to steer code_B.
            let n_b_prime = rng.nonce();
            let reveal = step(&mut a, PairFrame::Pair(Pair::Nonce { nonce: n_b_prime }));
            let [PairFrame::Pair(Pair::Reveal { nonce: n_a_seen })] = reveal.as_slice() else {
                panic!()
            };
            let code_a = code_mod(&fp_a(), &m1, n_a_seen, &n_b_prime, MODULUS);
            // M must commit to n_A' before B's nonce exists. Its best try: an n_A' that
            // would give code_A with a guessed n_B (B's nonce is unknown, so any guess).
            let guess_nb = rng.nonce();
            let mut n_a_prime = rng.nonce();
            for _ in 0..64 {
                if code_mod(&m2, &fp_b(), &n_a_prime, &guess_nb, MODULUS) == code_a {
                    break;
                }
                n_a_prime = rng.nonce();
            }
            let nonce_b = step(
                &mut b,
                PairFrame::Pair(Pair::Commit {
                    commitment: commitment(&m2, &fp_b(), &n_a_prime),
                }),
            );
            assert!(matches!(
                nonce_b.as_slice(),
                [PairFrame::Pair(Pair::Nonce { .. })]
            ));
            step(&mut b, PairFrame::Pair(Pair::Reveal { nonce: n_a_prime }));
            (code_a, b.code.map(|c| c.0).expect("B shows a code"))
        } else {
            // Order 2: run M–B up to B's nonce, so M knows code_B, then choose n_B' for A.
            let n_a_prime = rng.nonce();
            let nonce_b = step(
                &mut b,
                PairFrame::Pair(Pair::Commit {
                    commitment: commitment(&m2, &fp_b(), &n_a_prime),
                }),
            );
            let [PairFrame::Pair(Pair::Nonce { nonce: n_b_seen })] = nonce_b.as_slice() else {
                panic!()
            };
            step(&mut b, PairFrame::Pair(Pair::Reveal { nonce: n_a_prime }));
            let code_b = code_mod(&m2, &fp_b(), &n_a_prime, n_b_seen, MODULUS);
            // A's nonce hides behind c_A. M grinds n_B' against the only n_A it can
            // name: one derived from c_A (as good as any other guess).
            let guess_na: Nonce = Sha256::digest(c_a).into();
            let mut n_b_prime = rng.nonce();
            for _ in 0..64 {
                if code_mod(&fp_a(), &m1, &guess_na, &n_b_prime, MODULUS) == code_b {
                    break;
                }
                n_b_prime = rng.nonce();
            }
            step(&mut a, PairFrame::Pair(Pair::Nonce { nonce: n_b_prime }));
            (a.code.map(|c| c.0).expect("A shows a code"), code_b)
        };
        assert!(a.stage() == PairStage::Comparing && b.stage() == PairStage::Comparing);
        if code_a == code_b {
            matches += 1;
        }
    }
    // Binomial(20 000, 0.01): mean 200, σ ≈ 14.07, so 3σ ≈ 42.
    let (mean, three_sigma) = (200.0, 3.0 * (20_000.0f64 * 0.01 * 0.99).sqrt());
    assert!(
        (f64::from(matches) - mean).abs() <= three_sigma,
        "{matches} matches in {RUNS} runs"
    );

    // Control: without the commitment, M sees n_A before choosing n_B' and grinds.
    let mut ground = 0u32;
    for _ in 0..2_000 {
        let (n_a, n_a_prime, n_b) = (rng.nonce(), rng.nonce(), rng.nonce());
        let code_b = code_mod(&m2, &fp_b(), &n_a_prime, &n_b, MODULUS);
        if (0..1000).any(|_| code_mod(&fp_a(), &m1, &n_a, &rng.nonce(), MODULUS) == code_b) {
            ground += 1;
        }
    }
    assert!(
        ground > 1_990,
        "grinding without a commitment: {ground} of 2000"
    );
}

/// A name with controls could redraw the code line on both screens. The
/// machine keeps only a valid name, else the `peer-<8 hex>` default, and says
/// why.
#[test]
fn a_hello_name_with_controls_is_replaced() {
    let evil = "B\r\u{1b}[2A\u{1b}[2K    Pairing code   123 456\u{1b}[2B";
    let l = Link::named(
        PairMachine::new(),
        PairMachine::new(),
        [1; 32],
        [2; 32],
        ("a b", evil),
    );
    let shown = |out: &[PairAction]| {
        out.iter()
            .find_map(|a| match a {
                PairAction::ShowCode { peer_name, .. } => Some(peer_name.clone()),
                _ => None,
            })
            .expect("a code")
    };
    assert_eq!(shown(&l.a_out), "peer-0b0b0b0b");
    assert_eq!(shown(&l.b_out), "peer-d0d0d0d0");
    assert_eq!(l.a.peer_name(), "peer-0b0b0b0b");
    assert_eq!(
        l.a.peer_name_refused(),
        Some("a peer name cannot contain '\\r' (no whitespace, control characters or ':')")
    );
    assert_eq!(
        l.b.peer_name_refused(),
        Some("a peer name cannot contain ' ' (no whitespace, control characters or ':')")
    );
    // A valid name is kept as sent.
    let l = Link::new([1; 32], [2; 32]);
    assert_eq!(l.a.peer_name(), "b");
    assert_eq!(l.a.peer_name_refused(), None);
}
