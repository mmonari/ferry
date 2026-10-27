//! Socket-API shapes (`api::shapes`) and `layout.toml`
//! (`api::layout_toml`). The snapshots are the wire: compact JSON lines.

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::*;
use crate::config::LayoutFile;
use crate::geom::{Rect, Rel};
use crate::testkit::{desk, desk_monitors, layout_with_netbook, netbook, netbook_monitors, third};

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialize")
}

fn ok<T: Serialize>(id: u64, result: T) -> String {
    json(&ResponseLine {
        id,
        body: ResponseBody::Result(result),
    })
}

fn me() -> PeerRef {
    PeerRef {
        name: "desk".into(),
        fingerprint: desk().to_string(),
    }
}

fn lap() -> PeerRef {
    PeerRef {
        name: "laptop".into(),
        fingerprint: netbook().to_string(),
    }
}

#[test]
fn shapes() {
    let mut s = String::new();
    let mut line = |label: &str, v: String| s.push_str(&format!("{label}\n{v}\n"));

    line(
        "hello",
        json(&EventLine::Hello(HelloData {
            api: API_VERSION,
            version: "0.1.0".into(),
            peer: "desk".into(),
            fingerprint: desk().to_string(),
            methods: vec!["status".into(), "subscribe".into()],
            topics: vec!["state".into(), "layout".into()],
        })),
    );

    let mut counters = BTreeMap::new();
    counters.insert("bounces".to_string(), 2);
    counters.insert("silence_releases".to_string(), 0);
    let status = StatusResult {
        api: API_VERSION,
        me: me(),
        port: 4243,
        role: Role::Source,
        active_peer: Some(lap()),
        capture: CaptureStatus {
            backend: "portal".into(),
            gate: crate::source::Gate::Armed.as_str().into(),
            barriers: 1,
            failed_barriers: vec![],
            off_reason: None,
            notice: None,
        },
        screen_locked: Some(false),
        locked: false,
        peers: vec![
            PeerStatus {
                name: "laptop".into(),
                fingerprint: netbook().to_string(),
                state: LinkState::Connected,
                addr: Some("192.0.2.11:4243".into()),
                rtt_us: Some(850),
                proto: Some("1.0".into()),
                detail: None,
                suspect: true,
                locked: true,
                last_seen: None,
            },
            PeerStatus {
                name: "old".into(),
                fingerprint: "ab".repeat(32),
                state: LinkState::Refused,
                addr: None,
                rtt_us: None,
                proto: Some("2.0".into()),
                detail: Some("version mismatch: 1.0 vs 2.0".into()),
                suspect: false,
                locked: false,
                last_seen: None,
            },
        ],
        last_release: Some(LastRelease {
            reason: crate::wire::LeaveReason::Home.as_str().into(),
            ago_ms: 1200,
            mono_us: None,
        }),
        counters,
        pairing: None,
        summary: Some(Summary {
            state: SummaryState::Away,
            peer: Some("laptop".into()),
        }),
        attention: vec![],
        escape_chord: Some("Super+Ctrl+Escape".into()),
    };
    line("status", ok(1, status.clone()));
    // Unknown lock state: `screen_locked` is null, not absent; idle has no
    // active peer and no last release.
    let idle = StatusResult {
        role: Role::Idle,
        active_peer: None,
        screen_locked: None,
        last_release: None,
        peers: vec![],
        counters: BTreeMap::new(),
        capture: CaptureStatus {
            backend: "off".into(),
            gate: "off".into(),
            barriers: 0,
            failed_barriers: vec![],
            off_reason: None,
            notice: None,
        },
        summary: Some(Summary {
            state: SummaryState::Idle,
            peer: None,
        }),
        ..status.clone()
    };
    line("status (idle, lock unknown)", ok(2, idle));
    // Controlled by another peer, and locked to this machine.
    let controlled = StatusResult {
        role: Role::Target,
        locked: true,
        capture: CaptureStatus {
            backend: "portal".into(),
            gate: crate::source::Gate::Idle.as_str().into(),
            barriers: 0,
            failed_barriers: vec![],
            off_reason: Some("controlled".into()),
            notice: None,
        },
        peers: vec![],
        counters: BTreeMap::new(),
        summary: Some(Summary {
            state: SummaryState::Attention,
            peer: None,
        }),
        attention: vec![Attention {
            code: AttentionCode::PeerRefused,
            message: "laptop cannot connect: version mismatch: 1.0 vs 2.0".into(),
            hint: Some(
                "`ferry doctor` has the details. After a key change or if it unpaired this machine, `ferry unpair laptop` and pair again; after a version mismatch, update the older ferry"
                    .into(),
            ),
            peer: Some("laptop".into()),
        }],
        ..status
    };
    line("status (target, locked here, attention)", ok(9, controlled));
    line("lock", ok(10, LockResult { locked: true }));

    line("release", ok(3, EmptyResult {}));
    line(
        "id",
        ok(
            4,
            IdResult {
                name: "desk".into(),
                fingerprint: desk().to_string(),
                port: 4243,
                addrs: vec!["192.0.2.111".into()],
            },
        ),
    );
    line("trust", ok(5, TrustResult { peer: lap() }));

    let layout = LayoutResult {
        revision: RevisionView {
            counter: 1,
            peer: desk().to_string(),
        },
        peers: vec![
            LayoutPeer {
                name: "desk".into(),
                fingerprint: desk().to_string(),
                origin: Some((0, 0)),
                monitors: vec![MonitorView {
                    name: "HDMI-A-1".into(),
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                    scale: 1.0,
                    description: "Dell Inc. DELL U2415 7MT0167B1ZUL".into(),
                    transform: 0,
                }],
                online: true,
                trusted: true,
                is_self: true,
                bounds: Some(Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                }),
                shift: None,
            },
            LayoutPeer {
                name: "laptop".into(),
                fingerprint: netbook().to_string(),
                origin: Some((277, 1080)),
                monitors: vec![MonitorView {
                    name: "eDP-1".into(),
                    x: 277,
                    y: 1080,
                    w: 1366,
                    h: 768,
                    scale: 1.0,
                    description: "Chimei Innolux 0x1366".into(),
                    transform: 0,
                }],
                online: true,
                trusted: true,
                is_self: false,
                bounds: Some(Rect {
                    x: 277,
                    y: 1080,
                    w: 1366,
                    h: 768,
                }),
                shift: None,
            },
        ],
        links: vec![LinkView {
            from: "desk:HDMI-A-1".into(),
            side: crate::geom::Side::Bottom,
            to: "laptop:eDP-1".into(),
            span: (277, 1643),
            live: true,
        }],
    };
    line("layout.get / layout.place", ok(6, layout.clone()));
    // A group Hyprland moved (the anchor shift), a rotated monitor, and a third peer placed by
    // someone else that this peer is not paired with: no monitors known, so no bounds.
    let mut shifted = layout;
    shifted.peers[0].monitors[0].x = 1920;
    shifted.peers[0].monitors[0].transform = 1;
    shifted.peers[0].bounds = Some(Rect {
        x: 1920,
        y: 0,
        w: 1080,
        h: 1920,
    });
    shifted.peers[0].shift = Some((1920, 0));
    shifted.peers.push(LayoutPeer {
        name: "4b1f0a9c".into(),
        fingerprint: third().to_string(),
        origin: Some((-1920, 0)),
        monitors: vec![],
        online: false,
        trusted: false,
        is_self: false,
        bounds: None,
        shift: None,
    });
    line("layout.get (shifted group, third peer)", ok(7, shifted));

    for code in [
        ErrorCode::UnknownMethod,
        ErrorCode::BadParams,
        ErrorCode::BadFingerprint,
        ErrorCode::SelfFingerprint,
        ErrorCode::UnknownPeer,
        ErrorCode::AmbiguousPeer,
        ErrorCode::UnknownAnchor,
        ErrorCode::AmbiguousAnchor,
        ErrorCode::PeerMonitorsUnknown,
        ErrorCode::Overlap,
        ErrorCode::StaleLayout,
        ErrorCode::Internal,
    ] {
        let hint = (code == ErrorCode::PeerMonitorsUnknown)
            .then(|| "start ferryd on laptop and check ferry status".to_string());
        line(
            "error",
            json(&ResponseLine::<()> {
                id: 9,
                body: ResponseBody::Error(ApiError {
                    code,
                    message: "…".into(),
                    hint,
                }),
            }),
        );
    }
    // The two answers of `layout.set` that carry text a frontend shows as it is (M3 design 2.5).
    for (id, code, message, hint) in [
        (
            10,
            ErrorCode::Overlap,
            "laptop overlaps desk",
            "ferry layout show lists where each peer's monitors are",
        ),
        (
            11,
            ErrorCode::StaleLayout,
            "the layout changed (rev 15 by laptop)",
            "read the layout again with layout.get, then apply your change on top of it",
        ),
    ] {
        line(
            "error (layout.set)",
            json(&ResponseLine::<()> {
                id,
                body: ResponseBody::Error(ApiError {
                    code,
                    message: message.into(),
                    hint: Some(hint.into()),
                }),
            }),
        );
    }
    insta::assert_snapshot!(s);
}

#[test]
fn requests() {
    let mut s = String::new();
    for r in [
        Request::Status,
        Request::Release,
        Request::Id,
        Request::Trust(TrustParams {
            fingerprint: netbook().to_string(),
            name: Some("laptop".into()),
            addr: Some("192.0.2.11".into()),
        }),
        Request::LayoutGet,
        Request::LayoutPlace(PlaceParams {
            peer: "laptop".into(),
            rel: Rel::Below,
            anchor: "HDMI-A-1".into(),
            align: AlignParam::Named(AlignName::Center),
        }),
        Request::LayoutPlace(PlaceParams {
            peer: "laptop".into(),
            rel: Rel::LeftOf,
            anchor: "desk".into(),
            align: AlignParam::Px(-40),
        }),
        // Default align: omitted on the wire (it cannot be serialised).
        Request::LayoutPlace(PlaceParams {
            peer: "laptop".into(),
            rel: Rel::RightOf,
            anchor: "desk:DVI-D-1".into(),
            align: AlignParam::Default,
        }),
        // One origin, and no `base_rev`: what `ferry layout set` sends.
        Request::LayoutSet(SetParams {
            origins: vec![OriginParam {
                peer: "laptop".into(),
                x: 2560,
                y: 0,
            }],
            base_rev: None,
        }),
        // Several groups in one revision, from a draft made at revision 14 (the Arrange panel).
        Request::LayoutSet(SetParams {
            origins: vec![
                OriginParam {
                    peer: "laptop".into(),
                    x: -1366,
                    y: 156,
                },
                OriginParam {
                    peer: netbook().to_string(),
                    x: 3840,
                    y: -20,
                },
            ],
            base_rev: Some(RevisionView {
                counter: 14,
                peer: desk().to_string(),
            }),
        }),
        Request::Lock(LockParams { on: Some(true) }),
        // Toggle.
        Request::Lock(LockParams { on: None }),
        Request::Doctor,
    ] {
        let line = json(&RequestLine {
            id: 7,
            request: r.clone(),
        });
        // Every request round-trips through the two-step parser.
        assert_eq!(
            RequestLine::parse(&line),
            Ok(RequestLine { id: 7, request: r })
        );
        s.push_str(&line);
        s.push('\n');
    }
    insta::assert_snapshot!(s);
}

/// The pairing methods and events, `peers` and `unpair`.
#[test]
fn pairing_shapes() {
    let mut s = String::new();
    let mut line = |label: &str, v: String| s.push_str(&format!("{label}\n{v}\n"));
    for r in [
        Request::PairAccept,
        Request::PairStart(PairStartParams {
            addr: "192.0.2.11".into(),
            name: Some("laptop".into()),
        }),
        Request::PairAnswer(PairAnswerParams { matches: true }),
        Request::PairCancel,
        Request::Peers,
        Request::Unpair(UnpairParams {
            peer: "laptop".into(),
        }),
    ] {
        let text = json(&RequestLine {
            id: 7,
            request: r.clone(),
        });
        assert_eq!(
            RequestLine::parse(&text),
            Ok(RequestLine { id: 7, request: r })
        );
        line("request", text);
    }
    line(
        "pair.accept",
        ok(
            8,
            PairAcceptResult {
                expires_ms: 120_000,
            },
        ),
    );
    let peer = PairingPeer {
        name: "desktop".into(),
        fingerprint: desk().to_string(),
        addr: Some("192.0.2.111:4243".into()),
    };
    for ev in [
        PairingEvent {
            stage: PairingStage::Connecting,
            role: Some(PairingRole::Dialer),
            peer: None,
            code: None,
            expires_ms: 120_000,
            reason: None,
            message: None,
            cancelled_by: None,
        },
        PairingEvent {
            stage: PairingStage::Code,
            role: Some(PairingRole::Acceptor),
            peer: Some(peer.clone()),
            code: Some("418207".into()),
            expires_ms: 118_200,
            reason: None,
            message: None,
            cancelled_by: None,
        },
        PairingEvent {
            stage: PairingStage::Paired,
            role: Some(PairingRole::Acceptor),
            peer: Some(peer.clone()),
            code: None,
            expires_ms: 0,
            reason: None,
            message: None,
            cancelled_by: None,
        },
        PairingEvent {
            stage: PairingStage::Failed,
            role: Some(PairingRole::Dialer),
            peer: Some(peer.clone()),
            code: None,
            expires_ms: 95_000,
            reason: Some("rejected".into()),
            message: Some("cancelled: codes differ".into()),
            cancelled_by: None,
        },
        // a cancel says whose it was: this machine's (any connection), or the other peer's
        PairingEvent {
            stage: PairingStage::Failed,
            role: Some(PairingRole::Dialer),
            peer: Some(peer.clone()),
            code: None,
            expires_ms: 94_000,
            reason: Some("cancelled".into()),
            message: Some("cancelled".into()),
            cancelled_by: Some(CancelledBy::Local),
        },
        PairingEvent {
            stage: PairingStage::Failed,
            role: Some(PairingRole::Acceptor),
            peer: Some(peer),
            code: None,
            expires_ms: 93_000,
            reason: Some("cancelled".into()),
            message: Some("cancelled".into()),
            cancelled_by: Some(CancelledBy::Peer),
        },
        PairingEvent {
            stage: PairingStage::Closed,
            role: Some(PairingRole::Acceptor),
            peer: None,
            code: None,
            expires_ms: 92_000,
            reason: Some("cancelled".into()),
            message: Some("pairing window closed: cancelled".into()),
            cancelled_by: Some(CancelledBy::Local),
        },
        // a dial that never reached the other peer ends with a `failed` of its own
        PairingEvent {
            stage: PairingStage::Failed,
            role: Some(PairingRole::Dialer),
            peer: None,
            code: None,
            expires_ms: 119_000,
            reason: Some("not_accepting".into()),
            message: Some("192.0.2.11 is not accepting pairing".into()),
            cancelled_by: None,
        },
    ] {
        line("event", json(&EventLine::Pairing(ev)));
    }
    line(
        "status.pairing",
        json(&PairingStatus {
            role: PairingRole::Acceptor,
            stage: PairingStage::Listening,
            expires_ms: 61_000,
        }),
    );
    line(
        "peers",
        ok(
            9,
            PeersResult {
                peers: vec![PeerEntry {
                    name: "laptop".into(),
                    fingerprint: netbook().to_string(),
                    addr: Some("192.0.2.11:4243".into()),
                    last_seen: Some("2026-10-18T09:12:03Z".into()),
                    paired: Paired::Sas,
                    state: LinkState::Connected,
                    locked: false,
                }],
            },
        ),
    );
    for code in [
        ErrorCode::PairingNotAccepting,
        ErrorCode::PairingBusy,
        ErrorCode::PairingExpired,
        ErrorCode::NoPairing,
        ErrorCode::PeerUnreachable,
    ] {
        line(
            "error",
            json(&ResponseLine::<()> {
                id: 9,
                body: ResponseBody::Error(ApiError {
                    code,
                    message: "…".into(),
                    hint: None,
                }),
            }),
        );
    }
    // A unit method ignores params; `pair.answer` needs `match`.
    assert_eq!(
        RequestLine::parse(r#"{"id":1,"method":"pair.accept","params":{"x":1}}"#),
        Ok(RequestLine {
            id: 1,
            request: Request::PairAccept
        })
    );
    assert_eq!(
        RequestLine::parse(r#"{"id":2,"method":"pair.answer","params":{}}"#)
            .unwrap_err()
            .error
            .code,
        ErrorCode::BadParams
    );
    insta::assert_snapshot!(s);
}

#[test]
fn align_param_round_trip() {
    for (text, align, expect) in [
        (
            r#"{"peer":"p","rel":"below","anchor":"a"}"#,
            AlignParam::Default,
            crate::geom::Align::Start,
        ),
        (
            r#"{"peer":"p","rel":"below","anchor":"a","align":"start"}"#,
            AlignParam::Named(AlignName::Start),
            crate::geom::Align::Start,
        ),
        (
            r#"{"peer":"p","rel":"below","anchor":"a","align":"center"}"#,
            AlignParam::Named(AlignName::Center),
            crate::geom::Align::Center,
        ),
        (
            r#"{"peer":"p","rel":"below","anchor":"a","align":"end"}"#,
            AlignParam::Named(AlignName::End),
            crate::geom::Align::End,
        ),
        (
            r#"{"peer":"p","rel":"below","anchor":"a","align":-12}"#,
            AlignParam::Px(-12),
            crate::geom::Align::Offset(-12),
        ),
    ] {
        let p: PlaceParams = serde_json::from_str(text).expect(text);
        assert_eq!(p.align, align);
        assert_eq!(p.align.to_align(), expect);
        assert_eq!(json(&p), text);
    }
}

/// `layout.set` takes at least one origin with whole-pixel coordinates; `base_rev` is optional;
/// unknown fields are ignored (API versioning).
#[test]
fn layout_set_params_parse_and_refuse() {
    let parse = |line: &str| RequestLine::parse(line);
    let ok = parse(
        r#"{"id":3,"method":"layout.set","params":{"origins":[{"peer":"lap","x":2560,"y":0,"future":1}],"future":true}}"#,
    )
    .unwrap();
    assert_eq!(
        ok.request,
        Request::LayoutSet(SetParams {
            origins: vec![OriginParam {
                peer: "lap".into(),
                x: 2560,
                y: 0
            }],
            base_rev: None
        })
    );
    let with_base = parse(&format!(
        r#"{{"id":4,"method":"layout.set","params":{{"origins":[],"base_rev":{{"counter":14,"peer":"{}"}}}}}}"#,
        desk()
    ))
    .unwrap();
    let Request::LayoutSet(p) = with_base.request else {
        panic!()
    };
    assert_eq!(p.base_rev.unwrap().counter, 14);
    assert!(
        p.origins.is_empty(),
        "an empty list parses; ferryd refuses it"
    );
    for bad in [
        r#"{"id":5,"method":"layout.set"}"#,
        r#"{"id":5,"method":"layout.set","params":{}}"#,
        r#"{"id":5,"method":"layout.set","params":{"origins":[{"peer":"lap","x":1}]}}"#,
        r#"{"id":5,"method":"layout.set","params":{"origins":[{"peer":"lap","x":1.5,"y":0}]}}"#,
        r#"{"id":5,"method":"layout.set","params":{"origins":[{"peer":"lap","x":2147483648,"y":0}]}}"#,
        r#"{"id":5,"method":"layout.set","params":{"origins":[{"x":1,"y":0}]}}"#,
        r#"{"id":5,"method":"layout.set","params":{"origins":[],"base_rev":{"counter":-1,"peer":"x"}}}"#,
    ] {
        let e = parse(bad).expect_err(bad);
        assert_eq!(
            (e.id, e.error.code),
            (Some(5), ErrorCode::BadParams),
            "{bad}"
        );
    }
    assert!(Request::METHODS.contains(&"layout.set"));
}

/// A newer client reads what an older ferryd answers: the fields M3 added take their defaults,
/// and a peer is trusted unless the daemon says otherwise.
#[test]
fn a_layout_from_an_older_ferryd_still_reads() {
    let old = r#"{"revision":{"counter":1,"peer":"ab"},"peers":[
        {"name":"desk","fingerprint":"ab","origin":[0,0],"online":true,
         "monitors":[{"name":"HDMI-A-1","x":0,"y":0,"w":1920,"h":1080,"scale":1.0}]}],"links":[]}"#;
    let l: LayoutResult = serde_json::from_str(old).unwrap();
    let p = &l.peers[0];
    assert!(p.trusted && !p.is_self && p.bounds.is_none() && p.shift.is_none());
    assert_eq!(
        (p.monitors[0].description.as_str(), p.monitors[0].transform),
        ("", 0)
    );
}

#[test]
fn request_parse_tells_unknown_method_from_bad_params() {
    let err = |line: &str| RequestLine::parse(line).expect_err(line);
    let e = err(r#"{"id":3,"method":"layout.rotate","params":{}}"#);
    assert_eq!((e.id, e.error.code), (Some(3), ErrorCode::UnknownMethod));
    let e = err(r#"{"id":4,"method":"trust","params":{"name":"x"}}"#);
    assert_eq!((e.id, e.error.code), (Some(4), ErrorCode::BadParams));
    let e = err(
        r#"{"id":5,"method":"layout.place","params":{"peer":"p","rel":"behind","anchor":"a"}}"#,
    );
    assert_eq!((e.id, e.error.code), (Some(5), ErrorCode::BadParams));
    let e = err(r#"{"id":6}"#);
    assert_eq!((e.id, e.error.code), (Some(6), ErrorCode::BadParams));
    let e = err(r#"{"method":"status"}"#);
    assert_eq!((e.id, e.error.code), (None, ErrorCode::BadParams));
    let e = err("not json");
    assert_eq!((e.id, e.error.code), (None, ErrorCode::BadParams));
    let e = err("[1,2]");
    assert_eq!((e.id, e.error.code), (None, ErrorCode::BadParams));
    // Unknown params are ignored, including params on a method with none.
    assert_eq!(
        RequestLine::parse(r#"{"id":1,"method":"status","params":{"verbose":true}}"#),
        Ok(RequestLine {
            id: 1,
            request: Request::Status
        })
    );
    assert_eq!(
        RequestLine::parse(r#"{"id":2,"method":"trust","params":{"fingerprint":"ab","future":1}}"#),
        Ok(RequestLine {
            id: 2,
            request: Request::Trust(TrustParams {
                fingerprint: "ab".into(),
                name: None,
                addr: None
            })
        })
    );
}

fn arb_topic() -> impl Strategy<Value = Topic> {
    proptest::sample::select(Topic::ALL.to_vec())
}

fn arb_request() -> impl Strategy<Value = Request> {
    let text = proptest::collection::vec(any::<char>(), 0..=16)
        .prop_map(|v| v.into_iter().collect::<String>());
    prop_oneof![
        Just(Request::Status),
        Just(Request::Release),
        prop_oneof![
            Just(GoSide::Left),
            Just(GoSide::Right),
            Just(GoSide::Up),
            Just(GoSide::Down)
        ]
        .prop_map(|side| Request::Go(GoParams { side })),
        Just(Request::Id),
        Just(Request::LayoutGet),
        Just(Request::PairAccept),
        Just(Request::PairCancel),
        Just(Request::Peers),
        Just(Request::Doctor),
        Just(Request::Unsubscribe),
        Just(Request::ConfigSchema),
        proptest::option::of(proptest::collection::vec(arb_topic(), 0..4))
            .prop_map(|topics| Request::Subscribe(SubscribeParams { topics })),
        proptest::option::of(text.clone())
            .prop_map(|key| Request::ConfigGet(ConfigGetParams { key })),
        (text.clone(), arb_json())
            .prop_map(|(key, value)| Request::ConfigSet(ConfigSetParams { key, value })),
        (text.clone(), text.clone())
            .prop_map(|(peer, name)| Request::PeerRename(PeerRenameParams { peer, name })),
        proptest::option::of(any::<bool>()).prop_map(|on| Request::Lock(LockParams { on })),
        (text.clone(), proptest::option::of(text.clone()))
            .prop_map(|(addr, name)| Request::PairStart(PairStartParams { addr, name })),
        any::<bool>().prop_map(|matches| Request::PairAnswer(PairAnswerParams { matches })),
        text.clone()
            .prop_map(|peer| Request::Unpair(UnpairParams { peer })),
        (
            proptest::collection::vec(
                (text.clone(), any::<i32>(), any::<i32>()).prop_map(|(peer, x, y)| OriginParam {
                    peer,
                    x,
                    y
                }),
                0..4
            ),
            proptest::option::of((any::<u64>(), text))
                .prop_map(|r| r.map(|(counter, peer)| RevisionView { counter, peer })),
        )
            .prop_map(|(origins, base_rev)| Request::LayoutSet(SetParams { origins, base_rev })),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// A request line serialized by the CLI parses back to the same request and id.
    #[test]
    fn a_request_line_parses_to_what_was_serialized(id in any::<u64>(), request in arb_request()) {
        let line = RequestLine { id, request };
        prop_assert_eq!(RequestLine::parse(&json(&line)).ok(), Some(line));
    }

    /// Any text a local socket client sends gets `Ok` or a `RequestError`, never a panic.
    #[test]
    fn request_line_parse_never_panics(raw in "\\PC{0,200}") {
        let _ = RequestLine::parse(&raw);
    }

    /// Valid envelopes with a known or unknown method and arbitrary JSON params: every outcome
    /// is `Ok` or an error that echoes the id.
    #[test]
    fn request_line_with_any_params_is_ok_or_an_error_with_the_id(
        id in any::<u64>(),
        method in prop_oneof![
            proptest::sample::select(Request::METHODS.to_vec()).prop_map(str::to_owned),
            "\\PC{0,12}",
        ],
        params in arb_json(),
    ) {
        let line = serde_json::json!({ "id": id, "method": method, "params": params }).to_string();
        match RequestLine::parse(&line) {
            Ok(parsed) => prop_assert_eq!(parsed.id, id),
            Err(e) => prop_assert_eq!(e.id, Some(id)),
        }
    }
}

fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    use serde_json::Value;
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        "\\PC{0,8}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::vec(("[a-z]{1,6}", inner), 0..4)
                .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

#[test]
fn layout_toml() {
    let layout = layout_with_netbook(Rel::Below);
    let known = [desk_monitors(), netbook_monitors()];
    let file = LayoutFile::new(&layout, &known);
    let text = toml::to_string(&file).expect("toml");
    insta::assert_snapshot!(text);
    let back: LayoutFile = toml::from_str(&text).expect("parse");
    assert_eq!(back, file);
    assert_eq!(back.layout(netbook()), Ok(layout));
    assert_eq!(back.known(), Ok(known.to_vec()));
    // An empty file is the empty layout at (0, me).
    let empty: LayoutFile = toml::from_str("").expect("empty");
    assert_eq!(empty.layout(desk()), Ok(crate::geom::Layout::empty(desk())));
}

/// M3 design 4.1: the monitor memory is a table of `layout.toml`, and a peer that never moved
/// leaves none.
#[test]
fn the_monitor_memory_is_a_table_of_the_layout_file() {
    use crate::anchor::Anchors;
    use crate::testkit::mon;

    let layout = layout_with_netbook(Rel::Below);
    let known = [desk_monitors(), netbook_monitors()];
    let plain = LayoutFile::new(&layout, &known);
    assert!(plain.anchor.is_none());
    assert_eq!(plain.clone().with_anchors(&Anchors::default()), plain);
    assert!(!toml::to_string(&plain).expect("toml").contains("anchor"));

    // HDMI-A-1 unplugged and DVI-D-1 moved to 0: shift (1920, 0), HDMI-A-1 remembered as absent
    let mut anchors = Anchors::default();
    anchors.observe(&desk_monitors().monitors);
    anchors.observe(&[mon("DVI-D-1", 0, 0, 1920, 1080)]);
    let file = plain.with_anchors(&anchors);
    let text = toml::to_string(&file).expect("toml");
    // the other tables are the `layout_toml` snapshot's
    insta::assert_snapshot!(&text[text.find("[anchor]").expect("the table")..]);
    let back: LayoutFile = toml::from_str(&text).expect("parse");
    assert_eq!(back, file);
    assert_eq!(back.anchor.expect("the table").anchors(), Ok(anchors));
    // a table a hand broke is refused, not believed
    let broken: LayoutFile =
        toml::from_str("[anchor]\nx = 99999999\ny = 0\n").expect("the shape parses");
    assert!(broken.anchor.expect("the table").anchors().is_err());
}

/// `unpair` answers with the peer it removed, in the shape `trust` uses (RG-CFG-04: a type for
/// a result the daemon already sent).
#[test]
fn the_unpair_result_is_the_peer_that_went() {
    let v: UnpairResult =
        serde_json::from_str(r#"{"peer":{"name":"laptop","fingerprint":"ab"}}"#).unwrap();
    assert_eq!(v.peer.name, "laptop");
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        r#"{"peer":{"name":"laptop","fingerprint":"ab"}}"#
    );
    assert!(serde_json::from_str::<UnpairResult>(r#"{"peer":{"label":"x"}}"#).is_err());
}

// --- events, subscriptions and config (m3 design §2.2–2.7) ------------------------------------

fn sample_status() -> StatusResult {
    let mut counters = BTreeMap::new();
    counters.insert("crossings".to_string(), 3);
    StatusResult {
        api: API_VERSION,
        me: me(),
        port: 4243,
        role: Role::Idle,
        active_peer: None,
        capture: CaptureStatus {
            backend: "portal".into(),
            gate: crate::source::Gate::Armed.as_str().into(),
            barriers: 1,
            failed_barriers: vec![],
            off_reason: None,
            notice: None,
        },
        screen_locked: Some(false),
        locked: false,
        peers: vec![PeerStatus {
            name: "laptop".into(),
            fingerprint: netbook().to_string(),
            state: LinkState::Connected,
            addr: Some("192.0.2.11:4243".into()),
            rtt_us: Some(850),
            proto: Some("1.0".into()),
            detail: None,
            suspect: false,
            locked: false,
            last_seen: None,
        }],
        last_release: Some(LastRelease {
            reason: "home".into(),
            ago_ms: 4000,
            mono_us: Some(77_000_000),
        }),
        counters,
        pairing: Some(PairingStatus {
            role: PairingRole::Acceptor,
            stage: PairingStage::Listening,
            expires_ms: 61_000,
        }),
        summary: Some(Summary {
            state: SummaryState::Ready,
            peer: None,
        }),
        attention: vec![],
        escape_chord: Some("Super+Ctrl+Escape".into()),
    }
}

/// The requests of the event API round-trip through the two-step parser, and their shapes are
/// the wire.
#[test]
fn event_api_requests() {
    let mut s = String::new();
    for r in [
        Request::Subscribe(SubscribeParams { topics: None }),
        Request::Subscribe(SubscribeParams {
            topics: Some(vec![Topic::State, Topic::Layout]),
        }),
        Request::Unsubscribe,
        Request::ConfigSchema,
        Request::ConfigGet(ConfigGetParams { key: None }),
        Request::ConfigGet(ConfigGetParams {
            key: Some("peer.name".into()),
        }),
        Request::ConfigSet(ConfigSetParams {
            key: "crossing.edge_resistance_px".into(),
            value: serde_json::json!(40),
        }),
        Request::ConfigSet(ConfigSetParams {
            key: "crossing.escape_chord".into(),
            value: serde_json::json!("Super+Alt+F12"),
        }),
        Request::PeerRename(PeerRenameParams {
            peer: "laptop".into(),
            name: "netbook".into(),
        }),
    ] {
        let line = json(&RequestLine {
            id: 7,
            request: r.clone(),
        });
        assert_eq!(
            RequestLine::parse(&line),
            Ok(RequestLine { id: 7, request: r })
        );
        s.push_str(&line);
        s.push('\n');
    }
    insta::assert_snapshot!(s);
}

/// The results, events and errors of the event API: the JSON a frontend reads.
#[test]
fn event_api_results_and_events() {
    let mut s = String::new();
    let mut line = |label: &str, v: String| s.push_str(&format!("{label}\n{v}\n"));

    line(
        "subscribe",
        ok(
            1,
            SubscribeResult {
                topics: vec![Topic::State, Topic::Layout],
            },
        ),
    );
    line("unsubscribe", ok(2, EmptyResult {}));

    let schema = ConfigSchema {
        keys: vec![
            ConfigKey {
                key: "crossing.edge_resistance_px".into(),
                kind: ConfigType::Int,
                default: serde_json::json!(0),
                min: Some(0),
                max: Some(200),
                unit: Some("px".into()),
                slider_max: None,
                choices: None,
                applies: Applies::Live,
                doc: "Push this far into an edge before crossing".into(),
                available: true,
            },
            ConfigKey {
                key: "crossing.escape_chord".into(),
                kind: ConfigType::Chord,
                default: serde_json::json!("Super+Ctrl+Escape"),
                min: None,
                max: None,
                unit: None,
                slider_max: None,
                choices: None,
                applies: Applies::Setup,
                doc: "Keys that always give input back".into(),
                available: true,
            },
            ConfigKey {
                key: "input.capture".into(),
                kind: ConfigType::Enum,
                default: serde_json::json!("auto"),
                min: None,
                max: None,
                unit: None,
                slider_max: None,
                choices: Some(vec!["auto".into(), "portal".into(), "off".into()]),
                applies: Applies::Restart,
                doc: "How input is captured".into(),
                available: true,
            },
            // a key nothing reads yet: the one field that is left out when it is true
            ConfigKey {
                key: "clipboard.enabled".into(),
                kind: ConfigType::Bool,
                default: serde_json::json!(true),
                min: None,
                max: None,
                unit: None,
                slider_max: None,
                choices: None,
                applies: Applies::Restart,
                doc: "Share the clipboard between the machines".into(),
                available: false,
            },
        ],
    };
    line("config.schema", ok(3, schema));

    let mut values = BTreeMap::new();
    values.insert(
        "crossing.edge_resistance_px".to_string(),
        serde_json::json!(40),
    );
    values.insert("peer.name".to_string(), serde_json::json!("desk"));
    line(
        "config.get",
        ok(
            4,
            ConfigValues {
                values: values.clone(),
                pending: vec![],
            },
        ),
    );
    line(
        "config (with a key waiting for a restart)",
        json(&EventLine::Config(ConfigValues {
            values,
            pending: vec!["peer.name".into()],
        })),
    );
    line(
        "config.set",
        ok(
            5,
            ConfigSetResult {
                key: "crossing.edge_resistance_px".into(),
                value: serde_json::json!(40),
                applies: Applies::Live,
            },
        ),
    );
    line("peer.rename", ok(6, RenameResult { peer: lap() }));

    line("event", json(&EventLine::State(Box::new(sample_status()))));
    line(
        "event",
        json(&EventLine::Peers(PeersResult {
            peers: vec![PeerEntry {
                name: "laptop".into(),
                fingerprint: netbook().to_string(),
                addr: None,
                last_seen: None,
                paired: Paired::Sas,
                state: LinkState::Offline,
                locked: false,
            }],
        })),
    );
    // An empty layout keeps this line independent of the per-peer fields the layout stream adds.
    line(
        "event",
        json(&EventLine::Layout(LayoutResult {
            revision: RevisionView {
                counter: 0,
                peer: desk().to_string(),
            },
            peers: vec![],
            links: vec![],
        })),
    );
    line(
        "event",
        json(&EventLine::Notice(Notice {
            kind: NoticeKind::PeerBack,
            title: "laptop is back".into(),
            body: String::new(),
            peer: Some("laptop".into()),
            urgency: Urgency::Normal,
        })),
    );
    line(
        "event",
        json(&EventLine::Ui(UiEvent { open: UiView::Pair })),
    );

    for code in [
        ErrorCode::UnknownKey,
        ErrorCode::BadValue,
        ErrorCode::ReadOnly,
    ] {
        line(
            "error",
            json(&ResponseLine::<()> {
                id: 9,
                body: ResponseBody::Error(ApiError {
                    code,
                    message: "…".into(),
                    hint: Some("…".into()),
                }),
            }),
        );
    }
    insta::assert_snapshot!(s);
}

/// What the hello advertises is what the build has, so a frontend can feature-detect.
#[test]
fn hello_advertises_what_the_build_has() {
    let h = HelloData::new("0.1.0", "desk", desk().to_string());
    assert_eq!(h.api, API_VERSION);
    assert_eq!(h.methods, Request::METHODS);
    assert_eq!(
        h.topics,
        [
            "state", "peers", "layout", "pairing", "config", "notice", "ui", "edge"
        ]
    );
    // every advertised topic subscribes, every advertised method is known to the parser
    for t in &h.topics {
        assert!(t.parse::<Topic>().is_ok(), "{t}");
    }
    for m in &h.methods {
        let line = format!(r#"{{"id":1,"method":"{m}","params":{{}}}}"#);
        if let Err(e) = RequestLine::parse(&line) {
            assert_ne!(e.error.code, ErrorCode::UnknownMethod, "{m}");
        }
    }
    // a hello of an older ferryd has neither list
    let old: HelloData =
        serde_json::from_str(r#"{"api":1,"version":"0.1.0","peer":"desk","fingerprint":"ab"}"#)
            .unwrap();
    assert!(old.methods.is_empty() && old.topics.is_empty());
}

/// `METHODS` is the list of the variants: a variant added without a name here does not compile.
#[test]
fn every_request_variant_is_in_the_method_list() {
    fn name(r: &Request) -> &'static str {
        match r {
            Request::Status => "status",
            Request::Release => "release",
            Request::Go(_) => "go",
            Request::Id => "id",
            Request::Trust(_) => "trust",
            Request::LayoutGet => "layout.get",
            Request::LayoutPlace(_) => "layout.place",
            Request::LayoutSet(_) => "layout.set",
            Request::Lock(_) => "lock",
            Request::PairAccept => "pair.accept",
            Request::PairStart(_) => "pair.start",
            Request::PairAnswer(_) => "pair.answer",
            Request::PairCancel => "pair.cancel",
            Request::Peers => "peers",
            Request::Unpair(_) => "unpair",
            Request::Doctor => "doctor",
            Request::Subscribe(_) => "subscribe",
            Request::Unsubscribe => "unsubscribe",
            Request::ConfigSchema => "config.schema",
            Request::ConfigGet(_) => "config.get",
            Request::ConfigSet(_) => "config.set",
            Request::PeerRename(_) => "peer.rename",
        }
    }
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(256));
    runner
        .run(&arb_request(), |r| {
            let v = serde_json::to_value(&r).unwrap();
            prop_assert_eq!(v["method"].as_str(), Some(name(&r)));
            prop_assert!(Request::METHODS.contains(&name(&r)));
            Ok(())
        })
        .unwrap();
    // and no name is listed twice
    let mut sorted = Request::METHODS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), Request::METHODS.len());
}

/// `go` names the arrow keys; `top` and `bottom` read as `up` and `down`.
#[test]
fn go_takes_a_side() {
    for (text, side) in [
        ("left", GoSide::Left),
        ("right", GoSide::Right),
        ("up", GoSide::Up),
        ("top", GoSide::Up),
        ("down", GoSide::Down),
        ("bottom", GoSide::Down),
    ] {
        let line = format!(r#"{{"id":1,"method":"go","params":{{"side":"{text}"}}}}"#);
        assert_eq!(
            RequestLine::parse(&line).unwrap().request,
            Request::Go(GoParams { side })
        );
    }
    for bad in [r#"{"side":"north"}"#, r#"{}"#, r#"{"side":"left","x":1}"#] {
        let line = format!(r#"{{"id":1,"method":"go","params":{bad}}}"#);
        assert_eq!(
            RequestLine::parse(&line).unwrap_err().error.code,
            ErrorCode::BadParams
        );
    }
}

#[test]
fn subscribe_and_config_get_take_no_params_at_all() {
    for line in [
        r#"{"id":1,"method":"subscribe"}"#,
        r#"{"id":1,"method":"subscribe","params":null}"#,
        r#"{"id":1,"method":"subscribe","params":{}}"#,
        r#"{"id":1,"method":"subscribe","params":{"future":1}}"#,
    ] {
        assert_eq!(
            RequestLine::parse(line).map(|l| l.request),
            Ok(Request::Subscribe(SubscribeParams { topics: None })),
            "{line}"
        );
    }
    for line in [
        r#"{"id":1,"method":"config.get"}"#,
        r#"{"id":1,"method":"config.get","params":null}"#,
    ] {
        assert_eq!(
            RequestLine::parse(line).map(|l| l.request),
            Ok(Request::ConfigGet(ConfigGetParams { key: None })),
            "{line}"
        );
    }
    // the methods that take params still need them
    let e = RequestLine::parse(r#"{"id":2,"method":"config.set"}"#).unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(2), ErrorCode::BadParams));
    let e =
        RequestLine::parse(r#"{"id":3,"method":"peer.rename","params":{"peer":"a"}}"#).unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(3), ErrorCode::BadParams));
    // and the unit ones ignore any
    assert_eq!(
        RequestLine::parse(r#"{"id":4,"method":"unsubscribe","params":{"x":1}}"#)
            .map(|l| l.request),
        Ok(Request::Unsubscribe)
    );
    assert_eq!(
        RequestLine::parse(r#"{"id":5,"method":"config.schema","params":[1]}"#).map(|l| l.request),
        Ok(Request::ConfigSchema)
    );
}

#[test]
fn an_unknown_topic_is_bad_params_and_the_message_lists_the_topics() {
    let e =
        RequestLine::parse(r#"{"id":1,"method":"subscribe","params":{"topics":["state","fax"]}}"#)
            .unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(1), ErrorCode::BadParams));
    for topic in Topic::ALL {
        assert!(
            e.error.message.contains(topic.as_str()),
            "{}",
            e.error.message
        );
    }
    assert_eq!(
        "fax".parse::<Topic>().unwrap_err().to_string(),
        "unknown topic \"fax\"; the topics are state, peers, layout, pairing, config, notice, ui, edge"
    );
    for t in Topic::ALL {
        assert_eq!(t.as_str().parse::<Topic>(), Ok(t));
        assert_eq!(t.to_string(), t.as_str());
        assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
    }
    // the snapshot topics are exactly the ones whose events replace one another
    let snapshots: Vec<_> = Topic::ALL.into_iter().filter(|t| t.is_snapshot()).collect();
    assert_eq!(
        snapshots,
        [Topic::State, Topic::Peers, Topic::Layout, Topic::Config]
    );
}

/// A client built before a variant existed reads the one it does not know as
/// `Unknown` instead of failing on the whole line.
#[test]
fn what_a_newer_ferryd_adds_reads_as_unknown() {
    let ev: EventLine = serde_json::from_str(r#"{"event":"future","data":{"x":[1,2]}}"#).unwrap();
    assert_eq!(ev, EventLine::Unknown);
    let ev: EventLine = serde_json::from_str(r#"{"event":"hello","data":{"api":1,"version":"9","peer":"p","fingerprint":"f","topics":["state","future"],"extra":true}}"#).unwrap();
    assert!(matches!(ev, EventLine::Hello(h) if h.topics == ["state", "future"]));
    let e: ApiError = serde_json::from_str(r#"{"code":"future_code","message":"m"}"#).unwrap();
    assert_eq!(e.code, ErrorCode::Unknown);
    let v: Summary = serde_json::from_str(r#"{"state":"future"}"#).unwrap();
    assert_eq!(v.state, SummaryState::Unknown);
    let a: Attention = serde_json::from_str(r#"{"code":"future","message":"m"}"#).unwrap();
    assert_eq!(a.code, AttentionCode::Unknown);
    let k: ConfigKey = serde_json::from_str(
        r#"{"key":"k","type":"future","default":1,"applies":"future","doc":""}"#,
    )
    .unwrap();
    assert_eq!((k.kind, k.applies), (ConfigType::Unknown, Applies::Unknown));
    // a ferryd from before `available` sends no such field, and its keys are all in use
    assert!(k.available);
    let k: ConfigKey = serde_json::from_str(
        r#"{"key":"k","type":"bool","default":true,"applies":"restart","doc":"","available":false}"#,
    )
    .unwrap();
    assert!(!k.available);
    let n: Notice =
        serde_json::from_str(r#"{"kind":"future","title":"t","urgency":"future"}"#).unwrap();
    assert_eq!((n.kind, n.urgency), (NoticeKind::Unknown, Urgency::Unknown));
    assert_eq!(n.body, "");
    let u: UiEvent = serde_json::from_str(r#"{"open":"future"}"#).unwrap();
    assert_eq!(u.open, UiView::Unknown);
}

/// An old ferryd's `status` has none of the M3 fields; a new client still reads it.
#[test]
fn a_status_of_an_older_ferryd_still_parses() {
    let mut v = serde_json::to_value(sample_status()).unwrap();
    let o = v.as_object_mut().unwrap();
    for k in ["summary", "attention", "escape_chord"] {
        o.remove(k);
    }
    let s: StatusResult = serde_json::from_value(v).unwrap();
    assert_eq!(
        (s.summary, s.attention, s.escape_chord),
        (None, vec![], None)
    );
    // and a pairing event without a role, and without a `cancelled_by`
    let ev: PairingEvent =
        serde_json::from_str(r#"{"stage":"code","code":"418207","expires_ms":5}"#).unwrap();
    assert_eq!((ev.role, ev.cancelled_by), (None, None));
}

/// A newer ferryd may say a third thing about a cancel. The event still reads, and a
/// frontend falls back to the plain "cancelled".
#[test]
fn a_cancel_of_an_origin_this_build_does_not_know_still_reads() {
    let ev: PairingEvent = serde_json::from_str(
        r#"{"stage":"failed","expires_ms":5,"reason":"cancelled","cancelled_by":"moon"}"#,
    )
    .unwrap();
    assert_eq!(ev.cancelled_by, Some(CancelledBy::Unknown));
    assert_eq!(ev.reason.as_deref(), Some("cancelled"));
}

#[test]
fn a_status_always_lists_its_attention_even_when_empty() {
    let v = serde_json::to_value(sample_status()).unwrap();
    assert_eq!(v["attention"], serde_json::json!([]));
    assert_eq!(v["summary"], serde_json::json!({"state": "ready"}));
}

/// The noise rule of m3 design §2.2: counters, the clock of the last release, a peer's round
/// trip and the pairing countdown do not count as news; everything else does.
#[test]
fn without_noise_hides_only_the_noise() {
    let base = sample_status();
    let quiet = base.without_noise();
    let mut noisy = base.clone();
    noisy.counters.insert("crossings".into(), 4);
    noisy.counters.insert("a_new_counter".into(), 1);
    noisy.last_release.as_mut().unwrap().ago_ms = 9000;
    noisy.last_release.as_mut().unwrap().mono_us = Some(80_000_000);
    noisy.peers[0].rtt_us = Some(2100);
    noisy.pairing.as_mut().unwrap().expires_ms = 12;
    assert_ne!(noisy, base);
    assert_eq!(noisy.without_noise(), quiet);

    // each of these is news
    type Change = Box<dyn Fn(&mut StatusResult)>;
    let news: Vec<Change> = vec![
        Box::new(|s| s.role = Role::Source),
        Box::new(|s| s.locked = true),
        Box::new(|s| s.screen_locked = None),
        Box::new(|s| s.peers[0].state = LinkState::Offline),
        Box::new(|s| s.peers[0].locked = true),
        Box::new(|s| s.peers[0].suspect = true),
        Box::new(|s| s.last_release.as_mut().unwrap().reason = "silence".into()),
        Box::new(|s| s.last_release = None),
        Box::new(|s| s.pairing.as_mut().unwrap().stage = PairingStage::Code),
        Box::new(|s| s.pairing = None),
        Box::new(|s| s.capture.gate = "locked".into()),
        Box::new(|s| s.summary = None),
        Box::new(|s| s.escape_chord = None),
        Box::new(|s| {
            s.attention.push(Attention {
                code: AttentionCode::ConfigInvalid,
                message: "m".into(),
                hint: None,
                peer: None,
            });
        }),
    ];
    for (i, change) in news.iter().enumerate() {
        let mut changed = base.clone();
        change(&mut changed);
        assert_ne!(changed.without_noise(), quiet, "change {i} is news");
    }
}

fn facts<'a>(
    role: &'a Role,
    peer: Option<&'a str>,
    locked: bool,
    connected: bool,
    attention: bool,
) -> SummaryFacts<'a> {
    SummaryFacts {
        role,
        active_peer: peer,
        locked,
        connected,
        attention,
    }
}

/// The priority rule: attention, away, controlled, locked, ready, idle.
#[test]
fn the_summary_follows_the_priority_rule() {
    use SummaryState::*;
    let state = |f: SummaryFacts<'_>| Summary::derive(f).state;
    // each state alone
    assert_eq!(state(facts(&Role::Idle, None, false, false, false)), Idle);
    assert_eq!(state(facts(&Role::Idle, None, false, true, false)), Ready);
    assert_eq!(state(facts(&Role::Idle, None, true, false, false)), Locked);
    assert_eq!(
        state(facts(&Role::Source, Some("lap"), false, true, false)),
        Away
    );
    assert_eq!(
        state(facts(&Role::Target, Some("lap"), false, true, false)),
        Controlled
    );
    assert_eq!(
        state(facts(&Role::Idle, None, false, true, true)),
        Attention
    );
    // each beats the ones below it
    assert_eq!(
        state(facts(&Role::Source, Some("lap"), true, true, true)),
        Attention
    );
    assert_eq!(
        state(facts(&Role::Source, Some("lap"), true, true, false)),
        Away
    );
    assert_eq!(
        state(facts(&Role::Target, Some("lap"), true, true, false)),
        Controlled
    );
    assert_eq!(state(facts(&Role::Idle, None, true, true, false)), Locked);
    // the moments before a crossing read as ready, so the widget does not flicker
    assert_eq!(
        state(facts(&Role::Pending, Some("lap"), false, true, false)),
        Ready
    );
    assert_eq!(
        state(facts(&Role::Arming, Some("lap"), false, true, false)),
        Ready
    );
    // the peer's name goes with away and controlled only
    assert_eq!(
        Summary::derive(facts(&Role::Source, Some("lap"), false, true, false))
            .peer
            .as_deref(),
        Some("lap")
    );
    assert_eq!(
        Summary::derive(facts(&Role::Target, Some("lap"), false, true, false))
            .peer
            .as_deref(),
        Some("lap")
    );
    assert_eq!(
        Summary::derive(facts(&Role::Idle, Some("lap"), false, true, false)).peer,
        None
    );
    assert_eq!(
        Summary::derive(facts(&Role::Source, Some("lap"), false, true, true)).peer,
        None
    );
}

proptest! {
    /// Any facts give exactly one state, and it is the first of the priority list that holds.
    #[test]
    fn the_summary_is_the_first_state_that_holds(
        role in proptest::sample::select(vec![Role::Idle, Role::Pending, Role::Arming, Role::Source, Role::Target]),
        locked in any::<bool>(),
        connected in any::<bool>(),
        attention in any::<bool>(),
    ) {
        use SummaryState::*;
        let got = Summary::derive(facts(&role, Some("p"), locked, connected, attention)).state;
        let expect = [
            (attention, Attention),
            (role == Role::Source, Away),
            (role == Role::Target, Controlled),
            (locked, Locked),
            (connected, Ready),
        ]
        .into_iter()
        .find_map(|(holds, state)| holds.then_some(state))
        .unwrap_or(Idle);
        prop_assert_eq!(got, expect);
    }
}

/// Which methods may skip the check of the subscriptions afterwards.
#[test]
fn only_reads_are_read_only() {
    let read = [
        Request::Status,
        Request::Id,
        Request::LayoutGet,
        Request::Peers,
        Request::Doctor,
        Request::Subscribe(SubscribeParams::default()),
        Request::Unsubscribe,
        Request::ConfigSchema,
        Request::ConfigGet(ConfigGetParams::default()),
    ];
    assert!(read.iter().all(Request::is_read_only));
    let writes = [
        Request::Release,
        Request::Lock(LockParams::default()),
        Request::PairAccept,
        Request::PairCancel,
        Request::PairAnswer(PairAnswerParams { matches: true }),
        Request::ConfigSet(ConfigSetParams {
            key: "k".into(),
            value: serde_json::Value::Null,
        }),
        Request::PeerRename(PeerRenameParams {
            peer: "a".into(),
            name: "b".into(),
        }),
        Request::Unpair(UnpairParams { peer: "a".into() }),
    ];
    assert!(writes.iter().all(|r| !r.is_read_only()));
}

/// `EventLine` reads by event name (a hand-written step, see `RawEvent`), so each variant must
/// come back as itself, under the name of its topic.
#[test]
fn every_event_round_trips_under_the_name_of_its_topic() {
    let events = [
        (
            None,
            EventLine::Hello(HelloData::new("0.1.0", "desk", desk().to_string())),
        ),
        (
            Some(Topic::Pairing),
            EventLine::Pairing(PairingEvent {
                stage: PairingStage::Listening,
                role: Some(PairingRole::Acceptor),
                peer: None,
                code: None,
                expires_ms: 120_000,
                reason: None,
                message: None,
                cancelled_by: None,
            }),
        ),
        (
            Some(Topic::State),
            EventLine::State(Box::new(sample_status())),
        ),
        (
            Some(Topic::Peers),
            EventLine::Peers(PeersResult { peers: vec![] }),
        ),
        (
            Some(Topic::Layout),
            EventLine::Layout(LayoutResult {
                revision: RevisionView {
                    counter: 2,
                    peer: desk().to_string(),
                },
                peers: vec![],
                links: vec![],
            }),
        ),
        (
            Some(Topic::Config),
            EventLine::Config(ConfigValues {
                values: BTreeMap::new(),
                pending: vec!["peer.port".into()],
            }),
        ),
        (
            Some(Topic::Notice),
            EventLine::Notice(Notice {
                kind: NoticeKind::PairCode,
                title: "Pairing code".into(),
                body: "418 207".into(),
                peer: None,
                urgency: Urgency::Critical,
            }),
        ),
        (
            Some(Topic::Ui),
            EventLine::Ui(UiEvent {
                open: UiView::Settings,
            }),
        ),
        (
            Some(Topic::Edge),
            EventLine::Edge(EdgeEvent {
                role: EdgeRole::Target,
                home: false,
                peer: "netbook".into(),
                monitor: "HDMI-A-1".into(),
                side: crate::geom::Side::Bottom,
                start: 285,
                end: 1635,
            }),
        ),
    ];
    let mut topics = Vec::new();
    for (topic, event) in events {
        let text = json(&event);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if let Some(t) = topic {
            assert_eq!(v["event"], t.as_str(), "{text}");
            topics.push(t);
        }
        assert_eq!(
            serde_json::from_str::<EventLine>(&text).unwrap(),
            event,
            "{text}"
        );
    }
    // every topic has an event line, and the hello has none
    topics.sort();
    assert_eq!(topics, Topic::ALL);
    // a line that is not an event at all is an error, not `Unknown`
    assert!(serde_json::from_str::<EventLine>(r#"{"id":1,"result":{}}"#).is_err());
    // known name, wrong data: an error too
    assert!(serde_json::from_str::<EventLine>(r#"{"event":"ui","data":{"open":5}}"#).is_err());
}

#[test]
fn a_view_name_parses_and_names_the_views_it_knows() {
    for v in [
        UiView::Pair,
        UiView::Arrange,
        UiView::Popover,
        UiView::Settings,
    ] {
        assert_eq!(v.as_str().parse::<UiView>(), Ok(v));
        assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
    }
    let e = "dashboard".parse::<UiView>().unwrap_err();
    assert!(e.contains("pair, arrange, popover, settings"), "{e}");
}

/// The body of a `pair_code` notice is the code, so `{:?}` of a notice must not show
/// it, whoever logs it.
#[test]
fn a_notice_does_not_debug_print_what_it_says() {
    let n = Notice {
        kind: NoticeKind::PairCode,
        title: "Pairing code for lap".into(),
        body: "418 207".into(),
        peer: Some("lap".into()),
        urgency: Urgency::Critical,
    };
    let shown = format!("{n:?} {:?}", EventLine::Notice(n.clone()));
    assert!(
        !shown.contains("418") && !shown.contains("Pairing code"),
        "{shown}"
    );
    assert!(
        shown.contains("PairCode") && shown.contains("lap"),
        "{shown}"
    );
    // the wire form carries it, of course
    assert!(serde_json::to_string(&n).unwrap().contains("418 207"));
}
