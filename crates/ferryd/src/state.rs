//! Everything the `ctl` thread owns, and the pure lookups and views over it. The socket methods
//! in `api.rs` are thin wrappers around these.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use ferry_core::PeerId;
use ferry_core::api::{
    API_VERSION, ApiError, Attention, AttentionCode, CaptureStatus, EdgeEvent, EdgeRole, ErrorCode,
    IdResult, LastRelease, LayoutPeer, LayoutResult, LinkState, LinkView, MonitorView, PeerEntry,
    PeerRef, PeerStatus, PeersResult, RevisionView, Role, StatusResult, Summary, SummaryFacts,
};
use ferry_core::chord::EscapeChord;
use ferry_core::geom::{
    self, EdgeLink, MonitorId, MonitorInfo, PeerMonitors, Point, Rect, Side, Topology,
};
use ferry_core::source::SourceRole;
use ferry_core::wire::LeaveReason;
use ferry_net::Fingerprint;

use crate::chan::{Crossed, LinkUpdate};
use crate::config::Config;
use crate::layout_store::{LayoutStore, peer_hex, peer_id};
use crate::layout_sync::EditLog;
use crate::live::SharedLive;
use crate::trust_store::TrustStore;

/// This peer.
#[derive(Debug, Clone)]
pub struct Me {
    pub name: String,
    pub fp: Fingerprint,
    pub id: PeerId,
}

/// What `net` last said about a pinned peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub state: LinkState,
    pub remote: Option<String>,
    pub rtt_us: Option<u32>,
    pub proto: Option<String>,
    pub detail: Option<String>,
}

/// What [`State::inbound_unknown`] made of a refused handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inbound {
    /// The new peer of a pairing, dialling before its pin was live: not a refusal.
    BeforePin,
    /// A key not pinned here; `first` for its first refusal in this run.
    Refused { first: bool },
}

/// A peer that tried to connect with a fingerprint we have not pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub fingerprint: Fingerprint,
}

/// Unknown peers listed in `status`; older ones fall off.
const MAX_REFUSALS: usize = 8;

pub struct State {
    pub me: Me,
    /// The port the endpoint is bound to (the config says 0 in tests).
    pub port: u16,
    /// What this ferryd runs with: the file as read at start, and the live keys a `config.set`
    /// changed since. A key that applies after a restart is not here until then.
    pub config: Config,
    /// What `config.toml` says now: `config` plus every `config.set` since start, applied or not.
    pub saved: Config,
    /// What the loader said about the file at start (unknown keys, clamped values): a problem
    /// the user must fix, shown as `attention` `config_invalid`.
    pub config_warnings: Vec<String>,
    pub trust: TrustStore,
    pub layout: LayoutStore,
    /// The layout edits this peer made, for FR-LAYOUT-10.
    pub edits: EditLog,
    /// The own monitors as peers get them: with the anchor shift.
    pub own_monitors: Vec<MonitorInfo>,
    pub links: HashMap<Fingerprint, LinkInfo>,
    pub refusals: Vec<Refusal>,
    /// The unknown keys whose refusal was logged at WARN in this run: a peer that unpaired this
    /// one keeps dialling, and its later refusals go to debug.
    pub refusals_warned: HashSet<Fingerprint>,
    /// Peers this machine paired as the dialer (the initiating peer) and has not placed yet:
    /// FR-LAYOUT-12 places each right of this machine's group once its monitors are known, at
    /// its first connection. Not saved: after a restart the peer stays where it is, unplaced.
    pub place_on_up: HashSet<Fingerprint>,
    pub counters: BTreeMap<String, u64>,
    /// The last `ferry release` over the socket.
    pub last_release: Option<Instant>,
    /// What the machines publish (role, gate, counters).
    pub live: SharedLive,
    /// The capture side runs on this peer.
    pub capture_on: bool,
    /// Lock to this machine (`state.toml`).
    pub locked: bool,
}

/// The link from one of this peer's monitors to `peer` whose edge `entry` (local logical, the
/// `Enter` point) lies on. The source clamps the entry point into the monitor, so it sits on the
/// last row or column inside the edge: `line` for left and top, `line - 1` for right and bottom.
fn entry_link(topo: &Topology, peer: PeerId, entry: (f64, f64)) -> Option<EdgeLink> {
    let p = topo.to_plane(Point {
        x: entry.0,
        y: entry.1,
    });
    let gap = |l: &EdgeLink| {
        let m = topo.monitor(l.from)?;
        let line = f64::from(l.side.line(&m.rect));
        let row = match l.side {
            Side::Left | Side::Top => line,
            Side::Right | Side::Bottom => line - 1.0,
        };
        let along = l.side.along(p);
        let inside = along >= f64::from(l.span.0) && along < f64::from(l.span.1);
        let gap = (l.side.across(p) - row).abs();
        (inside && gap <= 1.0).then_some(gap)
    };
    topo.links
        .iter()
        .filter(|l| l.from.peer == topo.me && l.to.peer == peer)
        .filter_map(|l| gap(l).map(|g| (g, *l)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, l)| l)
}

fn err(code: ErrorCode, message: impl Into<String>, hint: Option<&str>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        hint: hint.map(str::to_string),
    }
}

impl State {
    pub fn bump(&mut self, counter: &str) {
        *self.counters.entry(counter.to_string()).or_default() += 1;
    }

    // --- peers -----------------------------------------------------------------------------

    /// Me first, then every pin, by name.
    pub fn all_peers(&self) -> Vec<(PeerId, String)> {
        std::iter::once((self.me.id, self.me.name.clone()))
            .chain(
                self.trust
                    .pins()
                    .iter()
                    .map(|p| (peer_id(p.fingerprint), p.name.clone())),
            )
            .collect()
    }

    pub fn name_of(&self, id: PeerId) -> String {
        self.all_peers()
            .into_iter()
            .find(|(p, _)| *p == id)
            .map_or_else(|| id.to_string()[..8].to_string(), |(_, n)| n)
    }

    pub fn monitors_of(&self, id: PeerId) -> &[MonitorInfo] {
        if id == self.me.id {
            &self.own_monitors
        } else {
            self.layout.known_monitors(id).unwrap_or(&[])
        }
    }

    /// Me, then each pinned peer with known monitors: what `geom::place` and `geom::topology`
    /// take. The order is stable, and a `MonitorId`'s index is into the list used here.
    pub fn peer_monitors(&self) -> Vec<PeerMonitors> {
        let mut v = vec![PeerMonitors {
            peer: self.me.id,
            monitors: self.own_monitors.clone(),
        }];
        for pin in self.trust.pins() {
            let id = peer_id(pin.fingerprint);
            if let Some(m) = self.layout.known_monitors(id) {
                v.push(PeerMonitors {
                    peer: id,
                    monitors: m.to_vec(),
                });
            }
        }
        v
    }

    /// [`peer_monitors`](Self::peer_monitors) plus the peers that are placed in the layout without
    /// being paired here (a third peer another peer placed), when their monitors are
    /// known. For checks that must see every group: overlap.
    pub fn layout_monitors(&self) -> Vec<PeerMonitors> {
        let mut v = self.peer_monitors();
        for o in &self.layout.layout.origins {
            if v.iter().any(|pm| pm.peer == o.peer) {
                continue;
            }
            if let Some(m) = self.layout.known_monitors(o.peer) {
                v.push(PeerMonitors {
                    peer: o.peer,
                    monitors: m.to_vec(),
                });
            }
        }
        v
    }

    /// [`resolve_peer`](Self::resolve_peer), and also a peer that is placed in the layout without
    /// being paired here, by at least 8 hex digits of its fingerprint.
    pub fn resolve_layout_peer(&self, what: &str) -> Result<PeerId, ApiError> {
        let paired = match self.resolve_peer(what) {
            Err(e) if e.code == ErrorCode::UnknownPeer => e,
            other => return other,
        };
        let prefix = what.to_ascii_lowercase();
        if prefix.len() < 8 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(paired);
        }
        let hits: Vec<PeerId> = self
            .layout
            .layout
            .origins
            .iter()
            .map(|o| o.peer)
            .filter(|p| p.to_string().starts_with(&prefix))
            .collect();
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => Err(paired),
            many => Err(err(
                ErrorCode::AmbiguousPeer,
                format!("{what:?} matches {} placed peers", many.len()),
                Some("use more digits of the fingerprint"),
            )),
        }
    }

    pub fn link_state(&self, fp: &Fingerprint) -> LinkState {
        self.links
            .get(fp)
            .map_or(LinkState::Offline, |l| l.state.clone())
    }

    fn is_up(&self, id: PeerId) -> bool {
        id == self.me.id
            || self.trust.pins().iter().any(|p| {
                peer_id(p.fingerprint) == id
                    && self.link_state(&p.fingerprint) == LinkState::Connected
            })
    }

    /// A peer by exact name, else by a fingerprint prefix of at least 8 hex digits.
    pub fn resolve_peer(&self, what: &str) -> Result<PeerId, ApiError> {
        let all = self.all_peers();
        if let Some((id, _)) = all.iter().find(|(_, n)| n == what) {
            return Ok(*id);
        }
        let prefix = what.to_ascii_lowercase();
        if prefix.len() >= 8 && prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            let hits: Vec<_> = all
                .iter()
                .filter(|(id, _)| id.to_string().starts_with(&prefix))
                .collect();
            return match hits.as_slice() {
                [(id, _)] => Ok(*id),
                [] => Err(err(
                    ErrorCode::UnknownPeer,
                    format!("no peer matches {what:?}"),
                    Some("ferry status lists the peers"),
                )),
                many => Err(err(
                    ErrorCode::AmbiguousPeer,
                    format!(
                        "{what:?} matches {} peers: {}",
                        many.len(),
                        many.iter()
                            .map(|(_, n)| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some("use more digits of the fingerprint, or the name"),
                )),
            };
        }
        Err(err(
            ErrorCode::UnknownPeer,
            format!("no peer is named {what:?}"),
            Some("ferry status lists the peers; a fingerprint prefix needs at least 8 hex digits"),
        ))
    }

    /// `<peer>`, `<monitor>` or `<peer>:<monitor>`.
    pub fn resolve_anchor(&self, what: &str) -> Result<geom::Anchor, ApiError> {
        let monitor_in = |peer: PeerId, name: &str| -> Option<geom::Anchor> {
            self.monitors_of(peer)
                .iter()
                .position(|m| m.name == name)
                .and_then(|i| u8::try_from(i).ok())
                .map(|index| geom::Anchor::Monitor(MonitorId { peer, index }))
        };
        if let Some((p, m)) = what.split_once(':') {
            let peer = self.resolve_peer(p).map_err(|e| {
                err(
                    match e.code {
                        ErrorCode::AmbiguousPeer => ErrorCode::AmbiguousAnchor,
                        _ => ErrorCode::UnknownAnchor,
                    },
                    format!("anchor {what:?}: {}", e.message),
                    e.hint.as_deref(),
                )
            })?;
            return monitor_in(peer, m).ok_or_else(|| {
                err(
                    ErrorCode::UnknownAnchor,
                    format!("{} has no monitor {m:?}", self.name_of(peer)),
                    Some("ferry layout show lists the monitors"),
                )
            });
        }
        match self.resolve_peer(what) {
            Ok(peer) => return Ok(geom::Anchor::Peer(peer)),
            Err(e) if e.code == ErrorCode::AmbiguousPeer => {
                return Err(err(
                    ErrorCode::AmbiguousAnchor,
                    e.message,
                    e.hint.as_deref(),
                ));
            }
            Err(_) => {}
        }
        // A bare monitor name: this peer first, then the others.
        if let Some(a) = monitor_in(self.me.id, what) {
            return Ok(a);
        }
        let others: Vec<_> = self
            .trust
            .pins()
            .iter()
            .filter_map(|p| monitor_in(peer_id(p.fingerprint), what).map(|a| (p.name.clone(), a)))
            .collect();
        match others.as_slice() {
            [(_, a)] => Ok(*a),
            [] => Err(err(
                ErrorCode::UnknownAnchor,
                format!("{what:?} is neither a peer nor a monitor"),
                Some("ferry layout show lists the peers and monitors"),
            )),
            many => Err(err(
                ErrorCode::AmbiguousAnchor,
                format!(
                    "monitor {what:?} exists on {}",
                    many.iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(" and ")
                ),
                Some("write <peer>:<monitor>"),
            )),
        }
    }

    fn trusted_ids(&self) -> Vec<PeerId> {
        self.trust
            .pins()
            .iter()
            .map(|p| peer_id(p.fingerprint))
            .collect()
    }

    /// The SourceMachine's geometry: every trusted, placed peer with known monitors, and the
    /// links between them.
    pub fn topology(&self) -> Topology {
        geom::topology(
            self.me.id,
            &self.layout.layout,
            &self.peer_monitors(),
            &self.trusted_ids(),
        )
    }

    /// The `edge` event for a crossing: the edge of one of this peer's monitors that the
    /// crossing went over, and its linked stretch minus the dead corners. `None` when the layout
    /// no longer has that edge (it changed under the crossing).
    pub fn edge_event(&self, c: Crossed) -> Option<EdgeEvent> {
        let topo = self.topology();
        let (role, home, link) = match c {
            Crossed::Source { link, home } => (EdgeRole::Source, home, link),
            Crossed::Target { peer, entry, home } => {
                let link = entry_link(&topo, peer, entry)?;
                (EdgeRole::Target, home, link)
            }
        };
        let m = topo.monitor(link.from)?;
        let dead = i32::from(self.config.crossing.corner_dead_zone_px);
        let lo = link.side.span(&m.rect).0;
        let (start, end) = (link.span.0 + dead - lo, link.span.1 - dead - lo);
        (start < end).then(|| EdgeEvent {
            role,
            home,
            peer: self.name_of(link.to.peer),
            monitor: m.name.clone(),
            side: link.side,
            start,
            end,
        })
    }

    // --- updates from net ------------------------------------------------------------------

    pub fn apply_link(&mut self, up: LinkUpdate) {
        let proto = up
            .proto
            .or_else(|| self.links.get(&up.peer).and_then(|l| l.proto.clone()));
        self.links.insert(
            up.peer,
            LinkInfo {
                state: up.state,
                remote: up.remote,
                rtt_us: up.rtt_us,
                proto,
                detail: up.detail,
            },
        );
    }

    /// An unknown key's handshake was refused. [`Inbound::BeforePin`] is the race at the first
    /// connect after pairing, when the new peer's dial beats its pin to the endpoint (the
    /// acceptor pins only after the final acknowledgement): `fp` is pinned by now, or it is the
    /// peer of the pairing running here (`pairing_with`). That one is counted apart and not
    /// listed, and the dialer's redial gets in. A refusal is [`Inbound::Refused`] with `first`
    /// set once per fingerprint in this run, so the caller warns once.
    pub fn inbound_unknown(&mut self, fp: Fingerprint, pairing_with: bool) -> Inbound {
        if pairing_with || self.trust.get(&fp).is_some() {
            self.bump("inbound_before_pin");
            return Inbound::BeforePin;
        }
        self.bump("inbound_refused");
        self.note_refusal(fp);
        Inbound::Refused {
            first: self.refusals_warned.insert(fp),
        }
    }

    /// An unknown key tried to connect: list it until it is pinned.
    pub fn note_refusal(&mut self, fp: Fingerprint) {
        if self.trust.get(&fp).is_some() {
            return;
        }
        self.refusals.retain(|r| r.fingerprint != fp);
        self.refusals.push(Refusal { fingerprint: fp });
        if self.refusals.len() > MAX_REFUSALS {
            self.refusals.remove(0);
        }
    }

    // --- views -----------------------------------------------------------------------------

    pub fn id_result(&self) -> IdResult {
        IdResult {
            name: self.me.name.clone(),
            fingerprint: self.me.fp.to_string(),
            port: self.port,
            addrs: crate::netaddr::local_addrs(),
        }
    }

    pub fn status(&self) -> StatusResult {
        let (suspect, locked_peers): (Vec<PeerId>, Vec<PeerId>) = {
            let l = self.live.lock();
            (
                l.source
                    .as_ref()
                    .map(|s| s.suspect.clone())
                    .unwrap_or_default(),
                l.locked_peers.clone(),
            )
        };
        let mut peers: Vec<PeerStatus> = self
            .trust
            .pins()
            .iter()
            .map(|p| {
                let link = self.links.get(&p.fingerprint);
                PeerStatus {
                    name: p.name.clone(),
                    fingerprint: p.fingerprint.to_string(),
                    state: link.map_or(LinkState::Offline, |l| l.state.clone()),
                    addr: link
                        .and_then(|l| l.remote.clone())
                        .or_else(|| p.addr.as_deref().map(crate::netaddr::shown)),
                    rtt_us: link.and_then(|l| l.rtt_us),
                    proto: link.and_then(|l| l.proto.clone()),
                    detail: link.and_then(|l| l.detail.clone()),
                    suspect: suspect.contains(&peer_id(p.fingerprint)),
                    locked: locked_peers.contains(&peer_id(p.fingerprint)),
                    last_seen: p.last_seen.clone(),
                }
            })
            .collect();
        peers.extend(self.refusals.iter().map(|r| PeerStatus {
            name: crate::trust_store::default_name(&r.fingerprint),
            fingerprint: r.fingerprint.to_string(),
            state: LinkState::Refused,
            addr: None,
            rtt_us: None,
            proto: None,
            detail: Some(format!("unknown peer {}: not trusted here", r.fingerprint)),
            suspect: false,
            locked: false,
            last_seen: None,
        }));
        let live = self.live.lock();
        let (role, active) = match live.source.as_ref().map(|s| (s.role, s.peer)) {
            Some((SourceRole::Remote, p)) => (Role::Source, p),
            Some((SourceRole::Arming, p)) => (Role::Arming, p),
            Some((SourceRole::Pending, p)) => (Role::Pending, p),
            _ => match live.target.as_ref().and_then(|t| t.entered_by) {
                Some(p) => (Role::Target, Some(p)),
                None => (Role::Idle, None),
            },
        };
        let problem = match (&live.source, self.capture_on) {
            (Some(_), true) => capture_problem(
                live.enforce_permissions,
                live.source_counters.crossings,
                &live.portal,
            ),
            _ => None,
        };
        let capture = match (&live.source, self.capture_on) {
            (Some(src), true) => CaptureStatus {
                backend: self.config.input.capture.backend_name().to_string(),
                gate: src.gate.as_str().to_string(),
                barriers: live.barriers,
                failed_barriers: live.failed_barriers.clone(),
                // Lock to this machine outlasts a session, so it is named first.
                off_reason: if src.locked_here {
                    Some("locked_here".to_string())
                } else if src.controlled {
                    Some("controlled".to_string())
                } else {
                    None
                },
                notice: problem.as_ref().map(CaptureProblem::notice),
            },
            (_, true) => CaptureStatus {
                backend: self.config.input.capture.backend_name().to_string(),
                gate: "idle".to_string(),
                barriers: 0,
                failed_barriers: Vec::new(),
                off_reason: None,
                notice: None,
            },
            (_, false) => CaptureStatus {
                backend: "off".to_string(),
                gate: "off".to_string(),
                barriers: 0,
                failed_barriers: Vec::new(),
                off_reason: None,
                notice: None,
            },
        };
        // The newest release on either side, or the last `ferry release`.
        let machine_release = live.last_release;
        let socket_release = self.last_release.map(|at| (LeaveReason::Forced, at));
        let last_release = match (machine_release, socket_release) {
            (Some(a), Some(b)) => Some(if a.1 >= b.1 { a } else { b }),
            (a, b) => a.or(b),
        };
        let mut counters = self.counters.clone();
        if live.source.is_some() {
            // Present from the start, so a drill can read 0 before the first re-zone.
            counters.entry("rezones".to_string()).or_insert(0);
            let c = &live.source_counters;
            for (k, v) in [
                ("activations", c.activations),
                ("crossings", c.crossings),
                ("bounces", c.bounces),
                ("lock_refusals", c.lock_refusals),
                ("repeat_pairs_dropped", c.repeat_pairs_dropped),
                ("repeat_split_pairs", c.repeat_split_pairs),
                ("repeat_split_max_us", c.repeat_split_max_us),
                ("repeat_future_stamps", c.repeat_future_stamps),
                ("orphan_key_releases", c.orphan_key_releases),
                ("drag_leaves", c.drag_leaves),
                ("silence_releases", c.silence_releases),
                ("pending_overflows", c.pending_overflows),
                ("datagrams_sent", c.datagrams_sent),
                ("frames_sent", c.frames_sent),
                ("keymaps_sent", c.keymaps_sent),
                ("lock_unknown", c.lock_unknown),
                ("barrier_failures", c.barrier_failures),
                ("edge_parks", c.edge_parks),
                ("go_crossings", c.go_crossings),
                ("go_refused", c.go_refused),
                ("nonfinite_eis_values", c.nonfinite_dropped),
                ("motion_coalesced", c.motion_coalesced),
                ("motion_suppressed", c.motion_suppressed),
            ] {
                counters.insert(k.to_string(), v);
            }
        }
        let t = &live.target_counters;
        for (k, v) in [
            ("enters", t.enters),
            ("watchdog_releases", t.watchdog_releases),
            ("stale_datagrams", t.stale_datagrams),
            ("stale_frames", t.stale_frames),
            ("orphan_releases", t.orphan_releases),
            ("snapshot_releases", t.snapshot_releases),
            ("keymap_swaps", t.keymap_swaps),
            ("refused_enters", t.refused),
            ("takeover_suspects", t.takeover_suspects),
            ("takeovers", t.takeovers),
            ("nonfinite_peer_values", t.nonfinite_dropped),
        ] {
            counters.insert(k.to_string(), v);
        }
        // Present from the start; net refreshes it every 2 s.
        counters.insert("handshakes_refused".to_string(), 0);
        // Pairing: present from the start.
        for k in ["pair_attempts", "pair_failures", "pairing_refused"] {
            counters.insert(k.to_string(), 0);
        }
        for (k, v) in &live.counters {
            counters.insert((*k).to_string(), *v);
        }
        let attention = self.attention(&capture, problem.as_ref());
        let summary = Summary::derive(SummaryFacts {
            role: &role,
            active_peer: active.map(|p| self.name_of(p)).as_deref(),
            locked: self.locked,
            connected: peers.iter().any(|p| p.state == LinkState::Connected),
            attention: !attention.is_empty(),
        });
        StatusResult {
            api: API_VERSION,
            me: PeerRef {
                name: self.me.name.clone(),
                fingerprint: self.me.fp.to_string(),
            },
            port: self.port,
            role,
            active_peer: active.map(|p| PeerRef {
                name: self.name_of(p),
                fingerprint: p.to_string(),
            }),
            capture,
            screen_locked: if self.capture_on {
                live.screen_locked
            } else {
                None
            },
            locked: self.locked,
            peers,
            last_release: last_release.map(|(reason, at)| {
                let ago = at.elapsed();
                LastRelease {
                    reason: reason.as_str().to_string(),
                    ago_ms: u64::try_from(ago.as_millis()).unwrap_or(u64::MAX),
                    mono_us: Some(
                        crate::clock::now()
                            .0
                            .saturating_sub(u64::try_from(ago.as_micros()).unwrap_or(0)),
                    ),
                }
            }),
            counters,
            pairing: None,
            summary: Some(summary),
            attention,
            escape_chord: Some(self.escape_chord()),
        }
    }

    /// The escape chord this ferryd runs with, as `Super+Ctrl+Escape`.
    fn escape_chord(&self) -> String {
        let text = &self.config.crossing.escape_chord;
        EscapeChord::parse(text).map_or_else(|_| text.clone(), |c| c.to_string())
    }

    /// The problems the user must act on, derived from what `status` already
    /// holds, in the order of the design's list. `escape_bind_missing` is left out: only
    /// Hyprland's IPC can tell, and `status` does no IO (open item).
    fn attention(
        &self,
        capture: &CaptureStatus,
        problem: Option<&CaptureProblem>,
    ) -> Vec<Attention> {
        let mut out = Vec::new();
        for pin in self.trust.pins() {
            let link = self.links.get(&pin.fingerprint);
            if link.is_some_and(|l| l.state == LinkState::Refused) {
                let why = link.and_then(|l| l.detail.as_deref()).unwrap_or("refused");
                out.push(Attention {
                    code: AttentionCode::PeerRefused,
                    message: format!("{} cannot connect: {}", pin.name, shorten_keys(why)),
                    // The link's text says what happened, not why in terms of a fix: a key that
                    // changed or a pairing the other side took back needs a new pairing, another
                    // ferry version needs an update.
                    hint: Some(format!(
                        "`ferry doctor` has the details. After a key change or if it unpaired this \
                         machine, `ferry unpair {}` and pair again; after a version mismatch, update \
                         the older ferry",
                        pin.name
                    )),
                    peer: Some(pin.name.clone()),
                });
            }
        }
        if self.capture_on
            && (capture.gate == "barriers_failed" || !capture.failed_barriers.is_empty())
        {
            out.push(Attention {
                code: AttentionCode::BarriersFailed,
                message: format!(
                    "{} of {} pointer barriers failed to set, so crossings do not start at those edges",
                    capture.failed_barriers.len().max(1),
                    capture.barriers
                ),
                hint: Some("`ferry release`, then check the monitors with `hyprctl monitors`".into()),
                peer: None,
            });
        }
        out.extend(problem.map(CaptureProblem::attention));
        // A peer whose monitors are known can be placed; one that never connected cannot yet.
        // A target-only peer places nobody: the source does.
        if self.capture_on {
            for pin in self.trust.pins() {
                let id = peer_id(pin.fingerprint);
                let placeable = self
                    .layout
                    .known_monitors(id)
                    .is_some_and(|m| !m.is_empty());
                if placeable && self.layout.origin_of(id).is_none() {
                    out.push(Attention {
                        code: AttentionCode::LayoutUnplaced,
                        message: format!("{} is not placed in the layout", pin.name),
                        hint: Some(format!(
                            "ferry layout place {} --right-of {}",
                            pin.name, self.me.name
                        )),
                        peer: Some(pin.name.clone()),
                    });
                }
            }
        }
        if let [first, rest @ ..] = self.config_warnings.as_slice() {
            out.push(Attention {
                code: AttentionCode::ConfigInvalid,
                message: if rest.is_empty() {
                    format!("config.toml: {first}")
                } else {
                    format!("config.toml has {} problems; the first: {first}", rest.len() + 1)
                },
                hint: Some(
                    "`ferry config get` shows what is saved; fix the file ~/.config/ferry/config.toml"
                        .into(),
                ),
                peer: None,
            });
        }
        out
    }

    /// `peers`: every pin with how it was made, where it is dialled, when it was
    /// last seen and its link state.
    pub fn peers_view(&self) -> PeersResult {
        let locked_peers = self.live.lock().locked_peers.clone();
        PeersResult {
            peers: self
                .trust
                .pins()
                .iter()
                .map(|p| PeerEntry {
                    name: p.name.clone(),
                    fingerprint: p.fingerprint.to_string(),
                    addr: p.addr.as_deref().map(crate::netaddr::shown),
                    last_seen: p.last_seen.clone(),
                    paired: p.paired,
                    state: self.link_state(&p.fingerprint),
                    locked: locked_peers.contains(&peer_id(p.fingerprint)),
                })
                .collect(),
        }
    }

    /// `layout.get` and the result of `layout.place` and `layout.set`.
    ///
    /// A monitor is listed where it is on the plane: its local position plus the peer's anchor
    /// shift plus the layout origin (M3 design 4.1), or its local position while the peer is not
    /// placed. `origin` is the layout's value alone.
    pub fn layout_view(&self) -> LayoutResult {
        let rev = self.layout.layout.rev;
        let name = |p: PeerId| self.name_of(p);
        let monitor_name = |m: MonitorId| {
            let pm = self.monitors_of(m.peer);
            let mon = pm
                .get(usize::from(m.index))
                .map_or("?", |m| m.name.as_str());
            format!("{}:{mon}", name(m.peer))
        };

        // This peer, the peers paired here, and then the peers that are only placed in the
        // layout (a third peer another peer placed): those are not trusted here, FR-UI-07.
        let paired = self.all_peers();
        let others: Vec<(PeerId, String)> = self
            .layout
            .layout
            .origins
            .iter()
            .map(|o| o.peer)
            .filter(|p| paired.iter().all(|(q, _)| q != p))
            .map(|p| (p, self.name_of(p)))
            .collect();
        let peers: Vec<LayoutPeer> = paired
            .iter()
            .map(|(id, n)| (*id, n.clone(), true))
            .chain(others.into_iter().map(|(id, n)| (id, n, false)))
            .map(|(id, peer_name, trusted)| {
                let origin = self.layout.origin_of(id);
                let monitors = self.monitors_of(id);
                let shift = monitors.first().map_or((0, 0), MonitorInfo::shift_xy);
                let (dx, dy) = origin.map_or((0, 0), |(x, y)| {
                    (x.saturating_add(shift.0), y.saturating_add(shift.1))
                });
                let views: Vec<MonitorView> = monitors
                    .iter()
                    .map(|m| MonitorView {
                        name: m.name.clone(),
                        x: m.rect.x.saturating_add(dx),
                        y: m.rect.y.saturating_add(dy),
                        w: i32::try_from(m.rect.w).unwrap_or(i32::MAX),
                        h: i32::try_from(m.rect.h).unwrap_or(i32::MAX),
                        scale: m.scale,
                        description: m.description.clone(),
                        transform: m.transform,
                    })
                    .collect();
                let bounds = Rect::bounding(views.iter().map(|m| Rect {
                    x: m.x,
                    y: m.y,
                    w: m.w,
                    h: m.h,
                }));
                LayoutPeer {
                    name: peer_name,
                    fingerprint: peer_hex(id),
                    origin,
                    monitors: views,
                    online: self.is_up(id),
                    trusted,
                    is_self: id == self.me.id,
                    bounds,
                    shift: (shift != (0, 0)).then_some(shift),
                }
            })
            .collect();

        // Links join monitors of two different placed peers; with fewer than two there is
        // nothing to derive.
        let placed = self
            .all_peers()
            .iter()
            .filter(|(id, _)| {
                self.layout.origin_of(*id).is_some() && !self.monitors_of(*id).is_empty()
            })
            .count();
        let links = if placed < 2 {
            Vec::new()
        } else {
            let topo = self.topology();
            topo.links
                .iter()
                .map(|l| LinkView {
                    from: monitor_name(l.from),
                    side: l.side,
                    to: monitor_name(l.to),
                    span: l.span,
                    live: self.is_up(l.from.peer) && self.is_up(l.to.peer),
                })
                .collect()
        };

        LayoutResult {
            revision: RevisionView {
                counter: rev.counter,
                peer: peer_hex(rev.peer),
            },
            peers,
            links,
        }
    }
}

/// `text` with each 64-digit fingerprint cut to the form `ferry pair` prints for a key
/// (`9c2e:41aa:0b7d:5512`), so a message in a toast stays one line. Only the display changes.
fn shorten_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if !c.is_ascii_hexdigit() {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let end = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        match PeerId::parse(run) {
            Ok(id) if run.len() == 64 => out.push_str(&id.short()),
            _ => out.push_str(run),
        }
        rest = tail;
    }
    out
}

/// The condition `ferry status` names for FR-SAFE-16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureProblem {
    /// The portal refused, with its error.
    Denied(String),
    /// `ecosystem:enforce_permissions` is on and no crossing has worked yet: Hyprland asks the
    /// user at the first one, and nothing reaches ferryd.
    DialogPending,
}

impl CaptureProblem {
    /// The line `ferry status` prints under the capture row.
    pub fn notice(&self) -> String {
        let (line, restart) = (
            crate::doctor::PERMISSION_LINE,
            crate::doctor::PERMISSION_RESTART,
        );
        match self {
            Self::Denied(e) => format!(
                "input capture not permitted ({e}); allow xdph in ~/.config/hypr/ferry.lua and {restart}: {line}; if capture is still refused, the rule is not enough"
            ),
            Self::DialogPending => format!(
                "ecosystem:enforce_permissions is on and no crossing has started yet: answer Hyprland's permission dialog at the first crossing, or allow xdph in ~/.config/hypr/ferry.lua and {restart}: {line}"
            ),
        }
    }

    /// The same as an `attention` entry for a frontend.
    fn attention(&self) -> Attention {
        let restart = crate::doctor::PERMISSION_RESTART;
        match self {
            Self::Denied(_) => Attention {
                code: AttentionCode::CaptureDenied,
                message: "Hyprland does not let Ferry capture input".into(),
                hint: Some(format!(
                    "run `ferry setup` to allow xdph in ~/.config/hypr/ferry.lua, then {restart}"
                )),
                peer: None,
            },
            Self::DialogPending => Attention {
                code: AttentionCode::CaptureDenied,
                message: "Hyprland will ask before Ferry may capture input".into(),
                hint: Some(format!(
                    "answer its dialog at the first crossing, or run `ferry setup` to allow xdph and {restart}"
                )),
                peer: None,
            },
        }
    }
}

/// A denied portal call is named at once; with `ecosystem:enforce_permissions` on, a denial shows
/// only at the first crossing (Hyprland asks the user for xdph, and nothing reaches ferryd), so
/// until one has worked the status says so.
pub fn capture_problem(
    enforce_permissions: Option<bool>,
    crossings: u64,
    portal: &crate::live::PortalHealth,
) -> Option<CaptureProblem> {
    if !portal.open
        && let Some(e) = &portal.last_error
        && crate::doctor::looks_denied(e)
    {
        return Some(CaptureProblem::Denied(e.clone()));
    }
    (enforce_permissions == Some(true) && crossings == 0).then_some(CaptureProblem::DialogPending)
}

/// [`capture_problem`], as the line `ferry status` prints.
pub fn capture_notice(
    enforce_permissions: Option<bool>,
    crossings: u64,
    portal: &crate::live::PortalHealth,
) -> Option<String> {
    capture_problem(enforce_permissions, crossings, portal).map(|p| p.notice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::api::SummaryState;
    use ferry_core::source::{Gate, SourceStatus};

    #[test]
    fn a_denied_portal_call_names_the_rule_and_says_it_may_not_be_enough() {
        let portal = crate::live::PortalHealth {
            open: false,
            failures: 1,
            last_error: Some("portal: Permission denied".into()),
        };
        let n = capture_notice(None, 0, &portal).unwrap();
        assert!(n.starts_with("input capture not permitted"), "{n}");
        assert!(n.contains("hl.permission("), "{n}");
        assert!(n.contains("and restart Hyprland"), "{n}");
        assert!(
            n.contains("if capture is still refused, the rule is not enough"),
            "{n}"
        );
    }

    fn state(capture_on: bool) -> (State, Fingerprint, Fingerprint) {
        let me = Fingerprint([0xd0; 32]);
        let (a, b) = (Fingerprint([0x0a; 32]), Fingerprint([0x0b; 32]));
        let mut trust = TrustStore::default();
        trust.upsert(a, Some("a"), None).unwrap();
        trust.upsert(b, Some("b"), None).unwrap();
        let mut s = State {
            me: Me {
                name: "desk".into(),
                fp: me,
                id: peer_id(me),
            },
            port: 4243,
            config: Config::default(),
            saved: Config::default(),
            config_warnings: Vec::new(),
            trust,
            layout: LayoutStore::empty(peer_id(me)),
            edits: EditLog::default(),
            own_monitors: Vec::new(),
            links: HashMap::new(),
            refusals: Vec::new(),
            refusals_warned: HashSet::new(),
            place_on_up: HashSet::new(),
            counters: BTreeMap::new(),
            last_release: None,
            live: SharedLive::default(),
            capture_on,
            locked: false,
        };
        for fp in [a, b] {
            s.apply_link(LinkUpdate {
                peer: fp,
                state: LinkState::Connected,
                remote: None,
                rtt_us: None,
                proto: None,
                detail: None,
            });
        }
        (s, a, b)
    }

    #[test]
    fn a_dial_that_beats_its_pin_is_not_a_refusal() {
        let (mut s, a, _) = state(false);
        let stranger = Fingerprint([0x5e; 32]);
        let new_peer = Fingerprint([0x9a; 32]);
        // A key nobody here knows: refused, counted and listed.
        assert_eq!(
            s.inbound_unknown(stranger, false),
            Inbound::Refused { first: true }
        );
        // The peer being paired here dials before the acceptor pins it (exit check row 2), or
        // it is pinned by the time ctl reads the refusal: neither is a refusal.
        assert_eq!(s.inbound_unknown(new_peer, true), Inbound::BeforePin);
        assert_eq!(s.inbound_unknown(a, false), Inbound::BeforePin);
        assert_eq!(s.counters["inbound_refused"], 1);
        assert_eq!(s.counters["inbound_before_pin"], 2);
        let listed: Vec<_> = s.refusals.iter().map(|r| r.fingerprint).collect();
        assert_eq!(listed, [stranger]);
    }

    /// A peer that this one unpaired keeps dialling. Each refusal is counted and listed,
    /// and only the first of each fingerprint is one the caller warns about.
    #[test]
    fn a_refused_key_is_warned_about_once_per_run() {
        let (mut s, _, _) = state(false);
        let (one, two) = (Fingerprint([0x5e; 32]), Fingerprint([0x6f; 32]));
        assert_eq!(
            s.inbound_unknown(one, false),
            Inbound::Refused { first: true }
        );
        for _ in 0..3 {
            assert_eq!(
                s.inbound_unknown(one, false),
                Inbound::Refused { first: false }
            );
        }
        assert_eq!(
            s.inbound_unknown(two, false),
            Inbound::Refused { first: true }
        );
        assert_eq!(s.counters["inbound_refused"], 5);
        assert_eq!(s.refusals.len(), 2);
    }

    #[test]
    fn a_suspect_peer_shows_in_status() {
        let (s, a, b) = state(true);
        let suspect = |s: &State, fp: Fingerprint| {
            s.status()
                .peers
                .iter()
                .find(|p| p.fingerprint == fp.to_string())
                .map(|p| (p.state.clone(), p.suspect))
        };
        // No source status yet: nobody is suspect.
        assert_eq!(suspect(&s, a), Some((LinkState::Connected, false)));
        s.live.source_status(SourceStatus {
            role: SourceRole::Local,
            peer: None,
            session: 3,
            last_leave: Some(LeaveReason::Silence),
            gate: Gate::Armed,
            suspect: vec![peer_id(b)],
            controlled: false,
            locked_here: false,
            locked_peers: vec![],
            edge: None,
        });
        // Connected and suspect: the link is up but its edges are walls.
        assert_eq!(suspect(&s, a), Some((LinkState::Connected, false)));
        assert_eq!(suspect(&s, b), Some((LinkState::Connected, true)));
        let json = serde_json::to_value(s.status()).unwrap();
        assert_eq!(json["peers"][1]["suspect"], true);
        assert_eq!(json["peers"][0]["suspect"], false);
    }

    fn codes(s: &State) -> Vec<AttentionCode> {
        s.status().attention.iter().map(|a| a.code).collect()
    }

    fn refuse(s: &mut State, fp: Fingerprint, why: &str) {
        s.apply_link(LinkUpdate {
            peer: fp,
            state: LinkState::Refused,
            remote: None,
            rtt_us: None,
            proto: Some("2.0".into()),
            detail: Some(why.into()),
        });
    }

    fn one_monitor() -> Vec<MonitorInfo> {
        vec![MonitorInfo {
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
        }]
    }

    /// The desk (1920×1080, `HDMI-A-1`) with the netbook `b` (`one_monitor`) below it at x = 277,
    /// as on the real pair: the link is x ∈ [277, 1643) on y = 1080.
    fn netbook_below() -> (State, PeerId) {
        let (mut s, _, b) = state(true);
        let idb = peer_id(b);
        s.own_monitors = vec![MonitorInfo {
            name: "HDMI-A-1".into(),
            rect: ferry_core::geom::WireRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            ..one_monitor().remove(0)
        }];
        s.layout.set_known(idb, one_monitor());
        s.layout.layout = s.layout.with_origin(
            s.me.id,
            ferry_core::geom::Origin {
                peer: idb,
                x: 277,
                y: 1080,
            },
        );
        (s, idb)
    }

    #[test]
    fn an_edge_event_names_the_linked_stretch_without_the_dead_corners() {
        let (s, b) = netbook_below();
        let dead = i32::from(s.config.crossing.corner_dead_zone_px);
        let topo = s.topology();
        let out = *topo.links.iter().find(|l| l.from.peer == s.me.id).unwrap();
        let e = s
            .edge_event(Crossed::Source {
                link: out,
                home: false,
            })
            .unwrap();
        assert_eq!(e.role, EdgeRole::Source);
        assert!(!e.home);
        assert_eq!(e.peer, "b");
        assert_eq!(e.monitor, "HDMI-A-1");
        assert_eq!(e.side, Side::Bottom);
        assert_eq!((e.start, e.end), (277 + dead, 1643 - dead));
        // The target side: b's input comes in on the bottom row of HDMI-A-1.
        let e = s
            .edge_event(Crossed::Target {
                peer: b,
                entry: (900.0, 1079.0),
                home: true,
            })
            .unwrap();
        assert_eq!(e.role, EdgeRole::Target);
        assert!(e.home);
        assert_eq!((e.monitor.as_str(), e.side), ("HDMI-A-1", Side::Bottom));
        assert_eq!((e.start, e.end), (277 + dead, 1643 - dead));
    }

    #[test]
    fn an_entry_off_every_linked_edge_has_no_edge_event() {
        let (s, b) = netbook_below();
        for entry in [(900.0, 500.0), (100.0, 1079.0), (0.0, 600.0)] {
            let c = Crossed::Target {
                peer: b,
                entry,
                home: false,
            };
            assert_eq!(s.edge_event(c), None, "{entry:?}");
        }
        // The right peer, the wrong one: no link from here to `a`.
        let c = Crossed::Target {
            peer: peer_id(Fingerprint([0x0a; 32])),
            entry: (900.0, 1079.0),
            home: false,
        };
        assert_eq!(s.edge_event(c), None);
    }

    #[test]
    fn a_healthy_machine_has_nothing_to_attend_to() {
        let (s, _, _) = state(true);
        let st = s.status();
        assert!(st.attention.is_empty());
        assert_eq!(st.summary.unwrap().state, SummaryState::Ready);
        assert_eq!(st.escape_chord.as_deref(), Some("Super+Ctrl+Escape"));
    }

    #[test]
    fn a_refused_pin_is_attention_naming_the_peer_and_why() {
        let (mut s, a, _) = state(true);
        refuse(&mut s, a, "version mismatch: 1.0 vs 2.0");
        let st = s.status();
        let [att] = st.attention.as_slice() else {
            panic!("{:?}", st.attention);
        };
        assert_eq!(att.code, AttentionCode::PeerRefused);
        assert_eq!(att.peer.as_deref(), Some("a"));
        assert_eq!(
            att.message,
            "a cannot connect: version mismatch: 1.0 vs 2.0"
        );
        assert!(att.hint.as_deref().unwrap().contains("ferry unpair a"));
        // it outranks everything else in the summary, and clears with the refusal
        assert_eq!(st.summary.unwrap().state, SummaryState::Attention);
        s.apply_link(LinkUpdate {
            peer: a,
            state: LinkState::Connected,
            remote: None,
            rtt_us: None,
            proto: None,
            detail: None,
        });
        assert!(s.status().attention.is_empty());
        // a stranger that was refused is listed in status, but is not the user's to fix
        s.note_refusal(Fingerprint([0x5e; 32]));
        assert!(s.status().attention.is_empty());
    }

    #[test]
    fn a_refusal_names_its_keys_short() {
        let (mut s, a, _) = state(true);
        let key = "9c2e41aa0b7d5512".repeat(4);
        refuse(
            &mut s,
            a,
            &format!("peer {key} does not trust this machine"),
        );
        let m = s.status().attention[0].message.clone();
        assert_eq!(
            m,
            "a cannot connect: peer 9c2e:41aa:0b7d:5512 does not trust this machine"
        );
        // what is not a fingerprint stays as it is
        assert_eq!(
            shorten_keys("version mismatch: 1.0 vs 2.0"),
            "version mismatch: 1.0 vs 2.0"
        );
        let other = "ab".repeat(32);
        assert_eq!(
            shorten_keys(&format!("expected {key}, got {other}.")),
            "expected 9c2e:41aa:0b7d:5512, got abab:abab:abab:abab."
        );
        // a hex run of another length, text in other scripts and the empty text are kept
        assert_eq!(shorten_keys("deadbeef é 🔑 12345"), "deadbeef é 🔑 12345");
        assert_eq!(shorten_keys(&"a".repeat(65)), "a".repeat(65));
        assert_eq!(shorten_keys(""), "");
    }

    #[test]
    fn failed_barriers_are_attention_when_this_peer_captures() {
        for (capture_on, expect) in [(true, vec![AttentionCode::BarriersFailed]), (false, vec![])] {
            let (s, _, _) = state(capture_on);
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
            assert_eq!(codes(&s), expect, "capture_on {capture_on}");
            if capture_on {
                let a = &s.status().attention[0];
                assert_eq!(
                    a.message,
                    "1 of 3 pointer barriers failed to set, so crossings do not start at those edges"
                );
                assert!(a.hint.as_deref().unwrap().contains("hyprctl monitors"));
            }
        }
    }

    #[test]
    fn a_denied_capture_and_a_pending_dialog_are_attention_with_the_fix() {
        let (s, _, _) = state(true);
        s.live.source_status(SourceStatus {
            role: SourceRole::Local,
            peer: None,
            session: 0,
            last_leave: None,
            gate: Gate::Idle,
            suspect: vec![],
            controlled: false,
            locked_here: false,
            locked_peers: vec![],
            edge: None,
        });
        // Hyprland will ask at the first crossing
        s.live.lock().enforce_permissions = Some(true);
        let st = s.status();
        let [att] = st.attention.as_slice() else {
            panic!("{:?}", st.attention);
        };
        assert_eq!(att.code, AttentionCode::CaptureDenied);
        assert!(att.message.contains("will ask"), "{att:?}");
        assert!(att.hint.as_deref().unwrap().contains("ferry setup"));
        // the status line says the same thing it did before, word for word
        assert!(
            st.capture
                .notice
                .as_deref()
                .unwrap()
                .starts_with("ecosystem:enforce_permissions is on")
        );

        // the portal said no
        s.live.lock().enforce_permissions = None;
        s.live.lock().portal = crate::live::PortalHealth {
            open: false,
            failures: 1,
            last_error: Some("portal: Permission denied".into()),
        };
        let st = s.status();
        assert_eq!(st.attention[0].code, AttentionCode::CaptureDenied);
        assert!(
            st.attention[0]
                .message
                .contains("does not let Ferry capture")
        );
        assert!(
            st.attention[0]
                .hint
                .as_deref()
                .unwrap()
                .contains("restart Hyprland")
        );
        assert!(
            st.capture
                .notice
                .as_deref()
                .unwrap()
                .starts_with("input capture not permitted")
        );
    }

    #[test]
    fn a_peer_that_can_be_placed_and_is_not_is_attention_until_it_is() {
        let (mut s, a, b) = state(true);
        let (ida, idb) = (peer_id(a), peer_id(b));
        // never connected: no monitors known, nothing to place
        assert!(codes(&s).is_empty());
        s.layout.set_known(ida, one_monitor());
        let st = s.status();
        let [att] = st.attention.as_slice() else {
            panic!("{:?}", st.attention);
        };
        assert_eq!(att.code, AttentionCode::LayoutUnplaced);
        assert_eq!(att.peer.as_deref(), Some("a"));
        assert_eq!(
            att.hint.as_deref(),
            Some("ferry layout place a --right-of desk")
        );
        // both unplaced: one entry each, in the order of the pins
        s.layout.set_known(idb, one_monitor());
        assert_eq!(codes(&s), [AttentionCode::LayoutUnplaced; 2]);
        // placing one clears its entry
        let layout = s.layout.with_origin(
            s.me.id,
            ferry_core::geom::Origin {
                peer: ida,
                x: 1920,
                y: 0,
            },
        );
        s.layout.layout = layout;
        let st = s.status();
        assert_eq!(st.attention.len(), 1);
        assert_eq!(st.attention[0].peer.as_deref(), Some("b"));
        // a target-only peer places nobody
        let (mut s, a, _) = state(false);
        s.layout.set_known(peer_id(a), one_monitor());
        assert!(codes(&s).is_empty());
    }

    #[test]
    fn what_the_loader_warned_about_is_attention_until_the_file_is_fixed() {
        let (mut s, _, _) = state(false);
        s.config_warnings = vec!["unknown key peer.nmae".into()];
        let st = s.status();
        assert_eq!(st.attention[0].code, AttentionCode::ConfigInvalid);
        assert_eq!(
            st.attention[0].message,
            "config.toml: unknown key peer.nmae"
        );
        s.config_warnings.push("unknown section [future]".into());
        let msg = s.status().attention[0].message.clone();
        assert_eq!(
            msg,
            "config.toml has 2 problems; the first: unknown key peer.nmae"
        );
    }

    #[test]
    fn attention_comes_in_the_order_of_the_design() {
        let (mut s, a, b) = state(true);
        s.config_warnings = vec!["w".into()];
        s.layout.set_known(peer_id(b), one_monitor());
        s.live.lock().enforce_permissions = Some(true);
        s.live.lock().failed_barriers = vec![1];
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
        refuse(&mut s, a, "refused");
        assert_eq!(
            codes(&s),
            [
                AttentionCode::PeerRefused,
                AttentionCode::BarriersFailed,
                AttentionCode::CaptureDenied,
                AttentionCode::LayoutUnplaced,
                AttentionCode::ConfigInvalid,
            ]
        );
    }

    #[test]
    fn the_summary_follows_the_machines_and_the_peers() {
        let (s, a, b) = state(true);
        let summary = |s: &State| s.status().summary.unwrap();
        assert_eq!(summary(&s).state, SummaryState::Ready);
        let src = |role, peer| SourceStatus {
            role,
            peer,
            session: 1,
            last_leave: None,
            gate: Gate::Armed,
            suspect: vec![],
            controlled: false,
            locked_here: false,
            locked_peers: vec![],
            edge: None,
        };
        s.live
            .source_status(src(SourceRole::Remote, Some(peer_id(a))));
        assert_eq!(
            summary(&s),
            Summary {
                state: SummaryState::Away,
                peer: Some("a".into())
            }
        );
        s.live
            .source_status(src(SourceRole::Arming, Some(peer_id(a))));
        assert_eq!(
            summary(&s).state,
            SummaryState::Ready,
            "arming reads as ready"
        );
        s.live.source_status(src(SourceRole::Local, None));
        s.live.target_status(ferry_core::target::TargetStatus {
            entered_by: Some(peer_id(b)),
            entry: None,
            session: 2,
            last_end: None,
        });
        assert_eq!(
            summary(&s),
            Summary {
                state: SummaryState::Controlled,
                peer: Some("b".into())
            }
        );
        s.live.target_status(ferry_core::target::TargetStatus {
            entered_by: None,
            entry: None,
            session: 2,
            last_end: None,
        });
        let mut s = s;
        s.locked = true;
        assert_eq!(summary(&s).state, SummaryState::Locked);
        // idle: nobody is connected
        for fp in [a, b] {
            s.apply_link(LinkUpdate {
                peer: fp,
                state: LinkState::Offline,
                remote: None,
                rtt_us: None,
                proto: None,
                detail: None,
            });
        }
        s.locked = false;
        assert_eq!(summary(&s).state, SummaryState::Idle);
    }

    /// Every field the step 6 streams added to `status`, from one State: lock to this machine,
    /// why capture is off, the FR-SAFE-16 notice, and per peer the lock and `last_seen`.
    #[test]
    fn status_carries_the_step_6_fields_together() {
        let (mut s, a, b) = state(true);
        s.locked = true;
        s.trust.seen(&a, None, "2026-10-18T09:12:03Z");
        {
            let mut l = s.live.lock();
            l.enforce_permissions = Some(true);
            l.locked_peers = vec![peer_id(b)];
        }
        s.live.source_status(SourceStatus {
            role: SourceRole::Local,
            peer: None,
            session: 0,
            last_leave: None,
            gate: Gate::Idle,
            suspect: vec![],
            controlled: false,
            locked_here: true,
            locked_peers: vec![peer_id(b)],
            edge: None,
        });
        let j = serde_json::to_value(s.status()).unwrap();
        assert_eq!(j["locked"], true);
        assert_eq!(j["capture"]["off_reason"], "locked_here");
        assert!(
            j["capture"]["notice"]
                .as_str()
                .is_some_and(|n| n.contains("enforce_permissions") && n.contains("hl.permission")),
            "{j}"
        );
        let notice = j["capture"]["notice"].as_str().unwrap();
        for must in [
            "answer Hyprland's permission dialog at the first crossing, or allow xdph in ~/.config/hypr/ferry.lua",
            "and restart Hyprland, which reads the rule only when it starts",
        ] {
            assert!(notice.contains(must), "{notice}");
        }
        assert!(!notice.contains("not enough"), "{notice}");
        assert_eq!(j["peers"][0]["last_seen"], "2026-10-18T09:12:03Z");
        assert_eq!(j["peers"][0]["locked"], false);
        assert_eq!(j["peers"][1]["locked"], true);
        // `pairing` is filled by the api layer from ctl's pairing state, and absent when idle
        assert!(j.get("pairing").is_none_or(serde_json::Value::is_null));
    }
}
