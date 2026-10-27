//! Monitor memory and the anchor shift (M3 design 4.1): the worked example of the
//! design, the edge cases, and properties over random hot-plug sequences.

use proptest::prelude::*;

use super::*;
use crate::geom::WireRect;

const W: u32 = 1920;
const H: u32 = 1080;

fn mon(name: &str, description: &str, x: i32, y: i32) -> MonitorInfo {
    MonitorInfo {
        name: name.into(),
        description: description.into(),
        rect: WireRect { x, y, w: W, h: H },
        scale: 1.0,
        transform: 0,
        shift: None,
    }
}

/// The two monitors of the desk: HDMI-A-1 at 0 and DVI-D-1 at 1920.
fn desk() -> Vec<MonitorInfo> {
    vec![
        mon("HDMI-A-1", "Dell left", 0, 0),
        mon("DVI-D-1", "Dell right", 1920, 0),
    ]
}

/// Where the monitor sits on the plane for a layout origin at (0, 0): `rect + shift`.
fn plane(out: &[MonitorInfo], description: &str) -> (i32, i32) {
    let m = out
        .iter()
        .find(|m| m.description == description)
        .unwrap_or_else(|| panic!("{description} is not in {out:?}"));
    let (sx, sy) = m.shift_xy();
    (m.rect.x + sx, m.rect.y + sy)
}

fn shift_of(out: &[MonitorInfo]) -> (i32, i32) {
    out.first().map_or((0, 0), MonitorInfo::shift_xy)
}

#[test]
fn a_monitor_is_known_by_its_description_else_its_connector() {
    let reading = [
        mon("DP-1", "Dell Inc. DELL U2415 7MT0167B1ZUL", 0, 0),
        mon("HDMI-A-1", "", 1920, 0),
    ];
    assert_eq!(
        monitor_keys(&reading),
        ["Dell Inc. DELL U2415 7MT0167B1ZUL", "HDMI-A-1"]
    );
}

#[test]
fn equal_keys_in_one_reading_are_numbered_in_reading_order() {
    let twins = [
        mon("DP-1", "Acme 27", 0, 0),
        mon("DP-2", "Acme 27", 1920, 0),
        mon("DP-3", "Acme 27", 3840, 0),
    ];
    assert_eq!(monitor_keys(&twins), ["Acme 27", "Acme 27#2", "Acme 27#3"]);
    // the key alone does not tell twins apart: the memory matches the connector first (F-1,
    // `the_second_of_two_identical_monitors_stays_in_place_when_the_first_one_goes`)
    let moved = [mon("DP-7", "Acme 27", 0, 0)];
    assert_eq!(monitor_keys(&moved), ["Acme 27"]);
}

#[test]
fn a_key_is_always_a_valid_monitor_text() {
    let long = "é".repeat(100);
    let twins = [mon("A", &long, 0, 0), mon("B", &long, 1, 0)];
    for key in monitor_keys(&twins) {
        assert!(validate_monitor_text(&key).is_ok(), "{} bytes", key.len());
    }
}

#[test]
fn the_first_reading_remembers_and_shifts_nothing() {
    let mut a = Anchors::default();
    let out = a.observe(&desk());
    assert_eq!(out, desk());
    assert_eq!(a.shift, (0, 0));
    assert_eq!(
        a.monitors,
        [
            Remembered {
                key: "Dell left".into(),
                connector: Some("HDMI-A-1".into()),
                x: 0,
                y: 0,
                present: true
            },
            Remembered {
                key: "Dell right".into(),
                connector: Some("DVI-D-1".into()),
                x: 1920,
                y: 0,
                present: true
            }
        ]
    );
    assert!(
        out.iter().all(|m| m.shift.is_none()),
        "a zero shift is not sent"
    );
}

/// The worked example of the design (4.1), row by row.
#[test]
fn an_unplugged_left_monitor_that_hyprland_moves_to_zero_leaves_the_other_in_place() {
    let mut a = Anchors::default();
    a.observe(&desk());

    // HDMI-A-1 unplugged; Hyprland moves DVI-D-1 to 0.
    let out = a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]);
    assert_eq!(shift_of(&out), (1920, 0));
    assert_eq!(
        plane(&out, "Dell right"),
        (1920, 0),
        "it stayed where it was"
    );

    // HDMI-A-1 comes back, Hyprland restores 0 and 1920.
    let out = a.observe(&desk());
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(
        plane(&out, "Dell left"),
        (0, 0),
        "back in its remembered place"
    );
    assert_eq!(plane(&out, "Dell right"), (1920, 0));
    assert!(out.iter().all(|m| m.shift.is_none()));
}

#[test]
fn when_every_monitor_was_gone_the_one_that_returns_finds_its_place() {
    let mut a = Anchors::default();
    a.observe(&desk());
    // the compositor has no monitor for a moment
    assert!(a.observe(&[]).is_empty());
    assert!(a.monitors.iter().all(|r| !r.present));
    assert_eq!(a.monitors.len(), 2, "the memory survives an empty reading");
    // DVI-D-1 returns alone, at 0
    let out = a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]);
    assert_eq!(shift_of(&out), (1920, 0));
    assert_eq!(plane(&out, "Dell right"), (1920, 0));
    // HDMI-A-1 returns to its left, at -1920 in Hyprland's new frame
    let out = a.observe(&[
        mon("DVI-D-1", "Dell right", 0, 0),
        mon("HDMI-A-1", "Dell left", -1920, 0),
    ]);
    assert_eq!(shift_of(&out), (1920, 0));
    assert_eq!(plane(&out, "Dell left"), (0, 0));
}

#[test]
fn a_new_monitor_that_pushes_the_others_right_leaves_them_in_place() {
    let mut a = Anchors::default();
    a.observe(&desk());
    // a third monitor takes x = 0 and Hyprland moves both others by 2560
    let out = a.observe(&[
        mon("DP-3", "Wide", 0, 0),
        mon("HDMI-A-1", "Dell left", 2560, 0),
        mon("DVI-D-1", "Dell right", 4480, 0),
    ]);
    assert_eq!(shift_of(&out), (-2560, 0));
    assert_eq!(plane(&out, "Dell left"), (0, 0));
    assert_eq!(plane(&out, "Dell right"), (1920, 0));
    assert_eq!(
        plane(&out, "Wide"),
        (-2560, 0),
        "it joins the group on its left"
    );
}

#[test]
fn a_bigger_monitor_moves_its_neighbour_and_not_itself() {
    let mut a = Anchors::default();
    a.observe(&desk());
    // HDMI-A-1 gets wider; Hyprland moves DVI-D-1 right by 480. One proposal each; the tie goes
    // to the current shift, which is the one HDMI-A-1 proposes.
    let mut wide = mon("HDMI-A-1", "Dell left", 0, 0);
    wide.rect.w = 2400;
    let out = a.observe(&[wide, mon("DVI-D-1", "Dell right", 2400, 0)]);
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(plane(&out, "Dell left"), (0, 0));
    assert_eq!(plane(&out, "Dell right"), (2400, 0));
}

#[test]
fn the_most_common_proposal_wins() {
    let mut a = Anchors::default();
    a.observe(&[
        mon("A", "a", 0, 0),
        mon("B", "b", 1000, 0),
        mon("C", "c", 2000, 0),
    ]);
    // Hyprland moves A and B by 300 and C by 50: two proposals of -300 beat one of -50
    let out = a.observe(&[
        mon("A", "a", 300, 0),
        mon("B", "b", 1300, 0),
        mon("C", "c", 2050, 0),
    ]);
    assert_eq!(shift_of(&out), (-300, 0));
    assert_eq!(plane(&out, "a"), (0, 0));
    assert_eq!(plane(&out, "b"), (1000, 0));
    assert_eq!(
        plane(&out, "c"),
        (1750, 0),
        "the odd one moved relative to the group"
    );
}

#[test]
fn a_monitor_that_did_not_move_keeps_the_frame_even_when_more_monitors_moved() {
    let mut a = Anchors::default();
    a.observe(&[
        mon("A", "a", 0, 0),
        mon("B", "b", 1920, 0),
        mon("C", "c", 3840, 0),
        mon("D", "d", 5760, 0),
    ]);
    // B is unplugged and Hyprland closes the gap: C and D move left by 1920, A stays. A did not
    // move, so the frame did not; C and D follow Hyprland and A keeps the place its neighbours
    // on the plane (a peer placed to the left of A, say) rely on.
    let out = a.observe(&[
        mon("A", "a", 0, 0),
        mon("C", "c", 1920, 0),
        mon("D", "d", 3840, 0),
    ]);
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(plane(&out, "a"), (0, 0));
    assert_eq!(plane(&out, "c"), (1920, 0), "closed the gap with Hyprland");
    assert_eq!(plane(&out, "d"), (3840, 0));
}

/// Finding F-1 of the three-peers run (lab VM c): the two outputs of a QEMU
/// guest both report "Red Hat Inc. QEMU Monitor". By reading order alone the second would take the
/// first one's key and its remembered place when the first goes, and the group would jump by the
/// distance between the twins. The connector, remembered with each entry and matched first, keeps
/// them apart.
#[test]
fn the_second_of_two_identical_monitors_stays_in_place_when_the_first_one_goes() {
    let twin = |name: &str, x: i32| mon(name, "Red Hat Inc. QEMU Monitor", x, 0);
    let mut a = Anchors::default();
    a.observe(&[twin("Virtual-1", 0), twin("Virtual-2", 1920)]);
    // Virtual-1 is turned off and Hyprland, on `auto` positions, moves Virtual-2 to 0
    let out = a.observe(&[twin("Virtual-2", 0)]);
    assert_eq!(
        shift_of(&out),
        (1920, 0),
        "the shift that keeps Virtual-2 at 1920"
    );
    assert_eq!(plane(&out, "Red Hat Inc. QEMU Monitor"), (1920, 0));
    // the same, when Hyprland leaves Virtual-2 at 1920 (an explicit position): nothing may move
    let mut a = Anchors::default();
    a.observe(&[twin("Virtual-1", 0), twin("Virtual-2", 1920)]);
    let out = a.observe(&[twin("Virtual-2", 1920)]);
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(plane(&out, "Red Hat Inc. QEMU Monitor"), (1920, 0));
    // both back: each twin finds its own place, and the memory still holds two entries
    let out = a.observe(&[twin("Virtual-1", 0), twin("Virtual-2", 1920)]);
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(a.monitors.len(), 2);
    assert!(a.check().is_ok());
}

#[test]
fn a_new_twin_gets_a_key_no_entry_holds() {
    let twin = |name: &str, x: i32| mon(name, "Acme 27", x, 0);
    let mut a = Anchors::default();
    a.observe(&[twin("DP-1", 0), twin("DP-2", 1920)]);
    // DP-1 goes, DP-2 keeps its entry "Acme 27#2"; a third twin then joins on DP-3
    a.observe(&[twin("DP-2", 1920)]);
    a.observe(&[twin("DP-2", 1920), twin("DP-3", 3840)]);
    assert!(a.check().is_ok(), "{a:?}");
    assert_eq!(a.monitors.len(), 3);
}

#[test]
fn a_tie_that_has_no_current_shift_goes_to_the_first_monitor_of_the_reading() {
    let mut a = Anchors::default();
    a.observe(&[mon("A", "a", 0, 0), mon("B", "b", 1920, 0)]);
    // the user swaps the two in Hyprland: A proposes -1920, B proposes +1920
    let out = a.observe(&[mon("A", "a", 1920, 0), mon("B", "b", 0, 0)]);
    assert_eq!(shift_of(&out), (-1920, 0));
    assert_eq!(
        plane(&out, "a"),
        (0, 0),
        "the first monitor of the reading is the anchor"
    );
    assert_eq!(plane(&out, "b"), (-1920, 0));
}

/// Finding F-2 of the three-peers run (T5.8 to T5.10, peer b): a plain
/// replug puts the returning monitor where Hyprland likes, and the user then restores the old
/// positions. Both monitors moved; one proposes the displaced frame, the other proposes zero. The
/// tie goes to zero, so the group is where it was and no layout edit is needed.
#[test]
fn a_replug_then_the_old_positions_by_hand_does_not_displace_the_group() {
    let mut a = Anchors::default();
    a.observe(&[mon("Virtual-1", "v1", 0, 0)]);
    // a head is added right of it
    a.observe(&[
        mon("Virtual-1", "v1", 0, 0),
        mon("HEADLESS-2", "h2", 1920, 0),
    ]);
    // Virtual-1 off: Hyprland moves the head to 0, the shift keeps it at 1920
    let out = a.observe(&[mon("HEADLESS-2", "h2", 0, 0)]);
    assert_eq!(shift_of(&out), (1920, 0));
    // T5.8, Virtual-1 on with `auto`: Hyprland puts it right of the head
    let out = a.observe(&[
        mon("HEADLESS-2", "h2", 0, 0),
        mon("Virtual-1", "v1", 1920, 0),
    ]);
    assert_eq!(shift_of(&out), (1920, 0));
    assert_eq!(
        plane(&out, "v1"),
        (3840, 0),
        "as designed: where Hyprland put it"
    );
    // T5.9, the old positions restored by hand: Virtual-1 proposes 3840, the head proposes 0
    let out = a.observe(&[
        mon("Virtual-1", "v1", 0, 0),
        mon("HEADLESS-2", "h2", 1920, 0),
    ]);
    assert_eq!(shift_of(&out), (0, 0), "the group is not displaced");
    assert_eq!(plane(&out, "v1"), (0, 0));
    assert_eq!(plane(&out, "h2"), (1920, 0));
    // T5.10 is not needed; turning Virtual-1 off and on again keeps the frame
    a.observe(&[mon("HEADLESS-2", "h2", 1920, 0)]);
    let out = a.observe(&[
        mon("Virtual-1", "v1", 0, 0),
        mon("HEADLESS-2", "h2", 1920, 0),
    ]);
    assert_eq!(shift_of(&out), (0, 0));
}

#[test]
fn monitors_that_stayed_decide_before_monitors_that_returned() {
    let mut a = Anchors::default();
    a.observe(&desk());
    a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]); // HDMI-A-1 absent, shift (1920, 0)
    // HDMI-A-1 returns at -5000, which would propose (1920+... something else); DVI-D-1 stayed
    // and proposes (0, 0) here, so it decides
    let out = a.observe(&[
        mon("DVI-D-1", "Dell right", 1920, 0),
        mon("HDMI-A-1", "Dell left", -5000, 0),
    ]);
    assert_eq!(shift_of(&out), (0, 0));
    assert_eq!(plane(&out, "Dell right"), (1920, 0));
    assert_eq!(plane(&out, "Dell left"), (-5000, 0));
}

#[test]
fn reading_the_same_monitors_again_changes_nothing() {
    let mut a = Anchors::default();
    a.observe(&desk());
    a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]);
    let once = a.clone();
    let first = a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]);
    assert_eq!(a, once);
    assert_eq!(a.observe(&[mon("DVI-D-1", "Dell right", 0, 0)]), first);
    assert_eq!(a, once);
}

#[test]
fn a_monitor_with_no_description_is_remembered_by_its_connector() {
    let mut a = Anchors::default();
    a.observe(&[mon("DP-1", "", 0, 0), mon("DP-2", "", 1920, 0)]);
    let out = a.observe(&[mon("DP-2", "", 0, 0)]);
    assert_eq!(shift_of(&out), (1920, 0));
}

#[test]
fn the_same_panel_on_another_connector_stays_in_place() {
    let mut a = Anchors::default();
    a.observe(&desk());
    // the dock comes back on other connectors, and Hyprland puts the right panel at 0
    let out = a.observe(&[mon("DP-9", "Dell right", 0, 0)]);
    assert_eq!(shift_of(&out), (1920, 0));
    assert_eq!(plane(&out, "Dell right"), (1920, 0));
}

#[test]
fn the_memory_is_bounded_and_forgets_the_oldest_absent_monitor_first() {
    let mut a = Anchors::default();
    for i in 0..(MAX_REMEMBERED + 10) {
        let name = format!("m{i}");
        a.observe(&[mon(&name, &name, 0, 0)]);
    }
    assert_eq!(a.monitors.len(), MAX_REMEMBERED);
    // the monitor on the desk right now is kept, the oldest ones are gone
    assert!(a.monitors.last().is_some_and(|r| r.present));
    assert!(!a.monitors.iter().any(|r| r.key == "m0"));
    assert!(
        a.monitors
            .iter()
            .any(|r| r.key == format!("m{}", MAX_REMEMBERED + 9))
    );
    assert!(a.check().is_ok());
}

#[test]
fn a_shift_past_the_cap_forgets_the_memory_and_is_not_sent() {
    let far = i32::try_from(caps::COORD).unwrap();
    let mut a = Anchors::default();
    a.observe(&[mon("A", "a", -far, 0)]);
    // Hyprland reports it at +far: the proposal would be -2 * far, which no peer accepts
    let out = a.observe(&[mon("A", "a", far, 0)]);
    assert_eq!(shift_of(&out), (0, 0));
    assert!(out.iter().all(MonitorInfo::in_range));
    assert!(a.check().is_ok());
    assert_eq!(
        a.monitors.len(),
        1,
        "the reading is remembered again from scratch"
    );
}

#[test]
fn a_memory_that_came_from_a_file_is_checked() {
    let good = Remembered {
        key: "a".into(),
        connector: Some("DP-1".into()),
        x: 0,
        y: 0,
        present: true,
    };
    let anchors = |shift, monitors| Anchors { shift, monitors };
    assert!(anchors((5, -5), vec![good.clone()]).check().is_ok());
    let far = i32::try_from(caps::COORD).unwrap() + 1;
    assert_eq!(
        anchors((far, 0), vec![]).check(),
        Err(AnchorsError::ShiftRange)
    );
    let at = |x| Remembered { x, ..good.clone() };
    assert!(matches!(
        anchors((0, 0), vec![at(far * 2 + 1)]).check(),
        Err(AnchorsError::Range(_))
    ));
    assert!(anchors((0, 0), vec![at(far * 2 - 2)]).check().is_ok());
    assert!(matches!(
        anchors(
            (0, 0),
            vec![Remembered {
                key: "a\nb".into(),
                ..good.clone()
            }]
        )
        .check(),
        Err(AnchorsError::Key(_))
    ));
    assert!(matches!(
        anchors(
            (0, 0),
            vec![Remembered {
                connector: Some("DP\n1".into()),
                ..good.clone()
            }]
        )
        .check(),
        Err(AnchorsError::Key(_))
    ));
    let old_file = Remembered {
        connector: None,
        ..good.clone()
    };
    assert!(anchors((0, 0), vec![old_file]).check().is_ok());
    assert_eq!(
        anchors((0, 0), vec![good.clone(), good.clone()]).check(),
        Err(AnchorsError::Duplicate("a".into()))
    );
    let many = (0..=MAX_REMEMBERED)
        .map(|i| Remembered {
            key: format!("k{i}"),
            ..good.clone()
        })
        .collect();
    assert_eq!(anchors((0, 0), many).check(), Err(AnchorsError::TooMany));
}

#[test]
fn the_input_shift_is_ignored_and_everything_else_is_kept() {
    let mut a = Anchors::default();
    let mut reading = desk();
    reading[0].shift = Some(WireOffset { x: 7, y: 7 });
    reading[0].scale = 2.0;
    reading[1].transform = 3;
    let out = a.observe(&reading);
    assert!(out.iter().all(|m| m.shift.is_none()));
    assert_eq!((out[0].scale, out[1].transform), (2.0, 3));
    assert_eq!(out[0].name, "HDMI-A-1");
}

// ---- properties ----------------------------------------------------------------------------

/// 1 to 5 monitors with distinct descriptions, side by side or stacked at random places.
fn arb_reading() -> impl Strategy<Value = Vec<MonitorInfo>> {
    proptest::collection::vec((-3000i32..3000, -3000i32..3000), 1..6).prop_map(|places| {
        places
            .into_iter()
            .enumerate()
            .map(|(i, (x, y))| mon(&format!("DP-{i}"), &format!("panel {i}"), x, y))
            .collect()
    })
}

fn moved(reading: &[MonitorInfo], dx: i32, dy: i32) -> Vec<MonitorInfo> {
    reading
        .iter()
        .map(|m| {
            let mut m = m.clone();
            m.rect.x += dx;
            m.rect.y += dy;
            m
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Whatever subset of the monitors is unplugged and however Hyprland moves the rest as a
    /// whole, the ones that stayed keep their place on the plane.
    #[test]
    fn monitors_that_stayed_keep_their_plane_position(
        reading in arb_reading(),
        keep in proptest::collection::vec(any::<bool>(), 5),
        dx in -4000i32..4000,
        dy in -4000i32..4000,
    ) {
        let mut a = Anchors::default();
        let before = a.observe(&reading);
        let subset: Vec<MonitorInfo> = reading
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(m, _)| m.clone())
            .collect();
        let after = a.observe(&moved(&subset, dx, dy));
        for m in &after {
            prop_assert_eq!(plane(&after, &m.description), plane(&before, &m.description));
        }
        prop_assert!(after.iter().all(MonitorInfo::in_range));
        prop_assert!(a.check().is_ok());
    }

    /// Unplug some, replug them, and Hyprland puts everything back (anywhere, as a whole): every
    /// monitor is where it was.
    #[test]
    fn a_replug_returns_every_monitor_to_its_place(
        reading in arb_reading(),
        keep in proptest::collection::vec(any::<bool>(), 5),
        dx1 in -4000i32..4000,
        dx2 in -4000i32..4000,
        dy2 in -4000i32..4000,
    ) {
        let mut a = Anchors::default();
        let start = a.observe(&reading);
        let subset: Vec<MonitorInfo> = reading
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(m, _)| m.clone())
            .collect();
        a.observe(&moved(&subset, dx1, 0));
        let back = a.observe(&moved(&reading, dx2, dy2));
        for m in &back {
            prop_assert_eq!(plane(&back, &m.description), plane(&start, &m.description));
        }
    }

    /// A reading that is read twice is read once.
    #[test]
    fn reading_twice_is_reading_once(reading in arb_reading(), later in arb_reading()) {
        let mut a = Anchors::default();
        a.observe(&reading);
        let first = a.observe(&later);
        let state = a.clone();
        prop_assert_eq!(a.observe(&later), first);
        prop_assert_eq!(a, state);
    }

    /// The shift stays inside what the wire accepts, and the memory is something a file could
    /// hold, after any sequence of readings.
    #[test]
    fn any_sequence_of_readings_leaves_a_valid_memory(
        readings in proptest::collection::vec(
            proptest::collection::vec((0usize..8, -20_000_000i32..20_000_000, -20_000_000i32..20_000_000), 0..6),
            1..12,
        ),
    ) {
        let mut a = Anchors::default();
        for r in readings {
            let reading: Vec<MonitorInfo> = r
                .iter()
                .enumerate()
                .map(|(i, (id, x, y))| mon(&format!("DP-{i}"), &format!("panel {id}"), *x, *y))
                .collect();
            let out = a.observe(&reading);
            prop_assert!(out.iter().all(MonitorInfo::shift_in_range));
            prop_assert!(a.check().is_ok(), "{a:?}");
            prop_assert_eq!(out.len(), reading.len());
        }
    }
}
