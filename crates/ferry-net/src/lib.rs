//! ferry's transport.
//!
//! QUIC (`quinn`) on one UDP port, on ferry's own socket with DSCP EF, with TLS 1.3 mutual
//! authentication pinned to peer fingerprints. It carries opaque bytes; the message
//! format belongs to `ferry-core`.
//!
//! | Piece | Type |
//! |---|---|
//! | This peer's key and certificate | [`Identity`], [`Fingerprint`] |
//! | Pinned peers, shared with the verifiers | [`TrustSet`] |
//! | The UDP port, dial and accept | [`Endpoint`], [`EndpointConfig`] |
//! | One peer connection | [`Connection`] (send), [`Events`] (receive) |
//! | Dial with backoff, simultaneous-dial dedupe | [`Manager`] |
//!
//! # Lanes
//!
//! - **Datagrams** ([`Connection::send_datagram`]): unreliable, never block; the send buffer is
//!   small and drops the oldest datagram when full.
//! - **Frames** ([`Connection::try_send_frame`]): one bidirectional control stream, opened by the
//!   dialer, carrying `u32 LE length | body` frames. Bodies are 1 byte to
//!   [`EndpointConfig::max_frame`] (default [`MAX_FRAME`]). Empty frames are reserved: the
//!   open exchange uses them. On a pairing connection the cap is
//!   [`EndpointConfig::pairing_max_frame`] (default [`MAX_PAIRING_FRAME`]). A peer that sends an
//!   oversized frame is closed with [`CloseCode::FRAME_TOO_LARGE`].
//! - **Data streams** ([`Connection::open_data`], [`Connection::accept_data`]): unidirectional
//!   streams that either side opens, below the control stream in priority, for bulk bytes (the
//!   clipboard, ADR 0013). At most [`MAX_DATA_STREAMS`] are open each way; ferry-net gives them
//!   no framing.
//!
//! # Threading
//!
//! Create the [`Endpoint`] and run [`Endpoint::accept`], [`Endpoint::dial`], [`Events::next`],
//! [`Manager::next`] and [`Connection::close_graceful`] on one tokio runtime (ferryd's
//! current-thread `net` runtime). [`Connection`] is `Clone + Send + Sync`; its other
//! methods are synchronous, non-blocking and callable from any thread.
//!
//! # Pairing (design `m1-rest-design.md` §2.2, ADR 0006 as amended)
//!
//! Pairing runs on the same port with ALPN [`ALPN_PAIRING`] (`ferry-pair/1`). Both sides present
//! their certificate, as on `ferry/1`; what changes is what the verifier accepts:
//!
//! - On `ferry-pair/1` it accepts any single certificate whose key is not this peer's own, still
//!   checks the TLS 1.3 handshake signature, and records the fingerprint
//!   ([`PairingConnection::peer_fp`]). On `ferry/1` it stays pinned only, even
//!   while a window is open.
//! - The acceptor learns the ALPN from the ClientHello, through a per-connection certificate
//!   resolver that rustls calls before it verifies the client certificate. A client must offer
//!   exactly one ALPN.
//! - Each incoming connection gets its own server config. It offers `ferry-pair/1` only while
//!   the window is open ([`Endpoint::set_pairing`]), no attempt is running, and the source
//!   address is under [`PAIRING_BURST`] attempts per minute. Otherwise a pairing dialer gets TLS
//!   alert 120, which [`Endpoint::dial_pairing`] reports as [`ConnectError::NotAccepting`].
//! - A pairing connection skips the trust and datagram checks and goes to
//!   [`Endpoint::accept_pairing`], never to [`Endpoint::accept`] or the [`Manager`]. Resumption
//!   and 0-RTT stay off.
//!
//! # Stateless reset
//!
//! The endpoint's stateless-reset key is derived from the identity's private key (HKDF-SHA256,
//! RFC 9000 §10.3), so a peer that restarts with the same identity resets the connections of its
//! previous run: a dialer drops its dead connection at the first packet after the restart
//! ([`CloseReason::Reset`]) instead of at the idle timeout.

mod congestion;
mod connection;
mod endpoint;
mod error;
mod fingerprint;
mod identity;
mod limit;
mod manager;
mod pairing;
mod socket;
mod tls;
mod trust;

pub use connection::{Connection, DataRecv, DataSend, Event, Events, NewConnection, PathInfo};
pub use endpoint::{BindError, Endpoint, EndpointConfig, MIN_WINDOW};
pub use error::{
    CloseCode, CloseReason, ConnectError, DatagramError, FrameError, IncomingError, StreamError,
};
pub use fingerprint::{Fingerprint, ParseFingerprintError};
pub use identity::{Identity, IdentityError};
pub use limit::{
    HANDSHAKE_BURST, HANDSHAKE_REFILL_PER_SEC, LIMITER_IDLE, LIMITER_MAX_ENTRIES,
    MAX_HANDSHAKES_IN_FLIGHT,
};
pub use manager::{
    BACKOFF_MAX, BACKOFF_MIN, Manager, ManagerEvent, REFUSED_BACKOFF_MAX, REFUSED_BACKOFF_MIN,
    SIMULTANEOUS_HOLD,
};
pub use pairing::{PAIRING_BURST, PAIRING_REFILL_EVERY, PairingConnection, PairingWindow};
pub use socket::{DSCP_EF, SocketStats};
pub use trust::TrustSet;

/// ALPN of a ferry session; the major protocol version.
pub const ALPN: &[u8] = b"ferry/1";
/// ALPN of a pairing connection (design `m1-rest-design.md` §2.2).
pub const ALPN_PAIRING: &[u8] = b"ferry-pair/1";
/// Default UDP port.
pub const DEFAULT_PORT: u16 = 4243;
/// Default control-frame cap: 1 MiB, room for any xkb keymap.
pub const MAX_FRAME: usize = 1 << 20;
/// Default control-frame cap on a `ferry-pair/1` connection: 4 KiB. A pairing frame is a
/// `Hello` without monitors (under 200 bytes), an `Error` (at most 512) or a 32-byte `Pair`;
/// a host with no pin must not make the decoder allocate more (A-05).
pub const MAX_PAIRING_FRAME: usize = 4096;
/// Data streams each side may have open at once ([`Connection::open_data`] waits beyond it).
pub const MAX_DATA_STREAMS: u32 = 4;
/// The priority of a data stream: below the control stream's 0, so a large transfer never delays
/// a frame queued behind it.
pub const DATA_PRIORITY: i32 = -1;
/// Smallest datagram limit a peer must accept: one motion datagram. A peer
/// with a lower limit, or with datagrams off, is refused.
pub const MIN_DATAGRAM: usize = 28;
