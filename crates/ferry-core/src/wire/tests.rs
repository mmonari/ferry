//! Wire tests (`wire::*`).

use proptest::prelude::*;

use super::control::{Pair, WireOrigin, WireRevision};
use super::datagram::seq_newer;
use super::*;
use crate::geom::{MonitorInfo, WireOffset, WireRect};

fn enc(m: &Control) -> Vec<u8> {
    let mut b = Vec::new();
    m.encode(&mut b);
    b
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn monitor() -> MonitorInfo {
    MonitorInfo {
        name: "eDP-1".into(),
        description: "Chimei Innolux 0x1366".into(),
        rect: WireRect {
            x: 0,
            y: 0,
            w: 1366,
            h: 768,
        },
        scale: 1.0,
        transform: 0,
        shift: None,
    }
}

fn pos() -> Pos {
    Pos {
        motion_seq: 41,
        x: 523.0,
        y: 0.0,
    }
}

/// One instance of every message, in variant order.
fn every_message() -> Vec<Control> {
    vec![
        Control::Hello {
            proto_major: 1,
            proto_minor: 0,
            name: "laptop".into(),
            features: vec![],
            build: "0.1.0".into(),
            monitors: vec![monitor()],
        },
        Control::Error {
            code: 1,
            message: "version mismatch: 1.0 vs 2.0".into(),
        },
        Control::Keymap {
            serial: 1,
            format: 1,
            text: "xkb_keymap {};".into(),
        },
        Control::Enter {
            session: 3,
            x: 523.0,
            y: 0.0,
            keymap_serial: 1,
            motion_seq: 40,
        },
        Control::Leave {
            session: 3,
            reason: LeaveReason::Home.code(),
        },
        Control::Key {
            session: 3,
            code: 30,
            pressed: true,
        },
        Control::Button {
            session: 3,
            code: 0x110,
            pressed: true,
            pos: pos(),
        },
        Control::Scroll {
            session: 3,
            axis: 0,
            source: 0,
            value: 15.0,
            value120: Some(120),
            stop: false,
            pos: pos(),
        },
        Control::Held {
            session: 3,
            n: 2,
            keys: vec![29, 125],
            buttons: vec![],
            pos: pos(),
        },
        Control::Monitors {
            monitors: vec![monitor()],
        },
        Control::Layout {
            rev: WireRevision {
                counter: 2,
                peer: [0xab; 32],
            },
            origins: vec![WireOrigin {
                peer: [0xcd; 32],
                x: 277,
                y: 1080,
            }],
        },
        Control::LayoutAck {
            rev: WireRevision {
                counter: 2,
                peer: [0xab; 32],
            },
        },
        Control::HeldAck { session: 3, n: 2 },
        Control::Ended {
            session: 3,
            reason: LeaveReason::Watchdog.code(),
        },
        Control::PeerState { locked: true },
    ]
}

#[test]
fn key_matches_the_design_note_example() {
    let b = enc(&Control::Key {
        session: 3,
        code: 30,
        pressed: true,
    });
    assert_eq!(hex(&b), "82 05 83 03 18 1e f5");
}

#[test]
fn control_bytes() {
    let mut s = String::new();
    for m in every_message() {
        let b = enc(&m);
        s.push_str(&format!("#{} {} B\n{}\n", m.variant(), b.len(), hex(&b)));
    }
    for p in [
        Pair::Commit {
            commitment: [1; 32],
        },
        Pair::Nonce { nonce: [2; 32] },
        Pair::Reveal { nonce: [3; 32] },
        Pair::Confirm {
            ok: true,
            reason: None,
        },
        Pair::Confirm {
            ok: false,
            reason: Some(1),
        },
    ] {
        let mut b = Vec::new();
        minicbor::encode(&p, &mut b).expect("vec writer");
        s.push_str(&format!("pair {} B\n{}\n", b.len(), hex(&b)));
    }
    insta::assert_snapshot!(s);
}

#[test]
fn variants_are_numbered_in_order() {
    for (i, m) in every_message().iter().enumerate() {
        assert_eq!(m.variant() as usize, i);
        assert_eq!(Control::decode(&enc(m)).ok(), Some(Decoded::Msg(m.clone())));
    }
}

#[test]
fn unknown_variant_skipped() {
    // [99, [1, "x"]]
    let unknown = [0x82, 0x18, 0x63, 0x82, 0x01, 0x61, b'x'];
    assert_eq!(Control::decode(&unknown).ok(), Some(Decoded::Unknown(99)));
    // Pairing numbers are unknown on the session ALPN.
    let mut b = Vec::new();
    minicbor::encode(
        Pair::Confirm {
            ok: true,
            reason: None,
        },
        &mut b,
    )
    .expect("vec writer");
    assert_eq!(Control::decode(&b).ok(), Some(Decoded::Unknown(33)));
    // The next frame decodes.
    let next = Control::HeldAck { session: 1, n: 1 };
    assert_eq!(Control::decode(&enc(&next)).ok(), Some(Decoded::Msg(next)));
}

#[test]
fn peer_state_only_with_feature() {
    let m = Control::PeerState { locked: true };
    assert_eq!(m.feature(), Some("peer-state"));
    // A peer without the feature (an older build) never gets it.
    assert!(!m.may_send_to(&[]));
    assert!(!m.may_send_to(&["layout-sync".to_string()]));
    assert!(m.may_send_to(&["layout-sync".to_string(), "peer-state".to_string()]));
    // This build lists it, so two of them exchange it.
    assert!(super::FEATURES.contains(&"peer-state"));
    // Every other message goes to any peer.
    for other in every_message() {
        if other.variant() != 14 {
            assert!(other.may_send_to(&[]), "#{}", other.variant());
        }
    }
    // An older build skips #14 as an unknown variant.
    assert_eq!(enc(&m)[..2], [0x82, 0x0e]);
}

#[test]
fn extra_field_ignored() {
    // A v1.1 `Key` with a field 3 (a string) decodes in v1.0.
    let v11 = [
        0x82, 0x05, 0x84, 0x03, 0x18, 0x1e, 0xf5, 0x63, b'n', b'e', b'w',
    ];
    assert_eq!(
        Control::decode(&v11).ok(),
        Some(Decoded::Msg(Control::Key {
            session: 3,
            code: 30,
            pressed: true
        }))
    );
}

#[test]
fn missing_option_is_none() {
    // A `Scroll` from a peer that never sent value120 (field 4 null).
    let m = Control::Scroll {
        session: 1,
        axis: 1,
        source: 1,
        value: -2.5,
        value120: None,
        stop: true,
        pos: pos(),
    };
    assert_eq!(Control::decode(&enc(&m)).ok(), Some(Decoded::Msg(m)));
}

#[test]
fn hello_head_frozen() {
    // A future `Hello` 2.3 with two extra fields and a monitor shape this
    // build cannot read past field 0: majors and minors still come through
    // the frozen head.
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(2).and_then(|e| e.u32(0)).expect("vec");
    e.array(8)
        .and_then(|e| e.u16(2))
        .and_then(|e| e.u16(3))
        .and_then(|e| e.str("future"))
        .and_then(|e| e.array(0))
        .and_then(|e| e.str("9.9.9"))
        .and_then(|e| e.array(0))
        .and_then(|e| e.str("field 6"))
        .and_then(|e| e.u64(7))
        .expect("vec");
    let b = e.into_writer();
    match Control::decode(&b).ok() {
        Some(Decoded::Msg(Control::Hello {
            proto_major,
            proto_minor,
            name,
            ..
        })) => {
            assert_eq!((proto_major, proto_minor), (2, 3));
            assert_eq!(name, "future");
        }
        other => panic!("{other:?}"),
    }
    // Even when the rest of a future Hello is unreadable, the head is.
    let mut d = minicbor::Decoder::new(&b);
    d.array().and_then(|_| d.u32()).expect("variant");
    d.array().expect("fields");
    assert_eq!((d.u16().ok(), d.u16().ok()), (Some(2), Some(3)));
}

#[test]
fn leave_reason_unknown_is_other() {
    assert_eq!(LeaveReason::from_code(200), LeaveReason::Other);
    assert_eq!(LeaveReason::from_code(18), LeaveReason::Other);
    for r in LeaveReason::ALL {
        assert_eq!(LeaveReason::from_code(r.code()), r);
        assert!(!r.as_str().is_empty());
    }
    assert_eq!(LeaveReason::EscapeChord.as_str(), "escape_chord");
    assert_eq!(LeaveReason::LinkLost.as_str(), "link_lost");
}

/// M3 design 4.1: the anchor shift is an optional trailing field of a monitor report.
#[test]
fn a_monitor_without_a_shift_is_the_five_elements_it_always_was() {
    let bytes = enc(&Control::Monitors {
        monitors: vec![monitor()],
    });
    // [9, [[monitor]]]: the monitor is an array of 5 (0x85), and no null follows it
    assert!(
        bytes.windows(2).any(|w| w == [0x81, 0x85]),
        "{}",
        hex(&bytes)
    );
    assert!(!bytes.contains(&0xf6), "{}", hex(&bytes));
}

#[test]
fn a_shift_is_the_sixth_element_and_round_trips() {
    let shifted = MonitorInfo {
        shift: Some(WireOffset { x: 1920, y: -5 }),
        ..monitor()
    };
    let msg = Control::Monitors {
        monitors: vec![shifted.clone()],
    };
    let bytes = enc(&msg);
    assert!(
        bytes.windows(2).any(|w| w == [0x81, 0x86]),
        "{}",
        hex(&bytes)
    );
    assert_eq!(Control::decode(&bytes).ok(), Some(Decoded::Msg(msg)));
    // the five-element form of the same monitor reads as no shift
    let five = minicbor::to_vec(MonitorInfo {
        shift: None,
        ..shifted
    })
    .expect("vec writer");
    let back: MonitorInfo = minicbor::decode(&five).expect("a five-element monitor");
    assert_eq!(back.shift, None);
}

/// A monitor as an older ferryd declares it: no field 5. Wire evolution: it ignores the shift.
#[derive(Debug, PartialEq, minicbor::Decode)]
struct MonitorBeforeTheShift {
    #[n(0)]
    name: String,
    #[n(1)]
    description: String,
    #[n(2)]
    rect: WireRect,
    #[n(3)]
    scale: f32,
    #[n(4)]
    transform: u8,
}

#[test]
fn an_older_peer_ignores_the_shift() {
    let shifted = MonitorInfo {
        shift: Some(WireOffset { x: 1920, y: 0 }),
        ..monitor()
    };
    let bytes = minicbor::to_vec(&shifted).expect("vec writer");
    let old: MonitorBeforeTheShift = minicbor::decode(&bytes).expect("an older decoder");
    assert_eq!(
        (
            old.name,
            old.description,
            old.rect,
            old.scale,
            old.transform
        ),
        (
            shifted.name,
            shifted.description,
            shifted.rect,
            shifted.scale,
            shifted.transform
        )
    );
}

#[test]
fn a_shift_past_the_cap_is_refused_on_decode() {
    for (x, y) in [(i32::MIN, 0), (0, i32::MAX), ((1 << 24) + 1, 0)] {
        let msg = Control::Monitors {
            monitors: vec![MonitorInfo {
                shift: Some(WireOffset { x, y }),
                ..monitor()
            }],
        };
        assert!(
            matches!(
                Control::decode(&enc(&msg)),
                Err(DecodeError::Limit {
                    field: "monitor.shift",
                    ..
                })
            ),
            "({x}, {y})"
        );
    }
    let at_cap = Control::Monitors {
        monitors: vec![MonitorInfo {
            shift: Some(WireOffset {
                x: -(1 << 24),
                y: 1 << 24,
            }),
            ..monitor()
        }],
    };
    assert!(matches!(
        Control::decode(&enc(&at_cap)),
        Ok(Decoded::Msg(_))
    ));
}

/// A peer's monitor name and description are checked where they are decoded.
#[test]
fn a_control_character_in_a_monitor_text_is_refused() {
    let hello = |m: MonitorInfo| Control::Hello {
        proto_major: 1,
        proto_minor: 0,
        name: "peer".into(),
        features: vec![],
        build: String::new(),
        monitors: vec![m],
    };
    assert!(Control::decode(&enc(&hello(monitor()))).is_ok());
    for bad in ["eDP-1\r\x1b[2K", "a\nb", "tab\there", "\u{9b}"] {
        let named = MonitorInfo {
            name: bad.into(),
            ..monitor()
        };
        assert!(
            matches!(
                Control::decode(&enc(&hello(named))),
                Err(DecodeError::Text {
                    field: "monitor.name"
                })
            ),
            "{bad:?}"
        );
        let described = MonitorInfo {
            description: bad.into(),
            ..monitor()
        };
        assert!(matches!(
            Control::decode(&enc(&Control::Monitors {
                monitors: vec![described]
            })),
            Err(DecodeError::Text {
                field: "monitor.description"
            })
        ));
    }
}

#[test]
fn caps_enforced() {
    let held = Control::Held {
        session: 1,
        n: 1,
        keys: (0..33).collect(),
        buttons: vec![],
        pos: pos(),
    };
    assert!(matches!(
        Control::decode(&enc(&held)),
        Err(DecodeError::Limit {
            field: "held.keys",
            cap: 32
        })
    ));
    let hello = Control::Hello {
        proto_major: 1,
        proto_minor: 0,
        name: "n".repeat(65),
        features: vec![],
        build: String::new(),
        monitors: vec![],
    };
    assert!(matches!(
        Control::decode(&enc(&hello)),
        Err(DecodeError::Limit {
            field: "name",
            cap: 64
        })
    ));
    let at_cap = Control::Held {
        session: 1,
        n: 1,
        keys: (0..32).collect(),
        buttons: (0..16).collect(),
        pos: pos(),
    };
    assert!(matches!(
        Control::decode(&enc(&at_cap)),
        Ok(Decoded::Msg(_))
    ));
    let buttons = Control::Held {
        session: 1,
        n: 1,
        keys: vec![],
        buttons: (0..17).collect(),
        pos: pos(),
    };
    assert!(matches!(
        Control::decode(&enc(&buttons)),
        Err(DecodeError::Limit { .. })
    ));
    let layout = Control::Layout {
        rev: WireRevision {
            counter: 1,
            peer: [0; 32],
        },
        origins: vec![
            WireOrigin {
                peer: [0; 32],
                x: 0,
                y: 0
            };
            5
        ],
    };
    assert!(matches!(
        Control::decode(&enc(&layout)),
        Err(DecodeError::Limit { .. })
    ));
}

/// Peer geometry is range-checked on decode, so every sum in `geom` fits in
/// `i32` (A-03).
#[test]
fn geometry_caps_enforced() {
    let monitors = |rect| Control::Monitors {
        monitors: vec![MonitorInfo { rect, ..monitor() }],
    };
    let r = |x, y, w, h| WireRect { x, y, w, h };
    for (rect, field) in [
        (r(0, 0, u32::MAX, 1080), "monitor.size"),
        (r(0, 0, 1920, caps::MONITOR_SIZE + 1), "monitor.size"),
        (r(i32::MIN, 0, 1920, 1080), "monitor.position"),
        (r(0, (1 << 24) + 1, 1920, 1080), "monitor.position"),
    ] {
        match Control::decode(&enc(&monitors(rect))) {
            Err(DecodeError::Limit { field: f, .. }) => assert_eq!(f, field, "{rect:?}"),
            other => panic!("{rect:?}: {other:?}"),
        }
    }
    let at_cap = r(-(1 << 24), 1 << 24, caps::MONITOR_SIZE, caps::MONITOR_SIZE);
    assert!(matches!(
        Control::decode(&enc(&monitors(at_cap))),
        Ok(Decoded::Msg(_))
    ));
    let hello = Control::Hello {
        proto_major: 1,
        proto_minor: 0,
        name: "n".into(),
        features: vec![],
        build: String::new(),
        monitors: vec![MonitorInfo {
            rect: r(0, 0, u32::MAX, 1),
            ..monitor()
        }],
    };
    assert!(matches!(
        Control::decode(&enc(&hello)),
        Err(DecodeError::Limit {
            field: "monitor.size",
            ..
        })
    ));
    let layout = |x, y| Control::Layout {
        rev: WireRevision {
            counter: 1,
            peer: [0; 32],
        },
        origins: vec![WireOrigin {
            peer: [0; 32],
            x,
            y,
        }],
    };
    for (x, y) in [(i32::MIN, 0), (0, i32::MAX), ((1 << 24) + 1, 0)] {
        assert!(matches!(
            Control::decode(&enc(&layout(x, y))),
            Err(DecodeError::Limit {
                field: "layout.origin",
                ..
            })
        ));
    }
    assert!(matches!(
        Control::decode(&enc(&layout(-(1 << 24), 1 << 24))),
        Ok(Decoded::Msg(_))
    ));
}

/// `[0, [major, minor, name, features, build, monitors]]` with `features`
/// given as raw CBOR (A-05).
fn hello_with_features(features: &[u8]) -> Vec<u8> {
    let mut b = vec![0x82, 0x00, 0x86, 0x01, 0x00, 0x61, b'n'];
    b.extend_from_slice(features);
    b.extend_from_slice(&[0x60, 0x80]);
    b
}

/// A-05: a capped `Vec` is refused at its length, before its items are
/// decoded. A 1 MiB `Hello` of 1,040,000 empty features used to decode into
/// a 24 MiB `Vec<String>` first.
#[test]
fn a_1_mib_hello_is_refused_before_its_items_are_decoded() {
    let n: u32 = 1_040_000;
    let mut header = vec![0x9a];
    header.extend_from_slice(&n.to_be_bytes());
    let mut full = header.clone();
    full.resize(header.len() + n as usize, 0x60);
    let body = hello_with_features(&full);
    assert!(body.len() <= MAX_FRAME_BODY);
    assert!(matches!(
        Control::decode(&body),
        Err(DecodeError::Limit {
            field: "features",
            cap: 16
        })
    ));
    // The proof that no item was read: the same header with no items at
    // all is still a cap error, not the end of the input.
    let mut cut = hello_with_features(&header);
    cut.truncate(cut.len() - 2);
    assert!(matches!(
        Control::decode(&cut),
        Err(DecodeError::Limit {
            field: "features",
            ..
        })
    ));
    // An indefinite-length array stops at the 17th item.
    let mut indef = vec![0x9f];
    indef.extend_from_slice(&[0x60; 17]);
    assert!(matches!(
        Control::decode(&hello_with_features(&indef)[..indef.len() + 7]),
        Err(DecodeError::Limit {
            field: "features",
            ..
        })
    ));
    indef.truncate(17);
    indef.push(0xff);
    assert!(matches!(
        Control::decode(&hello_with_features(&indef)),
        Ok(Decoded::Msg(Control::Hello { features, .. })) if features.len() == 16
    ));
    // Every capped list, by the length alone.
    let held = |keys: &[u8]| {
        let mut b = vec![0x82, 0x08, 0x85, 0x01, 0x01];
        b.extend_from_slice(keys);
        b
    };
    assert!(matches!(
        Control::decode(&held(&[0x99, 0xff, 0xff])),
        Err(DecodeError::Limit {
            field: "held.keys",
            ..
        })
    ));
    assert!(matches!(
        Control::decode(&held(&[0x80, 0x99, 0xff, 0xff])),
        Err(DecodeError::Limit {
            field: "held.buttons",
            ..
        })
    ));
    assert!(matches!(
        Control::decode(&[0x82, 0x09, 0x81, 0x99, 0xff, 0xff]),
        Err(DecodeError::Limit {
            field: "monitors",
            ..
        })
    ));
    let mut hello_monitors = vec![0x82, 0x00, 0x86, 0x01, 0x00, 0x60, 0x80, 0x60];
    hello_monitors.extend_from_slice(&[0x99, 0xff, 0xff]);
    assert!(matches!(
        Control::decode(&hello_monitors),
        Err(DecodeError::Limit {
            field: "monitors",
            ..
        })
    ));
    let mut layout = vec![0x82, 0x0a, 0x82, 0x82, 0x01, 0x58, 0x20];
    layout.extend_from_slice(&[0; 32]);
    layout.extend_from_slice(&[0x99, 0xff, 0xff]);
    assert!(matches!(
        Control::decode(&layout),
        Err(DecodeError::Limit {
            field: "layout.origins",
            ..
        })
    ));
}

#[test]
fn garbage_is_a_cbor_error() {
    assert!(matches!(Control::decode(&[]), Err(DecodeError::Cbor(_))));
    assert!(matches!(
        Control::decode(&[0xff]),
        Err(DecodeError::Cbor(_))
    ));
    // Known variant, wrong field type.
    assert!(matches!(
        Control::decode(&[0x82, 0x05, 0x83, 0x61, b'x', 0x01, 0xf5]),
        Err(DecodeError::Cbor(_))
    ));
}

#[test]
fn datagram_layout() {
    let mut b = [0u8; DATAGRAM_LEN];
    let mut s = String::new();
    for d in [
        Datagram::Motion(Motion {
            session_lo: 3,
            seq: 0x0102_0304,
            x: 523.0,
            y: 0.5,
            dx: -1.0,
            dy: 2.0,
            t_us: 0xdead_beef,
        }),
        Datagram::Ping {
            id: 7,
            t_us: 1_000_000,
        },
        Datagram::Pong {
            id: 7,
            t_us: 1_000_000,
        },
    ] {
        d.encode(&mut b);
        assert_eq!(Datagram::decode(&b).ok(), Some(d));
        s.push_str(&hex(&b));
        s.push('\n');
    }
    insta::assert_snapshot!(s);
    // Unknown flag bits are ignored.
    b[1] = 0x80;
    assert!(Datagram::decode(&b).is_ok());
    assert!(matches!(
        Datagram::decode(&b[..27]),
        Err(DecodeError::DatagramLength(27))
    ));
    assert!(matches!(
        Datagram::decode(&[0u8; 29]),
        Err(DecodeError::DatagramLength(29))
    ));
    b[0] = 9;
    assert!(matches!(
        Datagram::decode(&b),
        Err(DecodeError::DatagramType(9))
    ));
}

// ---- property tests --------------------------------------------------------

fn finite() -> impl Strategy<Value = f32> {
    -1.0e6f32..1.0e6
}

fn text(max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..=max / 4).prop_map(|v| v.into_iter().collect())
}

/// A monitor name or description: any character but a control one, which the decoder refuses.
fn monitor_text(max: usize) -> impl Strategy<Value = String> {
    text(max).prop_map(|s| s.chars().filter(|c| !c.is_control()).collect())
}

fn arb_pos() -> impl Strategy<Value = Pos> {
    (any::<u32>(), finite(), finite()).prop_map(|(motion_seq, x, y)| Pos { motion_seq, x, y })
}

fn arb_monitor() -> impl Strategy<Value = MonitorInfo> {
    (
        monitor_text(128),
        monitor_text(128),
        (coord(), coord(), size(), size()),
        0.25f32..4.0,
        0u8..8,
        proptest::option::of((coord(), coord())),
    )
        .prop_map(
            |(name, description, (x, y, w, h), scale, transform, shift)| MonitorInfo {
                name,
                description,
                rect: WireRect { x, y, w, h },
                scale,
                transform,
                shift: shift.map(|(x, y)| WireOffset { x, y }),
            },
        )
}

fn coord() -> impl Strategy<Value = i32> {
    prop_oneof![-(1i32 << 24)..=1 << 24, Just(-(1 << 24)), Just(1 << 24)]
}

fn size() -> impl Strategy<Value = u32> {
    prop_oneof![0..=caps::MONITOR_SIZE, Just(caps::MONITOR_SIZE)]
}

fn arb_rev() -> impl Strategy<Value = WireRevision> {
    (any::<u64>(), any::<[u8; 32]>()).prop_map(|(counter, peer)| WireRevision { counter, peer })
}

fn arb_control() -> impl Strategy<Value = Control> {
    let s = any::<u32>;
    prop_oneof![
        (
            any::<u16>(),
            any::<u16>(),
            text(64),
            proptest::collection::vec(text(32), 0..=16),
            text(64),
            proptest::collection::vec(arb_monitor(), 0..=16)
        )
            .prop_map(
                |(proto_major, proto_minor, name, features, build, monitors)| {
                    Control::Hello {
                        proto_major,
                        proto_minor,
                        name,
                        features,
                        build,
                        monitors,
                    }
                }
            ),
        (any::<u16>(), text(512)).prop_map(|(code, message)| Control::Error { code, message }),
        (s(), any::<u8>(), text(4096)).prop_map(|(serial, format, text)| Control::Keymap {
            serial,
            format,
            text
        }),
        (s(), finite(), finite(), s(), s()).prop_map(
            |(session, x, y, keymap_serial, motion_seq)| {
                Control::Enter {
                    session,
                    x,
                    y,
                    keymap_serial,
                    motion_seq,
                }
            }
        ),
        (s(), any::<u8>()).prop_map(|(session, reason)| Control::Leave { session, reason }),
        (s(), any::<u16>(), any::<bool>()).prop_map(|(session, code, pressed)| Control::Key {
            session,
            code,
            pressed
        }),
        (s(), any::<u16>(), any::<bool>(), arb_pos()).prop_map(|(session, code, pressed, pos)| {
            Control::Button {
                session,
                code,
                pressed,
                pos,
            }
        }),
        (
            s(),
            any::<u8>(),
            any::<u8>(),
            finite(),
            any::<Option<i32>>(),
            any::<bool>(),
            arb_pos()
        )
            .prop_map(|(session, axis, source, value, value120, stop, pos)| {
                Control::Scroll {
                    session,
                    axis,
                    source,
                    value,
                    value120,
                    stop,
                    pos,
                }
            }),
        (
            s(),
            s(),
            proptest::collection::vec(any::<u16>(), 0..=32),
            proptest::collection::vec(any::<u16>(), 0..=16),
            arb_pos()
        )
            .prop_map(|(session, n, keys, buttons, pos)| Control::Held {
                session,
                n,
                keys,
                buttons,
                pos
            }),
        proptest::collection::vec(arb_monitor(), 0..=16)
            .prop_map(|monitors| Control::Monitors { monitors }),
        (
            arb_rev(),
            proptest::collection::vec(
                (any::<[u8; 32]>(), coord(), coord()).prop_map(|(peer, x, y)| WireOrigin {
                    peer,
                    x,
                    y
                }),
                0..=4
            )
        )
            .prop_map(|(rev, origins)| Control::Layout { rev, origins }),
        arb_rev().prop_map(|rev| Control::LayoutAck { rev }),
        (s(), s()).prop_map(|(session, n)| Control::HeldAck { session, n }),
        (s(), any::<u8>()).prop_map(|(session, reason)| Control::Ended { session, reason }),
        any::<bool>().prop_map(|locked| Control::PeerState { locked }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn control_roundtrip(m in arb_control()) {
        let b = enc(&m);
        prop_assert_eq!(Control::decode(&b).ok(), Some(Decoded::Msg(m)));
    }
}

/// Mutations of valid frames, plus raw bytes.
#[derive(Debug, Clone)]
enum Mangle {
    Truncate(usize),
    Flip(Vec<(usize, u8)>),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8192))]

    #[test]
    fn decode_never_panics(
        m in arb_control(),
        mangle in prop_oneof![
            any::<usize>().prop_map(Mangle::Truncate),
            proptest::collection::vec((any::<usize>(), 1u8..=255), 1..8).prop_map(Mangle::Flip),
        ],
        raw in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut b = enc(&m);
        match mangle {
            Mangle::Truncate(at) => {
                let at = at % b.len();
                // A strict prefix of a definite-length item never decodes.
                prop_assert!(Control::decode(&b[..at]).is_err());
            }
            Mangle::Flip(flips) => {
                for (i, x) in flips {
                    let n = b.len();
                    b[i % n] ^= x;
                }
                let _ = Control::decode(&b);
            }
        }
        let _ = Control::decode(&raw);
        let _ = Datagram::decode(&raw);
        if raw.len() >= DATAGRAM_LEN {
            let _ = Datagram::decode(&raw[..DATAGRAM_LEN]);
        }
    }

    #[test]
    fn datagram_roundtrip(
        session_lo in any::<u16>(), seq in any::<u32>(),
        x in finite(), y in finite(), dx in finite(), dy in finite(), t_us in any::<u32>(),
    ) {
        let d = Datagram::Motion(Motion { session_lo, seq, x, y, dx, dy, t_us });
        let mut b = [0u8; DATAGRAM_LEN];
        d.encode(&mut b);
        prop_assert_eq!(Datagram::decode(&b).ok(), Some(d));
    }

    #[test]
    fn seq_newer_wraps(a in any::<u32>(), d in 1u32..=i32::MAX as u32) {
        let b = a.wrapping_add(d);
        prop_assert!(seq_newer(b, a));
        prop_assert!(!seq_newer(a, b));
        prop_assert!(!seq_newer(a, a));
    }
}

#[test]
fn seq_newer_across_max() {
    assert!(seq_newer(0, u32::MAX));
    assert!(seq_newer(5, u32::MAX - 5));
    assert!(!seq_newer(u32::MAX, 0));
}
