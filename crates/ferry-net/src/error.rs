//! Error and close-reason types, classified for ferryd's named states.

use crate::Fingerprint;
use crate::tls::Rejection;
use std::fmt;
use std::net::SocketAddr;

/// An application close code, carried in QUIC's CONNECTION_CLOSE.
///
/// Codes 0–5 are fixed with ferryd. ferry-net sends
/// [`VERSION`](Self::VERSION), [`PROTOCOL`](Self::PROTOCOL),
/// [`FRAME_TOO_LARGE`](Self::FRAME_TOO_LARGE) and [`DUPLICATE`](Self::DUPLICATE) itself;
/// ferryd sends [`SHUTDOWN`](Self::SHUTDOWN) and [`UNPAIRED`](Self::UNPAIRED). New codes take
/// the next free number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CloseCode(pub u32);

impl CloseCode {
    /// Orderly shutdown (also what quinn sends when every handle is dropped).
    pub const SHUTDOWN: Self = Self(0);
    /// The negotiated ALPN was not the one this endpoint serves.
    pub const VERSION: Self = Self(1);
    /// The peer broke ferry-net's rules: a bad open exchange, an empty frame, or no usable
    /// datagram lane.
    pub const PROTOCOL: Self = Self(2);
    /// A control frame exceeded the receiver's size cap.
    pub const FRAME_TOO_LARGE: Self = Self(3);
    /// The peer was unpaired or its pin revoked.
    pub const UNPAIRED: Self = Self(4);
    /// Another connection to the same peer was kept (simultaneous dial).
    pub const DUPLICATE: Self = Self(5);
}

impl fmt::Display for CloseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::SHUTDOWN => "shutdown",
            Self::VERSION => "version mismatch",
            Self::PROTOCOL => "protocol violation",
            Self::FRAME_TOO_LARGE => "frame too large",
            Self::UNPAIRED => "unpaired",
            Self::DUPLICATE => "duplicate connection",
            _ => return write!(f, "code {}", self.0),
        };
        f.write_str(name)
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer closed it with this code and reason text.
    Peer { code: CloseCode, reason: String },
    /// This side closed it ([`crate::Connection::close`], the manager, or ferry-net on a
    /// protocol error).
    Local { code: CloseCode, reason: String },
    /// Nothing heard from the peer for the idle timeout.
    TimedOut,
    /// The peer answered with a stateless reset: it restarted and lost the connection.
    Reset,
    /// A transport-level error, with quinn's text.
    Transport(String),
}

impl CloseReason {
    /// The application close code, if either side sent one.
    pub fn code(&self) -> Option<CloseCode> {
        match self {
            Self::Peer { code, .. } | Self::Local { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// `true` if the peer ended the connection (by close, reset or a transport error it
    /// signalled), `false` if this side did or the peer went silent.
    pub fn by_peer(&self) -> bool {
        matches!(self, Self::Peer { .. } | Self::Reset)
    }

    /// The reason text: the one sent with the close, or a description.
    pub fn reason(&self) -> String {
        match self {
            Self::Peer { reason, .. } | Self::Local { reason, .. } => reason.clone(),
            other => other.to_string(),
        }
    }
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tail = |r: &str| {
            if r.is_empty() {
                String::new()
            } else {
                format!(" ({r})")
            }
        };
        match self {
            Self::Peer { code, reason } => write!(f, "closed by peer: {code}{}", tail(reason)),
            Self::Local { code, reason } => write!(f, "closed locally: {code}{}", tail(reason)),
            Self::TimedOut => f.write_str("peer silent (idle timeout)"),
            Self::Reset => f.write_str("peer restarted (stateless reset)"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
        }
    }
}

/// TLS alert `access_denied`, which a [`crate::tls`] verifier sends on refusal, as a QUIC
/// crypto error code (0x100 + alert).
const CRYPTO_ACCESS_DENIED: u64 = 0x100 + 49;
/// TLS alert `no_application_protocol` (RFC 7301).
const CRYPTO_NO_ALPN: u64 = 0x100 + 120;
/// Transport error `APPLICATION_ERROR`: an application close sent before the handshake was
/// confirmed travels as this. quinn sends it when the last handle of a
/// connection that is still opening is dropped, as on a dial timeout.
const APPLICATION_ERROR: u64 = 0x0c;

/// The code and reason this side closed with, recorded before calling quinn's `close`.
pub(crate) type LocalClose = (CloseCode, String);

/// Classifies a quinn error. `local` is what this side closed with, if it did.
pub(crate) fn close_reason(e: &quinn::ConnectionError, local: Option<&LocalClose>) -> CloseReason {
    use quinn::ConnectionError as E;
    match e {
        E::ApplicationClosed(c) => CloseReason::Peer {
            code: CloseCode(u32::try_from(c.error_code.into_inner()).unwrap_or(u32::MAX)),
            reason: String::from_utf8_lossy(&c.reason).into_owned(),
        },
        E::LocallyClosed => {
            let (code, reason) = local
                .cloned()
                .unwrap_or((CloseCode::SHUTDOWN, String::new()));
            CloseReason::Local { code, reason }
        }
        E::TimedOut => CloseReason::TimedOut,
        E::Reset => CloseReason::Reset,
        other => CloseReason::Transport(other.to_string()),
    }
}

/// Opening a connection failed. Each variant is one of ferryd's named states.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    /// The peer presented a key this side has not pinned. ferryd logs it as
    /// `unknown peer <fp>`. The text carries the full 64-digit fingerprint, the form
    /// `ferry id` prints, so a user can compare it digit for digit before running
    /// `ferry trust`. `Debug` stays short.
    #[error("unknown peer {}", .0)]
    UnknownPeer(Fingerprint),
    /// A dial reached a peer whose fingerprint is not the expected one, trusted
    /// or not. Both fingerprints are printed in full, like [`Self::UnknownPeer`].
    #[error("fingerprint changed: expected {}, got {}", .expected, .got)]
    FingerprintChanged {
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// A dial: the peer refused this side's certificate, because it has not pinned this
    /// machine (or unpaired it). It names the peer that refused, the fingerprint that was
    /// dialled, in full like [`Self::UnknownPeer`].
    #[error("peer {} does not trust this machine", .0)]
    NotTrustedByPeer(Fingerprint),
    /// An inbound handshake: the dialer refused this side's certificate. It has pinned another
    /// fingerprint for this address, or none; its own side reports
    /// [`Self::FingerprintChanged`] or [`Self::UnknownPeer`]. The dialer's fingerprint is not
    /// known here, because a TLS 1.3 client refuses before it sends its own certificate.
    #[error("the dialing peer refused this machine's certificate")]
    RefusedByDialer,
    /// The peer closed the connection before it opened, without refusing it: its own dial or
    /// accept gave up (its handshake timeout, or a dial it dropped). This is not a refusal.
    ///
    /// A restarted peer can see one at once: a dial that was in flight while it was down times
    /// out, and the close it sends is the first packet the new endpoint receives, as a new
    /// connection that is closed on arrival.
    #[error("the peer gave up the handshake")]
    Abandoned,
    /// No common ALPN: the peer speaks another major protocol version.
    #[error("protocol version mismatch")]
    VersionMismatch,
    /// The peer's certificate was not a single parsable certificate.
    #[error("the peer sent an unusable certificate")]
    BadCertificate,
    /// The peer has datagrams disabled, or its limit is below a motion datagram
    /// ([`crate::MIN_DATAGRAM`]).
    #[error("the peer does not accept {}-byte datagrams", crate::MIN_DATAGRAM)]
    DatagramsUnsupported,
    /// A pairing dial ([`crate::Endpoint::dial_pairing`]): the peer has no pairing window open,
    /// is busy with another attempt, or this address is over its pairing limit (design
    /// `m1-rest-design.md` §2.2). The acceptor's TLS alert does not say which.
    #[error("not accepting pairing")]
    NotAccepting,
    /// An inbound handshake: the dialer asked for `ferry-pair/1` while this side takes no
    /// pairing (no window open, an attempt already running, or the address over its limit).
    /// The dialer sees [`Self::NotAccepting`].
    #[error(
        "a pairing request while not accepting pairing (no window open, an attempt running, or too many from this address)"
    )]
    PairingRefused,
    /// Pairing: the peer presented this machine's own key.
    #[error("the peer presented this machine's own key")]
    OwnKey,
    /// No complete handshake within the handshake timeout: unreachable, wrong port, or a
    /// firewall in the way.
    #[error("peer unreachable (handshake timed out)")]
    TimedOut,
    /// The connection closed during the open exchange.
    #[error("closed while opening: {0}")]
    Closed(CloseReason),
    /// Anything else: a local configuration or socket error, or a TLS failure not listed above.
    #[error("{0}")]
    Other(String),
}

impl ConnectError {
    /// Classifies a failed handshake. A local verifier's verdict wins over the transport error,
    /// because it names the presented fingerprint. `dialed` is the peer this side dialled, or
    /// `None` on an inbound connection.
    pub(crate) fn from_handshake(
        e: &quinn::ConnectionError,
        rejected: Option<Rejection>,
        dialed: Option<Fingerprint>,
    ) -> Self {
        if let Some(r) = rejected {
            return match r {
                Rejection::Unknown(fp) => Self::UnknownPeer(fp),
                Rejection::Changed { expected, got } => Self::FingerprintChanged { expected, got },
                Rejection::Malformed => Self::BadCertificate,
                Rejection::OwnKey => Self::OwnKey,
            };
        }
        use quinn::ConnectionError as E;
        match e {
            E::ConnectionClosed(c) => match u64::from(c.error_code) {
                CRYPTO_ACCESS_DENIED => match dialed {
                    Some(fp) => Self::NotTrustedByPeer(fp),
                    None => Self::RefusedByDialer,
                },
                CRYPTO_NO_ALPN => Self::VersionMismatch,
                APPLICATION_ERROR => Self::Abandoned,
                _ => Self::Closed(CloseReason::Transport(e.to_string())),
            },
            // the peer dropped its side after the handshake, during the open exchange
            E::ApplicationClosed(c)
                if c.error_code.into_inner() == u64::from(CloseCode::SHUTDOWN.0) =>
            {
                Self::Abandoned
            }
            E::TransportError(t) => match u64::from(t.code) {
                CRYPTO_NO_ALPN => Self::VersionMismatch,
                _ => Self::Other(e.to_string()),
            },
            E::TimedOut => Self::TimedOut,
            other => Self::Closed(close_reason(other, None)),
        }
    }
}

/// An inbound connection that was refused or failed to open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{remote}: {error}")]
pub struct IncomingError {
    pub remote: SocketAddr,
    pub error: ConnectError,
}

/// [`crate::Connection::try_send_frame`] failed; nothing was queued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The bounded outgoing queue is full: the network side is not draining. ferryd fails
    /// open (`ctrl_overflow`).
    #[error("frame queue full")]
    Full,
    /// The connection is closed, or the control stream was finished.
    #[error("connection closed")]
    Closed,
    /// Larger than the frame cap.
    #[error("frame too large ({len} bytes, limit {max})")]
    TooLarge { len: usize, max: usize },
    /// Empty frames are reserved by ferry-net.
    #[error("empty frame")]
    Empty,
}

/// [`crate::Connection::send_datagram`] failed; nothing was queued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatagramError {
    /// Larger than the path's current datagram limit.
    #[error("datagram too large ({len} bytes, limit {max})")]
    TooLarge { len: usize, max: usize },
    /// The peer does not accept datagrams.
    #[error("peer does not accept datagrams")]
    Unsupported,
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_errors_print_the_full_fingerprint_and_debug_stays_short() {
        let (a, b) = (Fingerprint([0xab; 32]), Fingerprint([0x01; 32]));
        let (full_a, full_b) = ("ab".repeat(32), "01".repeat(32));

        let unknown = ConnectError::UnknownPeer(a);
        assert_eq!(unknown.to_string(), format!("unknown peer {full_a}"));
        assert_eq!(
            IncomingError {
                remote: "192.0.2.1:4243".parse().unwrap(),
                error: unknown.clone(),
            }
            .to_string(),
            format!("192.0.2.1:4243: unknown peer {full_a}")
        );

        let changed = ConnectError::FingerprintChanged {
            expected: b,
            got: a,
        };
        assert_eq!(
            changed.to_string(),
            format!("fingerprint changed: expected {full_b}, got {full_a}")
        );

        let not_trusted = ConnectError::NotTrustedByPeer(b);
        assert_eq!(
            not_trusted.to_string(),
            format!("peer {full_b} does not trust this machine")
        );

        // Debug is for logs of structured values and stays short
        let debug = format!("{unknown:?}");
        assert!(debug.contains(&a.short()), "{debug}");
        assert!(!debug.contains(&full_a), "{debug}");
        let debug = format!("{not_trusted:?}");
        assert!(
            debug.contains(&b.short()) && !debug.contains(&full_b),
            "{debug}"
        );
    }
}
