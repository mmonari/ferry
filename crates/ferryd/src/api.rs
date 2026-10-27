//! The socket API methods. `dispatch` takes one request and returns a result or an
//! `{code, message, hint?}` error; `server.rs` frames the lines.

use ferry_core::api::{
    AlignName, AlignParam, ApiError, ErrorCode, GoParams, LockParams, LockResult, PeerRef,
    PeerRenameParams, PlaceParams, RenameResult, Request, SetParams, StatusResult, SubscribeParams,
    SubscribeResult, Topic, TrustParams,
};
use ferry_core::geom::{self, Align, Layout, Origin, PlaceError, Revision, SetError};
use ferry_core::source::SourceEvent;
use ferry_net::Fingerprint;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::chan::{CaptureCmd, NetCmd};
use crate::config::validate_peer_name;
use crate::ctl::Ctl;
use crate::layout_store::parse_peer;
use crate::netaddr::parse_host_port;
use crate::state_file::StateFile;
use crate::trust_store::{TrustError, Upsert};

fn err(code: ErrorCode, message: impl Into<String>, hint: Option<&str>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        hint: hint.map(str::to_string),
    }
}

fn internal(message: impl std::fmt::Display) -> ApiError {
    err(
        ErrorCode::Internal,
        message.to_string(),
        Some("see the ferryd log"),
    )
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(internal)
}

/// The `status` result and the `state` event: one function, so they cannot differ.
pub fn status_snapshot(ctl: &Ctl) -> StatusResult {
    let mut status = ctl.state.borrow().status();
    status.pairing = crate::pairing::status(ctl);
    // Present from the start, like the other counters: events a slow reader never saw
    // because a newer one replaced them.
    status
        .counters
        .insert("events_superseded".to_string(), ctl.hub.superseded());
    status
}

/// One parsed request (`RequestLine::parse` already told an unknown method from bad params).
pub async fn dispatch(ctl: &Ctl, request: Request) -> Result<Value, ApiError> {
    match request {
        Request::Status => to_value(&status_snapshot(ctl)),
        Request::Release => release(ctl).await,
        Request::Go(p) => go(ctl, p).await,
        Request::Id => to_value(&ctl.state.borrow().id_result()),
        Request::Trust(p) => trust(ctl, p).await,
        Request::LayoutGet => to_value(&ctl.state.borrow().layout_view()),
        Request::LayoutPlace(p) => layout_place(ctl, p).await,
        Request::LayoutSet(p) => layout_set(ctl, p).await,
        Request::Lock(p) => lock(ctl, p).await,
        Request::Peers => to_value(&ctl.state.borrow().peers_view()),
        Request::Unpair(p) => crate::pairing::unpair(ctl, p).await,
        // their events go to the connection that asked: see `dispatch_conn`
        Request::PairAccept
        | Request::PairStart(_)
        | Request::PairAnswer(_)
        | Request::PairCancel => Err(internal("a pairing method needs its socket connection")),
        Request::Doctor => to_value(&crate::doctor::collect(ctl).await),
        Request::Subscribe(_) | Request::Unsubscribe => {
            Err(internal("a subscription needs its socket connection"))
        }
        Request::ConfigSchema => to_value(&crate::config_api::schema()),
        Request::ConfigGet(p) => to_value(&crate::config_api::get(ctl, p)?),
        Request::ConfigSet(p) => to_value(&crate::config_api::set(ctl, p).await?),
        Request::PeerRename(p) => peer_rename(ctl, p).await,
    }
}

/// [`dispatch`], for a request that came on socket connection `conn`: the pairing methods
/// belong to the connection that started the pairing.
pub async fn dispatch_conn(
    ctl: &Ctl,
    conn: &crate::server::ConnCtx,
    request: Request,
) -> Result<Value, ApiError> {
    match request {
        Request::PairAccept => crate::pairing::accept(ctl, conn).await,
        Request::PairStart(p) => crate::pairing::start(ctl, conn, p).await,
        Request::PairAnswer(p) => crate::pairing::answer(ctl, conn, p.matches).await,
        Request::PairCancel => crate::pairing::cancel(ctl, conn).await,
        Request::Subscribe(p) => subscribe(ctl, conn, p),
        Request::Unsubscribe => {
            ctl.hub.unsubscribe(conn.id);
            Ok(json!({}))
        }
        other => dispatch(ctl, other).await,
    }
}

/// `subscribe`: the answer now, the events after it (`server.rs` starts them once the answer is
/// queued, so a snapshot never precedes it).
fn subscribe(
    ctl: &Ctl,
    conn: &crate::server::ConnCtx,
    p: SubscribeParams,
) -> Result<Value, ApiError> {
    let wanted = p.topics.unwrap_or_else(|| Topic::ALL.to_vec());
    // in the order of `hello.topics`, each once
    let topics: Vec<Topic> = Topic::ALL
        .into_iter()
        .filter(|t| wanted.contains(t))
        .collect();
    if topics.is_empty() {
        ctl.hub.unsubscribe(conn.id);
    } else {
        *conn.after.borrow_mut() = Some(crate::server::After::Subscribe(topics.clone()));
    }
    to_value(&SubscribeResult { topics })
}

/// Lock to this machine: persisted first, because a lock a restart forgets
/// is not one; then the capture thread (`LockedHere`: its barriers go once the
/// pointer is home) and the peers (`PeerState`). It never ends a session.
async fn lock(ctl: &Ctl, p: LockParams) -> Result<Value, ApiError> {
    let on = p.on.unwrap_or_else(|| !ctl.state.borrow().locked);
    StateFile { locked: on }
        .save(&ctl.paths.state_file())
        .map_err(internal)?;
    ctl.state.borrow_mut().locked = on;
    if let Err(e) = ctl
        .capture
        .try_send(CaptureCmd::Event(SourceEvent::LockedHere(on)))
    {
        tracing::error!(error = %e, "cannot hand the lock to the capture thread");
    }
    if ctl.net.send(NetCmd::SetLocked(on)).await.is_err() {
        tracing::warn!("the net thread is gone: the peers were not told");
    }
    tracing::info!(locked = on, "lock to this machine");
    to_value(&LockResult { locked: on })
}

/// Force a release from any state: both machines get `ForceRelease`, and ctl
/// dispatches `release_input_capture()` over Hyprland IPC.
async fn release(ctl: &Ctl) -> Result<Value, ApiError> {
    {
        let mut s = ctl.state.borrow_mut();
        s.bump("releases");
        s.last_release = Some(std::time::Instant::now());
    }
    if !crate::ctl::force_release(ctl).await {
        tracing::warn!("a thread did not take the release");
        return Err(internal("a thread did not take the release"));
    }
    tracing::info!("force release requested over the socket");
    Ok(json!({}))
}

/// `go`: read the cursor, then hand the SourceMachine the side; it warps and nudges,
/// goes home or hops, or logs why it did nothing.
async fn go(ctl: &Ctl, p: GoParams) -> Result<Value, ApiError> {
    if !ctl.capture_on {
        tracing::info!("key crossing: capture is off on this machine");
        return Ok(json!({}));
    }
    let cursor = match (&ctl.fake_portal, &ctl.ipc) {
        (Some(fake), _) => fake.cursor(),
        (None, Some(ipc)) => crate::ctl::cursor_pos(ipc, crate::ctl::CURSOR_TIMEOUT).await,
        (None, None) => None,
    };
    let ev = SourceEvent::Go {
        side: p.side.side(),
        cursor: cursor.map(|(x, y)| ferry_core::geom::Point { x, y }),
    };
    if ctl.capture.try_send(CaptureCmd::Event(ev)).is_err() {
        return Err(internal("the capture thread did not take the request"));
    }
    tracing::info!(
        side = p.side.as_str(),
        "key crossing requested over the socket"
    );
    Ok(json!({}))
}

async fn trust(ctl: &Ctl, p: TrustParams) -> Result<Value, ApiError> {
    let fp: Fingerprint = p.fingerprint.parse().map_err(|_| {
        err(
            ErrorCode::BadFingerprint,
            format!("not a fingerprint: {:?}", p.fingerprint),
            Some("a fingerprint is 64 hex digits, as `ferry id` prints it"),
        )
    })?;
    if let Some(name) = &p.name {
        validate_peer_name(name).map_err(|m| err(ErrorCode::BadParams, m, None))?;
    }
    if let Some(addr) = &p.addr {
        parse_host_port(addr, ferry_net::DEFAULT_PORT).map_err(|e| {
            err(
                ErrorCode::BadParams,
                format!("addr {addr:?}: {e}"),
                Some("write host, host:port or ip:port"),
            )
        })?;
    }

    // Change a copy, persist it, and only then make it live: a failed write leaves no pin that a
    // restart would forget.
    let (pin, outcome, fps) = {
        let s = ctl.state.borrow();
        if s.me.fp == fp {
            return Err(err(
                ErrorCode::SelfFingerprint,
                "that is this machine's own fingerprint",
                Some("run `ferry id` on the other machine and trust that fingerprint"),
            ));
        }
        let mut trust = s.trust.clone();
        let outcome = trust
            .upsert(fp, p.name.as_deref(), p.addr.as_deref())
            .map_err(|TrustError::NameTaken { name, fingerprint }| {
                err(
                    ErrorCode::BadParams,
                    format!("the name {name:?} already belongs to peer {fingerprint}"),
                    Some("pick another --name"),
                )
            })?;
        if outcome == Upsert::Added {
            trust.set_paired_at(&fp, &crate::trust_store::now_rfc3339());
        }
        if outcome != Upsert::Unchanged {
            trust.save(&ctl.paths.trusted_file()).map_err(internal)?;
        }
        let pin = trust.get(&fp).cloned().expect("just upserted");
        (pin, outcome, trust)
    };
    {
        let mut s = ctl.state.borrow_mut();
        s.trust = fps;
        s.refusals.retain(|r| r.fingerprint != fp);
        s.links.entry(fp).or_insert(crate::state::LinkInfo {
            state: ferry_core::api::LinkState::Offline,
            remote: None,
            rtt_us: None,
            proto: None,
            detail: None,
        });
    }
    tracing::info!(peer = %pin.name, fingerprint = %fp, ?outcome, "trust");

    // Swap the live trust set and wait until it is live.
    let all = ctl.state.borrow().trust.fingerprints();
    let (ack, acked) = oneshot::channel();
    ctl.net
        .send(NetCmd::SetTrust { fps: all, ack })
        .await
        .map_err(|_| internal("the net thread is gone"))?;
    acked
        .await
        .map_err(|_| internal("the net thread did not confirm the new trust set"))?;

    // Another request ran on ctl during those awaits: an `unpair` of this peer removed the pin
    // and the link. There is nothing to dial then.
    if ctl.state.borrow().trust.get(&fp).is_none() {
        return Err(err(
            ErrorCode::UnknownPeer,
            format!("{} was unpaired while it was being trusted", pin.name),
            Some("ferry peers lists the peers"),
        ));
    }

    // A new pin can turn a wall into a link.
    crate::ctl::push_topology(ctl);

    // Dial if the request gave an address.
    if let Some(addr) = &p.addr {
        if let Some(l) = ctl.state.borrow_mut().links.get_mut(&fp)
            && l.state != ferry_core::api::LinkState::Connected
        {
            l.state = ferry_core::api::LinkState::Connecting;
            l.detail = None;
        }
        ctl.net
            .send(NetCmd::Dial {
                peer: fp,
                addr: addr.clone(),
            })
            .await
            .map_err(|_| internal("the net thread is gone"))?;
    }
    to_value(&json!({ "peer": PeerRef { name: pin.name, fingerprint: fp.to_string() } }))
}

/// `peer.rename`: another name for a trusted peer. Persisted first, then live, like `trust`.
async fn peer_rename(ctl: &Ctl, p: PeerRenameParams) -> Result<Value, ApiError> {
    validate_peer_name(&p.name).map_err(|m| err(ErrorCode::BadParams, m, None))?;
    let (fp, next, changed) = {
        let s = ctl.state.borrow();
        let id = s.resolve_peer(&p.peer)?;
        if id == s.me.id {
            return Err(err(
                ErrorCode::BadParams,
                "that is this machine; its name is the setting peer.name",
                Some("ferry config set peer.name <name>"),
            ));
        }
        if p.name == s.me.name {
            return Err(err(
                ErrorCode::BadParams,
                format!("{:?} is this machine's own name", p.name),
                Some("pick another name"),
            ));
        }
        let fp = Fingerprint(id.0);
        let mut trust = s.trust.clone();
        let outcome = trust.upsert(fp, Some(&p.name), None).map_err(
            |TrustError::NameTaken { name, fingerprint }| {
                err(
                    ErrorCode::BadParams,
                    format!("the name {name:?} already belongs to peer {fingerprint}"),
                    Some("pick another name"),
                )
            },
        )?;
        (fp, trust, outcome != Upsert::Unchanged)
    };
    if changed {
        next.save(&ctl.paths.trusted_file()).map_err(internal)?;
        ctl.state.borrow_mut().trust = next;
        tracing::info!(peer = %fp, name = %p.name, "peer.rename");
    }
    to_value(&RenameResult {
        peer: PeerRef {
            name: p.name,
            fingerprint: fp.to_string(),
        },
    })
}

fn align(a: AlignParam) -> Align {
    match a {
        AlignParam::Default => Align::Start,
        AlignParam::Named(AlignName::Start) => Align::Start,
        AlignParam::Named(AlignName::Center) => Align::Center,
        AlignParam::Named(AlignName::End) => Align::End,
        AlignParam::Px(px) => Align::Offset(px),
    }
}

async fn layout_place(ctl: &Ctl, p: PlaceParams) -> Result<Value, ApiError> {
    let (view, wire) = place(ctl, p)?;
    publish(ctl, wire).await;
    Ok(view)
}

/// The barriers follow the new topology, and every up peer gets the whole layout;
/// it keeps the higher revision.
pub(crate) async fn publish(ctl: &Ctl, wire: ferry_core::wire::Control) {
    crate::ctl::push_topology(ctl);
    if ctl.net.send(NetCmd::Broadcast(wire)).await.is_err() {
        tracing::warn!("the net thread is gone: the layout was not pushed");
    }
}

/// Saves `layout` as the new layout and remembers it as this peer's own edit. A
/// failed write leaves the old layout in force and answers `internal`.
fn commit(
    ctl: &Ctl,
    s: &mut crate::state::State,
    layout: Layout,
) -> Result<ferry_core::wire::Control, ApiError> {
    let mut next = s.layout.clone();
    next.layout = layout;
    next.save(&ctl.paths.layout_file()).map_err(internal)?;
    s.layout = next;
    s.edits.made(s.layout.layout.rev);
    Ok(s.layout.layout.to_wire())
}

/// A layout of more peers than a `Layout` frame carries would be dropped by every other peer.
fn check_peer_cap(layout: &Layout) -> Result<(), ApiError> {
    if layout.origins.len() > ferry_core::wire::caps::LAYOUT_PEERS {
        return Err(err(
            ErrorCode::BadParams,
            format!(
                "a layout holds at most {} peers",
                ferry_core::wire::caps::LAYOUT_PEERS
            ),
            Some("unpair a peer you no longer use"),
        ));
    }
    Ok(())
}

fn place(ctl: &Ctl, p: PlaceParams) -> Result<(Value, ferry_core::wire::Control), ApiError> {
    let mut s = ctl.state.borrow_mut();
    let peer = s.resolve_peer(&p.peer)?;
    if peer == s.me.id {
        return Err(err(
            ErrorCode::BadParams,
            "this machine is the reference of the layout; place the other peers around it",
            None,
        ));
    }
    let anchor = s.resolve_anchor(&p.anchor)?;
    let peer_name = s.name_of(peer);
    if s.monitors_of(peer).is_empty() {
        return Err(err(
            ErrorCode::PeerMonitorsUnknown,
            format!("{peer_name} has never connected"),
            Some("start ferryd on that machine and check ferry status"),
        ));
    }

    // My own origin defaults to (0, 0) the first time anything is placed.
    let mut base = s.layout.layout.clone();
    if !base.origins.iter().any(|o| o.peer == s.me.id) {
        base.origins.insert(
            0,
            Origin {
                peer: s.me.id,
                x: 0,
                y: 0,
            },
        );
    }
    let origin = geom::place(
        &base,
        &s.layout_monitors(),
        peer,
        p.rel,
        anchor,
        align(p.align),
    )
    .map_err(|e| match e {
        PlaceError::SelfAnchor => err(
            ErrorCode::BadParams,
            "cannot place a peer relative to itself",
            None,
        ),
        PlaceError::UnknownMonitors(who) => err(
            ErrorCode::PeerMonitorsUnknown,
            format!("{} has never connected", s.name_of(who)),
            Some("start ferryd on that machine and check ferry status"),
        ),
        PlaceError::AnchorUnplaced(who) => err(
            ErrorCode::UnknownAnchor,
            format!("{} is not placed yet", s.name_of(who)),
            Some("place the anchor first"),
        ),
        PlaceError::Overlap(who) => err(
            ErrorCode::Overlap,
            format!("the placement overlaps {}", s.name_of(who)),
            Some("pick another side, or another --align"),
        ),
        PlaceError::OutOfRange => err(
            ErrorCode::BadParams,
            "the placement is too far from the rest of the layout",
            Some("use a smaller --align offset"),
        ),
    })?;

    let me = s.me.id;
    let layout = s.layout.with_origin(me, origin);
    check_peer_cap(&layout)?;
    let wire = commit(ctl, &mut s, layout)?;
    tracing::info!(peer = %peer_name, x = origin.x, y = origin.y, rev = s.layout.layout.rev.counter, "layout.place");
    Ok((to_value(&s.layout_view())?, wire))
}

/// Places `peer`, which this machine just paired as the initiating peer, right of
/// this machine's group, tops aligned, as `layout.place <peer> right-of <me>` would. Called at the
/// peer's first connection, when its monitors are known. Returns the new layout to send, or
/// `None` when nothing was placed: the peer is placed already (another peer placed it, or it was
/// paired before), or the place is taken, which is logged and left to the user.
pub(crate) fn place_paired(ctl: &Ctl, fp: Fingerprint) -> Option<ferry_core::wire::Control> {
    let mut s = ctl.state.borrow_mut();
    if !s.place_on_up.remove(&fp) {
        return None;
    }
    let (me, peer) = (s.me.id, crate::layout_store::peer_id(fp));
    if s.layout.origin_of(peer).is_some() {
        return None;
    }
    let mut base = s.layout.layout.clone();
    if !base.origins.iter().any(|o| o.peer == me) {
        base.origins.insert(
            0,
            Origin {
                peer: me,
                x: 0,
                y: 0,
            },
        );
    }
    let name = s.name_of(peer);
    let origin = match geom::place(
        &base,
        &s.layout_monitors(),
        peer,
        geom::Rel::RightOf,
        geom::Anchor::Peer(me),
        Align::Start,
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::info!(peer = %name, error = ?e, "a newly paired peer is not placed: place it with ferry layout");
            return None;
        }
    };
    let layout = s.layout.with_origin(me, origin);
    if check_peer_cap(&layout).is_err() {
        tracing::info!(peer = %name, "a newly paired peer is not placed: the layout is full");
        return None;
    }
    match commit(ctl, &mut s, layout) {
        Ok(wire) => {
            tracing::info!(peer = %name, x = origin.x, y = origin.y, rev = s.layout.layout.rev.counter, "layout: placed the newly paired peer right of this machine");
            Some(wire)
        }
        Err(e) => {
            tracing::warn!(peer = %name, error = %e.message, "layout: the newly paired peer is not placed");
            None
        }
    }
}

/// Removes an unpaired peer from the layout in a new revision. Returns the layout
/// to send, or `None` when the peer was not placed (or the write failed, which is logged and
/// leaves the old layout in force).
pub(crate) fn remove_unpaired(ctl: &Ctl, fp: Fingerprint) -> Option<ferry_core::wire::Control> {
    let mut s = ctl.state.borrow_mut();
    s.place_on_up.remove(&fp);
    let (me, peer) = (s.me.id, crate::layout_store::peer_id(fp));
    let layout = s.layout.layout.without(me, peer)?;
    match commit(ctl, &mut s, layout) {
        Ok(wire) => {
            tracing::info!(peer = %fp, rev = s.layout.layout.rev.counter, "layout: removed the unpaired peer");
            Some(wire)
        }
        Err(e) => {
            tracing::warn!(peer = %fp, error = %e.message, "layout: the unpaired peer is not removed");
            None
        }
    }
}

/// `layout.set` (M3 design 2.5 and 4.2): any number of groups in one revision.
async fn layout_set(ctl: &Ctl, p: SetParams) -> Result<Value, ApiError> {
    let (view, wire) = set(ctl, p)?;
    if let Some(wire) = wire {
        publish(ctl, wire).await;
    }
    Ok(view)
}

/// The checks run in this order, and the first to fail answers: the params (`bad_params`,
/// `unknown_peer`, `peer_monitors_unknown`), then `base_rev` (`stale_layout`), then the geometry
/// (`overlap`). Nothing changes unless all pass.
fn set(ctl: &Ctl, p: SetParams) -> Result<(Value, Option<ferry_core::wire::Control>), ApiError> {
    let mut s = ctl.state.borrow_mut();
    if p.origins.is_empty() {
        return Err(err(
            ErrorCode::BadParams,
            "origins is empty",
            Some("name at least one peer with its x and y"),
        ));
    }
    let mut moves: Vec<Origin> = Vec::new();
    for o in &p.origins {
        let peer = s.resolve_layout_peer(&o.peer)?;
        let peer_name = s.name_of(peer);
        if moves.iter().any(|m| m.peer == peer) {
            return Err(err(
                ErrorCode::BadParams,
                format!("{peer_name} is named twice"),
                Some("name each peer once"),
            ));
        }
        if !(geom::coord_in_range(o.x) && geom::coord_in_range(o.y)) {
            return Err(err(
                ErrorCode::BadParams,
                format!(
                    "{peer_name}: x and y are at most {} px",
                    ferry_core::wire::caps::COORD
                ),
                None,
            ));
        }
        if s.monitors_of(peer).is_empty() {
            return Err(if peer == s.me.id {
                err(
                    ErrorCode::PeerMonitorsUnknown,
                    "this machine reports no monitors",
                    Some("is Hyprland running? ferry doctor checks"),
                )
            } else {
                err(
                    ErrorCode::PeerMonitorsUnknown,
                    format!("the monitors of {peer_name} were never received"),
                    Some("start ferryd on that machine and check ferry status"),
                )
            });
        }
        moves.push(Origin {
            peer,
            x: o.x,
            y: o.y,
        });
    }

    let now = s.layout.layout.rev;
    if let Some(base) = &p.base_rev {
        let peer = parse_peer(&base.peer).ok_or_else(|| {
            err(
                ErrorCode::BadParams,
                format!("base_rev.peer {:?} is not a fingerprint", base.peer),
                Some("send the revision that layout.get answered"),
            )
        })?;
        let base = Revision {
            counter: base.counter,
            peer,
        };
        if base != now {
            return Err(err(
                ErrorCode::StaleLayout,
                format!(
                    "the layout changed (rev {} by {})",
                    now.counter,
                    s.name_of(now.peer)
                ),
                Some("read the layout again with layout.get, then apply your change on top of it"),
            ));
        }
    }

    let me = s.me.id;
    let next = geom::set_origins(&s.layout.layout, &s.layout_monitors(), me, &moves).map_err(
        |e| match e {
            SetError::Overlap { moved, other } => err(
                ErrorCode::Overlap,
                format!("{} overlaps {}", s.name_of(moved), s.name_of(other)),
                Some("ferry layout show lists where each peer's monitors are"),
            ),
            SetError::OutOfRange(who) => err(
                ErrorCode::BadParams,
                format!(
                    "{}: the origin is too far from the rest of the layout",
                    s.name_of(who)
                ),
                None,
            ),
            SetError::TooManyPeers => err(
                ErrorCode::BadParams,
                e.to_string(),
                Some("unpair a peer you no longer use"),
            ),
        },
    )?;
    let Some(next) = next else {
        // every named origin already holds: no edit, no revision, no push
        return Ok((to_value(&s.layout_view())?, None));
    };
    let wire = commit(ctl, &mut s, next)?;
    tracing::info!(
        peers = moves.len(),
        rev = s.layout.layout.rev.counter,
        "layout.set"
    );
    Ok((to_value(&s.layout_view())?, Some(wire)))
}
