//! Geometry tests (`geom::*`).

use proptest::prelude::*;

use super::*;
use crate::testkit::*;

fn desk_at_origin() -> Layout {
    let mut layout = Layout::empty(desk());
    layout.set_origin(Origin {
        peer: desk(),
        x: 0,
        y: 0,
    });
    layout
}

#[test]
fn first_light() {
    let layout = layout_with_netbook(Rel::Below);
    assert_eq!(layout.origin(netbook()), Some((277, 1080)));
    let topo = crate::testkit::first_light();
    assert_eq!(
        topo.links,
        vec![
            EdgeLink {
                from: hdmi(),
                side: Side::Bottom,
                to: edp(),
                line: 1080,
                span: (277, 1643),
            },
            EdgeLink {
                from: edp(),
                side: Side::Top,
                to: hdmi(),
                line: 1080,
                span: (277, 1643),
            },
        ]
    );
    let b = barriers(&topo, &[netbook()]);
    assert_eq!(
        b,
        vec![Barrier {
            id: 1,
            monitor: hdmi(),
            side: Side::Bottom,
            pos: (0, 1080, 1919, 1080),
        }]
    );
}

/// The netbook as source, from the layout the desktop
/// pushed, in the netbook's own local coordinates.
#[test]
fn first_light_reverse() {
    let layout = layout_with_netbook(Rel::Below);
    let topo = topology(
        netbook(),
        &layout,
        &[desk_monitors(), netbook_monitors()],
        &[desk()],
    );
    // Link eDP-1 Top → HDMI-A-1 over the whole top edge: plane [277, 1643)
    // on y = 1080. DVI-D-1 has no link to the netbook.
    let out: Vec<_> = topo.links.iter().filter(|l| l.from == edp()).collect();
    assert_eq!(
        out,
        vec![&EdgeLink {
            from: edp(),
            side: Side::Top,
            to: hdmi(),
            line: 1080,
            span: (277, 1643),
        }]
    );
    assert!(!topo.links.iter().any(|l| l.from == dvi() || l.to == dvi()));
    // Barrier: eDP-1's top, local.
    assert_eq!(
        barriers(&topo, &[desk()]),
        vec![Barrier {
            id: 1,
            monitor: edp(),
            side: Side::Top,
            pos: (0, 0, 1365, 0),
        }]
    );
    // Local x ∈ [0, 1366) is plane [277, 1643).
    assert_eq!(
        topo.to_plane(Point { x: 0.0, y: 0.0 }),
        Point {
            x: 277.0,
            y: 1080.0
        }
    );
    // A contact at local x = 523 enters the desktop at (800, 1079): the same
    // plane point, clamped into HDMI-A-1.
    let contact = topo.to_plane(Point { x: 523.0, y: 0.0 });
    let entry = topo.monitor(hdmi()).unwrap().rect.clamp(contact);
    assert_eq!(
        topo.to_peer_local(desk(), entry),
        Some(Point {
            x: 800.0,
            y: 1079.0
        })
    );
}

#[test]
fn links_each_side() {
    // HDMI-A-1 is 1920×1080 at (0,0); the netbook is 1366×768, centred:
    // (1920-1366)/2 = 277 along x, (1080-768)/2 = 156 along y. Right of
    // HDMI-A-1 is DVI-D-1, so the right-hand case anchors on DVI-D-1.
    let dvi = MonitorId {
        peer: desk(),
        index: 1,
    };
    for (rel, anchor, origin, side, line, span) in [
        (
            Rel::Below,
            hdmi(),
            (277, 1080),
            Side::Bottom,
            1080,
            (277, 1643),
        ),
        (Rel::Above, hdmi(), (277, -768), Side::Top, 0, (277, 1643)),
        (Rel::LeftOf, hdmi(), (-1366, 156), Side::Left, 0, (156, 924)),
        (
            Rel::RightOf,
            dvi,
            (3840, 156),
            Side::Right,
            3840,
            (156, 924),
        ),
    ] {
        let mut layout = desk_at_origin();
        let monitors = [desk_monitors(), netbook_monitors()];
        let o = place(
            &layout,
            &monitors,
            netbook(),
            rel,
            Anchor::Monitor(anchor),
            Align::Center,
        )
        .expect("placement");
        assert_eq!((o.x, o.y), origin, "{rel:?}");
        layout.set_origin(o);
        let topo = topology(desk(), &layout, &monitors, &[netbook()]);
        assert_eq!(
            topo.links,
            vec![
                EdgeLink {
                    from: anchor,
                    side,
                    to: edp(),
                    line,
                    span
                },
                EdgeLink {
                    from: edp(),
                    side: side.opposite(),
                    to: anchor,
                    line,
                    span
                }
            ],
            "{rel:?}"
        );
    }
}

#[test]
fn peer_anchor_uses_the_whole_group() {
    let o = place(
        &desk_at_origin(),
        &[desk_monitors(), netbook_monitors()],
        netbook(),
        Rel::RightOf,
        Anchor::Peer(desk()),
        Align::Start,
    )
    .expect("placement");
    assert_eq!((o.x, o.y), (3840, 0));
}

#[test]
fn a_placement_past_the_coordinate_cap_is_refused() {
    let monitors = [desk_monitors(), netbook_monitors()];
    let at = |px| {
        place(
            &desk_at_origin(),
            &monitors,
            netbook(),
            Rel::Below,
            Anchor::Peer(desk()),
            Align::Offset(px),
        )
    };
    let c = 1 << 24;
    assert_eq!(at(c).map(|o| o.x), Ok(c));
    assert_eq!(at(c + 1).map(|o| o.x), Ok(c), "the offset is clamped");
    assert_eq!(at(i32::MIN).map(|o| o.x), Ok(-c));
    let mut far = desk_at_origin();
    far.set_origin(Origin {
        peer: desk(),
        x: c - 100,
        y: 0,
    });
    let o = place(
        &far,
        &monitors,
        netbook(),
        Rel::Below,
        Anchor::Peer(desk()),
        Align::Offset(1000),
    );
    assert_eq!(o, Err(PlaceError::OutOfRange));
}

#[test]
fn place_align() {
    let layout = desk_at_origin();
    let monitors = [desk_monitors(), netbook_monitors()];
    let at = |rel, anchor, align| {
        let o = place(&layout, &monitors, netbook(), rel, anchor, align).expect("placement");
        (o.x, o.y)
    };
    let hd = Anchor::Monitor(hdmi());
    // Below / above: along x. 1920 - 1366 = 554, /2 = 277.
    for (rel, y) in [(Rel::Below, 1080), (Rel::Above, -768)] {
        assert_eq!(at(rel, hd, Align::Start), (0, y));
        assert_eq!(at(rel, hd, Align::Center), (277, y));
        assert_eq!(at(rel, hd, Align::End), (554, y));
        assert_eq!(at(rel, hd, Align::Offset(100)), (100, y));
        assert_eq!(at(rel, hd, Align::Offset(-50)), (-50, y));
    }
    // Left of: along y. 1080 - 768 = 312, /2 = 156.
    assert_eq!(at(Rel::LeftOf, hd, Align::Start), (-1366, 0));
    assert_eq!(at(Rel::LeftOf, hd, Align::Center), (-1366, 156));
    assert_eq!(at(Rel::LeftOf, hd, Align::End), (-1366, 312));
    assert_eq!(at(Rel::LeftOf, hd, Align::Offset(7)), (-1366, 7));
    // Right of DVI-D-1.
    let dvi = Anchor::Monitor(MonitorId {
        peer: desk(),
        index: 1,
    });
    assert_eq!(at(Rel::RightOf, dvi, Align::Start), (3840, 0));
    assert_eq!(at(Rel::RightOf, dvi, Align::Center), (3840, 156));
    assert_eq!(at(Rel::RightOf, dvi, Align::End), (3840, 312));
    assert_eq!(at(Rel::RightOf, dvi, Align::Offset(-10)), (3840, -10));
}

#[test]
fn center_rounds_down() {
    // A 3-px group centred on a 2-px anchor: (2 - 3) / 2 = -0.5 → -1.
    let a = PeerMonitors {
        peer: desk(),
        monitors: vec![mon("A", 0, 0, 2, 2)],
    };
    let b = PeerMonitors {
        peer: netbook(),
        monitors: vec![mon("B", 0, 0, 3, 3)],
    };
    let o = place(
        &desk_at_origin(),
        &[a, b],
        netbook(),
        Rel::Below,
        Anchor::Peer(desk()),
        Align::Center,
    )
    .expect("placement");
    assert_eq!((o.x, o.y), (-1, 2));
}

#[test]
fn place_uses_the_group_box() {
    // A group whose local monitors do not start at (0,0).
    let lap = PeerMonitors {
        peer: netbook(),
        monitors: vec![mon("a", 100, 50, 800, 600), mon("b", 900, 50, 800, 600)],
    };
    let o = place(
        &desk_at_origin(),
        &[desk_monitors(), lap],
        netbook(),
        Rel::Below,
        Anchor::Monitor(hdmi()),
        Align::Start,
    )
    .expect("placement");
    // The box (100,50)–(1700,650) lands at (0,1080): origin = box − local.
    assert_eq!((o.x, o.y), (-100, 1030));
}

#[test]
fn place_rejects_overlap() {
    let mut layout = layout_with_netbook(Rel::Below);
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 1000, 500)],
        },
    ];
    // Below HDMI-A-1 at the start would cover the netbook at (277,1080).
    assert_eq!(
        place(
            &layout,
            &monitors,
            third(),
            Rel::Below,
            Anchor::Monitor(hdmi()),
            Align::Start,
        ),
        Err(PlaceError::Overlap(netbook()))
    );
    // Below the netbook it fits.
    let o = place(
        &layout,
        &monitors,
        third(),
        Rel::Below,
        Anchor::Peer(netbook()),
        Align::Start,
    )
    .expect("fits");
    assert_eq!((o.x, o.y), (277, 1848));
    layout.set_origin(o);
    // The other refusals name the problem.
    assert_eq!(
        place(
            &layout,
            &monitors,
            netbook(),
            Rel::Below,
            Anchor::Peer(netbook()),
            Align::Start
        ),
        Err(PlaceError::SelfAnchor)
    );
    let unknown = PeerId([0x99; 32]);
    assert_eq!(
        place(
            &layout,
            &monitors,
            unknown,
            Rel::Below,
            Anchor::Peer(desk()),
            Align::Start
        ),
        Err(PlaceError::UnknownMonitors(unknown))
    );
    assert_eq!(
        place(
            &Layout::empty(desk()),
            &monitors,
            netbook(),
            Rel::Below,
            Anchor::Peer(desk()),
            Align::Start
        ),
        Err(PlaceError::AnchorUnplaced(desk()))
    );
}

#[test]
fn barriers_whole_edge_only_live() {
    // The netbook below HDMI-A-1 and a third peer right of DVI-D-1.
    let mut layout = layout_with_netbook(Rel::Below);
    layout.set_origin(Origin {
        peer: third(),
        x: 3840,
        y: 200,
    });
    let monitors = [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 800, 600)],
        },
    ];
    let topo = topology(desk(), &layout, &monitors, &[netbook(), third()]);
    let dvi = MonitorId {
        peer: desk(),
        index: 1,
    };
    // Both up: the whole bottom edge of HDMI-A-1 and the whole right edge of
    // DVI-D-1, although the right link spans only y ∈ [200, 800).
    assert_eq!(
        barriers(&topo, &[netbook(), third()]),
        vec![
            Barrier {
                id: 1,
                monitor: hdmi(),
                side: Side::Bottom,
                pos: (0, 1080, 1919, 1080),
            },
            Barrier {
                id: 2,
                monitor: dvi,
                side: Side::Right,
                pos: (3840, 0, 3840, 1079),
            },
        ]
    );
    // The netbook down: its side has no barrier.
    assert_eq!(
        barriers(&topo, &[third()]),
        vec![Barrier {
            id: 1,
            monitor: dvi,
            side: Side::Right,
            pos: (3840, 0, 3840, 1079),
        }]
    );
    assert!(barriers(&topo, &[]).is_empty());
    // Untrusted: no links to it at all.
    let untrusted = topology(desk(), &layout, &monitors, &[netbook()]);
    assert!(untrusted.links.iter().all(|l| l.to.peer != third()));
    assert!(barriers(&untrusted, &[netbook(), third()]).len() == 1);
}

#[test]
fn barriers_are_local_coordinates() {
    // The desk's origin is not (0,0) on the plane: barriers stay local.
    let mut layout = layout_with_netbook(Rel::Below);
    for o in &mut layout.origins {
        o.x += 500;
        o.y -= 300;
    }
    let topo = topology(
        desk(),
        &layout,
        &[desk_monitors(), netbook_monitors()],
        &[netbook()],
    );
    assert_eq!(topo.my_origin, (500, -300));
    assert_eq!(barriers(&topo, &[netbook()])[0].pos, (0, 1080, 1919, 1080));
    let p = Point {
        x: 800.0,
        y: 1080.0,
    };
    assert_eq!(topo.to_local(topo.to_plane(p)), p);
    assert_eq!(
        topo.to_peer_local(
            netbook(),
            Point {
                x: 1300.0,
                y: 780.0
            }
        ),
        Some(Point { x: 523.0, y: 0.0 })
    );
}

#[test]
fn rect_helpers() {
    let r = Rect {
        x: 10,
        y: 20,
        w: 100,
        h: 50,
    };
    assert!(r.contains(Point { x: 10.0, y: 20.0 }));
    assert!(r.contains(Point { x: 109.9, y: 69.9 }));
    assert!(!r.contains(Point { x: 110.0, y: 30.0 }));
    assert!(!r.contains(Point { x: 50.0, y: 70.0 }));
    assert_eq!(
        r.clamp(Point { x: 500.0, y: -5.0 }),
        Point { x: 109.0, y: 20.0 }
    );
    assert!(r.overlaps(&r.translate(99, 49)));
    assert!(!r.overlaps(&r.translate(100, 0)));
    assert!(!r.overlaps(&r.translate(0, 50)));
    assert_eq!(Side::Right.line(&r), 110);
    assert_eq!(Side::Bottom.line(&r), 70);
    assert_eq!(Side::Left.span(&r), (20, 70));
    assert_eq!(Side::Top.span(&r), (10, 110));
    assert!(Side::Right.beyond(&r, Point { x: 110.0, y: 30.0 }));
    assert!(!Side::Right.beyond(&r, Point { x: 109.5, y: 30.0 }));
    assert!(Side::Top.beyond(&r, Point { x: 50.0, y: 19.9 }));
    // inside(): 2 px in from the side (S02 `inside`, S14 warp target 1917).
    let p = |x, y| Point { x, y };
    assert_eq!(inside(&r, Side::Bottom, p(50.0, 70.0), 2.0), p(50.0, 67.0));
    assert_eq!(inside(&r, Side::Top, p(50.0, 20.0), 2.0), p(50.0, 22.0));
    assert_eq!(inside(&r, Side::Right, p(110.0, 30.0), 2.0), p(107.0, 30.0));
    assert_eq!(inside(&r, Side::Left, p(10.0, 30.0), 2.0), p(12.0, 30.0));
    // Already further in: unchanged.
    assert_eq!(inside(&r, Side::Bottom, p(50.0, 40.0), 2.0), p(50.0, 40.0));
}

#[test]
fn logical_size_per_scale_and_transform() {
    assert_eq!(logical_size(1920, 1080, 1.0, 0), (1920, 1080));
    // 1920×1080 at 1.6 → 1200×675.
    assert_eq!(logical_size(1920, 1080, 1.6, 0), (1200, 675));
    assert_eq!(logical_size(2560, 1440, 1.25, 0), (2048, 1152));
    for t in [1, 3, 5, 7] {
        assert_eq!(logical_size(1920, 1080, 1.0, t), (1080, 1920));
    }
    for t in [2, 4, 6] {
        assert_eq!(logical_size(1920, 1080, 2.0, t), (960, 540));
    }
    assert_eq!(logical_size(1920, 1080, 0.0, 0), (1920, 1080));
}

#[test]
fn layout_merge_keeps_the_higher_revision() {
    let rev = |counter, peer| Revision { counter, peer };
    let with = |r: Revision, x| Layout {
        rev: r,
        origins: vec![Origin {
            peer: netbook(),
            x,
            y: 0,
        }],
    };
    let mut mine = with(rev(3, desk()), 1);
    // Lower counter: kept.
    assert!(!mine.merge(with(rev(2, netbook()), 2)));
    assert_eq!(mine.origin(netbook()), Some((1, 0)));
    // Equal revision: kept.
    assert!(!mine.merge(with(rev(3, desk()), 3)));
    assert_eq!(mine.origin(netbook()), Some((1, 0)));
    // Same counter: the higher peer id wins (desk 0xd0… > netbook 0x0b…).
    assert!(!mine.merge(with(rev(3, netbook()), 4)));
    let mut theirs = with(rev(3, netbook()), 5);
    assert!(theirs.merge(with(rev(3, desk()), 6)));
    assert_eq!(theirs.origin(netbook()), Some((6, 0)));
    // Higher counter: replaced.
    assert!(mine.merge(with(rev(4, netbook()), 7)));
    assert_eq!(mine.rev, rev(4, netbook()));
    // The next local edit outranks everything seen.
    let next = Revision::next(mine.rev, desk());
    assert_eq!(next, rev(5, desk()));
    assert!(next > mine.rev);
    // Wire #10 round trip.
    let w = mine.to_wire();
    assert_eq!(Layout::from_wire(&w), Some(mine));
}

// ---- property tests --------------------------------------------------------

/// Up to 6 small monitors on 3 peers in a 24×24 area, with overlapping ones
/// dropped (a valid layout never overlaps).
fn arb_monitors() -> impl Strategy<Value = Vec<PlacedMonitor>> {
    proptest::collection::vec((0u8..3, 0i32..24, 0i32..24, 1i32..9, 1i32..9), 2..7).prop_map(|v| {
        let mut out: Vec<PlacedMonitor> = Vec::new();
        for (p, x, y, w, h) in v {
            let rect = Rect { x, y, w, h };
            if out.iter().any(|m| m.rect.overlaps(&rect)) {
                continue;
            }
            let peer = PeerId([p; 32]);
            let index = out.iter().filter(|m| m.id.peer == peer).count() as u8;
            out.push(PlacedMonitor {
                id: MonitorId { peer, index },
                name: format!("m{p}.{index}"),
                rect,
            });
        }
        out
    })
}

/// Brute force: the pixels along `side` of `a` whose outward neighbour pixel
/// lies in `b`.
fn touching_pixels(a: &Rect, side: Side, b: &Rect) -> Vec<i32> {
    let (lo, hi) = side.span(a);
    (lo..hi)
        .filter(|&k| {
            let (x, y) = match side {
                Side::Left => (a.x - 1, k),
                Side::Right => (a.x + a.w, k),
                Side::Top => (k, a.y - 1),
                Side::Bottom => (k, a.y + a.h),
            };
            b.contains(Point {
                x: f64::from(x) + 0.5,
                y: f64::from(y) + 0.5,
            })
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn links_symmetric(ms in arb_monitors()) {
        let links = derive_links(&ms);
        for l in &links {
            prop_assert!(l.span.0 < l.span.1);
            prop_assert!(l.from.peer != l.to.peer);
            let back = links.iter().filter(|r| {
                r.from == l.to && r.to == l.from && r.side == l.side.opposite()
                    && r.line == l.line && r.span == l.span
            }).count();
            prop_assert_eq!(back, 1);
        }
    }

    #[test]
    fn links_equal_touching_segments(ms in arb_monitors()) {
        let links = derive_links(&ms);
        for a in &ms {
            for b in &ms {
                for side in Side::ALL {
                    let brute = if a.id.peer == b.id.peer {
                        Vec::new()
                    } else {
                        touching_pixels(&a.rect, side, &b.rect)
                    };
                    let derived: Vec<i32> = links
                        .iter()
                        .filter(|l| l.from == a.id && l.to == b.id && l.side == side)
                        .flat_map(|l| l.span.0..l.span.1)
                        .collect();
                    prop_assert_eq!(&derived, &brute, "{:?} {:?} {:?}", a, side, b);
                    for l in links.iter().filter(|l| l.from == a.id && l.to == b.id && l.side == side) {
                        prop_assert_eq!(l.line, side.line(&a.rect));
                    }
                }
            }
        }
    }

    #[test]
    fn placement_never_overlaps(
        rel in prop_oneof![Just(Rel::Below), Just(Rel::Above), Just(Rel::LeftOf), Just(Rel::RightOf)],
        align in prop_oneof![
            Just(Align::Start), Just(Align::Center), Just(Align::End),
            (-3000i32..3000).prop_map(Align::Offset),
        ],
        w in 200u32..3000, h in 200u32..2000,
    ) {
        let mut layout = desk_at_origin();
        let lap = PeerMonitors { peer: netbook(), monitors: vec![mon("L", 0, 0, w, h)] };
        let monitors = [desk_monitors(), lap];
        match place(&layout, &monitors, netbook(), rel, Anchor::Monitor(hdmi()), align) {
            Ok(o) => {
                layout.set_origin(o);
                let topo = topology(desk(), &layout, &monitors, &[netbook()]);
                let lap = topo.monitors.iter().find(|m| m.id.peer == netbook()).map(|m| m.rect);
                prop_assert!(lap.is_some());
                for m in topo.monitors.iter().filter(|m| m.id.peer == desk()) {
                    prop_assert!(!lap.is_some_and(|l| l.overlaps(&m.rect)));
                }
                // Start, center and end always touch HDMI-A-1 on `rel`'s side.
                if !matches!(align, Align::Offset(_)) {
                    prop_assert!(topo.links.iter().any(|l| l.from == hdmi() && l.side == rel.side()));
                }
            }
            Err(e) => prop_assert_eq!(e, PlaceError::Overlap(desk())),
        }
    }
}

// ---- peer geometry at the caps (A-03) --------------------------------------

/// A peer's monitors anywhere inside the wire caps (`caps::COORD`,
/// `caps::MONITOR_SIZE`), as `check_caps` lets them through.
fn arb_capped_peer() -> impl Strategy<Value = (Vec<MonitorInfo>, (i32, i32))> {
    let c = 1i32 << 24;
    let coord = prop_oneof![-c..=c, Just(-c), Just(c)];
    let size = prop_oneof![
        0..=crate::wire::caps::MONITOR_SIZE,
        Just(crate::wire::caps::MONITOR_SIZE)
    ];
    let monitor = (coord.clone(), coord.clone(), size.clone(), size)
        .prop_map(|(x, y, w, h)| MonitorInfo {
            name: "M".into(),
            description: String::new(),
            rect: WireRect { x, y, w, h },
            scale: 1.0,
            transform: 0,
            shift: None,
        })
        .prop_filter("in range", MonitorInfo::in_range);
    (
        proptest::collection::vec(monitor, 1..=4),
        (coord.clone(), coord),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Debug builds check overflow: a topology (links, barriers, conversions)
    /// over capped peer geometry never panics.
    #[test]
    fn topology_of_capped_peers_never_overflows(
        peers in proptest::collection::vec(arb_capped_peer(), 1..=3),
    ) {
        let ids = [desk(), netbook(), third()];
        let mut layout = Layout::empty(desk());
        let mut monitors = Vec::new();
        for (id, (ms, (x, y))) in ids.iter().zip(peers) {
            layout.set_origin(Origin { peer: *id, x, y });
            monitors.push(PeerMonitors { peer: *id, monitors: ms });
        }
        let topo = topology(desk(), &layout, &monitors, &ids);
        let _ = barriers(&topo, &ids);
        for m in &topo.monitors {
            let _ = Rect::bounding(topo.monitors.iter().map(|n| n.rect));
            for side in Side::ALL {
                let _ = side.line(&m.rect);
                let _ = side.span(&m.rect);
            }
        }
        let _ = place(&layout, &monitors, netbook(), Rel::Below, Anchor::Peer(desk()), Align::Center);
        let _ = place(&layout, &monitors, netbook(), Rel::LeftOf, Anchor::Peer(desk()), Align::Offset(i32::MIN));
    }
}

// ---- the anchor shift (M3 design 4.1) -----------------------------------------

/// `desk_monitors()` with every monitor carrying the anchor shift `(x, y)`.
fn desk_shifted(x: i32, y: i32) -> PeerMonitors {
    let mut pm = desk_monitors();
    for m in &mut pm.monitors {
        m.shift = WireOffset::of((x, y));
    }
    pm
}

#[test]
fn a_group_sits_at_its_origin_plus_its_anchor_shift() {
    // Hyprland moved desk's monitors 1920 to the left; desk announced a shift of +1920, so the
    // group is where it was.
    let mut shifted = desk_monitors();
    for m in &mut shifted.monitors {
        m.rect.x -= 1920;
    }
    for m in &mut shifted.monitors {
        m.shift = WireOffset::of((1920, 0));
    }
    let layout = layout_with_netbook(Rel::Below);
    let plain = topology(
        desk(),
        &layout,
        &[desk_monitors(), netbook_monitors()],
        &[netbook()],
    );
    let topo = topology(
        desk(),
        &layout,
        &[shifted.clone(), netbook_monitors()],
        &[netbook()],
    );
    assert_eq!(topo.monitors, plain.monitors, "nothing moved on the plane");
    assert_eq!(topo.links, plain.links);
    assert_eq!(topo.my_origin, (1920, 0), "local (0, 0) is 1920 along");
    assert_eq!(topo.origin(desk()), Some((1920, 0)));
    // the barrier is still in desk's own (Hyprland) coordinates
    assert_eq!(
        barriers(&topo, &[netbook()]),
        vec![Barrier {
            id: 1,
            monitor: hdmi(),
            side: Side::Bottom,
            pos: (-1920, 1080, -1, 1080),
        }]
    );
    // the netbook as source: crossing up into desk lands in desk's own coordinates
    let from_netbook = topology(
        netbook(),
        &layout,
        &[shifted, netbook_monitors()],
        &[desk()],
    );
    assert_eq!(
        from_netbook.to_peer_local(
            desk(),
            Point {
                x: 800.0,
                y: 1079.0
            }
        ),
        Some(Point {
            x: 800.0 - 1920.0,
            y: 1079.0
        })
    );
}

#[test]
fn place_counts_the_anchor_shift_of_the_anchor_and_of_the_placed_group() {
    let layout = desk_at_origin();
    let place_below_hdmi = |d: PeerMonitors, lap: PeerMonitors| {
        place(
            &layout,
            &[d, lap],
            netbook(),
            Rel::Below,
            Anchor::Monitor(hdmi()),
            Align::Start,
        )
        .expect("placement")
    };
    // no shift: the netbook's origin is (0, 1080)
    let o = place_below_hdmi(desk_monitors(), netbook_monitors());
    assert_eq!((o.x, o.y), (0, 1080));
    // the anchor group shifted by (100, 0): its HDMI-A-1 sits at plane x 100, the group goes with it
    let o = place_below_hdmi(desk_shifted(100, 0), netbook_monitors());
    assert_eq!((o.x, o.y), (100, 1080));
    // the placed group shifted by (-30, 7): the layout origin makes up for it, so that its box
    // still lands under the anchor's
    let mut lap = netbook_monitors();
    lap.monitors[0].shift = WireOffset::of((-30, 7));
    let o = place_below_hdmi(desk_monitors(), lap.clone());
    assert_eq!((o.x, o.y), (30, 1073));
    // and the topology puts it there
    let mut l = desk_at_origin();
    l.set_origin(o);
    let topo = topology(desk(), &l, &[desk_monitors(), lap], &[netbook()]);
    let edp = topo.monitor(edp()).expect("placed");
    assert_eq!((edp.rect.x, edp.rect.y), (0, 1080));
}

#[test]
fn overlap_is_judged_where_the_groups_really_are() {
    // Desk is placed at (0, 0) and the netbook right below it. A shift of the netbook by (0, -1080)
    // lifts it onto desk: the layout origin alone looks fine, the real positions overlap.
    let layout = layout_with_netbook(Rel::Below);
    let mut lap = netbook_monitors();
    assert_eq!(
        first_overlap(&layout, &[desk_monitors(), lap.clone()], &[netbook()]),
        None
    );
    lap.monitors[0].shift = WireOffset::of((0, -1080));
    assert_eq!(
        first_overlap(&layout, &[desk_monitors(), lap], &[netbook()]),
        Some((netbook(), desk()))
    );
}

#[test]
fn a_batch_is_one_revision_and_keeps_the_peers_it_does_not_name() {
    let mut layout = layout_with_netbook(Rel::Below); // revision 1 (desk), netbook at (277, 1080)
    layout.set_origin(Origin {
        peer: third(),
        x: 5000,
        y: 0,
    });
    let moves = [
        Origin {
            peer: netbook(),
            x: 0,
            y: 2000,
        },
        Origin {
            peer: desk(),
            x: -10,
            y: -20,
        },
    ];
    let next = layout.with_origins(desk(), &moves);
    assert_eq!(
        next.rev,
        Revision {
            counter: 2,
            peer: desk()
        }
    );
    assert_eq!(next.origin(netbook()), Some((0, 2000)));
    assert_eq!(next.origin(desk()), Some((-10, -20)));
    assert_eq!(
        next.origin(third()),
        Some((5000, 0)),
        "not named, not moved"
    );
    assert_eq!(next.origins.len(), 3);
    // the same peer twice: the last one counts
    let twice = layout.with_origins(
        desk(),
        &[
            Origin {
                peer: netbook(),
                x: 1,
                y: 1,
            },
            Origin {
                peer: netbook(),
                x: 2,
                y: 2,
            },
        ],
    );
    assert_eq!(twice.origin(netbook()), Some((2, 2)));
    assert_eq!(
        twice.origins.iter().filter(|o| o.peer == netbook()).count(),
        1
    );
    // this machine gets (0, 0) the first time anything is placed
    let first = Layout::empty(desk()).with_origins(
        desk(),
        &[Origin {
            peer: netbook(),
            x: 7,
            y: 8,
        }],
    );
    assert_eq!(first.origin(desk()), Some((0, 0)));
    assert_eq!(
        first.rev,
        Revision {
            counter: 1,
            peer: desk()
        }
    );
}

fn all_three() -> [PeerMonitors; 3] {
    [
        desk_monitors(),
        netbook_monitors(),
        PeerMonitors {
            peer: third(),
            monitors: vec![mon("X", 0, 0, 1000, 500)],
        },
    ]
}

#[test]
fn set_origins_refuses_an_overlap_and_names_both_peers() {
    let layout = layout_with_netbook(Rel::Below);
    let monitors = all_three();
    let move_to = |peer, x, y| [Origin { peer, x, y }];
    // Onto desk's HDMI-A-1
    assert_eq!(
        set_origins(&layout, &monitors, desk(), &move_to(netbook(), 100, 100)),
        Err(SetError::Overlap {
            moved: netbook(),
            other: desk()
        })
    );
    // moving desk itself onto the netbook names desk first
    assert_eq!(
        set_origins(&layout, &monitors, desk(), &move_to(desk(), 300, 1000)),
        Err(SetError::Overlap {
            moved: desk(),
            other: netbook()
        })
    );
    // touching is not overlapping
    let ok = set_origins(&layout, &monitors, desk(), &move_to(netbook(), 0, 1080))
        .expect("touching is fine")
        .expect("it moved");
    assert_eq!(ok.origin(netbook()), Some((0, 1080)));
    // a batch whose members only overlap each other at their new places is refused too
    let batch = [
        Origin {
            peer: netbook(),
            x: 6000,
            y: 0,
        },
        Origin {
            peer: third(),
            x: 6500,
            y: 100,
        },
    ];
    assert_eq!(
        set_origins(&layout, &monitors, desk(), &batch),
        Err(SetError::Overlap {
            moved: netbook(),
            other: third()
        })
    );
    // ...and a batch that swaps two groups is fine, though each would overlap the other's old place
    let mut two = Layout::empty(desk());
    two.set_origin(Origin {
        peer: desk(),
        x: 0,
        y: 0,
    });
    two.set_origin(Origin {
        peer: netbook(),
        x: 0,
        y: 1080,
    });
    two.set_origin(Origin {
        peer: third(),
        x: 1400,
        y: 1080,
    });
    let swap = [
        Origin {
            peer: netbook(),
            x: 1000,
            y: 1080,
        },
        Origin {
            peer: third(),
            x: 0,
            y: 1080,
        },
    ];
    // third (1000 wide) at 0 and netbook (1366 wide) at 1000: they touch at 1000
    assert!(
        set_origins(&two, &monitors, desk(), &swap)
            .unwrap()
            .is_some()
    );
}

#[test]
fn set_origins_refuses_what_every_peer_would_drop_and_skips_what_changes_nothing() {
    let layout = layout_with_netbook(Rel::Below);
    let monitors = all_three();
    let c = i32::try_from(crate::wire::caps::COORD).unwrap();
    assert_eq!(
        set_origins(
            &layout,
            &monitors,
            desk(),
            &[Origin {
                peer: netbook(),
                x: c + 1,
                y: 0
            }]
        ),
        Err(SetError::OutOfRange(netbook()))
    );
    // a fifth peer does not fit in a `Layout` frame
    let mut full = layout.clone();
    for n in 0..3u8 {
        full.set_origin(Origin {
            peer: PeerId([0xa0 + n; 32]),
            x: 100_000 * i32::from(n + 1),
            y: 0,
        });
    }
    assert_eq!(full.origins.len(), 5, "desk, the netbook and three more");
    assert_eq!(
        set_origins(
            &full,
            &monitors,
            desk(),
            &[Origin {
                peer: netbook(),
                x: 0,
                y: 1080
            }]
        ),
        Err(SetError::TooManyPeers)
    );
    // everything named is already there: nothing to edit, no revision spent
    assert_eq!(
        set_origins(
            &layout,
            &monitors,
            desk(),
            &[Origin {
                peer: netbook(),
                x: 277,
                y: 1080
            }]
        ),
        Ok(None)
    );
    // ...but a first placement is an edit even if the named origin matches nothing yet
    assert!(
        set_origins(
            &Layout::empty(desk()),
            &monitors,
            desk(),
            &[Origin {
                peer: netbook(),
                x: 0,
                y: 1080
            }]
        )
        .unwrap()
        .is_some()
    );
}

/// A tiny three-peer world for the properties: desk (two monitors, maybe shifted), the netbook
/// and a third peer, each at an origin on a 250 px grid, so touching and overlapping both occur.
fn arb_world() -> impl Strategy<Value = (Vec<PeerMonitors>, Layout)> {
    let grid = || (-8i32..24).prop_map(|n| n * 250);
    (
        (-2i32..3, -2i32..3),
        proptest::collection::vec((grid(), grid()), 3),
    )
        .prop_map(|((sx, sy), places)| {
            let mut monitors = all_three().to_vec();
            for m in &mut monitors[0].monitors {
                m.shift = WireOffset::of((sx * 250, sy * 250));
            }
            let mut layout = Layout::empty(desk());
            for (peer, (x, y)) in [desk(), netbook(), third()].into_iter().zip(places) {
                layout.set_origin(Origin { peer, x, y });
            }
            (monitors, layout)
        })
}

/// Where a group is on the plane, worked out apart from `geom`: origin plus shift plus local.
fn plane_rects(monitors: &[PeerMonitors], layout: &Layout, peer: PeerId) -> Vec<Rect> {
    let (ox, oy) = layout.origin(peer).expect("placed");
    let pm = monitors.iter().find(|m| m.peer == peer).expect("known");
    let (sx, sy) = pm.shift();
    pm.monitors
        .iter()
        .map(|m| Rect {
            x: m.rect.x + sx + ox,
            y: m.rect.y + sy + oy,
            w: i32::try_from(m.rect.w).unwrap(),
            h: i32::try_from(m.rect.h).unwrap(),
        })
        .collect()
}

fn groups_overlap(monitors: &[PeerMonitors], layout: &Layout, a: PeerId, b: PeerId) -> bool {
    let (ra, rb) = (
        plane_rects(monitors, layout, a),
        plane_rects(monitors, layout, b),
    );
    ra.iter().any(|x| rb.iter().any(|y| x.overlaps(y)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// FR-LAYOUT-05 for a batch: `set_origins` refuses exactly when a moved group overlaps
    /// another group at the new positions; it names a pair that really overlaps; and when it
    /// accepts, one revision was spent, the named peers are where they were asked to be and the
    /// others did not move. A refusal changes nothing (the input is only borrowed).
    #[test]
    fn a_batch_is_refused_exactly_when_a_moved_group_would_overlap(
        (monitors, layout) in arb_world(),
        asked in proptest::collection::vec((0usize..3, (-8i32..24).prop_map(|n| n * 250), (-8i32..24).prop_map(|n| n * 250)), 1..4),
    ) {
        let peers = [desk(), netbook(), third()];
        // a peer is named once
        let mut moves: Vec<Origin> = Vec::new();
        for (i, x, y) in asked {
            if moves.iter().all(|m| m.peer != peers[i]) {
                moves.push(Origin { peer: peers[i], x, y });
            }
        }
        let before = layout.clone();
        let result = set_origins(&layout, &monitors, desk(), &moves);
        prop_assert_eq!(&layout, &before);
        // the world after the move, built apart from `set_origins`
        let mut after = layout.clone();
        for m in &moves {
            after.set_origin(*m);
        }
        let expected = moves.iter().find_map(|m| {
            peers
                .iter()
                .filter(|p| **p != m.peer)
                .find(|p| groups_overlap(&monitors, &after, m.peer, **p))
                .map(|p| (m.peer, *p))
        });
        let unchanged = moves.iter().all(|m| layout.origin(m.peer) == Some((m.x, m.y)));
        match result {
            Ok(None) => prop_assert!(unchanged),
            Ok(Some(next)) => {
                prop_assert!(!unchanged);
                prop_assert_eq!(expected, None, "accepted a batch that overlaps");
                prop_assert_eq!(next.rev, Revision::next(layout.rev, desk()));
                for p in peers {
                    let asked = moves.iter().find(|m| m.peer == p).map(|m| (m.x, m.y));
                    prop_assert_eq!(next.origin(p), asked.or(layout.origin(p)));
                }
            }
            Err(SetError::Overlap { moved, other }) => {
                prop_assert!(!unchanged);
                prop_assert!(moves.iter().any(|m| m.peer == moved));
                prop_assert!(groups_overlap(&monitors, &after, moved, other), "{moved:?} {other:?}");
                prop_assert!(expected.is_some());
            }
            Err(other) => prop_assert!(false, "unexpected {other:?}"),
        }
    }

    /// `place` and `set_origins` agree on what overlaps: a placement `place` computes is one
    /// `set_origins` accepts.
    #[test]
    fn a_placement_is_a_batch_that_set_origins_accepts(
        (monitors, layout) in arb_world(),
        rel in prop_oneof![Just(Rel::Below), Just(Rel::Above), Just(Rel::LeftOf), Just(Rel::RightOf)],
        align in prop_oneof![Just(Align::Start), Just(Align::Center), Just(Align::End)],
    ) {
        if let Ok(o) = place(&layout, &monitors, netbook(), rel, Anchor::Peer(desk()), align) {
            let set = set_origins(&layout, &monitors, desk(), &[o]);
            prop_assert!(set.is_ok(), "{set:?}");
        }
    }
}

// ---- convergence with multi-origin edits ----------------------------------------

#[derive(Debug, Clone)]
enum Step {
    /// Replica `at` makes an edit that moves these peers (by index) to these origins.
    Edit {
        at: usize,
        moves: Vec<(usize, i32, i32)>,
    },
    /// Replica `to` receives the `msg`-th edit made so far (modulo the count); may repeat.
    Deliver { to: usize, msg: usize },
}

fn arb_steps() -> impl Strategy<Value = Vec<Step>> {
    let edit = (
        0usize..3,
        proptest::collection::vec((0usize..3, -500i32..500, -500i32..500), 1..4),
    )
        .prop_map(|(at, moves)| Step::Edit { at, moves });
    let deliver = (0usize..3, any::<usize>()).prop_map(|(to, msg)| Step::Deliver { to, msg });
    proptest::collection::vec(prop_oneof![edit, deliver], 0..40)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Whatever the interleaving of edits (each moving several groups in one
    /// revision) and deliveries, however late or repeated, once every edit has reached every peer
    /// they all hold the layout with the highest revision. A peer's revision never goes down,
    /// and an edit is always higher than anything its author had seen.
    #[test]
    fn edits_that_move_several_groups_converge_in_any_delivery_order(
        steps in arb_steps(),
        order in Just((0usize..64).collect::<Vec<_>>()).prop_shuffle(),
    ) {
        let peers = [desk(), netbook(), third()];
        let mut replicas: Vec<Layout> = peers.iter().map(|p| Layout::empty(*p)).collect();
        let mut sent: Vec<Layout> = Vec::new();
        for step in &steps {
            match step {
                Step::Edit { at, moves } => {
                    let moves: Vec<Origin> = moves
                        .iter()
                        .map(|&(i, x, y)| Origin { peer: peers[i], x, y })
                        .collect();
                    let edit = replicas[*at].with_origins(peers[*at], &moves);
                    prop_assert!(edit.rev > replicas[*at].rev, "an edit outranks what its author has");
                    replicas[*at] = edit.clone();
                    sent.push(edit);
                }
                Step::Deliver { to, msg } if !sent.is_empty() => {
                    let before = replicas[*to].rev;
                    replicas[*to].merge(sent[msg % sent.len()].clone());
                    prop_assert!(replicas[*to].rev >= before, "a revision never goes down");
                }
                Step::Deliver { .. } => {}
            }
        }
        // the rest of the network: every edit to every peer, in a random order, some twice
        for (k, i) in order.iter().enumerate() {
            if sent.is_empty() {
                break;
            }
            let to = k % 3;
            replicas[to].merge(sent[i % sent.len()].clone());
        }
        for replica in &mut replicas {
            for edit in sent.iter().rev().chain(sent.iter()) {
                replica.merge(edit.clone());
            }
        }
        match sent.iter().map(|l| l.rev).max() {
            Some(rev) => {
                let winner = sent.iter().find(|l| l.rev == rev).unwrap();
                for r in &replicas {
                    prop_assert_eq!(r, winner);
                }
            }
            None => prop_assert!(replicas.iter().all(|r| r.origins.is_empty())),
        }
    }
}

/// An unpaired peer leaves the layout in a new revision; a peer that is not placed,
/// or this peer itself, spends none.
#[test]
fn an_unpaired_peer_leaves_the_layout() {
    let layout = desk_at_origin().with_origins(
        desk(),
        &[
            Origin {
                peer: netbook(),
                x: 1920,
                y: 0,
            },
            Origin {
                peer: third(),
                x: 5000,
                y: 0,
            },
        ],
    );
    let next = layout
        .without(desk(), netbook())
        .expect("netbook is placed");
    assert_eq!(next.rev, Revision::next(layout.rev, desk()));
    assert_eq!(next.origin(netbook()), None);
    assert_eq!(next.origin(third()), Some((5000, 0)));
    assert_eq!(next.origin(desk()), Some((0, 0)));
    assert_eq!(next.without(desk(), netbook()), None, "not placed any more");
    assert_eq!(layout.without(desk(), desk()), None);
}
