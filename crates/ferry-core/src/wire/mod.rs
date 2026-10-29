//! What ferry puts inside ferry-net's bytes: CBOR control frames on the one
//! bidirectional control stream, and fixed 28-byte datagrams.
//!
//! ferry-net knows nothing of these types; ferry-core knows nothing of
//! sockets. ferry-net carries opaque `Bytes`: length-delimited frames on the
//! control stream (`u32 LE length | body`, body ≤ [`MAX_FRAME_BODY`]) and
//! datagrams.
//!
//! # Evolution rules
//!
//! (adapted from gliff `crates/gliff-proto/src/msg.rs` L1-26 @ 089662f)
//!
//! - A control frame body is `[variant, [field 0, field 1, …]]`; every
//!   variant and field is numbered with `#[n]`. Never renumber or reuse a
//!   variant or field number. Reserved numbers are listed in [`control`].
//! - A new field takes the next number and is an `Option`, so an older peer
//!   ignores it and a peer that lacks it decodes `None`.
//! - A required field that has shipped is sent forever.
//! - Enum-like fields travel as `u8` codes. An unknown code decodes as the
//!   type's `Other` value and never fails the message.
//! - A new message, a new code with new semantics or a new datagram type goes
//!   only to a peer that listed the feature introducing it in `Hello`. A peer
//!   skips a message whose variant it does not know (it is length-delimited)
//!   and counts it.
//! - `Hello` fields 0–1 (major, minor) and `Error` fields 0–1 (code, message)
//!   never change, so a version mismatch can always be reported.
//! - A message that arrives in the wrong direction (for example `Key` from a
//!   peer this peer is not entered by) is dropped and counted.
//! - Datagram type 1 (`Motion`) is frozen at 28 bytes; a new layout is a new
//!   type.
//! - A change these rules cannot express bumps [`PROTO_MAJOR`].

pub mod clip;
pub mod control;
pub mod datagram;

pub use control::{Control, Decoded, ErrorCode, LeaveReason, Pos};
pub use datagram::{DATAGRAM_LEN, Datagram, Motion};

/// ALPN of a session connection. Fixed: it names the framing generation
/// (length prefix + CBOR + datagram header), not the protocol version; the
/// version is checked in `Hello` (question Q1).
pub const ALPN: &[u8] = b"ferry/1";
/// ALPN of a pairing connection (design `m1-rest-design.md` §2.2):
/// only `Hello`, `Error` and `Pair` frames ([`crate::pair::PairFrame`]).
pub const ALPN_PAIR: &[u8] = b"ferry-pair/1";

/// Protocol major: a mismatch refuses the peer.
pub const PROTO_MAJOR: u16 = 1;
/// Protocol minor: informational; behaviour differences go through features.
pub const PROTO_MINOR: u16 = 0;
/// `PeerState` (#14): lock to this machine travels to the other peers.
pub const FEATURE_PEER_STATE: &str = "peer-state";
/// `ClipOffer`, `ClipRequest`, `ClipAbort` (#20, #21, #23) and the clipboard data streams.
/// Listed by every build that has them, on or off: a peer with sharing off ignores
/// offers and refuses requests.
pub const FEATURE_CLIPBOARD: &str = "clipboard";
/// Optional behaviours this build supports, listed in `Hello`.
pub const FEATURES: &[&str] = &[FEATURE_PEER_STATE, FEATURE_CLIPBOARD];

/// Largest control frame body either side sends or accepts. ferry-net
/// enforces it on read and closes the connection beyond it.
pub const MAX_FRAME_BODY: usize = 1 << 20;
/// Outbound control frames queued per connection before `try_send_frame`
/// reports `Full` (architecture §3: capture → net, 256).
pub const MAX_QUEUED_FRAMES: usize = 256;

/// Field caps checked on decode.
pub mod caps {
    pub const NAME: usize = 64;
    pub const BUILD: usize = 64;
    pub const FEATURES: usize = 16;
    pub const FEATURE_LEN: usize = 32;
    pub const MONITORS: usize = 16;
    pub const MONITOR_TEXT: usize = 128;
    pub const KEYMAP_TEXT: usize = 1_000_000;
    pub const HELD_KEYS: usize = crate::held::MAX_HELD_KEYS;
    pub const HELD_BUTTONS: usize = crate::held::MAX_HELD_BUTTONS;
    pub const ERROR_MESSAGE: usize = 512;
    pub const LAYOUT_PEERS: usize = 4;
    /// Mime types in one `ClipOffer`; a browser offers about ten.
    pub const CLIP_MIMES: usize = 32;
    pub const CLIP_MIME_LEN: usize = 128;
    /// A peer monitor's logical width or height (A-03): four times an 8K
    /// panel at scale 1.
    pub const MONITOR_SIZE: u32 = 1 << 16;
    /// `|x|`, `|y|` of a peer monitor's position, of its anchor shift and of
    /// a layout origin (A-03). Every sum in [`crate::geom`] is at most
    /// origin + shift + position + size, below 2^26 (3 * 2^24 + 2^16), and a
    /// difference of two such below 2^27: all fit in `i32`, so peer geometry
    /// can neither panic a debug build nor wrap.
    pub const COORD: u32 = 1 << 24;
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cbor: {0}")]
    Cbor(#[from] minicbor::decode::Error),
    #[error("{field} exceeds its cap of {cap}")]
    Limit { field: &'static str, cap: usize },
    /// A control character in a text field a terminal or a log would show.
    #[error("{field} holds a control character")]
    Text { field: &'static str },
    #[error("datagram of {0} bytes")]
    DatagramLength(usize),
    #[error("unknown datagram type {0}")]
    DatagramType(u8),
}

#[cfg(test)]
mod tests;
