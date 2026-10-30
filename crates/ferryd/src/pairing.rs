//! SAS pairing in ferryd (design `m1-rest-design.md` §2.11).
//!
//! | Thread | Owns |
//! |---|---|
//! | `net` | the pairing window or the dial ([`NetPairing`]), and one attempt at a time: a task that runs the [`PairMachine`] over a [`PairingConnection`] ([`run_attempt`]) |
//! | `ctl` | the socket connection that started the pairing ([`CtlPairing`]), its `pairing` events, and the trust store: the pin is written here ([`on_note`]) |
//!
//! ctl → net: `NetCmd::{PairAccept, PairStart, PairAnswer, PairCancel}`. net → ctl:
//! `CtlMsg::Pairing(PairNote)`. On `Agreed` the attempt runs `close_graceful` (§2.6 condition 3:
//! its own `Confirm` acknowledged) and only then sends [`PairNote::Pinned`].
//!
//! **Nothing secret is logged**: not the code, not a nonce, not the
//! commitment, at any level. The code leaves ferryd only in the `code` event: on the socket
//! connection that started the attempt, and on every connection subscribed to `pairing` (m3
//! design M3-D8: a notification, a panel and a terminal are equals), and never in `status`. Any of
//! them may answer, and any of them may cancel; the first answer wins.
//!
//! **Every pairing ends with exactly one last event** (m3 design section 9, pair-polish): `paired`,
//! `failed` for a dialer, `closed` for an acceptor. An acceptor's `failed` ends one attempt, and
//! `listening` (the window stays open) or `closed` follows it. The run is the [`CtlPairing`] in
//! `ctl.pairing`: each last event is sent where that value is taken out (`on_note`, `pinned`,
//! `start`), so a path that clears it without a word is the thing to look for. A cancel is an
//! ordinary end: from any connection, with `cancelled_by` saying whose it was. A dial that fails
//! ends with a `failed` of its own, for the subscribers; the connection that sent `pair.start`
//! has the error that answers it.

use std::cell::Cell;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ferry_core::api::{
    ApiError, CancelledBy, ErrorCode, EventLine, PairAcceptResult, PairStartParams, PairingEvent,
    PairingPeer, PairingRole, PairingStage, PairingStatus, Topic, UnpairParams,
};
use ferry_core::pair::{
    PairAction, PairCode, PairEvent, PairFailure, PairFrame, PairMachine, PairReason, PairStage,
    Role, Start,
};
use ferry_core::wire::control::Pair;
use ferry_core::wire::{Control, PROTO_MAJOR, PROTO_MINOR};
use ferry_core::{Actions, PeerId};
use ferry_net::{
    CloseCode, ConnectError, Endpoint, Event, Fingerprint, PairingConnection, PairingWindow,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::chan::{CtlMsg, NetCmd};
use crate::clock;
use crate::ctl::Ctl;
use crate::live::SharedLive;
use crate::netaddr::parse_host_port;
use crate::server::ConnCtx;
use crate::trust_store::now_rfc3339;

/// The pairing window (acceptor) and the attempt (dialer): 120 s.
pub const WINDOW: Duration = Duration::from_secs(120);
/// Failed attempts that close the acceptor's window.
pub const MAX_FAILURES: u32 = 3;
/// How long the attempt waits for the peer to acknowledge its last `Confirm` (§2.6 condition 3).
const FINAL_ACK: Duration = Duration::from_secs(2);
/// How long a failed attempt waits for its `Confirm{ok: false}` to go out before it closes.
const FAILED_CLOSE: Duration = Duration::from_millis(500);
/// What `Hello.build` carries.
const BUILD: &str = env!("CARGO_PKG_VERSION");

// --- messages -------------------------------------------------------------------------------

/// net → ctl: how the pairing goes.
pub enum PairNote {
    /// Dialer: dialling, and the exchange before the code.
    Connecting,
    /// The code is shown.
    Code {
        peer: Fingerprint,
        name: String,
        remote: SocketAddr,
        code: PairCode,
    },
    /// The local user said yes; the peer has not yet.
    WaitingPeer,
    /// §2.6's three conditions hold: write the pin.
    Pinned {
        role: Role,
        peer: Fingerprint,
        /// The peer's `Hello.name`, or its default name when that was not valid.
        name: String,
        /// Why the peer's `Hello.name` was not valid (an escaped, printable text).
        name_refused: Option<String>,
        remote: SocketAddr,
    },
    /// This attempt ended without a pin. `cancelled_by` says whose cancel it was, when it was one.
    Failed {
        reason: String,
        message: String,
        cancelled_by: Option<CancelledBy>,
    },
    /// Acceptor: the window stays open for another attempt.
    Listening { until: Instant },
    /// Acceptor: the window ended without a pin.
    Closed {
        reason: String,
        message: String,
        cancelled_by: Option<CancelledBy>,
    },
}

impl std::fmt::Debug for PairNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => f.write_str("Connecting"),
            // never the code
            Self::Code { peer, remote, .. } => f
                .debug_struct("Code")
                .field("peer", peer)
                .field("remote", remote)
                .finish_non_exhaustive(),
            Self::WaitingPeer => f.write_str("WaitingPeer"),
            Self::Pinned { role, peer, .. } => f
                .debug_struct("Pinned")
                .field("role", role)
                .field("peer", peer)
                .finish_non_exhaustive(),
            Self::Failed { reason, .. } => {
                f.debug_struct("Failed").field("reason", reason).finish()
            }
            Self::Listening { .. } => f.write_str("Listening"),
            Self::Closed { reason, .. } => {
                f.debug_struct("Closed").field("reason", reason).finish()
            }
        }
    }
}

/// Why net refused a pairing command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// A pairing already runs on this machine.
    Busy,
    /// No pairing, or no attempt to answer.
    NoPairing,
    /// The peer has no window open, is busy, or this address is over its limit.
    NotAccepting,
    /// The peer did not answer, or the address does not resolve.
    Unreachable(String),
    /// The address is this machine.
    OwnKey,
    /// The peer closed the connection while it was opening (the text says how).
    Closed(String),
    /// The peer speaks another major protocol version.
    Version,
    Other(String),
}

pub type PairReply = Result<(), PairError>;

// --- one attempt (on net) -------------------------------------------------------------------

/// What an attempt task needs besides its connection.
pub(crate) struct AttemptSpec {
    pub role: Role,
    pub me: Fingerprint,
    pub hello_name: String,
    pub until: Instant,
}

#[derive(Debug)]
pub(crate) enum AttemptCmd {
    Answer(bool),
    Cancel,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Pinned {
        peer: Fingerprint,
        name: String,
        name_refused: Option<String>,
        remote: SocketAddr,
    },
    Failed {
        failure: PairFailure,
        /// The local user cancelled (Ctrl+C, `pair.cancel`, the socket closed).
        local_cancel: bool,
    },
}

fn micros_at(t: Instant) -> ferry_core::Micros {
    let now = clock::now();
    now.plus(ferry_core::Micros(
        u64::try_from(t.saturating_duration_since(Instant::now()).as_micros()).unwrap_or(u64::MAX),
    ))
}

async fn sleep_until(deadline: Option<ferry_core::Micros>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(clock::instant_at(d).into()).await,
        None => std::future::pending().await,
    }
}

/// One attempt over `pc`, from both `Hello`s to the pin. Returns once the connection is
/// closed; `pc` is dropped first, so the acceptor's endpoint can take the next attempt.
pub(crate) async fn run_attempt(
    mut pc: PairingConnection,
    spec: AttemptSpec,
    mut cmds: mpsc::UnboundedReceiver<AttemptCmd>,
    ctl: mpsc::Sender<CtlMsg>,
) -> Outcome {
    let (peer, remote) = (pc.peer_fp(), pc.remote);
    let mut nonce = [0u8; ferry_core::pair::NONCE_LEN];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).is_err() {
        tracing::error!("pairing: the OS random number generator failed");
        pc.conn.close(CloseCode::SHUTDOWN, "");
        return Outcome::Failed {
            failure: PairFailure::Reason(PairReason::Other),
            local_cancel: false,
        };
    }
    let mut m = PairMachine::new();
    let mut out = Actions::new();
    let mut buf = Vec::with_capacity(256);
    let (mut agreed, mut failure, mut close_code, mut local_cancel) = (false, None, None, false);
    let note = |n: PairNote| {
        let ctl = ctl.clone();
        async move {
            let _ = ctl.send(CtlMsg::Pairing(n)).await;
        }
    };
    m.handle(
        PairEvent::Start(Start {
            role: spec.role,
            me: PeerId(spec.me.0),
            peer: PeerId(peer.0),
            nonce,
            until: micros_at(spec.until),
            hello: Control::Hello {
                proto_major: PROTO_MAJOR,
                proto_minor: PROTO_MINOR,
                name: spec.hello_name.clone(),
                features: Vec::new(),
                build: BUILD.to_string(),
                monitors: Vec::new(),
            },
        }),
        clock::now(),
        &mut out,
    );
    loop {
        let actions: Vec<PairAction> = out.drain().collect();
        for a in actions {
            match a {
                PairAction::Send(f) => {
                    f.encode(&mut buf);
                    if let Err(e) = pc.conn.try_send_frame(Bytes::copy_from_slice(&buf)) {
                        tracing::debug!(error = %e, variant = f.variant(), "pairing: frame not sent");
                    }
                }
                PairAction::ShowCode { code, peer_name } => {
                    tracing::info!(peer = %peer, name = %peer_name, %remote, "pairing: the code is shown");
                    note(PairNote::Code {
                        peer,
                        name: peer_name,
                        remote,
                        code,
                    })
                    .await;
                }
                PairAction::Agreed { .. } => agreed = true,
                PairAction::Failed(f) => failure = Some(f),
                PairAction::Close { code } => close_code = Some(code),
            }
        }
        if agreed || failure.is_some() {
            break;
        }
        tokio::select! {
            ev = pc.events.next() => match ev {
                Some(Event::Frame(body)) => match PairFrame::decode(&body) {
                    Ok(f) => m.handle(PairEvent::Frame(f), clock::now(), &mut out),
                    Err(e) => {
                        tracing::warn!(peer = %peer, error = %e, "pairing: a frame that is not valid on ferry-pair/1: closing");
                        out.push(PairAction::Send(PairFrame::Pair(Pair::Confirm {
                            ok: false,
                            reason: Some(PairReason::Protocol.code()),
                        })));
                        out.push(PairAction::Failed(PairFailure::Reason(PairReason::Protocol)));
                        out.push(PairAction::Close { code: CloseCode::PROTOCOL.0 });
                    }
                },
                Some(Event::Datagram(_)) => {}
                Some(Event::Closed(reason)) => {
                    tracing::debug!(peer = %peer, %reason, "pairing: the connection ended");
                    m.handle(PairEvent::Closed, clock::now(), &mut out);
                }
                None => m.handle(PairEvent::Closed, clock::now(), &mut out),
            },
            cmd = cmds.recv() => match cmd {
                Some(AttemptCmd::Answer(ok)) => {
                    let before = m.stage();
                    m.handle(PairEvent::Answer(ok), clock::now(), &mut out);
                    tracing::info!(peer = %peer, matches = ok, "pairing: the local user answered");
                    if before == PairStage::Comparing && m.stage() == PairStage::WaitingPeer {
                        note(PairNote::WaitingPeer).await;
                    }
                }
                Some(AttemptCmd::Cancel) | None => {
                    local_cancel = true;
                    m.handle(PairEvent::Cancel, clock::now(), &mut out);
                }
            },
            () = sleep_until(m.next_deadline()) => m.handle(PairEvent::Tick, clock::now(), &mut out),
        }
    }

    if let Some(failure) = failure {
        match &failure {
            PairFailure::Reason(PairReason::BadReveal) => tracing::warn!(
                peer = %peer, %remote,
                "pairing: the peer's reveal does not match its commitment: a possible man in the middle"
            ),
            f => tracing::info!(peer = %peer, reason = f.as_str(), "pairing failed"),
        }
        let code = CloseCode(close_code.unwrap_or(CloseCode::SHUTDOWN.0));
        pc.conn
            .close_graceful(code, "pairing ended", FAILED_CLOSE)
            .await;
        drop(pc);
        return Outcome::Failed {
            failure,
            local_cancel,
        };
    }

    // §2.6 condition 3: the peer acknowledged this side's `Confirm`. A `Confirm{ok: false}` that
    // still arrives before the peer's close turns the agreement into a failure.
    tracing::info!(peer = %peer, "pairing: both said yes; waiting for the acknowledgement");
    let conn = pc.conn.clone();
    let events = &mut pc.events;
    let drain = async {
        while let Some(ev) = events.next().await {
            match ev {
                Event::Frame(body) => {
                    if let Ok(f) = PairFrame::decode(&body) {
                        m.handle(PairEvent::Frame(f), clock::now(), &mut out);
                    }
                }
                Event::Closed(_) => break,
                Event::Datagram(_) => {}
            }
        }
    };
    let (acked, ()) = tokio::join!(
        conn.close_graceful(CloseCode::SHUTDOWN, "paired", FINAL_ACK),
        drain
    );
    let name = m.peer_name().to_string();
    let name_refused = m.peer_name_refused().map(str::to_string);
    let late = out.drain().find_map(|a| match a {
        PairAction::Failed(f) => Some(f),
        _ => None,
    });
    drop(pc);
    if let Some(failure) = late {
        tracing::info!(peer = %peer, reason = failure.as_str(), "pairing failed after both said yes");
        return Outcome::Failed {
            failure,
            local_cancel: false,
        };
    }
    if !acked {
        tracing::info!(peer = %peer, "pairing: the peer did not acknowledge the final confirm: nothing pinned here");
        return Outcome::Failed {
            failure: PairFailure::Lost,
            local_cancel: false,
        };
    }
    Outcome::Pinned {
        peer,
        name,
        name_refused,
        remote,
    }
}

// --- the window and the dial (on net) -------------------------------------------------------

/// Why a dial did not reach a pairing connection.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DialError {
    /// The address did not resolve (the text says what it was and why).
    #[error("{0}")]
    Resolve(String),
    /// The connection attempt failed.
    #[error(transparent)]
    Connect(ConnectError),
}

impl From<DialError> for PairError {
    fn from(e: DialError) -> Self {
        match e {
            DialError::Resolve(text) => PairError::Unreachable(text),
            DialError::Connect(e) => match e {
                ConnectError::NotAccepting => PairError::NotAccepting,
                ConnectError::OwnKey => PairError::OwnKey,
                ConnectError::VersionMismatch => PairError::Version,
                ConnectError::TimedOut => PairError::Unreachable(format!(
                    "no answer within {} s",
                    ferry_net::EndpointConfig::default()
                        .handshake_timeout
                        .as_secs()
                )),
                // the peer closed before the connection opened, or gave the handshake up
                e @ (ConnectError::Closed(_) | ConnectError::Abandoned) => {
                    PairError::Closed(e.to_string())
                }
                other => PairError::Other(other.to_string()),
            },
        }
    }
}

/// From the attempt and dial tasks back to the net loop.
pub(crate) enum PairInternal {
    Dialed(Result<PairingConnection, DialError>),
    Done { id: u64, outcome: Outcome },
}

struct Running {
    id: u64,
    cmds: mpsc::UnboundedSender<AttemptCmd>,
}

struct Session {
    role: Role,
    until: Instant,
    failures: u32,
    attempt: Option<Running>,
    /// Dialer: the `pair.start` reply, sent once the dial is done.
    dialing: Option<oneshot::Sender<PairReply>>,
    cancelled: bool,
}

/// A cancel on this machine ended the attempt (a `pair.cancel` from any connection, the
/// connection that started the pairing closing, ferryd stopping).
fn attempt_cancelled() -> PairNote {
    PairNote::Failed {
        reason: PairReason::Cancelled.as_str().into(),
        message: PairFailure::Reason(PairReason::Cancelled).message(),
        cancelled_by: Some(CancelledBy::Local),
    }
}

/// A cancel on this machine closed the acceptor's window.
fn window_cancelled() -> PairNote {
    PairNote::Closed {
        reason: PairReason::Cancelled.as_str().into(),
        message: "pairing window closed: cancelled".into(),
        cancelled_by: Some(CancelledBy::Local),
    }
}

/// The 120 s ran out with no attempt running.
fn window_timed_out() -> PairNote {
    PairNote::Closed {
        reason: PairReason::Timeout.as_str().into(),
        message: "pairing window closed: timed out".into(),
        cancelled_by: None,
    }
}

/// Whose cancel `failure` is, when it is one: this machine's if a cancel reached the attempt,
/// else the other peer's, which sent `Confirm{ok: false}` with the reason `cancelled`. (When both
/// cancel in the same moment the event says local, and the window then closes as `cancelled` too.)
fn cancelled_by(failure: &PairFailure, local_cancel: bool) -> Option<CancelledBy> {
    matches!(failure, PairFailure::Reason(PairReason::Cancelled)).then_some(if local_cancel {
        CancelledBy::Local
    } else {
        CancelledBy::Peer
    })
}

/// The net thread's side of pairing: at most one window or dial, and one attempt at a time.
pub(crate) struct NetPairing {
    endpoint: Endpoint,
    me: Fingerprint,
    name: String,
    /// For the attempt tasks, which may wait for ctl; the loop's notes go through `out`.
    ctl: mpsc::Sender<CtlMsg>,
    out: crate::ctl_out::CtlOut,
    live: SharedLive,
    tx: mpsc::Sender<PairInternal>,
    session: Option<Session>,
    next_id: u64,
}

impl NetPairing {
    pub(crate) fn new(
        endpoint: Endpoint,
        name: String,
        ctl: mpsc::Sender<CtlMsg>,
        out: crate::ctl_out::CtlOut,
        live: SharedLive,
        tx: mpsc::Sender<PairInternal>,
    ) -> Self {
        Self {
            me: endpoint.fingerprint(),
            endpoint,
            name,
            ctl,
            out,
            live,
            tx,
            session: None,
            next_id: 0,
        }
    }

    /// When the acceptor's window ends, for the net loop's timer.
    pub(crate) fn window_end(&self) -> Option<Instant> {
        self.session
            .as_ref()
            .filter(|s| s.role == Role::Acceptor && s.attempt.is_none())
            .map(|s| s.until)
    }

    /// To ctl in order, without waiting for it (RG-CONC-02: this runs on the net loop).
    fn note(&self, n: PairNote) {
        self.out.push(CtlMsg::Pairing(n));
    }

    pub(crate) async fn command(&mut self, cmd: NetCmd) {
        match cmd {
            NetCmd::PairAccept { until, reply } => {
                if self.session.is_some() {
                    let _ = reply.send(Err(PairError::Busy));
                    return;
                }
                self.endpoint.set_pairing(Some(PairingWindow { until }));
                self.session = Some(Session {
                    role: Role::Acceptor,
                    until,
                    failures: 0,
                    attempt: None,
                    dialing: None,
                    cancelled: false,
                });
                tracing::info!(
                    secs = until.saturating_duration_since(Instant::now()).as_secs(),
                    "pairing window open"
                );
                let _ = reply.send(Ok(()));
            }
            NetCmd::PairStart { addr, until, reply } => {
                if self.session.is_some() {
                    let _ = reply.send(Err(PairError::Busy));
                    return;
                }
                self.session = Some(Session {
                    role: Role::Dialer,
                    until,
                    failures: 0,
                    attempt: None,
                    dialing: Some(reply),
                    cancelled: false,
                });
                self.note(PairNote::Connecting);
                let (endpoint, tx) = (self.endpoint.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let resolved = match parse_host_port(&addr, ferry_net::DEFAULT_PORT) {
                        Ok(hp) => hp.resolve().await.map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    let result = match resolved {
                        Ok(sock) => {
                            tracing::info!(addr = %sock, "pairing: dialing");
                            endpoint
                                .dial_pairing(sock)
                                .await
                                .map_err(DialError::Connect)
                        }
                        Err(e) => Err(DialError::Resolve(format!("cannot resolve {addr}: {e}"))),
                    };
                    let _ = tx.send(PairInternal::Dialed(result)).await;
                });
            }
            NetCmd::PairAnswer { ok, reply } => {
                let r = match self.session.as_ref().and_then(|s| s.attempt.as_ref()) {
                    Some(a) => {
                        let _ = a.cmds.send(AttemptCmd::Answer(ok));
                        Ok(())
                    }
                    None => Err(PairError::NoPairing),
                };
                let _ = reply.send(r);
            }
            NetCmd::PairCancel { reply } => {
                let r = self.cancel().await;
                let _ = reply.send(r);
            }
            other => tracing::error!(?other, "not a pairing command"),
        }
    }

    async fn cancel(&mut self) -> PairReply {
        let Some(s) = self.session.as_mut() else {
            return Err(PairError::NoPairing);
        };
        s.cancelled = true;
        if let Some(a) = &s.attempt {
            let _ = a.cmds.send(AttemptCmd::Cancel);
            return Ok(());
        }
        if s.dialing.is_some() {
            return Ok(()); // the dial's end sees `cancelled`
        }
        // an acceptor with no attempt running
        self.end(window_cancelled()).await;
        Ok(())
    }

    async fn end(&mut self, note: PairNote) {
        if self
            .session
            .take()
            .is_some_and(|s| s.role == Role::Acceptor)
        {
            self.endpoint.set_pairing(None);
            tracing::info!("pairing window closed");
        }
        self.note(note);
    }

    fn spawn_attempt(&mut self, pc: PairingConnection) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        self.next_id += 1;
        let id = self.next_id;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        s.attempt = Some(Running { id, cmds: cmd_tx });
        self.live.bump("pair_attempts");
        let spec = AttemptSpec {
            role: s.role,
            me: self.me,
            hello_name: self.name.clone(),
            until: s.until,
        };
        let (ctl, tx) = (self.ctl.clone(), self.tx.clone());
        tokio::spawn(async move {
            let outcome = run_attempt(pc, spec, cmd_rx, ctl).await;
            let _ = tx.send(PairInternal::Done { id, outcome }).await;
        });
    }

    /// An accepted `ferry-pair/1` connection.
    pub(crate) fn incoming(&mut self, pc: PairingConnection) {
        let open = self.session.as_ref().is_some_and(|s| {
            s.role == Role::Acceptor && s.attempt.is_none() && Instant::now() < s.until
        });
        if !open {
            tracing::debug!(remote = %pc.remote, "pairing: no window for this connection: closed");
            return; // dropping it closes it
        }
        tracing::info!(peer = %pc.peer_fp(), remote = %pc.remote, "pairing: a request arrived");
        self.spawn_attempt(pc);
    }

    pub(crate) async fn internal(&mut self, msg: PairInternal) {
        match msg {
            PairInternal::Dialed(result) => {
                let Some(s) = self.session.as_mut() else {
                    return;
                };
                let reply = s.dialing.take();
                let cancelled = s.cancelled;
                let send = |r: PairReply| {
                    if let Some(reply) = reply {
                        let _ = reply.send(r);
                    }
                };
                match result {
                    Ok(pc) if !cancelled => {
                        send(Ok(()));
                        self.spawn_attempt(pc);
                    }
                    Ok(_) => {
                        send(Ok(()));
                        self.end(attempt_cancelled()).await;
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "pairing: the dial failed");
                        // No note: ctl hears it from the reply, which `pairing::start` turns into
                        // the error that answers `pair.start` and the `failed` event of the
                        // subscribers. A note could reach ctl before or after that reply.
                        self.session = None;
                        send(Err(e.into()));
                    }
                }
            }
            PairInternal::Done { id, outcome } => {
                let Some(s) = self.session.as_mut() else {
                    return;
                };
                if s.attempt.as_ref().is_none_or(|a| a.id != id) {
                    return;
                }
                s.attempt = None;
                match outcome {
                    Outcome::Pinned {
                        peer,
                        name,
                        name_refused,
                        remote,
                    } => {
                        let role = s.role;
                        self.end(PairNote::Pinned {
                            role,
                            peer,
                            name,
                            name_refused,
                            remote,
                        })
                        .await;
                    }
                    Outcome::Failed {
                        failure,
                        local_cancel,
                    } => {
                        self.live.bump("pair_failures");
                        let failed = PairNote::Failed {
                            reason: failure.as_str().into(),
                            message: failure.message(),
                            cancelled_by: cancelled_by(&failure, local_cancel),
                        };
                        if s.role == Role::Dialer {
                            self.end(failed).await;
                            return;
                        }
                        s.failures += 1;
                        let (failures, until, cancelled) = (s.failures, s.until, s.cancelled);
                        self.note(failed);
                        if local_cancel || cancelled {
                            self.end(window_cancelled()).await;
                        } else if failures >= MAX_FAILURES {
                            self.end(PairNote::Closed {
                                reason: "failures".into(),
                                message: format!(
                                    "pairing window closed after {MAX_FAILURES} failed attempts"
                                ),
                                cancelled_by: None,
                            })
                            .await;
                        } else if Instant::now() >= until {
                            self.end(window_timed_out()).await;
                        } else {
                            self.note(PairNote::Listening { until });
                        }
                    }
                }
            }
        }
    }

    /// The acceptor's window ended with no attempt running.
    pub(crate) async fn window_due(&mut self) {
        if self.window_end().is_some_and(|t| Instant::now() >= t) {
            self.end(window_timed_out()).await;
        }
    }

    /// Shutdown: cancel what runs.
    pub(crate) fn shutdown(&mut self) {
        if let Some(a) = self.session.as_ref().and_then(|s| s.attempt.as_ref()) {
            let _ = a.cmds.send(AttemptCmd::Cancel);
        }
        self.endpoint.set_pairing(None);
    }
}

// --- the socket side (on ctl) ---------------------------------------------------------------

/// The pairing a socket connection started. No `Debug`: it holds the code.
pub struct CtlPairing {
    pub owner: u64,
    sink: crate::server::LineSink,
    pub role: PairingRole,
    pub stage: PairingStage,
    pub until: Instant,
    /// The six digits while the stage is `code`, for a frontend that subscribes then.
    code: Option<String>,
    /// The code was answered; a second answer is too late (the first wins).
    answered: bool,
    /// Dialer: `--name`.
    name: Option<String>,
    /// Dialer: the address as the user wrote it.
    addr: Option<String>,
    peer: Option<PairingPeer>,
    /// An event of this pairing has gone out. A `pair.start` that fails before the first one
    /// leaves nothing for the subscribers to end.
    announced: Cell<bool>,
    /// A cancel was asked for on this machine (by any connection). A dial that then fails
    /// reads as the cancel it was, not as the network's fault.
    cancelled: bool,
}

impl CtlPairing {
    fn new(owner: u64, sink: crate::server::LineSink, role: PairingRole, until: Instant) -> Self {
        Self {
            owner,
            sink,
            role,
            stage: match role {
                PairingRole::Acceptor => PairingStage::Listening,
                PairingRole::Dialer => PairingStage::Connecting,
            },
            until,
            code: None,
            answered: false,
            name: None,
            addr: None,
            peer: None,
            announced: Cell::new(false),
            cancelled: false,
        }
    }

    /// The peer of the attempt, once its code is shown.
    pub fn peer_fingerprint(&self) -> Option<&str> {
        self.peer.as_ref().map(|p| p.fingerprint.as_str())
    }

    fn expires_ms(&self) -> u64 {
        u64::try_from(
            self.until
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    /// The event to every `pairing` subscriber and to the connection that started the pairing:
    /// through its subscription when it has one, on its own sink (M1) when not, never both.
    fn emit(&self, ctl: &Ctl, ev: PairingEvent) {
        self.announced.set(true);
        if ctl.hub.has(self.owner, Topic::Pairing) {
            ctl.hub.publish_pairing(&ev, None);
            return;
        }
        ctl.hub.publish_pairing(&ev, Some(self.owner));
        let mut line = serde_json::to_vec(&EventLine::Pairing(ev)).expect("an event serialises");
        line.push(b'\n');
        // a full queue closes the connection; that cancels this pairing
        self.sink.send(line);
    }

    fn event(&self, stage: PairingStage) -> PairingEvent {
        PairingEvent {
            stage,
            role: Some(self.role),
            peer: self.peer.clone(),
            code: None,
            expires_ms: self.expires_ms(),
            reason: None,
            message: None,
            cancelled_by: None,
        }
    }

    /// An event that says why an attempt (`failed`) or the window (`closed`) ended.
    fn ended(
        &self,
        stage: PairingStage,
        reason: String,
        message: String,
        cancelled_by: Option<CancelledBy>,
    ) -> PairingEvent {
        let mut ev = self.event(stage);
        if stage == PairingStage::Closed {
            ev.peer = None;
        }
        ev.reason = Some(reason);
        ev.message = Some(message);
        ev.cancelled_by = cancelled_by;
        ev
    }
}

fn err(code: ErrorCode, message: impl Into<String>, hint: Option<&str>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        hint: hint.map(str::to_string),
    }
}

fn gone() -> ApiError {
    err(
        ErrorCode::Internal,
        "the net thread is gone",
        Some("see the ferryd log"),
    )
}

async fn ask(
    ctl: &Ctl,
    make: impl FnOnce(oneshot::Sender<PairReply>) -> NetCmd,
) -> Result<PairReply, ApiError> {
    let (tx, rx) = oneshot::channel();
    ctl.net.send(make(tx)).await.map_err(|_| gone())?;
    rx.await.map_err(|_| gone())
}

fn busy() -> ApiError {
    err(
        ErrorCode::PairingBusy,
        "a pairing already runs on this machine",
        Some("finish or cancel it first (Ctrl+C in the terminal that runs ferry pair)"),
    )
}

/// The pairing that runs now, as the event of its current stage, for a frontend that subscribes in
/// the middle of it. At stage `code` it carries the code, like the event did.
pub fn current(ctl: &Ctl) -> Option<PairingEvent> {
    let g = ctl.pairing.borrow();
    let p = g.as_ref()?;
    let mut ev = p.event(p.stage);
    if p.stage == PairingStage::Code {
        ev.code.clone_from(&p.code);
    }
    Some(ev)
}

/// `status.pairing`.
pub fn status(ctl: &Ctl) -> Option<PairingStatus> {
    ctl.pairing.borrow().as_ref().map(|p| PairingStatus {
        role: p.role,
        stage: p.stage,
        expires_ms: p.expires_ms(),
    })
}

/// `pair.accept`: open the window; the events go to `conn`.
pub async fn accept(ctl: &Ctl, conn: &ConnCtx) -> Result<Value, ApiError> {
    if ctl.pairing.borrow().is_some() {
        return Err(busy());
    }
    let until = Instant::now() + WINDOW;
    *ctl.pairing.borrow_mut() = Some(CtlPairing::new(
        conn.id,
        conn.out.clone(),
        PairingRole::Acceptor,
        until,
    ));
    match ask(ctl, |reply| NetCmd::PairAccept { until, reply }).await {
        Ok(Ok(())) => {
            tracing::info!("pair.accept");
            // The window opening is a stage like the others: every `pairing`
            // subscriber hears it, so a frontend that did not open the window knows one is
            // open. The connection that opened it hears it only if it subscribed: M1's stream
            // has no event before the answer to `pair.accept`, and clients rely on it.
            if let Some(p) = ctl.pairing.borrow().as_ref() {
                p.announced.set(true);
                ctl.hub
                    .publish_pairing(&p.event(PairingStage::Listening), None);
            }
            serde_json::to_value(PairAcceptResult {
                expires_ms: u64::try_from(WINDOW.as_millis()).unwrap_or(u64::MAX),
            })
            .map_err(|e| err(ErrorCode::Internal, e.to_string(), None))
        }
        Ok(Err(_)) => {
            *ctl.pairing.borrow_mut() = None;
            Err(busy())
        }
        Err(e) => {
            *ctl.pairing.borrow_mut() = None;
            Err(e)
        }
    }
}

/// `pair.start`: dial; answers once the peer took the pairing connection.
pub async fn start(ctl: &Ctl, conn: &ConnCtx, p: PairStartParams) -> Result<Value, ApiError> {
    if let Some(name) = &p.name {
        crate::config::validate_peer_name(name).map_err(|m| err(ErrorCode::BadParams, m, None))?;
    }
    parse_host_port(&p.addr, ferry_net::DEFAULT_PORT).map_err(|e| {
        err(
            ErrorCode::BadParams,
            format!("addr {:?}: {e}", p.addr),
            Some("write host, host:port or ip:port"),
        )
    })?;
    if ctl.pairing.borrow().is_some() {
        return Err(busy());
    }
    let until = Instant::now() + WINDOW;
    let mut run = CtlPairing::new(conn.id, conn.out.clone(), PairingRole::Dialer, until);
    run.name.clone_from(&p.name);
    run.addr = Some(p.addr.clone());
    *ctl.pairing.borrow_mut() = Some(run);
    let addr = p.addr.clone();
    let reply = ask(ctl, |reply| NetCmd::PairStart {
        addr: addr.clone(),
        until,
        reply,
    })
    .await;
    match reply {
        Ok(Ok(())) => Ok(json!({})),
        // Net refused before anything went out: no pairing began, so there is nothing to end.
        Ok(Err(PairError::Busy)) => {
            *ctl.pairing.borrow_mut() = None;
            Err(busy())
        }
        Ok(Err(e)) => {
            let f = dial_failure(&p.addr, e);
            end_dial(ctl, f.reason, &f.error.message);
            Err(f.error)
        }
        // the net thread is gone: ferryd is stopping
        Err(e) => {
            end_dial(ctl, "other", &e.message);
            Err(e)
        }
    }
}

/// A dial that did not reach a pairing connection: the error that answers `pair.start`, and the
/// `reason` of the `failed` event the subscribers hear (the event's `message` is the error's).
struct DialFailure {
    error: ApiError,
    reason: &'static str,
}

fn dial_failure(addr: &str, e: PairError) -> DialFailure {
    let (error, reason) = match e {
        PairError::NotAccepting => (
            err(
                ErrorCode::PairingNotAccepting,
                format!("{addr} is not accepting pairing"),
                Some("run ferry pair --accept there, or wait: it may be busy with another request"),
            ),
            "not_accepting",
        ),
        PairError::Unreachable(why) => (
            err(
                ErrorCode::PeerUnreachable,
                format!("{addr} did not answer: {why}"),
                Some("check the address, that ferryd runs there, and its firewall (UDP 4243)"),
            ),
            "unreachable",
        ),
        PairError::OwnKey => (
            err(
                ErrorCode::BadParams,
                format!("{addr} is this machine"),
                Some("give the other machine's address"),
            ),
            "own_key",
        ),
        PairError::Version => (
            err(
                ErrorCode::PeerUnreachable,
                format!("{addr} speaks another Ferry protocol version"),
                Some("update Ferry on both machines to the same release"),
            ),
            "version",
        ),
        PairError::Closed(why) | PairError::Other(why) => (
            err(
                ErrorCode::Internal,
                format!("could not connect to {addr}: {why}"),
                Some("see the ferryd log"),
            ),
            "connect_failed",
        ),
        PairError::Busy => (busy(), "other"),
        PairError::NoPairing => (
            err(ErrorCode::Internal, "the net thread lost the pairing", None),
            "other",
        ),
    };
    DialFailure { error, reason }
}

/// A `pair.start` that failed: the pairing is over. The subscribers were told it began
/// (`connecting`), so they hear it end with a `failed` that says why, or `cancelled` when a cancel
/// had been asked for meanwhile. The connection that sent `pair.start` has the error that
/// answers it; it hears the event only if it subscribed, so M1's stream (the `connecting` and the
/// error) stays as it was. Nothing goes out when no event of this pairing did.
fn end_dial(ctl: &Ctl, reason: &str, message: &str) {
    let Some(p) = ctl.pairing.borrow_mut().take() else {
        return;
    };
    if !p.announced.get() {
        return;
    }
    let ev = if p.cancelled {
        p.ended(
            PairingStage::Failed,
            PairReason::Cancelled.as_str().into(),
            PairFailure::Reason(PairReason::Cancelled).message(),
            Some(CancelledBy::Local),
        )
    } else {
        p.ended(PairingStage::Failed, reason.into(), message.into(), None)
    };
    ctl.hub.publish_pairing(&ev, None);
}

/// `pair.answer`, from any connection while a code is shown (m3 design M3-D8). The first answer
/// wins: the flag is set before ctl awaits net, so a second answer that arrives meanwhile is
/// refused.
pub async fn answer(ctl: &Ctl, _conn: &ConnCtx, ok: bool) -> Result<Value, ApiError> {
    {
        let mut g = ctl.pairing.borrow_mut();
        let Some(p) = g.as_mut() else {
            return Err(err(
                ErrorCode::NoPairing,
                "no pairing runs on this machine",
                Some("start one with ferry pair, or open the Pair panel"),
            ));
        };
        if Instant::now() >= p.until {
            return Err(err(
                ErrorCode::PairingExpired,
                "the pairing has timed out",
                None,
            ));
        }
        if p.stage != PairingStage::Code {
            return Err(err(ErrorCode::NoPairing, "no code to answer yet", None));
        }
        if p.answered {
            return Err(err(
                ErrorCode::NoPairing,
                "the code was already answered",
                Some("the first answer counts; the pairing events say how it went"),
            ));
        }
        p.answered = true;
    }
    match ask(ctl, |reply| NetCmd::PairAnswer { ok, reply }).await? {
        Ok(()) => Ok(json!({})),
        Err(_) => Err(err(ErrorCode::NoPairing, "the attempt has ended", None)),
    }
}

/// `pair.cancel`, from any connection, like `pair.answer` (pair-polish; the panel, a terminal
/// and a notification are equals). The connection that started the pairing and every `pairing`
/// subscriber hear it end, with `cancelled_by: local`.
pub async fn cancel(ctl: &Ctl, conn: &ConnCtx) -> Result<Value, ApiError> {
    {
        let mut g = ctl.pairing.borrow_mut();
        let Some(p) = g.as_mut() else {
            return Err(err(
                ErrorCode::NoPairing,
                "no pairing runs on this machine",
                Some("start one with ferry pair, or open the Pair panel"),
            ));
        };
        p.cancelled = true;
        tracing::info!(by_opener = p.owner == conn.id, "pair.cancel");
    }
    let _ = ask(ctl, |reply| NetCmd::PairCancel { reply }).await?;
    Ok(json!({}))
}

/// The socket connection `id` closed: a pairing it started is cancelled.
pub async fn socket_closed(ctl: &Ctl, id: u64) {
    let owned = ctl.pairing.borrow().as_ref().is_some_and(|p| p.owner == id);
    if owned {
        tracing::info!("the socket connection that started the pairing closed: cancelling");
        let _ = ask(ctl, |reply| NetCmd::PairCancel { reply }).await;
    }
}

/// A note from net.
pub async fn on_note(ctl: &Ctl, note: PairNote) {
    match note {
        PairNote::Connecting => update(ctl, PairingStage::Connecting, |_| {}),
        PairNote::Code {
            peer,
            name,
            remote,
            code,
        } => {
            let mut g = ctl.pairing.borrow_mut();
            if let Some(p) = g.as_mut() {
                p.stage = PairingStage::Code;
                let shown_name = name.clone();
                p.answered = false;
                p.peer = Some(PairingPeer {
                    name,
                    fingerprint: peer.to_string(),
                    addr: Some(remote.to_string()),
                });
                let digits = code.digits();
                p.code = Some(digits.clone());
                let mut ev = p.event(PairingStage::Code);
                ev.code = Some(digits);
                p.emit(ctl, ev);
                // The code is also a critical toast, while the window is open.
                ctl.notify
                    .pair_code(p.role, &shown_name, &code, p.expires_ms());
            }
        }
        PairNote::WaitingPeer => {
            update(ctl, PairingStage::WaitingPeer, |_| {});
            ctl.notify.pairing_moved();
        }
        PairNote::Listening { until } => {
            update(ctl, PairingStage::Listening, |p| {
                p.until = until;
                p.peer = None;
            });
            ctl.notify.pairing_moved();
        }
        PairNote::Failed {
            reason,
            message,
            cancelled_by,
        } => {
            let mut g = ctl.pairing.borrow_mut();
            if let Some(p) = g.as_mut() {
                p.stage = PairingStage::Failed;
                ctl.notify.pair_failed(&reason, &message);
                p.code = None;
                let ev = p.ended(PairingStage::Failed, reason, message, cancelled_by);
                p.emit(ctl, ev);
                // a dialer has no window to keep: this was its last event
                if p.role == PairingRole::Dialer {
                    *g = None;
                }
            }
        }
        PairNote::Closed {
            reason,
            message,
            cancelled_by,
        } => {
            let p = ctl.pairing.borrow_mut().take();
            if let Some(p) = p {
                let ev = p.ended(PairingStage::Closed, reason, message, cancelled_by);
                p.emit(ctl, ev);
            }
            ctl.notify.pairing_moved();
        }
        PairNote::Pinned {
            role,
            peer,
            name,
            name_refused,
            remote,
        } => pinned(ctl, role, peer, name, name_refused, remote).await,
    }
}

fn update(ctl: &Ctl, stage: PairingStage, f: impl FnOnce(&mut CtlPairing)) {
    if let Some(p) = ctl.pairing.borrow_mut().as_mut() {
        p.stage = stage;
        p.code = None;
        f(p);
        p.emit(ctl, p.event(stage));
    }
}

/// §2.6 holds on this side: write the pin, make it live, dial the peer, then tell the client.
async fn pinned(
    ctl: &Ctl,
    role: Role,
    peer: Fingerprint,
    hello_name: String,
    name_refused: Option<String>,
    remote: SocketAddr,
) {
    let p = ctl.pairing.borrow_mut().take();
    // The dialer's `--name` replaces the peer's name, refused or not.
    let name_refused =
        name_refused.filter(|_| !matches!((role, &p), (Role::Dialer, Some(p)) if p.name.is_some()));
    let (wanted, addr) = match (role, &p) {
        // the dialer pins under --name, at the address the user gave
        (Role::Dialer, Some(p)) => (
            p.name.clone().unwrap_or_else(|| hello_name.clone()),
            p.addr.clone().unwrap_or_else(|| remote.to_string()),
        ),
        // the acceptor pins A's address from the pairing connection: A dials from its bound
        // port, so it is dialable
        _ => (hello_name.clone(), remote.to_string()),
    };
    let now = now_rfc3339();
    let saved = {
        let s = ctl.state.borrow();
        let mut trust = s.trust.clone();
        let stored = trust.pin_paired(peer, &wanted, Some(&addr), &now);
        trust
            .save(&ctl.paths.trusted_file())
            .map(|()| (stored, trust))
    };
    let (stored, trust) = match saved {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, peer = %peer, "pairing: cannot save the pin");
            let message = format!("could not save the pin: {e}");
            ctl.notify.pair_failed("save_failed", &message);
            if let Some(p) = &p {
                p.emit(
                    ctl,
                    p.ended(PairingStage::Failed, "save_failed".into(), message, None),
                );
                // Net ended its side when it sent `Pinned`, so an acceptor's window is closed
                // too: the attempt failed and the window with it.
                if p.role == PairingRole::Acceptor {
                    p.emit(
                        ctl,
                        p.ended(
                            PairingStage::Closed,
                            "save_failed".into(),
                            "pairing window closed: could not save the pin".into(),
                            None,
                        ),
                    );
                }
            }
            return;
        }
    };
    {
        let mut s = ctl.state.borrow_mut();
        s.trust = trust;
        s.refusals.retain(|r| r.fingerprint != peer);
        // The initiating peer places the new one at its first connection
        if role == Role::Dialer {
            s.place_on_up.insert(peer);
        }
        s.links.insert(
            peer,
            crate::state::LinkInfo {
                state: ferry_core::api::LinkState::Connecting,
                remote: None,
                rtt_us: None,
                proto: None,
                detail: None,
            },
        );
    }
    tracing::info!(peer = %peer, name = %stored, ?role, "pairing: pinned (sas)");
    // A pairing starts a relationship: its first connect is not a "back".
    ctl.notify.forget_peer(peer);
    let all = ctl.state.borrow().trust.fingerprints();
    let (ack, acked) = oneshot::channel();
    if ctl
        .net
        .send(NetCmd::SetTrust { fps: all, ack })
        .await
        .is_ok()
    {
        let _ = acked.await;
    }
    crate::ctl::push_topology(ctl);
    let _ = ctl
        .net
        .send(NetCmd::Dial {
            peer,
            addr: addr.clone(),
        })
        .await;
    // Say why it was renamed; a name that is not valid is shown escaped.
    let note = match (&name_refused, crate::config::validate_peer_name(&wanted)) {
        (Some(why), _) => Some(format!(
            "pinned as {stored}: the name it sent is not allowed; {why}"
        )),
        (None, Err(why)) => Some(format!(
            "pinned as {stored}: the name {wanted:?} is not allowed; {why}"
        )),
        (None, Ok(())) if stored != wanted => Some(format!(
            "pinned as {stored}: the name {wanted} belongs to another peer"
        )),
        (None, Ok(())) => None,
    };
    // J2 step 4: a toast on both machines, whoever started the pairing.
    ctl.notify.paired(&stored, note.as_deref());
    if let Some(p) = p {
        let mut ev = p.event(PairingStage::Paired);
        ev.peer = Some(PairingPeer {
            name: stored.clone(),
            fingerprint: peer.to_string(),
            addr: Some(addr),
        });
        ev.message = note;
        p.emit(ctl, ev);
    }
}

// --- unpair ---------------------------------------------------------------------------------

/// `unpair {peer}`: swap the trust set without the pin, which closes the peer's
/// connection with code 4 and so releases any crossing at once, and only then write
/// `trusted.toml`. The release must not wait for the disk: the write is two `fsync`s, and one that
/// took 126 ms under load put D19's release at 181 ms against its 100 ms bound (exit run 1). If
/// the write fails, the old set goes back: the pin stays, the file says so, and the caller is
/// told. The peer was disconnected for that moment and dials again.
pub async fn unpair(ctl: &Ctl, p: UnpairParams) -> Result<Value, ApiError> {
    let (fp, name, addr, next, before) = {
        let s = ctl.state.borrow();
        let id = s.resolve_peer(&p.peer)?;
        if id == s.me.id {
            return Err(err(
                ErrorCode::BadParams,
                "that is this machine",
                Some("ferry peers lists the peers"),
            ));
        }
        let fp = Fingerprint(id.0);
        let mut trust = s.trust.clone();
        let pin = trust.remove(&fp).ok_or_else(|| {
            err(
                ErrorCode::UnknownPeer,
                format!("{} is not pinned", p.peer),
                Some("ferry peers lists the peers"),
            )
        })?;
        (fp, pin.name, pin.addr, trust, s.trust.fingerprints())
    };
    set_trust(ctl, next.fingerprints()).await?;
    if let Err(e) = next.save(&ctl.paths.trusted_file()) {
        tracing::error!(peer = %fp, error = %e, "unpair: trusted.toml not written; the pin stays");
        let _ = set_trust(ctl, before).await;
        if let Some(addr) = addr {
            // the net thread forgot where to dial when the pin went
            let _ = ctl.net.send(NetCmd::Dial { peer: fp, addr }).await;
        }
        return Err(err(
            ErrorCode::Internal,
            e.to_string(),
            Some("see the ferryd log; the peer stays paired"),
        ));
    }
    {
        let mut s = ctl.state.borrow_mut();
        s.trust = next;
        s.links.remove(&fp);
    }
    tracing::info!(peer = %fp, %name, "unpair");
    // FR-LAYOUT-13; the peer's connection is closed, so the others get the revision
    if let Some(wire) = crate::api::remove_unpaired(ctl, fp) {
        crate::api::publish(ctl, wire).await;
    }
    crate::ctl::push_topology(ctl);
    // The user knows: no toast, and what the notifier knew of the link is void.
    ctl.notify.forget_peer(fp);
    Ok(json!({ "peer": { "name": name, "fingerprint": fp.to_string() } }))
}

/// FR-PAIR-08, the receiving side: the peer closed its connection with `UNPAIRED`, so it removed
/// this machine's pin, and this machine removes its own. The toast is already shown (`ctl` does
/// that first). The connection is gone, so unlike [`unpair`] nothing here is in a hurry: the
/// file is written first, and nothing awaits between reading the pins and writing them, so a
/// `trust` or a pairing that runs meanwhile is never overwritten. A pin that cannot be written
/// stays, and is logged.
pub async fn revoked(ctl: &Ctl, peer: Fingerprint, name: &str) {
    let saved = {
        let mut s = ctl.state.borrow_mut();
        let mut trust = s.trust.clone();
        if trust.remove(&peer).is_none() {
            return; // unpaired here at the same time
        }
        let saved = trust.save(&ctl.paths.trusted_file());
        if saved.is_ok() {
            s.trust = trust;
            s.links.remove(&peer);
        }
        saved
    };
    if let Err(e) = saved {
        tracing::warn!(peer = %peer, %name, error = %e, "the peer unpaired this machine, but trusted.toml is not written: the pin stays");
        return;
    }
    tracing::info!(peer = %peer, %name, "the peer unpaired this machine: its pin is removed");
    let all = ctl.state.borrow().trust.fingerprints();
    if set_trust(ctl, all).await.is_err() {
        tracing::warn!(peer = %peer, "the net thread did not confirm the new trust set");
    }
    // FR-LAYOUT-13, on this side too. This runs in a task of its own, after ctl's look at the
    // message that started it, so the subscribers are told here.
    if let Some(wire) = crate::api::remove_unpaired(ctl, peer) {
        crate::api::publish(ctl, wire).await;
    }
    crate::ctl::push_topology(ctl);
    ctl.hub.poke();
}

/// Swaps the live trust set and waits until the net thread has (it closes a connection whose pin
/// is gone as `unpaired`).
async fn set_trust(ctl: &Ctl, fps: Vec<Fingerprint>) -> Result<(), ApiError> {
    let (ack, acked) = oneshot::channel();
    ctl.net
        .send(NetCmd::SetTrust { fps, ack })
        .await
        .map_err(|_| gone())?;
    acked.await.map_err(|_| gone())
}

// --- address upkeep -----------------------------------------------------------------------

/// A connection to a pinned peer came up or ended: keep `last_seen` and the
/// address that worked. An inbound connection's remote is the peer's bound port, so a pin that
/// had no address (or an old one) learns where to dial, and this side starts dialling it.
pub async fn seen(ctl: &Ctl, peer: Fingerprint, remote: SocketAddr, dialed: bool, up: bool) {
    let now = now_rfc3339();
    let learned = {
        let mut s = ctl.state.borrow_mut();
        if s.trust.get(&peer).is_none() {
            return;
        }
        let mut trust = s.trust.clone();
        let changed = trust.seen(&peer, (up && !dialed).then_some(remote), &now);
        match trust.save(&ctl.paths.trusted_file()) {
            Ok(()) => s.trust = trust,
            Err(e) => {
                tracing::warn!(error = %e, "cannot save last_seen");
                return;
            }
        }
        changed
            .then(|| s.trust.get(&peer).and_then(|p| p.addr.clone()))
            .flatten()
    };
    if let Some(addr) = learned {
        tracing::info!(peer = %peer, %addr, "a peer's address learned from its connection");
        // ctl's main loop runs this: it never awaits net.
        crate::ctl::to_net(ctl, NetCmd::Dial { peer, addr });
    }
}

#[cfg(test)]
mod dial_tests {
    use super::*;
    use ferry_net::CloseReason;

    /// Every way a dial can fail ends with one reason the `failed` event carries (pair-polish).
    #[test]
    fn every_failed_dial_has_a_reason() {
        let table: Vec<(DialError, &str, ErrorCode)> = vec![
            (
                DialError::Resolve("cannot resolve x: no".into()),
                "unreachable",
                ErrorCode::PeerUnreachable,
            ),
            (
                DialError::Connect(ConnectError::TimedOut),
                "unreachable",
                ErrorCode::PeerUnreachable,
            ),
            (
                DialError::Connect(ConnectError::NotAccepting),
                "not_accepting",
                ErrorCode::PairingNotAccepting,
            ),
            (
                DialError::Connect(ConnectError::OwnKey),
                "own_key",
                ErrorCode::BadParams,
            ),
            (
                DialError::Connect(ConnectError::VersionMismatch),
                "version",
                ErrorCode::PeerUnreachable,
            ),
            (
                DialError::Connect(ConnectError::Abandoned),
                "connect_failed",
                ErrorCode::Internal,
            ),
            (
                DialError::Connect(ConnectError::Closed(CloseReason::Reset)),
                "connect_failed",
                ErrorCode::Internal,
            ),
            (
                DialError::Connect(ConnectError::Other("tls".into())),
                "connect_failed",
                ErrorCode::Internal,
            ),
        ];
        for (dial, reason, code) in table {
            let what = format!("{dial:?}");
            let f = dial_failure("192.0.2.11", dial.into());
            assert_eq!((f.reason, f.error.code), (reason, code), "{what}");
            assert!(!f.error.message.is_empty(), "{what}");
        }
    }
}
