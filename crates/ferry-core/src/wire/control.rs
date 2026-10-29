//! Control-stream messages. One frame = one [`Control`].
//!
//! Numbering, including reserved numbers that must never be reused:
//!
//! | # | Message | Direction |
//! |---|---|---|
//! | 0 | `Hello` | both, first frame |
//! | 1 | `Error` | both |
//! | 2 | `Keymap` | source → target |
//! | 3 | `Enter` | source → target |
//! | 4 | `Leave` | source → target |
//! | 5 | `Key` | source → target |
//! | 6 | `Button` | source → target |
//! | 7 | `Scroll` | source → target |
//! | 8 | `Held` | source → target |
//! | 9 | `Monitors` | both |
//! | 10 | `Layout` | both (feature `layout-sync`) |
//! | 11 | `LayoutAck` | both (feature `layout-sync`) |
//! | 12 | `HeldAck` | target → source |
//! | 13 | `Ended` | target → source |
//! | 14 | `PeerState` | both (feature `peer-state`) |
//! | 15–19 | reserved (input) | |
//! | 20 | `ClipOffer` | both (feature `clipboard`) |
//! | 21 | `ClipRequest` | both (feature `clipboard`) |
//! | 22 | reserved: the data stream's header is not a control frame ([`super::clip`]) | |
//! | 23 | `ClipAbort` | both (feature `clipboard`) |
//! | 24–29 | reserved | |
//! | 30–33 | pairing, only on `ferry-pair/1`: see [`Pair`] and [`crate::pair`] | |

use minicbor::{Decode, Encode};

use crate::geom::MonitorInfo;

/// Where the source's cursor model was when an input frame was sent:
/// a position fence against the unordered motion datagrams. The target
/// applies it before the input if `motion_seq` is newer than the last motion
/// it injected, and then ignores datagrams at or below it.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
pub struct Pos {
    /// The seq of the newest motion datagram sent before this frame.
    #[n(0)]
    pub motion_seq: u32,
    /// Target-local logical position.
    #[n(1)]
    pub x: f32,
    #[n(2)]
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub enum Control {
    /// First frame on the control stream from each side. The dialer sends
    /// first; the acceptor answers. Fields 0–1 are frozen forever.
    #[n(0)]
    Hello {
        #[n(0)]
        proto_major: u16,
        #[n(1)]
        proto_minor: u16,
        /// Display name, ≤ 64 bytes.
        #[n(2)]
        name: String,
        #[n(3)]
        #[cbor(decode_with = "capped_features")]
        features: Vec<String>,
        /// `CARGO_PKG_VERSION` + git describe, for `ferry status`.
        #[n(4)]
        build: String,
        /// This peer's monitors, local coordinates.
        #[n(5)]
        #[cbor(decode_with = "capped_monitors")]
        monitors: Vec<MonitorInfo>,
    },
    /// Fields 0–1 are frozen forever. Codes 1–255 are incompatibilities
    /// (do not redial in a loop); 256+ are transient.
    #[n(1)]
    Error {
        #[n(0)]
        code: u16,
        #[n(1)]
        message: String,
    },
    /// The source's XKB keymap (format 1 = XKB v1 text, without the NUL).
    /// Sent to every peer when it comes up and whenever the EIS keymap
    /// changes; always before an `Enter` that names its serial.
    #[n(2)]
    Keymap {
        #[n(0)]
        serial: u32,
        #[n(1)]
        format: u8,
        #[n(2)]
        text: String,
    },
    /// Start session `session` at the target-local entry point.
    #[n(3)]
    Enter {
        #[n(0)]
        session: u32,
        #[n(1)]
        x: f32,
        #[n(2)]
        y: f32,
        #[n(3)]
        keymap_serial: u32,
        /// Motion seq fence: datagrams at or below it are older than the
        /// entry point.
        #[n(4)]
        motion_seq: u32,
    },
    #[n(4)]
    Leave {
        #[n(0)]
        session: u32,
        /// [`LeaveReason`] code.
        #[n(1)]
        reason: u8,
    },
    #[n(5)]
    Key {
        #[n(0)]
        session: u32,
        /// evdev keycode.
        #[n(1)]
        code: u16,
        #[n(2)]
        pressed: bool,
    },
    #[n(6)]
    Button {
        #[n(0)]
        session: u32,
        /// evdev `BTN_*`.
        #[n(1)]
        code: u16,
        #[n(2)]
        pressed: bool,
        #[n(3)]
        pos: Pos,
    },
    /// One axis event (ADR 0015 order on the target: `axis`, `axis_source`,
    /// `frame`).
    #[n(7)]
    Scroll {
        #[n(0)]
        session: u32,
        /// 0 vertical, 1 horizontal.
        #[n(1)]
        axis: u8,
        /// 0 wheel, 1 finger, 2 continuous.
        #[n(2)]
        source: u8,
        /// Logical scroll distance (finger/continuous).
        #[n(3)]
        value: f32,
        /// Hi-res wheel steps (one notch = 120); wheel only.
        #[n(4)]
        value120: Option<i32>,
        /// `axis_stop` (end of a finger/continuous scroll).
        #[n(5)]
        stop: bool,
        #[n(6)]
        pos: Pos,
    },
    /// Held-set snapshot, every 100 ms while entered; also the heartbeat and
    /// the position resync.
    #[n(8)]
    Held {
        #[n(0)]
        session: u32,
        /// 1 for the snapshot sent with `Enter`, then +1 per snapshot.
        #[n(1)]
        n: u32,
        #[n(2)]
        #[cbor(decode_with = "capped_keys")]
        keys: Vec<u16>,
        #[n(3)]
        #[cbor(decode_with = "capped_buttons")]
        buttons: Vec<u16>,
        #[n(4)]
        pos: Pos,
    },
    /// This peer's monitors changed.
    #[n(9)]
    Monitors {
        #[n(0)]
        #[cbor(decode_with = "capped_monitors")]
        monitors: Vec<MonitorInfo>,
    },
    /// Reserved for M2 layout sync (FR-LAYOUT-07/08); defined now so the
    /// value M1 stores is the value M2 sends. Sent only with `layout-sync`.
    #[n(10)]
    Layout {
        #[n(0)]
        rev: WireRevision,
        #[n(1)]
        #[cbor(decode_with = "capped_origins")]
        origins: Vec<WireOrigin>,
    },
    #[n(11)]
    LayoutAck {
        #[n(0)]
        rev: WireRevision,
    },
    /// One per `Held` received and applied.
    #[n(12)]
    HeldAck {
        #[n(0)]
        session: u32,
        #[n(1)]
        n: u32,
    },
    /// The target ended or refused `session` (watchdog, preempted, refused,
    /// stale, takeover, shutdown, forced).
    #[n(13)]
    Ended {
        #[n(0)]
        session: u32,
        /// [`LeaveReason`] code.
        #[n(1)]
        reason: u8,
    },
    /// This peer's own state: sent after `Hello` at
    /// peer up and on every change, only to a peer whose `Hello.features`
    /// lists [`super::FEATURE_PEER_STATE`]. Later fields are trailing
    /// `Option`s (for example clipboard on/off in M4).
    #[n(14)]
    PeerState {
        /// Lock to this machine: no crossing starts from this
        /// peer while it is set.
        #[n(0)]
        locked: bool,
    },
    /// This peer's clipboard changed: the types it offers, not the data. A peer
    /// that pastes answers with [`Control::ClipRequest`] naming `serial`.
    #[n(20)]
    ClipOffer {
        /// +1 per local selection change; a request for an older one is refused.
        #[n(0)]
        serial: u32,
        #[n(1)]
        #[cbor(decode_with = "capped_mimes")]
        mimes: Vec<String>,
    },
    /// An application on the requesting peer pastes: send offer `serial` in `mime` on a data
    /// stream that starts with `id` ([`super::clip::DataHeader`]).
    #[n(21)]
    ClipRequest {
        #[n(0)]
        serial: u32,
        /// Odd from the dialer, even from the acceptor ([`super::clip::TransferIds`]), so an
        /// abort is never ambiguous.
        #[n(1)]
        id: u32,
        #[n(2)]
        mime: String,
    },
    /// Transfer `id` ends without data: the provider cannot serve it, or the requester no
    /// longer wants it.
    #[n(23)]
    ClipAbort {
        #[n(0)]
        id: u32,
        /// [`super::clip::AbortReason`] code.
        #[n(1)]
        reason: u8,
    },
}

/// A capped list was longer than its cap. Raised inside the CBOR decoder
/// and turned into [`super::DecodeError::Limit`] by [`Control::decode`].
#[derive(Debug, thiserror::Error)]
#[error("{field} exceeds its cap of {cap}")]
struct CapExceeded {
    field: &'static str,
    cap: usize,
}

/// Decode a `Vec` of at most `cap` items (A-05). A definite
/// length over the cap is refused before any item is decoded, and an
/// indefinite one at item `cap + 1`, so a frame can never allocate more
/// than `cap` items. [`Control::check_caps`] then checks the items.
fn capped<'b, Ctx, T: Decode<'b, Ctx>>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
    field: &'static str,
    cap: usize,
) -> Result<Vec<T>, minicbor::decode::Error> {
    let over = || minicbor::decode::Error::custom(CapExceeded { field, cap });
    let len = match d.probe().array()? {
        Some(n) => match usize::try_from(n) {
            Ok(n) if n <= cap => n,
            _ => return Err(over()),
        },
        None => 0,
    };
    let mut v = Vec::with_capacity(len);
    for item in d.array_iter_with(ctx)? {
        if v.len() == cap {
            return Err(over());
        }
        v.push(item?);
    }
    Ok(v)
}

fn capped_features<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<String>, minicbor::decode::Error> {
    capped(d, ctx, "features", super::caps::FEATURES)
}

fn capped_monitors<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<MonitorInfo>, minicbor::decode::Error> {
    capped(d, ctx, "monitors", super::caps::MONITORS)
}

fn capped_keys<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<u16>, minicbor::decode::Error> {
    capped(d, ctx, "held.keys", super::caps::HELD_KEYS)
}

fn capped_buttons<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<u16>, minicbor::decode::Error> {
    capped(d, ctx, "held.buttons", super::caps::HELD_BUTTONS)
}

fn capped_mimes<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<String>, minicbor::decode::Error> {
    capped(d, ctx, "clip.mimes", super::caps::CLIP_MIMES)
}

fn capped_origins<'b, Ctx>(
    d: &mut minicbor::Decoder<'b>,
    ctx: &mut Ctx,
) -> Result<Vec<WireOrigin>, minicbor::decode::Error> {
    capped(d, ctx, "layout.origins", super::caps::LAYOUT_PEERS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct WireRevision {
    #[n(0)]
    pub counter: u64,
    #[n(1)]
    #[cbor(with = "minicbor::bytes")]
    pub peer: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct WireOrigin {
    #[n(0)]
    #[cbor(with = "minicbor::bytes")]
    pub peer: [u8; 32],
    #[n(1)]
    pub x: i32,
    #[n(2)]
    pub y: i32,
}

/// Pairing messages on `ferry-pair/1` only (ADR 0006 as amended, design
/// `m1-rest-design.md` §2.3). On that ALPN only `Hello`, `Error` and these
/// four are valid; on `ferry/1` they are unknown variants (skipped and
/// counted). [`crate::pair`] decodes and runs them.
///
/// `Debug` never prints the commitment or a nonce.
#[derive(Clone, PartialEq, Encode, Decode)]
pub enum Pair {
    /// Dialer → acceptor, after both `Hello`s.
    #[n(30)]
    Commit {
        /// `SHA-256("ferry-commit" ‖ fp_A ‖ fp_B ‖ n_A)`: the dialer's nonce,
        /// bound to both keys ([`crate::pair::commitment`]).
        #[n(0)]
        #[cbor(with = "minicbor::bytes")]
        commitment: [u8; 32],
    },
    /// Acceptor → dialer, drawn before and sent only after `Commit`.
    #[n(31)]
    Nonce {
        #[n(0)]
        #[cbor(with = "minicbor::bytes")]
        nonce: [u8; 32],
    },
    /// Dialer → acceptor: the nonce committed to, sent only after `Nonce`.
    #[n(32)]
    Reveal {
        #[n(0)]
        #[cbor(with = "minicbor::bytes")]
        nonce: [u8; 32],
    },
    /// Both ways: the local user's answer.
    #[n(33)]
    Confirm {
        #[n(0)]
        ok: bool,
        /// [`crate::pair::PairReason`] code when `ok` is false.
        #[n(1)]
        reason: Option<u8>,
    },
}

impl std::fmt::Debug for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pair::Commit { .. } => f.write_str("Commit"),
            Pair::Nonce { .. } => f.write_str("Nonce"),
            Pair::Reveal { .. } => f.write_str("Reveal"),
            Pair::Confirm { ok, reason } => f
                .debug_struct("Confirm")
                .field("ok", ok)
                .field("reason", reason)
                .finish(),
        }
    }
}

impl Pair {
    /// The wire variant number (30–33).
    pub fn variant(&self) -> u32 {
        match self {
            Pair::Commit { .. } => 30,
            Pair::Nonce { .. } => 31,
            Pair::Reveal { .. } => 32,
            Pair::Confirm { .. } => 33,
        }
    }
}

/// Why a session ended, as a `u8` on the wire. Unknown codes decode as
/// [`LeaveReason::Other`]; the receiver acts the same for every reason
/// (release), so a new reason never needs a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LeaveReason {
    Other = 0,
    /// The cursor model crossed a link back home.
    Home = 1,
    /// The cursor model crossed a link to a third peer.
    Hop = 2,
    /// The escape chord, detected by ferryd.
    EscapeChord = 3,
    /// Hyprland ended capture: the escape bind or a layout change.
    CaptureEnded = 4,
    /// Source: no new acknowledgement for 650 ms.
    Silence = 5,
    /// Source screen locked (lock gate).
    Locked = 6,
    /// logind `PrepareForSleep`.
    Suspend = 7,
    /// ferryd exiting.
    Shutdown = 8,
    /// An orphan button release: a local drag crossed.
    Drag = 9,
    /// `ferry release` / socket `release`.
    Forced = 10,
    /// The target's geometry changed and the model left its monitors.
    Layout = 11,
    /// Target: no snapshot for 650 ms.
    Watchdog = 12,
    /// Target: another source entered.
    Preempted = 13,
    /// Target: refused `Enter` (lock to this machine, unknown keymap).
    Refused = 14,
    /// Target: an input frame for a session it no longer runs.
    Stale = 15,
    /// Target: local input on the target (later in M1).
    Takeover = 16,
    /// Either side: the connection to the peer closed under the session
    /// (`PeerDown`). Local only: there is no link left to send it on.
    LinkLost = 17,
}

impl LeaveReason {
    pub const ALL: [LeaveReason; 18] = [
        LeaveReason::Other,
        LeaveReason::Home,
        LeaveReason::Hop,
        LeaveReason::EscapeChord,
        LeaveReason::CaptureEnded,
        LeaveReason::Silence,
        LeaveReason::Locked,
        LeaveReason::Suspend,
        LeaveReason::Shutdown,
        LeaveReason::Drag,
        LeaveReason::Forced,
        LeaveReason::Layout,
        LeaveReason::Watchdog,
        LeaveReason::Preempted,
        LeaveReason::Refused,
        LeaveReason::Stale,
        LeaveReason::Takeover,
        LeaveReason::LinkLost,
    ];

    /// An unknown code is [`LeaveReason::Other`]: every reason releases, so a
    /// newer peer's reason never needs a feature.
    pub fn from_code(code: u8) -> Self {
        Self::ALL
            .get(usize::from(code))
            .copied()
            .unwrap_or(LeaveReason::Other)
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    /// The `snake_case` name used in `ferry status` and the socket API.
    pub fn as_str(self) -> &'static str {
        match self {
            LeaveReason::Other => "other",
            LeaveReason::Home => "home",
            LeaveReason::Hop => "hop",
            LeaveReason::EscapeChord => "escape_chord",
            LeaveReason::CaptureEnded => "capture_ended",
            LeaveReason::Silence => "silence",
            LeaveReason::Locked => "locked",
            LeaveReason::Suspend => "suspend",
            LeaveReason::Shutdown => "shutdown",
            LeaveReason::Drag => "drag",
            LeaveReason::Forced => "forced",
            LeaveReason::Layout => "layout",
            LeaveReason::Watchdog => "watchdog",
            LeaveReason::Preempted => "preempted",
            LeaveReason::Refused => "refused",
            LeaveReason::Stale => "stale",
            LeaveReason::Takeover => "takeover",
            LeaveReason::LinkLost => "link_lost",
        }
    }
}

/// `Error.code` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// Major versions differ; message `"version mismatch: <ours> vs <theirs>"`.
    Version = 1,
    /// A feature this peer requires is missing.
    Feature = 2,
    /// A known message failed to decode or broke a cap.
    BadFrame = 3,
    /// `Hello` was not the first frame, or came twice.
    Protocol = 4,
}

/// The result of decoding one frame body.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    Msg(Control),
    /// A variant this build does not know: skip it and count it.
    Unknown(u32),
}

impl Control {
    /// Encode into `buf` (cleared first; reused across calls).
    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.clear();
        // Writing into a `Vec` cannot fail, and no field has a fallible
        // encoder; an error would leave an empty body, which the peer treats
        // as a protocol fault rather than a wrong message.
        if minicbor::encode(self, &mut *buf).is_err() {
            debug_assert!(false, "encoding {:?} failed", self.variant());
            buf.clear();
        }
    }

    /// Decode one frame body; enforces [`super::caps`].
    ///
    /// - A variant this build does not know is [`Decoded::Unknown`]: skip the
    ///   frame and count it (the frame is length-delimited, so the next one
    ///   decodes).
    /// - A known message over a cap is [`super::DecodeError::Limit`]: drop it
    ///   and count it.
    /// - Anything else that fails is [`super::DecodeError::Cbor`]: close the
    ///   connection with code 2.
    pub fn decode(body: &[u8]) -> Result<Decoded, super::DecodeError> {
        // Read the variant number first so an unknown one can be reported.
        let variant = {
            let mut d = minicbor::Decoder::new(body);
            d.array()?;
            d.u32()?
        };
        match minicbor::decode::<Control>(body) {
            Ok(msg) => {
                msg.check_caps()?;
                Ok(Decoded::Msg(msg))
            }
            Err(e) if e.is_unknown_variant() => Ok(Decoded::Unknown(variant)),
            Err(e) => {
                match std::error::Error::source(&e).and_then(|s| s.downcast_ref::<CapExceeded>()) {
                    Some(c) => Err(super::DecodeError::Limit {
                        field: c.field,
                        cap: c.cap,
                    }),
                    None => Err(e.into()),
                }
            }
        }
    }

    /// The wire variant number (for counters and logs).
    pub fn variant(&self) -> u32 {
        match self {
            Control::Hello { .. } => 0,
            Control::Error { .. } => 1,
            Control::Keymap { .. } => 2,
            Control::Enter { .. } => 3,
            Control::Leave { .. } => 4,
            Control::Key { .. } => 5,
            Control::Button { .. } => 6,
            Control::Scroll { .. } => 7,
            Control::Held { .. } => 8,
            Control::Monitors { .. } => 9,
            Control::Layout { .. } => 10,
            Control::LayoutAck { .. } => 11,
            Control::HeldAck { .. } => 12,
            Control::Ended { .. } => 13,
            Control::PeerState { .. } => 14,
            Control::ClipOffer { .. } => 20,
            Control::ClipRequest { .. } => 21,
            Control::ClipAbort { .. } => 23,
        }
    }

    /// The `Hello.features` entry a peer must list before this message may be
    /// sent to it (ADR 0018: a new message goes only to a peer that listed
    /// the feature introducing it). `None`: every M1 peer takes it.
    pub fn feature(&self) -> Option<&'static str> {
        match self {
            Control::PeerState { .. } => Some(super::FEATURE_PEER_STATE),
            Control::ClipOffer { .. } | Control::ClipRequest { .. } | Control::ClipAbort { .. } => {
                Some(super::FEATURE_CLIPBOARD)
            }
            _ => None,
        }
    }

    /// Whether this message may go to a peer whose `Hello` listed `features`.
    pub fn may_send_to(&self, features: &[String]) -> bool {
        self.feature()
            .is_none_or(|f| features.iter().any(|g| g == f))
    }

    /// The session an input frame belongs to (`Enter`, `Leave`, `Key`,
    /// `Button`, `Scroll`, `Held`, `HeldAck`, `Ended`); `None` for the rest.
    pub fn session(&self) -> Option<u32> {
        match self {
            Control::Enter { session, .. }
            | Control::Leave { session, .. }
            | Control::Key { session, .. }
            | Control::Button { session, .. }
            | Control::Scroll { session, .. }
            | Control::Held { session, .. }
            | Control::HeldAck { session, .. }
            | Control::Ended { session, .. } => Some(*session),
            _ => None,
        }
    }

    /// Enforce the decode caps.
    fn check_caps(&self) -> Result<(), super::DecodeError> {
        use super::caps;
        fn cap(field: &'static str, len: usize, cap: usize) -> Result<(), super::DecodeError> {
            if len > cap {
                Err(super::DecodeError::Limit { field, cap })
            } else {
                Ok(())
            }
        }
        fn coord(field: &'static str, ok: bool) -> Result<(), super::DecodeError> {
            if ok {
                Ok(())
            } else {
                Err(super::DecodeError::Limit {
                    field,
                    cap: caps::COORD as usize,
                })
            }
        }
        fn monitors(m: &[MonitorInfo]) -> Result<(), super::DecodeError> {
            cap("monitors", m.len(), caps::MONITORS)?;
            for m in m {
                // Peer geometry is range-checked here, where it is decoded,
                // so every sum in `geom` fits in `i32` (A-03).
                if !m.size_in_range() {
                    return Err(super::DecodeError::Limit {
                        field: "monitor.size",
                        cap: caps::MONITOR_SIZE as usize,
                    });
                }
                coord("monitor.position", m.position_in_range())?;
                coord("monitor.shift", m.shift_in_range())?;
                cap("monitor.name", m.name.len(), caps::MONITOR_TEXT)?;
                cap(
                    "monitor.description",
                    m.description.len(),
                    caps::MONITOR_TEXT,
                )?;
                // Printed by `ferry layout show`, `status` and the log: no control characters
                // get in.
                for (field, text) in [
                    ("monitor.name", &m.name),
                    ("monitor.description", &m.description),
                ] {
                    if crate::geom::validate_monitor_text(text).is_err() {
                        return Err(super::DecodeError::Text { field });
                    }
                }
            }
            Ok(())
        }
        match self {
            Control::Hello {
                name,
                features,
                build,
                monitors: m,
                ..
            } => {
                cap("name", name.len(), caps::NAME)?;
                cap("build", build.len(), caps::BUILD)?;
                cap("features", features.len(), caps::FEATURES)?;
                for f in features {
                    cap("feature", f.len(), caps::FEATURE_LEN)?;
                }
                monitors(m)
            }
            Control::Error { message, .. } => {
                cap("error.message", message.len(), caps::ERROR_MESSAGE)
            }
            Control::Keymap { text, .. } => cap("keymap.text", text.len(), caps::KEYMAP_TEXT),
            Control::Held { keys, buttons, .. } => {
                cap("held.keys", keys.len(), caps::HELD_KEYS)?;
                cap("held.buttons", buttons.len(), caps::HELD_BUTTONS)
            }
            Control::Monitors { monitors: m } => monitors(m),
            Control::Layout { origins, .. } => {
                cap("layout.origins", origins.len(), caps::LAYOUT_PEERS)?;
                for o in origins {
                    coord(
                        "layout.origin",
                        crate::geom::coord_in_range(o.x) && crate::geom::coord_in_range(o.y),
                    )?;
                }
                Ok(())
            }
            Control::ClipOffer { mimes, .. } => {
                cap("clip.mimes", mimes.len(), caps::CLIP_MIMES)?;
                for m in mimes {
                    clip_mime(m)?;
                }
                Ok(())
            }
            Control::ClipRequest { mime, .. } => clip_mime(mime),
            Control::Enter { .. }
            | Control::Leave { .. }
            | Control::Key { .. }
            | Control::Button { .. }
            | Control::Scroll { .. }
            | Control::LayoutAck { .. }
            | Control::HeldAck { .. }
            | Control::Ended { .. }
            | Control::PeerState { .. }
            | Control::ClipAbort { .. } => Ok(()),
        }
    }
}

/// A mime type from a peer: at most [`super::caps::CLIP_MIME_LEN`] bytes of printable ASCII, so
/// it can go to the compositor and the log as it is.
fn clip_mime(m: &str) -> Result<(), super::DecodeError> {
    if m.len() > super::caps::CLIP_MIME_LEN {
        return Err(super::DecodeError::Limit {
            field: "clip.mime",
            cap: super::caps::CLIP_MIME_LEN,
        });
    }
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
        return Err(super::DecodeError::Text { field: "clip.mime" });
    }
    Ok(())
}
