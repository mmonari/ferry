//! SAS pairing (design `m1-rest-design.md` §2, ADR 0006 as amended): the
//! commitment, the code, the `ferry-pair/1` frame decoder, and the pure
//! [`PairMachine`] that runs one attempt.
//!
//! A is the dialer (`ferry pair <host>`), B the acceptor (`ferry pair
//! --accept`). TLS 1.3 fixes both keys first; then A commits to its nonce, B
//! answers with its own, and A reveals. Both sides show
//! `SHA-256("ferry-sas" ‖ fp_lo ‖ fp_hi ‖ n_A ‖ n_B)[0..8] mod 10^6` and ask
//! their user. The machine never pins: it says [`PairAction::Agreed`] when the
//! local user said yes and the peer's `Confirm{ok: true}` arrived (§2.6
//! conditions 1 and 2); ferryd then waits for its own `Confirm` to be
//! acknowledged (condition 3) before it writes the pin.
//!
//! Nothing secret is printed: [`PairCode`], [`PairMachine`] and the `Pair`
//! frames have a `Debug` that hides the code, the nonces and the commitment.
//! The code leaves the machine only in
//! [`PairAction::ShowCode`].

use sha2::{Digest, Sha256};

use crate::actions::Actions;
use crate::peer::PeerId;
use crate::time::Micros;
use crate::wire::control::Pair;
use crate::wire::{Control, DecodeError, Decoded, ErrorCode, PROTO_MAJOR, PROTO_MINOR};

/// Bytes in a nonce: 32 from the OS RNG, drawn by ferryd (the core has no IO).
pub const NONCE_LEN: usize = 32;
pub type Nonce = [u8; NONCE_LEN];

/// Domain tag of the commitment (§2.4 step 3).
pub const COMMIT_TAG: &[u8] = b"ferry-commit";
/// Domain tag of the code (ADR 0006's formula).
pub const CODE_TAG: &[u8] = b"ferry-sas";
/// Six digits.
pub const CODE_MODULUS: u32 = 1_000_000;
/// Machine steps 2–5 (`Hello`, `Commit`, `Nonce`, `Reveal`) each have this
/// long: no person is involved there.
pub const STEP_TIMEOUT: Micros = Micros::from_millis(5000);

/// QUIC application close codes the machine asks for (ferry-net's
/// `CloseCode`, core §2.2).
pub mod close {
    pub const SHUTDOWN: u32 = 0;
    pub const VERSION: u32 = 1;
    pub const PROTOCOL: u32 = 2;
}

/// `SHA-256("ferry-commit" ‖ fp_A ‖ fp_B ‖ n_A)`: the dialer's nonce, bound to
/// both keys in role order, so a commitment cannot be replayed into another
/// session (**Q3**).
pub fn commitment(fp_a: &PeerId, fp_b: &PeerId, n_a: &Nonce) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(COMMIT_TAG);
    h.update(fp_a.0);
    h.update(fp_b.0);
    h.update(n_a);
    h.finalize().into()
}

/// The pairing code of §2.5. `fp_a` and `fp_b` are the dialer's and the
/// acceptor's fingerprints; the formula sorts them, the nonces stay in role
/// order. Both sides compute the same value.
pub fn code(fp_a: &PeerId, fp_b: &PeerId, n_a: &Nonce, n_b: &Nonce) -> PairCode {
    PairCode(code_mod(fp_a, fp_b, n_a, n_b, CODE_MODULUS))
}

/// [`code`] with another modulus (the toy-modulus test uses 100).
pub(crate) fn code_mod(
    fp_a: &PeerId,
    fp_b: &PeerId,
    n_a: &Nonce,
    n_b: &Nonce,
    modulus: u32,
) -> u32 {
    let (lo, hi) = if fp_a.0 <= fp_b.0 {
        (fp_a, fp_b)
    } else {
        (fp_b, fp_a)
    };
    let mut h = Sha256::new();
    h.update(CODE_TAG);
    h.update(lo.0);
    h.update(hi.0);
    h.update(n_a);
    h.update(n_b);
    let digest: [u8; 32] = h.finalize().into();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    // the bias of a 64-bit value mod 10^6 is below 10^-13
    (u64::from_be_bytes(first) % u64::from(modulus)) as u32
}

/// A pairing code: shown as two groups of three digits, zero-padded
/// (`418 207`). `Debug` hides the value, so a `{:?}` in a log never leaks it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PairCode(pub u32);

impl PairCode {
    /// Six digits, no space (`418207`): the socket API's form.
    pub fn digits(&self) -> String {
        format!("{:06}", self.0)
    }
}

impl std::fmt::Display for PairCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:03} {:03}", self.0 / 1000, self.0 % 1000)
    }
}

impl std::fmt::Debug for PairCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairCode(..)")
    }
}

/// Why an attempt ended, as one `u8` in `Confirm{ok: false}`. An
/// unknown code is [`PairReason::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PairReason {
    Other = 0,
    /// The user said the codes differ.
    Rejected = 1,
    /// Ctrl+C, `pair.cancel`, or the socket connection closed.
    Cancelled = 2,
    /// A machine step took over 5 s, or the window or attempt ended.
    Timeout = 3,
    /// `Reveal` does not match `Commit`: a possible attack.
    BadReveal = 4,
    /// A frame out of order, or one that is not valid on `ferry-pair/1`.
    Protocol = 5,
}

impl PairReason {
    pub const ALL: [PairReason; 6] = [
        PairReason::Other,
        PairReason::Rejected,
        PairReason::Cancelled,
        PairReason::Timeout,
        PairReason::BadReveal,
        PairReason::Protocol,
    ];

    pub fn from_code(code: u8) -> Self {
        Self::ALL
            .get(usize::from(code))
            .copied()
            .unwrap_or(PairReason::Other)
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    /// The `snake_case` name used in the socket API.
    pub fn as_str(self) -> &'static str {
        match self {
            PairReason::Other => "other",
            PairReason::Rejected => "rejected",
            PairReason::Cancelled => "cancelled",
            PairReason::Timeout => "timeout",
            PairReason::BadReveal => "bad_reveal",
            PairReason::Protocol => "protocol",
        }
    }
}

/// Why this side's attempt failed: a wire reason (sent or received), a
/// version mismatch, or a link that dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairFailure {
    Reason(PairReason),
    /// The majors differ (`Hello` fields 0–1).
    Version {
        ours: String,
        theirs: String,
    },
    /// The connection ended before the attempt did.
    Lost,
}

impl PairFailure {
    /// The socket API's `reason`.
    pub fn as_str(&self) -> &'static str {
        match self {
            PairFailure::Reason(r) => r.as_str(),
            PairFailure::Version { .. } => "version",
            PairFailure::Lost => "lost",
        }
    }

    /// What the user reads.
    pub fn message(&self) -> String {
        match self {
            PairFailure::Reason(PairReason::Rejected) => "cancelled: codes differ".into(),
            PairFailure::Reason(PairReason::Cancelled) => "cancelled".into(),
            PairFailure::Reason(PairReason::Timeout) => "timed out".into(),
            PairFailure::Reason(PairReason::BadReveal) => {
                "cancelled: the peer broke the protocol (its reveal does not match its commitment)"
                    .into()
            }
            PairFailure::Reason(PairReason::Protocol) => {
                "cancelled: the peer broke the protocol".into()
            }
            PairFailure::Reason(PairReason::Other) => "cancelled".into(),
            PairFailure::Version { ours, theirs } => {
                format!("version mismatch: {ours} vs {theirs}")
            }
            PairFailure::Lost => "connection lost".into(),
        }
    }
}

/// One frame on `ferry-pair/1`: `Hello` or `Error` (as on `ferry/1`), or a
/// `Pair` message.
#[derive(Debug, Clone, PartialEq)]
pub enum PairFrame {
    Control(Control),
    Pair(Pair),
}

/// A frame that is not valid on `ferry-pair/1`: close with code 2.
#[derive(Debug, thiserror::Error)]
pub enum PairDecodeError {
    #[error("variant {0} is not valid on ferry-pair/1")]
    Variant(u32),
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

impl From<minicbor::decode::Error> for PairDecodeError {
    fn from(e: minicbor::decode::Error) -> Self {
        PairDecodeError::Decode(DecodeError::Cbor(e))
    }
}

impl PairFrame {
    /// Reads the variant first: 0 and 1 decode as [`Control`] (with its caps),
    /// 30–33 as [`Pair`]; anything else is an error, which closes the
    /// connection with code 2.
    pub fn decode(body: &[u8]) -> Result<PairFrame, PairDecodeError> {
        let variant = {
            let mut d = minicbor::Decoder::new(body);
            d.array()?;
            d.u32()?
        };
        match variant {
            0 | 1 => match Control::decode(body)? {
                Decoded::Msg(m @ (Control::Hello { .. } | Control::Error { .. })) => {
                    Ok(PairFrame::Control(m))
                }
                _ => Err(PairDecodeError::Variant(variant)),
            },
            30..=33 => Ok(PairFrame::Pair(minicbor::decode::<Pair>(body)?)),
            v => Err(PairDecodeError::Variant(v)),
        }
    }

    /// Encode into `buf` (cleared first).
    pub fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            PairFrame::Control(c) => c.encode(buf),
            PairFrame::Pair(p) => {
                buf.clear();
                if minicbor::encode(p, &mut *buf).is_err() {
                    debug_assert!(false, "encoding {p:?} failed");
                    buf.clear();
                }
            }
        }
    }

    /// The wire variant number, for logs and counters.
    pub fn variant(&self) -> u32 {
        match self {
            PairFrame::Control(c) => c.variant(),
            PairFrame::Pair(p) => p.variant(),
        }
    }
}

/// Which side of the attempt this machine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A: dialed `ferry-pair/1`, sends `Hello` first, commits and reveals.
    Dialer,
    /// B: opened the pairing window and accepted the connection.
    Acceptor,
}

/// `PairEvent::Start`: everything one attempt needs.
#[derive(Clone)]
pub struct Start {
    pub role: Role,
    /// This peer's fingerprint.
    pub me: PeerId,
    /// The fingerprint the peer proved in TLS.
    pub peer: PeerId,
    /// 32 bytes from the OS RNG. The acceptor's is sent only after `Commit`
    /// has arrived, so it is independent of it and unknown to the dialer
    /// until then.
    pub nonce: Nonce,
    /// The end of the window (acceptor) or of the attempt (dialer): the human
    /// wait ends here.
    pub until: Micros,
    /// This side's `Hello` (monitors and features empty).
    pub hello: Control,
}

impl std::fmt::Debug for Start {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Start")
            .field("role", &self.role)
            .field("me", &self.me)
            .field("peer", &self.peer)
            .field("until", &self.until)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum PairEvent {
    Start(Start),
    /// A decoded frame from the peer.
    Frame(PairFrame),
    /// The local user's answer to "Same code?".
    Answer(bool),
    /// Ctrl+C, `pair.cancel`, or the socket connection that started it closed.
    Cancel,
    /// The connection ended.
    Closed,
    /// Time passed: check `next_deadline`.
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PairAction {
    /// Encode and send on the pairing connection.
    Send(PairFrame),
    /// Show the code with the peer's name, short fingerprint and address,
    /// then ask the local user.
    ShowCode { code: PairCode, peer_name: String },
    /// §2.6 conditions 1 and 2 hold: the local user said yes and the peer's
    /// `Confirm{ok: true}` arrived. ferryd runs condition 3 (its own
    /// `Confirm` acknowledged) before it pins. A `Confirm{ok: false}` that
    /// still arrives turns it into [`PairAction::Failed`].
    Agreed { peer: PeerId, name: String },
    /// Nothing is pinned on this side.
    Failed(PairFailure),
    /// Close the connection with this code once the queued frames are out.
    Close { code: u32 },
}

/// A coarse stage for `status` and the socket events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStage {
    Idle,
    /// Machine steps 2–5.
    Exchanging,
    /// The code is shown; the local user has not answered.
    Comparing,
    /// The local user said yes; waiting for the peer's `Confirm`.
    WaitingPeer,
    Agreed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Idle,
    /// Both: waiting for the peer's `Hello`.
    Hello,
    /// B: waiting for `Commit`.
    Commit,
    /// A: waiting for `Nonce`.
    Nonce,
    /// B: waiting for `Reveal`.
    Reveal,
    /// Both: the code is shown; waiting for the two answers.
    Human,
    Agreed,
    Failed,
}

/// One pairing attempt, pure (`handle(ev, now, out)`, `next_deadline()`).
pub struct PairMachine {
    st: St,
    role: Role,
    me: PeerId,
    peer: PeerId,
    nonce: Nonce,
    until: Micros,
    deadline: Option<Micros>,
    hello: Option<Control>,
    peer_name: String,
    /// Why the peer's `Hello.name` was not kept, if it was not.
    peer_name_refused: Option<String>,
    peer_version: Option<(u16, u16)>,
    /// B: A's commitment.
    commit: Option<[u8; 32]>,
    code: Option<PairCode>,
    local_yes: bool,
    remote_yes: bool,
    modulus: u32,
}

impl std::fmt::Debug for PairMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairMachine")
            .field("stage", &self.st)
            .field("role", &self.role)
            .field("peer", &self.peer)
            .field("deadline", &self.deadline)
            .field("local_yes", &self.local_yes)
            .field("remote_yes", &self.remote_yes)
            .finish_non_exhaustive()
    }
}

impl Default for PairMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl PairMachine {
    pub fn new() -> Self {
        Self {
            st: St::Idle,
            role: Role::Dialer,
            me: PeerId([0; 32]),
            peer: PeerId([0; 32]),
            nonce: [0; NONCE_LEN],
            until: Micros::ZERO,
            deadline: None,
            hello: None,
            peer_name: String::new(),
            peer_name_refused: None,
            peer_version: None,
            commit: None,
            code: None,
            local_yes: false,
            remote_yes: false,
            modulus: CODE_MODULUS,
        }
    }

    /// A machine whose code is reduced mod `modulus` (the toy-modulus test).
    #[cfg(test)]
    pub(crate) fn with_modulus(modulus: u32) -> Self {
        Self {
            modulus,
            ..Self::new()
        }
    }

    pub fn stage(&self) -> PairStage {
        match self.st {
            St::Idle => PairStage::Idle,
            St::Hello | St::Commit | St::Nonce | St::Reveal => PairStage::Exchanging,
            St::Human if self.local_yes => PairStage::WaitingPeer,
            St::Human => PairStage::Comparing,
            St::Agreed => PairStage::Agreed,
            St::Failed => PairStage::Failed,
        }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// The peer's `Hello.name` once it arrived, if it is a valid peer name;
    /// else the peer's [`PeerId::default_name`].
    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    /// Why the peer's `Hello.name` was replaced ([`crate::peer::validate_peer_name`]'s
    /// message, which quotes the offending character escaped); `None` when it was kept.
    pub fn peer_name_refused(&self) -> Option<&str> {
        self.peer_name_refused.as_deref()
    }

    /// When `Tick` is due.
    pub fn next_deadline(&self) -> Option<Micros> {
        self.deadline
    }

    pub fn handle(&mut self, ev: PairEvent, now: Micros, out: &mut Actions<PairAction>) {
        // A due deadline fires before the event, as in `SourceMachine` and `TargetMachine`: an
        // answer or a frame that reaches the machine before its `Tick` must not agree after the
        // window closed (A-07). `Start` begins an attempt and `Tick` is the check itself.
        if !matches!(ev, PairEvent::Start(_) | PairEvent::Tick)
            && self.active()
            && self.deadline.is_some_and(|d| now >= d)
        {
            self.fail(PairReason::Timeout, true, out);
            return;
        }
        match ev {
            PairEvent::Start(s) => self.start(s, now, out),
            PairEvent::Frame(f) => self.frame(f, now, out),
            PairEvent::Answer(ok) => self.answer(ok, out),
            PairEvent::Cancel => {
                // After Agreed the answer stands: this side's yes is already out.
                if self.active() {
                    self.fail(PairReason::Cancelled, true, out);
                }
            }
            PairEvent::Closed => {
                if self.active() {
                    self.end(PairFailure::Lost, out);
                }
            }
            PairEvent::Tick => {
                if self.active() && self.deadline.is_some_and(|d| now >= d) {
                    self.fail(PairReason::Timeout, true, out);
                }
            }
        }
    }

    /// Between `Start` and an outcome.
    fn active(&self) -> bool {
        !matches!(self.st, St::Idle | St::Agreed | St::Failed)
    }

    fn step_deadline(&self, now: Micros) -> Option<Micros> {
        Some(now.plus(STEP_TIMEOUT).min(self.until))
    }

    fn start(&mut self, s: Start, now: Micros, out: &mut Actions<PairAction>) {
        if self.st != St::Idle {
            return;
        }
        self.role = s.role;
        self.me = s.me;
        self.peer = s.peer;
        self.nonce = s.nonce;
        self.until = s.until;
        self.hello = Some(s.hello);
        self.st = St::Hello;
        self.deadline = self.step_deadline(now);
        if self.role == Role::Dialer {
            self.send_hello(out);
        }
    }

    fn send_hello(&mut self, out: &mut Actions<PairAction>) {
        if let Some(h) = self.hello.take() {
            out.push(PairAction::Send(PairFrame::Control(h)));
        }
    }

    fn send(&self, p: Pair, out: &mut Actions<PairAction>) {
        out.push(PairAction::Send(PairFrame::Pair(p)));
    }

    /// Fail with a wire reason: tell the peer (when `tell`), then close.
    fn fail(&mut self, reason: PairReason, tell: bool, out: &mut Actions<PairAction>) {
        if tell {
            self.send(
                Pair::Confirm {
                    ok: false,
                    reason: Some(reason.code()),
                },
                out,
            );
        }
        let code = match reason {
            PairReason::Protocol | PairReason::BadReveal => close::PROTOCOL,
            _ => close::SHUTDOWN,
        };
        self.end(PairFailure::Reason(reason), out);
        out.push(PairAction::Close { code });
    }

    fn end(&mut self, why: PairFailure, out: &mut Actions<PairAction>) {
        self.st = St::Failed;
        self.deadline = None;
        out.push(PairAction::Failed(why));
    }

    fn answer(&mut self, ok: bool, out: &mut Actions<PairAction>) {
        if self.st != St::Human || self.local_yes {
            return;
        }
        if !ok {
            self.fail(PairReason::Rejected, true, out);
            return;
        }
        self.local_yes = true;
        self.send(
            Pair::Confirm {
                ok: true,
                reason: None,
            },
            out,
        );
        self.maybe_agree(out);
    }

    fn maybe_agree(&mut self, out: &mut Actions<PairAction>) {
        if self.st == St::Human && self.local_yes && self.remote_yes {
            self.st = St::Agreed;
            self.deadline = None;
            out.push(PairAction::Agreed {
                peer: self.peer,
                name: self.peer_name.clone(),
            });
        }
    }

    fn show_code(&mut self, n_a: &Nonce, n_b: &Nonce, now: Micros, out: &mut Actions<PairAction>) {
        let (fp_a, fp_b) = match self.role {
            Role::Dialer => (self.me, self.peer),
            Role::Acceptor => (self.peer, self.me),
        };
        let code = PairCode(code_mod(&fp_a, &fp_b, n_a, n_b, self.modulus));
        self.code = Some(code);
        self.st = St::Human;
        // the person may take until the window or attempt ends
        self.deadline = Some(self.until.max(now));
        out.push(PairAction::ShowCode {
            code,
            peer_name: self.peer_name.clone(),
        });
    }

    fn frame(&mut self, f: PairFrame, now: Micros, out: &mut Actions<PairAction>) {
        // A `Confirm{ok: false}` ends the attempt at any stage, even after Agreed: a
        // `false` never pins.
        if let PairFrame::Pair(Pair::Confirm { ok: false, reason }) = &f {
            if self.active() || self.st == St::Agreed {
                // an older peer without field 1 meant: its user said no
                let r = reason.map_or(PairReason::Rejected, PairReason::from_code);
                self.end(PairFailure::Reason(r), out);
                out.push(PairAction::Close {
                    code: close::SHUTDOWN,
                });
            }
            return;
        }
        if !self.active() {
            if self.st == St::Agreed {
                // anything but a `false` after the agreement breaks the protocol
                self.fail(PairReason::Protocol, true, out);
            }
            return;
        }
        match (self.st, self.role, f) {
            (
                St::Hello,
                _,
                PairFrame::Control(Control::Hello {
                    proto_major,
                    proto_minor,
                    name,
                    ..
                }),
            ) => {
                // The acceptor answers with its own Hello first, even to a peer it refuses,
                // so both sides can show both versions.
                if self.role == Role::Acceptor {
                    self.send_hello(out);
                }
                self.peer_version = Some((proto_major, proto_minor));
                if proto_major != PROTO_MAJOR {
                    let (ours, theirs) = (
                        format!("{PROTO_MAJOR}.{PROTO_MINOR}"),
                        format!("{proto_major}.{proto_minor}"),
                    );
                    out.push(PairAction::Send(PairFrame::Control(Control::Error {
                        code: ErrorCode::Version as u16,
                        message: format!("version mismatch: {ours} vs {theirs}"),
                    })));
                    self.end(PairFailure::Version { ours, theirs }, out);
                    out.push(PairAction::Close {
                        code: close::VERSION,
                    });
                    return;
                }
                // Checked where it enters: a name with `CR` and
                // cursor escapes could rewrite the code line the user compares.
                match crate::peer::validate_peer_name(&name) {
                    Ok(()) => {
                        self.peer_name = name;
                        self.peer_name_refused = None;
                    }
                    Err(why) => {
                        self.peer_name = self.peer.default_name();
                        self.peer_name_refused = Some(why);
                    }
                }
                self.deadline = self.step_deadline(now);
                match self.role {
                    Role::Dialer => {
                        let c = commitment(&self.me, &self.peer, &self.nonce);
                        self.send(Pair::Commit { commitment: c }, out);
                        self.st = St::Nonce;
                    }
                    Role::Acceptor => self.st = St::Commit,
                }
            }
            (_, _, PairFrame::Control(Control::Error { code, .. })) => {
                if code == ErrorCode::Version as u16 {
                    let theirs = self
                        .peer_version
                        .map_or_else(|| "?".to_string(), |(a, b)| format!("{a}.{b}"));
                    self.end(
                        PairFailure::Version {
                            ours: format!("{PROTO_MAJOR}.{PROTO_MINOR}"),
                            theirs,
                        },
                        out,
                    );
                    out.push(PairAction::Close {
                        code: close::VERSION,
                    });
                } else {
                    self.fail(PairReason::Protocol, false, out);
                }
            }
            (St::Commit, Role::Acceptor, PairFrame::Pair(Pair::Commit { commitment })) => {
                self.commit = Some(commitment);
                self.send(Pair::Nonce { nonce: self.nonce }, out);
                self.st = St::Reveal;
                self.deadline = self.step_deadline(now);
            }
            (St::Nonce, Role::Dialer, PairFrame::Pair(Pair::Nonce { nonce: n_b })) => {
                self.send(Pair::Reveal { nonce: self.nonce }, out);
                let n_a = self.nonce;
                self.show_code(&n_a, &n_b, now, out);
            }
            (St::Reveal, Role::Acceptor, PairFrame::Pair(Pair::Reveal { nonce: n_a })) => {
                let expected = self.commit.take();
                if expected != Some(commitment(&self.peer, &self.me, &n_a)) {
                    self.fail(PairReason::BadReveal, true, out);
                    return;
                }
                let n_b = self.nonce;
                self.show_code(&n_a, &n_b, now, out);
            }
            (St::Human, _, PairFrame::Pair(Pair::Confirm { ok: true, .. })) if !self.remote_yes => {
                self.remote_yes = true;
                self.maybe_agree(out);
            }
            // early Reveal, early Confirm, a second Commit, a second Hello, a message in the
            // wrong direction: protocol
            _ => self.fail(PairReason::Protocol, true, out),
        }
    }
}

#[cfg(test)]
mod tests;
