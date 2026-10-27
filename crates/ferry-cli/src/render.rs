//! Human output. Plain text, no colour: it is read in terminals, in `ssh`, and in snapshots.

use std::borrow::Cow;
use std::fmt::Write;

use ferry_core::api::{
    Applies, Attention, ConfigKey, ConfigSchema, ConfigSetResult, ConfigType, ConfigValues,
    IdResult, LayoutResult, LinkState, LockResult, PairingRole, PairingStage, PairingStatus,
    PeerStatus, Role, StatusResult, Summary, SummaryState,
};
use ferry_core::geom::Side;

/// Text that ferryd got from the network (a peer name, an `Error` message, a monitor name), made
/// safe for a terminal: every control character, C0, DEL and C1 (so `ESC`,
/// `CR`, `BEL` and the 8-bit `CSI`), is written as its Rust escape (`\r`, `\u{1b}`), so it can
/// neither move the cursor nor redraw a line. Everything else is kept. `--json` needs none of
/// this: serde escapes control characters itself.
pub fn plain(s: &str) -> Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

fn role(r: &Role) -> &'static str {
    match r {
        Role::Idle => "idle",
        Role::Pending => "pending",
        Role::Arming => "arming",
        Role::Source => "source",
        Role::Target => "target",
    }
}

fn link(s: &LinkState) -> &'static str {
    match s {
        LinkState::Connected => "connected",
        LinkState::Connecting => "connecting",
        LinkState::Offline => "offline",
        LinkState::Refused => "refused",
    }
}

fn side(s: Side) -> &'static str {
    match s {
        Side::Left => "left",
        Side::Right => "right",
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

fn rtt(us: u32) -> String {
    if us < 1000 {
        format!("{us} us")
    } else {
        format!("{:.1} ms", f64::from(us) / 1000.0)
    }
}

/// Left-aligned columns, two spaces apart, no trailing spaces.
fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            let _ = write!(line, "{cell:<width$}", width = widths[i]);
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
    out
}

/// `ferry status`.
pub fn status(s: &StatusResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} (this machine)", s.me.name);
    let mut kv: Vec<Vec<String>> = vec![vec!["  fingerprint".into(), s.me.fingerprint.clone()]];
    if let Some(summary) = &s.summary {
        kv.push(vec!["  state".into(), summary_text(summary)]);
    }
    kv.extend([
        vec!["  port".into(), format!("{}/udp", s.port)],
        vec![
            "  role".into(),
            match &s.active_peer {
                Some(p) => format!("{} ({})", role(&s.role), p.name),
                None => role(&s.role).to_string(),
            },
        ],
        vec![
            "  capture".into(),
            format!(
                "{} ({}, {} barrier{}{}{})",
                s.capture.backend,
                s.capture.gate,
                s.capture.barriers,
                if s.capture.barriers == 1 { "" } else { "s" },
                if s.capture.failed_barriers.is_empty() {
                    String::new()
                } else {
                    format!(", {} failed", s.capture.failed_barriers.len())
                },
                match s.capture.off_reason.as_deref() {
                    Some("controlled") => ", off: another peer drives this one".to_string(),
                    Some("locked_here") => ", off: locked to this machine".to_string(),
                    Some(other) => format!(", off: {other}"),
                    None => String::new(),
                }
            ),
        ],
        vec![
            "  lock".into(),
            if s.locked {
                "on: this machine keeps the pointer".into()
            } else {
                "off".into()
            },
        ],
        vec![
            "  screen".into(),
            match s.screen_locked {
                Some(true) => "locked".into(),
                Some(false) => "unlocked".into(),
                None => "unknown".into(),
            },
        ],
    ]);
    if let Some(chord) = &s.escape_chord {
        kv.push(vec!["  escape".into(), plain(chord).into_owned()]);
    }
    if let Some(p) = &s.pairing {
        kv.push(vec!["  pairing".into(), pairing(p)]);
    }
    out.push_str(&table(&kv));
    if let Some(n) = &s.capture.notice {
        let _ = writeln!(out, "  note: {n}");
    }
    if !s.attention.is_empty() {
        out.push_str("\nAttention\n");
        for a in &s.attention {
            attention(&mut out, a);
        }
    }

    out.push('\n');
    if s.peers.is_empty() {
        out.push_str(
            "No peers. Pin one with: ferry trust <fingerprint> --name <name> --addr <ip>\n",
        );
    } else {
        out.push_str("Peers\n");
        let mut rows = vec![vec![
            "  NAME".to_string(),
            "STATE".into(),
            "ADDRESS".into(),
            "RTT".into(),
            "PROTO".into(),
            "LAST SEEN".into(),
        ]];
        // The LOCK column shows only when a peer is locked to itself.
        let any_locked = s.peers.iter().any(|p| p.locked);
        if any_locked {
            rows[0].push("LOCK".into());
        }
        for p in &s.peers {
            let mut row = peer_row(p);
            if any_locked {
                row.push(if p.locked { "locked" } else { "-" }.into());
            }
            rows.push(row);
        }
        out.push_str(&table(&rows));
        for p in s.peers.iter().filter(|p| p.detail.is_some()) {
            let _ = writeln!(
                out,
                "  {}: {}",
                p.name,
                plain(p.detail.as_deref().unwrap_or_default())
            );
        }
        for p in s.peers.iter().filter(|p| p.suspect) {
            let _ = writeln!(
                out,
                "  {}: suspect: released for silence and not heard since; its edges are walls until it answers a ping",
                p.name
            );
        }
    }

    if let Some(r) = &s.last_release {
        let _ = writeln!(out, "\nLast release: {}, {} ago", r.reason, ago(r.ago_ms));
    }
    if !s.counters.is_empty() {
        let c: Vec<String> = s.counters.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(out, "Counters: {}", c.join(" "));
    }
    out
}

/// `away (lap)`: the word a bar widget shows, and the peer it is about.
fn summary_text(s: &Summary) -> String {
    let word = match s.state {
        SummaryState::Idle => "idle",
        SummaryState::Ready => "ready",
        SummaryState::Away => "away",
        SummaryState::Controlled => "controlled",
        SummaryState::Locked => "locked",
        SummaryState::Attention => "attention",
        SummaryState::Unknown => "unknown",
    };
    match &s.peer {
        Some(p) => format!("{word} ({})", plain(p)),
        None => word.to_string(),
    }
}

fn attention(out: &mut String, a: &Attention) {
    let _ = writeln!(out, "  {}", plain(&a.message));
    if let Some(hint) = &a.hint {
        let _ = writeln!(out, "    hint: {}", plain(hint));
    }
}

fn peer_row(p: &PeerStatus) -> Vec<String> {
    vec![
        format!("  {}", p.name),
        if p.suspect {
            format!("{} (suspect)", link(&p.state))
        } else {
            link(&p.state).to_string()
        },
        p.addr.clone().unwrap_or_else(|| "-".into()),
        p.rtt_us.map_or_else(|| "-".into(), rtt),
        p.proto.clone().unwrap_or_else(|| "-".into()),
        if p.state == LinkState::Connected {
            "now".into()
        } else {
            p.last_seen.clone().unwrap_or_else(|| "-".into())
        },
    ]
}

/// `status.pairing`: what this machine is doing in a pairing, never the code.
fn pairing(p: &PairingStatus) -> String {
    let role = match p.role {
        PairingRole::Acceptor => "accepting",
        PairingRole::Dialer => "asking a peer",
    };
    let stage = match p.stage {
        PairingStage::Listening => "waiting for a request",
        PairingStage::Connecting => "connecting",
        PairingStage::Code => "comparing codes",
        PairingStage::WaitingPeer => "waiting for the other side",
        PairingStage::Paired => "paired",
        PairingStage::Failed => "failed",
        PairingStage::Closed => "closed",
    };
    format!("{role}: {stage}, {} s left", p.expires_ms.div_ceil(1000))
}

/// A config value as `ferry config get KEY` prints it: bare, so `$(ferry config get KEY)` is the
/// value.
fn value_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => plain(s).into_owned(),
        other => other.to_string(),
    }
}

/// `when a change takes effect`, for `config set` and `config schema`.
fn applies_text(a: Applies) -> &'static str {
    match a {
        Applies::Live => "applies at once",
        Applies::Restart => "applies after a restart of ferryd (systemctl --user restart ferryd)",
        Applies::Setup => "applies at once in ferryd; `ferry setup` updates the Hyprland bind",
        Applies::Unknown => "applies at a time this ferry does not know",
    }
}

/// `ferry config get [KEY]`: one value bare, or a table of every key.
pub fn config_values(v: &ConfigValues, one: Option<&str>) -> String {
    if one.is_some() {
        return v
            .values
            .values()
            .next()
            .map_or_else(String::new, |x| format!("{}\n", value_text(x)));
    }
    let rows: Vec<Vec<String>> = v
        .values
        .iter()
        .map(|(k, x)| {
            let shown = match x {
                serde_json::Value::String(s) if s.is_empty() => "\"\"".to_string(),
                other => value_text(other),
            };
            let note = if v.pending.contains(k) {
                "(saved; applies after a restart of ferryd)"
            } else {
                ""
            };
            vec![k.clone(), shown, note.to_string()]
        })
        .collect();
    table(&rows)
}

/// `ferry config set KEY VALUE` after ferryd saved it.
pub fn config_set(r: &ConfigSetResult) -> String {
    let when = match r.applies {
        Applies::Live => "applied",
        Applies::Restart => "saved; applies after systemctl --user restart ferryd",
        Applies::Setup => "applied; run `ferry setup` to update the Hyprland bind",
        Applies::Unknown => "saved",
    };
    format!("{} = {} ({when})\n", plain(&r.key), value_text(&r.value))
}

/// What a key accepts, in words.
fn describe(k: &ConfigKey) -> String {
    match k.kind {
        ConfigType::Bool => "true or false".to_string(),
        ConfigType::Int => match (k.min, k.max) {
            (Some(min), Some(max)) => format!(
                "a whole number, {min} to {max}{}",
                k.unit
                    .as_deref()
                    .map_or_else(String::new, |u| format!(" {u}"))
            ),
            _ => "a whole number".to_string(),
        },
        ConfigType::Enum => format!(
            "one of {}",
            k.choices.as_deref().unwrap_or_default().join(", ")
        ),
        ConfigType::Chord => "a key combination, such as Super+Ctrl+Escape".to_string(),
        ConfigType::Name => "a name without spaces or ':' (empty: the hostname)".to_string(),
        ConfigType::String => "text".to_string(),
        ConfigType::Unknown => "a value of a type this ferry does not know".to_string(),
    }
}

/// `ferry config schema`: each key with what it accepts, its default and when a change applies.
pub fn config_schema(s: &ConfigSchema) -> String {
    let mut out = String::new();
    for k in &s.keys {
        let default = match &k.default {
            serde_json::Value::String(d) if d.is_empty() => "(empty)".to_string(),
            other => value_text(other),
        };
        let _ = writeln!(out, "{}", plain(&k.key));
        let _ = writeln!(
            out,
            "    {}; default {default}; {}",
            describe(k),
            applies_text(k.applies)
        );
        let _ = writeln!(out, "    {}", plain(&k.doc));
        if !k.available {
            // saved and read back, but nothing in this version acts on it yet
            let _ = writeln!(out, "    Not available yet: it arrives in a later version.");
        }
    }
    out
}

/// `ferry lock on|off|toggle`.
pub fn lock(r: &LockResult) -> String {
    if r.locked {
        "locked: this machine keeps the pointer\n".to_string()
    } else {
        "unlocked\n".to_string()
    }
}

fn ago(ms: u64) -> String {
    match ms {
        0..1000 => format!("{ms} ms"),
        1000..60_000 => format!("{} s", ms / 1000),
        60_000..3_600_000 => format!("{} min", ms / 60_000),
        _ => format!("{} h", ms / 3_600_000),
    }
}

/// `ferry id`: the facts, then the line to paste on the other machine.
pub fn id(r: &IdResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", r.name);
    let _ = writeln!(out, "  fingerprint  {}", r.fingerprint);
    let _ = writeln!(out, "  port         {}/udp", r.port);
    if r.addrs.is_empty() {
        let _ = writeln!(out, "  addresses    (none found)");
    } else {
        let _ = writeln!(out, "  addresses    {}", r.addrs.join(", "));
    }
    let _ = writeln!(out, "\nTo pair, run this on the other machine:");
    let _ = writeln!(out, "  {}", paste_line(r));
    out
}

/// `ferry trust <fp> --name <name> --addr <ip>:<port>`. With several addresses it
/// takes the first, which ferryd lists IPv4 first. Without one it leaves `--addr` out.
pub fn paste_line(r: &IdResult) -> String {
    let mut line = format!("ferry trust {} --name {}", r.fingerprint, r.name);
    if let Some(addr) = r.addrs.first() {
        // an IPv6 literal needs brackets before the port
        if addr.contains(':') {
            let _ = write!(line, " --addr [{addr}]:{}", r.port);
        } else {
            let _ = write!(line, " --addr {addr}:{}", r.port);
        }
    }
    line
}

/// `ferry trust` after ferryd answered.
pub fn trusted(name: &str, fingerprint: &str, addr: Option<&str>) -> String {
    let mut out = format!("Trusted {name} ({fingerprint})\n");
    match addr {
        Some(a) => {
            let _ = writeln!(
                out,
                "ferryd is connecting to {a}; `ferry status` shows the link."
            );
        }
        None => out.push_str(
            "No address given: this machine accepts its connection and does not dial it.\n",
        ),
    }
    out
}

/// `ferry layout show`, and the answer to `ferry layout place`.
pub fn layout(l: &LayoutResult) -> String {
    let mut out = String::new();
    let by = l
        .peers
        .iter()
        .find(|p| p.fingerprint == l.revision.peer)
        .map_or_else(
            || l.revision.peer.chars().take(8).collect(),
            |p| p.name.clone(),
        );
    let _ = writeln!(
        out,
        "Layout revision {} (set by {by})\n",
        l.revision.counter
    );

    // An older ferryd does not say which peer is this machine: it lists it first.
    let says_self = l.peers.iter().any(|p| p.is_self);
    for (i, p) in l.peers.iter().enumerate() {
        let mut head = p.name.clone();
        if p.is_self || (!says_self && i == 0) {
            head.push_str(" (this machine)");
        }
        let _ = match p.origin {
            Some((x, y)) => writeln!(out, "{head}  origin ({x}, {y})  {}", online(p.online)),
            None => writeln!(out, "{head}  not placed  {}", online(p.online)),
        };
        if !p.trusted {
            out.push_str("  not paired with this machine: nothing crosses to it from here\n");
        }
        if let Some((sx, sy)) = p.shift {
            let _ = writeln!(
                out,
                "  Hyprland moved these monitors; the group is kept in place by a shift of ({sx}, {sy})"
            );
        }
        if p.monitors.is_empty() {
            out.push_str("  no monitors known\n");
        }
        let rows: Vec<Vec<String>> = p
            .monitors
            .iter()
            .map(|m| {
                vec![
                    format!("  {}", plain(&m.name)),
                    format!("{}x{}", m.w, m.h),
                    format!(
                        "at ({}, {}){}",
                        m.x,
                        m.y,
                        if p.origin.is_some() {
                            " on the plane"
                        } else {
                            " local"
                        }
                    ),
                    if (m.scale - 1.0).abs() > f32::EPSILON {
                        format!("scale {}", m.scale)
                    } else {
                        String::new()
                    },
                ]
            })
            .collect();
        out.push_str(&table(&rows));
    }

    out.push('\n');
    if l.links.is_empty() {
        out.push_str("No links. Place a peer next to this machine with: ferry layout place <peer> --below|--above|--left-of|--right-of <anchor>\n");
    } else {
        out.push_str("Links\n");
        let rows: Vec<Vec<String>> = l
            .links
            .iter()
            .map(|k| {
                vec![
                    format!("  {}", plain(&k.from)),
                    format!("--{}-->", side(k.side)),
                    plain(&k.to).into_owned(),
                    format!("span {}..{}", k.span.0, k.span.1),
                    if k.live { "live" } else { "not live" }.to_string(),
                ]
            })
            .collect();
        out.push_str(&table(&rows));
    }
    out
}

/// `ferry peers`.
pub fn peers(r: &ferry_core::api::PeersResult) -> String {
    use ferry_core::api::Paired;
    if r.peers.is_empty() {
        return "No peers. Pair one with: ferry pair --accept (there), then ferry pair <host> (here)\n"
            .into();
    }
    let mut rows = vec![vec![
        "NAME".to_string(),
        "KEY".into(),
        "ADDRESS".into(),
        "LAST SEEN".into(),
        "PAIRED".into(),
        "STATE".into(),
        "LOCK".into(),
    ]];
    for p in &r.peers {
        let key = ferry_core::PeerId::parse(&p.fingerprint).map_or_else(
            |_| p.fingerprint.chars().take(16).collect(),
            |id| id.short(),
        );
        rows.push(vec![
            p.name.clone(),
            key,
            p.addr.clone().unwrap_or_else(|| "-".into()),
            if p.state == LinkState::Connected {
                "now".into()
            } else {
                p.last_seen.clone().unwrap_or_else(|| "never".into())
            },
            match p.paired {
                Paired::Sas => "code",
                Paired::Manual => "by hand",
            }
            .to_string(),
            link(&p.state).to_string(),
            if p.locked { "locked" } else { "-" }.to_string(),
        ]);
    }
    table(&rows)
}

fn online(up: bool) -> &'static str {
    if up { "online" } else { "offline" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::api::*;

    fn fp(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn peer(name: &str, f: char, state: LinkState) -> PeerStatus {
        PeerStatus {
            name: name.into(),
            fingerprint: fp(f),
            state,
            addr: None,
            rtt_us: None,
            proto: None,
            detail: None,
            suspect: false,
            locked: false,
            last_seen: None,
        }
    }

    #[test]
    fn peers_is_a_table_of_pins_with_state_and_lock() {
        let r = PeersResult {
            peers: vec![
                PeerEntry {
                    name: "laptop".into(),
                    fingerprint: format!("9c2e41aa0b7d5512{}", fp('0').split_at(16).1),
                    addr: Some("192.0.2.11:4243".into()),
                    last_seen: Some("2026-10-18T09:12:03Z".into()),
                    paired: Paired::Sas,
                    state: LinkState::Connected,
                    locked: false,
                },
                PeerEntry {
                    name: "lap".into(),
                    fingerprint: fp('a'),
                    addr: None,
                    last_seen: Some("2026-10-17T21:40:00Z".into()),
                    paired: Paired::Manual,
                    state: LinkState::Offline,
                    locked: true,
                },
                PeerEntry {
                    name: "tab".into(),
                    fingerprint: fp('b'),
                    addr: None,
                    last_seen: None,
                    paired: Paired::Sas,
                    state: LinkState::Refused,
                    locked: false,
                },
            ],
        };
        insta::assert_snapshot!(peers(&r));
        assert!(peers(&PeersResult { peers: vec![] }).starts_with("No peers."));
    }

    fn sample_status() -> StatusResult {
        let mut counters = std::collections::BTreeMap::new();
        counters.insert("releases".to_string(), 2);
        StatusResult {
            api: 1,
            me: PeerRef {
                name: "desk".into(),
                fingerprint: fp('4'),
            },
            port: 4243,
            role: Role::Idle,
            active_peer: None,
            capture: CaptureStatus {
                backend: "portal".into(),
                gate: "armed".into(),
                barriers: 1,
                failed_barriers: vec![],
                off_reason: None,
                notice: None,
            },
            screen_locked: Some(false),
            locked: false,
            peers: vec![
                PeerStatus {
                    addr: Some("192.0.2.11:4243".into()),
                    rtt_us: Some(412),
                    proto: Some("1.0".into()),
                    ..peer("laptop", '9', LinkState::Connected)
                },
                PeerStatus {
                    detail: Some(format!("peer {} does not trust this machine", fp('a'))),
                    ..peer("lap", 'a', LinkState::Refused)
                },
                PeerStatus {
                    rtt_us: Some(2600),
                    ..peer("tab", 'b', LinkState::Connecting)
                },
            ],
            last_release: Some(LastRelease {
                reason: "escape_chord".into(),
                ago_ms: 12_000,
                mono_us: None,
            }),
            counters,
            pairing: None,
            summary: Some(Summary {
                state: SummaryState::Ready,
                peer: None,
            }),
            attention: vec![],
            escape_chord: Some("Super+Ctrl+Escape".into()),
        }
    }

    #[test]
    fn status_shows_this_machine_then_its_peers() {
        insta::assert_snapshot!(status(&sample_status()));
    }

    #[test]
    fn status_shows_a_suspect_peer() {
        let mut s = sample_status();
        s.peers.truncate(1);
        s.peers[0].suspect = true;
        s.last_release = Some(LastRelease {
            reason: "silence".into(),
            ago_ms: 1500,
            mono_us: None,
        });
        insta::assert_snapshot!(status(&s));
    }

    #[test]
    fn status_shows_both_locks_and_why_capture_is_off() {
        let mut s = sample_status();
        s.locked = true;
        s.capture.gate = "idle".into();
        s.capture.barriers = 0;
        s.capture.off_reason = Some("locked_here".into());
        s.peers.truncate(2);
        s.peers[0].locked = true;
        s.counters.clear();
        insta::assert_snapshot!(status(&s));
        s.capture.off_reason = Some("controlled".into());
        s.locked = false;
        s.peers[0].locked = false;
        let text = status(&s);
        assert!(text.contains("off: another peer drives this one"), "{text}");
        assert!(!text.contains("LOCK"), "{text}");
    }

    /// Every field the step 6 streams added to `status`, in one answer: each stream rendered its own
    /// and none showed another's. The snapshot is the contract for the human text.
    #[test]
    fn status_shows_every_step_6_field_together() {
        let mut s = sample_status();
        s.locked = true;
        s.capture.gate = "idle".into();
        s.capture.barriers = 0;
        s.capture.off_reason = Some("locked_here".into());
        s.capture.notice = Some(
            "ecosystem:enforce_permissions is on and no crossing has started yet: answer Hyprland's permission dialog at the first crossing, or allow xdph in ferry.lua and restart Hyprland, which reads the rule only when it starts".into(),
        );
        s.pairing = Some(PairingStatus {
            role: PairingRole::Acceptor,
            stage: PairingStage::Listening,
            expires_ms: 74_300,
        });
        s.peers.truncate(2);
        s.peers[0].locked = true;
        s.peers[0].last_seen = Some("2026-10-18T09:12:03Z".into());
        s.peers[1].last_seen = Some("2026-10-17T21:40:00Z".into());
        s.counters.clear();
        let text = status(&s);
        insta::assert_snapshot!(text);
        for must in [
            "off: locked to this machine",
            "note: ecosystem:enforce_permissions",
            "accepting: waiting for a request, 75 s left",
            "LOCK",
            "LAST SEEN",
            "2026-10-17T21:40:00Z",
        ] {
            assert!(text.contains(must), "{must:?} missing:\n{text}");
        }
        // the JSON carries the same six, under the names the design gives
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["locked"], true);
        assert_eq!(v["capture"]["off_reason"], "locked_here");
        assert!(v["capture"]["notice"].is_string());
        assert_eq!(v["pairing"]["stage"], "listening");
        assert_eq!(v["peers"][0]["locked"], true);
        assert_eq!(v["peers"][1]["last_seen"], "2026-10-17T21:40:00Z");
    }

    #[test]
    fn lock_says_what_locked_means_and_unlocked_plainly() {
        assert_eq!(
            lock(&LockResult { locked: true }),
            "locked: this machine keeps the pointer\n"
        );
        assert_eq!(lock(&LockResult { locked: false }), "unlocked\n");
    }

    #[test]
    fn status_without_peers_says_how_to_pair() {
        let mut s = sample_status();
        s.peers.clear();
        s.last_release = None;
        s.counters.clear();
        s.screen_locked = None;
        s.role = Role::Source;
        s.active_peer = Some(PeerRef {
            name: "laptop".into(),
            fingerprint: fp('9'),
        });
        s.summary = Some(Summary {
            state: SummaryState::Away,
            peer: Some("laptop".into()),
        });
        insta::assert_snapshot!(status(&s));
    }

    /// The problems come right under the facts, each with what to do about it, and the state row
    /// says `attention`. Text a peer could influence is escaped like everywhere else.
    #[test]
    fn status_lists_what_needs_attention() {
        let mut s = sample_status();
        s.summary = Some(Summary {
            state: SummaryState::Attention,
            peer: None,
        });
        s.attention = vec![
            Attention {
                code: AttentionCode::PeerRefused,
                message: "lap cannot connect: version mismatch: 1.0 vs 2.0".into(),
                hint: Some(
                    "`ferry doctor` has the details. After a key change or if it unpaired this machine, `ferry unpair lap` and pair again; after a version mismatch, update the older ferry"
                        .into(),
                ),
                peer: Some("lap".into()),
            },
            Attention {
                code: AttentionCode::ConfigInvalid,
                message: "config.toml: unknown key peer.nmae\r\u{1b}[2K".into(),
                hint: None,
                peer: None,
            },
        ];
        let text = status(&s);
        insta::assert_snapshot!(text);
        assert!(clean(&text), "{text:?}");
        // an older ferryd sends none of it, and the text has none of it
        s.summary = None;
        s.attention.clear();
        s.escape_chord = None;
        let text = status(&s);
        let has_row = |row: &str| text.lines().any(|l| l.starts_with(row));
        assert!(!has_row("  state ") && !has_row("  escape ") && !has_row("Attention"));
    }

    #[test]
    fn every_summary_state_has_its_word() {
        for (state, peer, shown) in [
            (SummaryState::Idle, None, "idle"),
            (SummaryState::Ready, None, "ready"),
            (SummaryState::Away, Some("lap"), "away (lap)"),
            (SummaryState::Controlled, Some("lap"), "controlled (lap)"),
            (SummaryState::Locked, None, "locked"),
            (SummaryState::Attention, None, "attention"),
        ] {
            let summary = Summary {
                state,
                peer: peer.map(str::to_string),
            };
            assert_eq!(summary_text(&summary), shown);
        }
    }

    fn sample_schema() -> ConfigSchema {
        ConfigSchema {
            keys: vec![
                ConfigKey {
                    key: "peer.name".into(),
                    kind: ConfigType::Name,
                    default: serde_json::json!(""),
                    min: None,
                    max: None,
                    unit: None,
                    slider_max: None,
                    choices: None,
                    applies: Applies::Restart,
                    doc: "This machine's name".into(),
                    available: true,
                },
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
                    doc: "How far to push".into(),
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
                    doc: "The keys that give input back".into(),
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
                ConfigKey {
                    key: "input.scroll_invert".into(),
                    kind: ConfigType::Bool,
                    default: serde_json::json!(false),
                    min: None,
                    max: None,
                    unit: None,
                    slider_max: None,
                    choices: None,
                    applies: Applies::Restart,
                    doc: "Invert scrolling".into(),
                    available: false,
                },
            ],
        }
    }

    #[test]
    fn the_schema_says_what_each_key_accepts_and_when_it_applies() {
        insta::assert_snapshot!(config_schema(&sample_schema()));
    }

    #[test]
    fn config_values_are_bare_for_one_key_and_a_table_for_all() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            "crossing.edge_resistance_px".to_string(),
            serde_json::json!(40),
        );
        values.insert("peer.name".to_string(), serde_json::json!(""));
        values.insert("peer.port".to_string(), serde_json::json!(4300));
        values.insert("input.scroll_invert".to_string(), serde_json::json!(true));
        let all = ConfigValues {
            values,
            pending: vec!["peer.port".into()],
        };
        insta::assert_snapshot!(config_values(&all, None));
        let one = ConfigValues {
            values: [(
                "crossing.edge_resistance_px".to_string(),
                serde_json::json!(40),
            )]
            .into(),
            pending: vec![],
        };
        assert_eq!(
            config_values(&one, Some("crossing.edge_resistance_px")),
            "40\n"
        );
        let name = ConfigValues {
            values: [("peer.name".to_string(), serde_json::json!("desk"))].into(),
            pending: vec![],
        };
        assert_eq!(config_values(&name, Some("peer.name")), "desk\n");
    }

    #[test]
    fn config_set_says_whether_the_value_is_in_force() {
        let set = |applies| {
            config_set(&ConfigSetResult {
                key: "crossing.edge_resistance_px".into(),
                value: serde_json::json!(40),
                applies,
            })
        };
        assert_eq!(
            set(Applies::Live),
            "crossing.edge_resistance_px = 40 (applied)\n"
        );
        assert_eq!(
            set(Applies::Restart),
            "crossing.edge_resistance_px = 40 (saved; applies after systemctl --user restart ferryd)\n"
        );
        assert!(set(Applies::Setup).contains("ferry setup"));
    }

    #[test]
    fn id_text_and_the_paste_line() {
        let r = IdResult {
            name: "desk".into(),
            fingerprint: fp('4'),
            port: 4243,
            addrs: vec!["192.0.2.111".into(), "10.0.0.2".into()],
        };
        insta::assert_snapshot!(id(&r));
        assert_eq!(
            paste_line(&r),
            format!(
                "ferry trust {} --name desk --addr 192.0.2.111:4243",
                fp('4')
            )
        );
    }

    #[test]
    fn the_paste_line_without_or_with_ipv6() {
        let mut r = IdResult {
            name: "d".into(),
            fingerprint: fp('1'),
            port: 5000,
            addrs: vec![],
        };
        assert_eq!(paste_line(&r), format!("ferry trust {} --name d", fp('1')));
        r.addrs = vec!["fd00::2".into()];
        assert_eq!(
            paste_line(&r),
            format!("ferry trust {} --name d --addr [fd00::2]:5000", fp('1'))
        );
        assert!(id(&r).contains("(none found)") || id(&r).contains("fd00::2"));
    }

    fn sample_layout() -> LayoutResult {
        LayoutResult {
            revision: RevisionView {
                counter: 3,
                peer: fp('4'),
            },
            peers: vec![
                LayoutPeer {
                    name: "desk".into(),
                    fingerprint: fp('4'),
                    origin: Some((0, 0)),
                    monitors: vec![
                        MonitorView {
                            name: "HDMI-A-1".into(),
                            x: 0,
                            y: 0,
                            w: 1920,
                            h: 1080,
                            scale: 1.0,
                            description: "Dell left".into(),
                            transform: 0,
                        },
                        MonitorView {
                            name: "DVI-D-1".into(),
                            x: 1920,
                            y: 0,
                            w: 1920,
                            h: 1080,
                            scale: 1.0,
                            description: "Dell right".into(),
                            transform: 0,
                        },
                    ],
                    online: true,
                    trusted: true,
                    is_self: true,
                    bounds: None,
                    shift: None,
                },
                LayoutPeer {
                    name: "laptop".into(),
                    fingerprint: fp('9'),
                    origin: Some((277, 1080)),
                    monitors: vec![MonitorView {
                        name: "eDP-1".into(),
                        x: 277,
                        y: 1080,
                        w: 1366,
                        h: 768,
                        scale: 1.0,
                        description: "Chimei Innolux".into(),
                        transform: 0,
                    }],
                    online: false,
                    trusted: true,
                    is_self: false,
                    bounds: None,
                    shift: None,
                },
                LayoutPeer {
                    name: "lap".into(),
                    fingerprint: fp('a'),
                    origin: None,
                    monitors: vec![],
                    online: false,
                    trusted: true,
                    is_self: false,
                    bounds: None,
                    shift: None,
                },
            ],
            links: vec![
                LinkView {
                    from: "desk:HDMI-A-1".into(),
                    side: Side::Bottom,
                    to: "laptop:eDP-1".into(),
                    span: (277, 1643),
                    live: false,
                },
                LinkView {
                    from: "laptop:eDP-1".into(),
                    side: Side::Top,
                    to: "desk:HDMI-A-1".into(),
                    span: (277, 1643),
                    live: false,
                },
            ],
        }
    }

    #[test]
    fn layout_lists_each_peer_with_its_origin_state_and_monitors() {
        insta::assert_snapshot!(layout(&sample_layout()));
    }

    #[test]
    fn layout_names_a_peer_that_is_not_paired_and_a_group_hyprland_moved() {
        let mut l = sample_layout();
        l.peers[0].shift = Some((1920, 0));
        l.peers.push(LayoutPeer {
            name: "4b1f0a9c".into(),
            fingerprint: fp('b'),
            origin: Some((-1920, 0)),
            monitors: vec![],
            online: false,
            trusted: false,
            is_self: false,
            bounds: None,
            shift: None,
        });
        insta::assert_snapshot!(layout(&l));
    }

    #[test]
    fn an_older_ferryd_lists_this_machine_first_and_the_header_says_so() {
        // no peer says `self`: the first one is this machine, as before
        let mut l = sample_layout();
        for p in &mut l.peers {
            p.is_self = false;
        }
        assert!(layout(&l).contains("desk (this machine)  origin (0, 0)"));
        // a daemon that does say it is believed, wherever the peer is listed
        l.peers.swap(0, 1);
        l.peers[0].is_self = false;
        l.peers[1].is_self = true;
        let text = layout(&l);
        assert!(text.contains("desk (this machine)"), "{text}");
        assert!(!text.contains("laptop (this machine)"), "{text}");
    }

    #[test]
    fn plain_escapes_control_characters_only() {
        assert!(matches!(plain("eDP-1 é ✓"), Cow::Borrowed("eDP-1 é ✓")));
        assert_eq!(
            plain("B\r\u{1b}[2A\u{7}\u{9b}2K\n"),
            "B\\r\\u{1b}[2A\\u{7}\\u{9b}2K\\n"
        );
    }

    /// No byte below 0x20 except `\n`, and no DEL or C1.
    fn clean(text: &str) -> bool {
        text.chars().all(|c| c == '\n' || !c.is_control())
    }

    #[test]
    fn a_monitor_or_link_name_with_escapes_is_shown_escaped() {
        let mut l = sample_layout();
        l.peers[1].monitors[0].name = "eDP-1\r\u{1b}[2K".into();
        l.links[0].to = "laptop:eDP-1\r\u{1b}[2K".into();
        l.links[1].from = "laptop:eDP-1\r\u{1b}[2K".into();
        let text = layout(&l);
        assert!(clean(&text), "{text:?}");
        assert!(text.contains("  eDP-1\\r\\u{1b}[2K "), "{text}");
        assert!(text.contains("laptop:eDP-1\\r\\u{1b}[2K"), "{text}");
    }

    #[test]
    fn a_peer_error_detail_with_escapes_is_shown_escaped() {
        let mut s = sample_status();
        s.peers[1].detail = Some("version mismatch\r\u{1b}[1A\u{1b}[2Kall good".into());
        let text = status(&s);
        assert!(clean(&text), "{text:?}");
        assert!(
            text.contains("  lap: version mismatch\\r\\u{1b}[1A\\u{1b}[2Kall good"),
            "{text}"
        );
    }

    #[test]
    fn layout_without_links_says_how_to_place() {
        let mut l = sample_layout();
        l.links.clear();
        l.peers.truncate(1);
        l.peers[0].origin = None;
        l.peers[0].monitors[0].scale = 1.25;
        insta::assert_snapshot!(layout(&l));
    }

    #[test]
    fn trusted_tells_whether_the_peer_will_be_dialled_or_only_accepted() {
        insta::assert_snapshot!(format!(
            "{}{}",
            trusted("laptop", &fp('9'), Some("192.0.2.11:4243")),
            trusted("lap", &fp('a'), None)
        ));
    }

    #[test]
    fn ago_and_rtt_round_to_the_unit_that_reads_best() {
        assert_eq!(ago(10), "10 ms");
        assert_eq!(ago(12_000), "12 s");
        assert_eq!(ago(120_000), "2 min");
        assert_eq!(ago(7_200_000), "2 h");
        assert_eq!(rtt(412), "412 us");
        assert_eq!(rtt(2600), "2.6 ms");
    }
}
