//! The QUIC endpoint on ferry's UDP port: dial and accept pinned connections.

use crate::congestion::FlooredConfig;
use crate::connection::{Params, establish};
use crate::limit::HandshakeGate;
use crate::pairing::{PairingConnection, PairingGate, PairingWindow};
use crate::socket::{self, FerrySocket, SocketStats, StatsHandle};
use crate::tls::{self, AlpnResolver, Asked, ConnAlpn, PinVerifier, Rule};
use crate::{
    ALPN, ALPN_PAIRING, CloseCode, ConnectError, DEFAULT_PORT, Fingerprint, Identity,
    IncomingError, MAX_FRAME, MAX_PAIRING_FRAME, MIN_DATAGRAM, NewConnection, TrustSet,
};
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig};
use rustls::pki_types::CertificateDer;
use std::io;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

/// Endpoint settings. `Default` is the production setting.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Default `[::]:4243`: one dual-stack socket for IPv4 and IPv6, or
    /// `0.0.0.0:4243` on a host without IPv6.
    pub bind: SocketAddr,
    /// Mark every packet DSCP EF. Default `true`.
    pub dscp_ef: bool,
    /// Largest control frame accepted or sent, in bytes. Default [`MAX_FRAME`] (1 MiB).
    pub max_frame: usize,
    /// The same on a `ferry-pair/1` connection. Default [`MAX_PAIRING_FRAME`] (4 KiB): any
    /// host on the LAN reaches one while a window is open.
    pub pairing_max_frame: usize,
    /// Outgoing frames that may wait per connection before `try_send_frame` reports
    /// `QueueFull`. Default 256.
    pub frame_queue: usize,
    /// quinn's datagram send buffer in bytes. quinn accounts a small datagram as about 57
    /// bytes, so the default 4096 holds about 70; beyond that the oldest is
    /// dropped.
    pub datagram_send_buffer: usize,
    /// QUIC keep-alive while idle. Default 5 s.
    pub keep_alive: Duration,
    /// QUIC idle timeout: a silent peer is dropped after this. Default 10 s. ferry's own
    /// liveness acts far sooner and does not depend on it.
    pub idle_timeout: Duration,
    /// Limit for the QUIC handshake plus the open exchange. Default 2 s: a LAN
    /// handshake takes tens of milliseconds, and a timed-out dial is redialled at once, so a
    /// short limit finds a peer that has just come back sooner.
    pub handshake_timeout: Duration,
    /// quinn's round-trip estimate before the first sample. Default 50 ms. quinn's
    /// 333 ms puts the Initial's retransmits at about +1 s and +3 s; on a LAN they leave at
    /// about +0.2 s and +0.6 s, so a dial that lands just after the link returns is not lost.
    pub initial_rtt: Duration,
    /// Floor under quinn's congestion window, in bytes. Default [`MIN_WINDOW`]; 0 is stock
    /// CUBIC. A stall leaves its packets in flight until an acknowledgement shows them lost,
    /// and with a full window quinn sends only its own loss probes, at a timeout that doubles
    /// each time; the floor keeps ferryd's stall probes leaving (`congestion.rs`).
    pub min_window: u64,
}

/// Default [`EndpointConfig::min_window`]: 64 KiB, above what a source sends in a 650 ms stall
/// before its silence release (1 kHz motion is about 37 KiB), so a Ping still leaves after it.
pub const MIN_WINDOW: u64 = 64 * 1024;

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv6Addr::UNSPECIFIED, DEFAULT_PORT)),
            dscp_ef: true,
            max_frame: MAX_FRAME,
            pairing_max_frame: MAX_PAIRING_FRAME,
            frame_queue: 256,
            datagram_send_buffer: 4096,
            keep_alive: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(2),
            initial_rtt: Duration::from_millis(50),
            min_window: MIN_WINDOW,
        }
    }
}

/// Binding the endpoint failed.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// The UDP port is taken, or the address is not local ("port in use").
    #[error("cannot bind UDP {addr}: {source}")]
    Socket {
        addr: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("TLS setup: {0}")]
    Tls(#[from] rustls::Error),
    #[error("QUIC setup: {0}")]
    Quic(String),
}

/// What dial and accept tasks share.
struct Ctx {
    identity: Identity,
    trust: TrustSet,
    cfg: EndpointConfig,
    pairing: Arc<PairingGate>,
    server_transport: Arc<quinn::TransportConfig>,
    client_transport: Arc<quinn::TransportConfig>,
}

impl Ctx {
    fn params(&self) -> Params {
        Params {
            max_frame: self.cfg.max_frame,
            frame_queue: self.cfg.frame_queue,
        }
    }

    fn pairing_params(&self) -> Params {
        Params {
            max_frame: self.cfg.pairing_max_frame,
            ..self.params()
        }
    }

    fn server_config(
        &self,
        resolver: Arc<dyn rustls::server::ResolvesServerCert>,
        verifier: Arc<PinVerifier>,
        alpns: &[&[u8]],
    ) -> Result<quinn::ServerConfig, BindError> {
        let tls = tls::server(resolver, verifier, alpns)?;
        let quic = QuicServerConfig::try_from(tls).map_err(|e| BindError::Quic(e.to_string()))?;
        let mut sc = quinn::ServerConfig::with_crypto(Arc::new(quic));
        sc.transport_config(self.server_transport.clone());
        Ok(sc)
    }

    /// Checks the negotiated ALPN and the accepted peer, then runs the open exchange.
    async fn finish(
        &self,
        quic: quinn::Connection,
        dialed: bool,
        expected: Option<Fingerprint>,
    ) -> Result<NewConnection, ConnectError> {
        if negotiated(&quic).as_deref() != Some(ALPN) {
            quic.close(CloseCode::VERSION.0.into(), b"");
            return Err(ConnectError::VersionMismatch);
        }
        // The verifier already accepted this certificate; check it again, independently.
        let peer = quic
            .peer_identity()
            .and_then(|p| p.downcast::<Vec<CertificateDer<'static>>>().ok())
            .and_then(|chain| chain.first().and_then(|c| Fingerprint::of_cert(c).ok()));
        let peer = match peer {
            Some(fp) if self.trust.contains(&fp) && expected.is_none_or(|e| e == fp) => fp,
            _ => {
                quic.close(CloseCode::PROTOCOL.0.into(), b"");
                return Err(ConnectError::Other("peer identity check failed".into()));
            }
        };
        // Motion needs the datagram lane.
        if quic.max_datagram_size().is_none_or(|n| n < MIN_DATAGRAM) {
            quic.close(CloseCode::PROTOCOL.0.into(), b"no datagram lane");
            return Err(ConnectError::DatagramsUnsupported);
        }
        establish(quic, peer, dialed, &self.params()).await
    }

    /// A `ferry-pair/1` connection: the verifier accepted any key but our own and recorded it.
    /// No trust check and no datagram check ("After the handshake").
    async fn finish_pairing(
        &self,
        quic: quinn::Connection,
        dialed: bool,
    ) -> Result<NewConnection, ConnectError> {
        if negotiated(&quic).as_deref() != Some(ALPN_PAIRING) {
            quic.close(CloseCode::VERSION.0.into(), b"");
            return Err(ConnectError::NotAccepting);
        }
        let own = self.identity.fingerprint();
        let peer = quic
            .peer_identity()
            .and_then(|p| p.downcast::<Vec<CertificateDer<'static>>>().ok())
            .and_then(|chain| chain.first().and_then(|c| Fingerprint::of_cert(c).ok()));
        let peer = match peer {
            Some(fp) if fp != own => fp,
            _ => {
                quic.close(CloseCode::PROTOCOL.0.into(), b"");
                return Err(ConnectError::Other("peer identity check failed".into()));
            }
        };
        establish(quic, peer, dialed, &self.pairing_params()).await
    }
}

/// The ALPN the handshake settled on.
fn negotiated(quic: &quinn::Connection) -> Option<Vec<u8>> {
    quic.handshake_data()
        .and_then(|h| h.downcast::<HandshakeData>().ok())
        .and_then(|h| h.protocol)
}

/// What a pairing dial's refusals mean: the acceptor's alert does not say why (no window,
/// busy, over the address limit), so they all read as "not accepting".
fn pairing_error(e: ConnectError) -> ConnectError {
    match e {
        ConnectError::VersionMismatch
        | ConnectError::RefusedByDialer
        | ConnectError::NotTrustedByPeer(_) => ConnectError::NotAccepting,
        other => other,
    }
}

/// What an incoming handshake turned into.
enum Accepted {
    Session(NewConnection),
    Pairing(PairingConnection),
}

/// ferry's QUIC endpoint: one UDP socket, serving and dialling.
///
/// Create it inside the `net` tokio runtime; its I/O runs there. Clones
/// share the endpoint.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<Inner>,
}

struct Inner {
    quic: quinn::Endpoint,
    ctx: Arc<Ctx>,
    incoming: Mutex<mpsc::Receiver<Result<NewConnection, IncomingError>>>,
    pairings: Mutex<mpsc::Receiver<PairingConnection>>,
    stats: StatsHandle,
    gate: Arc<HandshakeGate>,
    accept_task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.accept_task.abort();
        self.quic.close(CloseCode::SHUTDOWN.0.into(), b"");
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("local_addr", &self.local_addr().ok())
            .field("fingerprint", &self.inner.ctx.identity.fingerprint())
            .finish_non_exhaustive()
    }
}

/// Receive window of each stream, in bytes: 256 KiB, a fifth of quinn's 1.25 MiB. quinn
/// aborts a connection ("too many gaps in stream buffer") once a stream's out-of-order data
/// holds more than 1024 separate spans, and with full packets the stock window has room for
/// about 1100 of them: a frame flood that overflows the receiving socket, losing every other
/// packet, ended a trusted peer's connection. This one holds about 220. The control
/// stream carries little, and a window this size still moves 50 MB/s at a 5 ms round trip.
const STREAM_WINDOW: u32 = 256 * 1024;

fn transport(cfg: &EndpointConfig, peer_may_open_bidi: bool) -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.initial_rtt(cfg.initial_rtt)
        .stream_receive_window(STREAM_WINDOW.into())
        .datagram_send_buffer_size(cfg.datagram_send_buffer)
        .keep_alive_interval(Some(cfg.keep_alive))
        .max_idle_timeout(cfg.idle_timeout.try_into().ok())
        // Only the dialer opens a stream: the control stream. Clipboard uni streams come later.
        .max_concurrent_bidi_streams(u32::from(peer_may_open_bidi).into())
        .max_concurrent_uni_streams(0u32.into())
        .congestion_controller_factory(Arc::new(FlooredConfig::new(cfg.min_window)));
    t
}

impl Endpoint {
    /// Binds the UDP socket and starts serving. Must be called inside a tokio runtime.
    pub fn bind(
        identity: Identity,
        trust: TrustSet,
        cfg: EndpointConfig,
    ) -> Result<Self, BindError> {
        let sock = socket::bind(cfg.bind, cfg.dscp_ef).map_err(|source| BindError::Socket {
            addr: cfg.bind,
            source,
        })?;
        let (sock, stats) = FerrySocket::new(sock).map_err(|source| BindError::Socket {
            addr: cfg.bind,
            source,
        })?;
        // The stateless-reset key and the connection-ID check key come from the identity, so a
        // restarted peer recognises its previous run's connection IDs and resets them (RFC 9000
        // §10.3; core §9, "Restart seen late").
        let mut endpoint_config = quinn::EndpointConfig::new(identity.reset_key());
        let cid_key = identity.cid_key();
        endpoint_config.cid_generator(move || {
            Box::new(quinn_proto::HashedConnectionIdGenerator::from_key(cid_key))
        });
        let ctx = Arc::new(Ctx {
            server_transport: Arc::new(transport(&cfg, true)),
            client_transport: Arc::new(transport(&cfg, false)),
            identity,
            trust,
            cfg,
            pairing: Arc::new(PairingGate::new()),
        });
        // The endpoint-wide server config is only the fallback; every incoming connection is
        // accepted with its own verifier and ALPN list (accept_loop).
        let default_server = ctx.server_config(
            ctx.identity.resolver(),
            PinVerifier::new(ctx.trust.clone(), None),
            &[ALPN],
        )?;
        let quic = quinn::Endpoint::new_with_abstract_socket(
            endpoint_config,
            Some(default_server),
            Arc::new(sock),
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(|source| BindError::Socket {
            addr: ctx.cfg.bind,
            source,
        })?;
        let (tx, rx) = mpsc::channel(16);
        let (pair_tx, pair_rx) = mpsc::channel(4);
        let gate = Arc::new(HandshakeGate::new());
        let accept_task = tokio::spawn(accept_loop(
            quic.clone(),
            ctx.clone(),
            gate.clone(),
            tx,
            pair_tx,
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                quic,
                ctx,
                incoming: Mutex::new(rx),
                pairings: Mutex::new(pair_rx),
                stats,
                gate,
                accept_task,
            }),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.quic.local_addr()
    }

    /// This peer's fingerprint.
    pub fn fingerprint(&self) -> Fingerprint {
        self.inner.ctx.identity.fingerprint()
    }

    /// The trust set the verifiers consult (shared, not a copy).
    pub fn trust(&self) -> &TrustSet {
        &self.inner.ctx.trust
    }

    /// Replaces the pinned fingerprints while running (`ferry trust`, `ferry unpair`). The next
    /// handshake uses the new set; live connections stay open until ferryd closes them
    /// (with [`CloseCode::UNPAIRED`]).
    pub fn set_trust(&self, fps: impl IntoIterator<Item = Fingerprint>) {
        self.inner.ctx.trust.replace(fps);
    }

    /// Connections open or still closing on this endpoint, including ones not yet accepted.
    pub fn open_connections(&self) -> usize {
        self.inner.quic.open_connections()
    }

    pub fn socket_stats(&self) -> SocketStats {
        self.inner.stats.get()
    }

    /// Inbound connection attempts refused before any TLS work, since the endpoint started:
    /// over [`HANDSHAKE_BURST`](crate::HANDSHAKE_BURST) per source IP plus
    /// [`HANDSHAKE_REFILL_PER_SEC`](crate::HANDSHAKE_REFILL_PER_SEC) a second, or with
    /// [`MAX_HANDSHAKES_IN_FLIGHT`](crate::MAX_HANDSHAKES_IN_FLIGHT) already running. A count
    /// that climbs means something on the LAN is flooding the port. It counts Initial packets,
    /// so a client that retransmits before it hears the refusal counts more than once. Refused
    /// attempts never reach [`Endpoint::accept`].
    pub fn handshakes_refused(&self) -> u64 {
        self.inner.gate.refused()
    }

    /// Initial packets answered with a stateless Retry because over half the in-flight slots
    /// were taken (A-09). A count that climbs with [`Endpoint::handshakes_refused`] means a
    /// flood; on its own it is a busy moment.
    pub fn handshakes_retried(&self) -> u64 {
        self.inner.gate.retried()
    }

    /// Dials `peer` at `addr`. Succeeds only if the answering certificate is `peer`'s and
    /// `peer` is trusted, the peer accepts this side's certificate, and the open exchange
    /// completes, all within the handshake timeout.
    pub async fn dial(
        &self,
        addr: SocketAddr,
        peer: Fingerprint,
    ) -> Result<NewConnection, ConnectError> {
        let ctx = &self.inner.ctx;
        let verifier = PinVerifier::new(ctx.trust.clone(), Some(peer));
        let tls = tls::client(&ctx.identity, verifier.clone(), ALPN)
            .map_err(|e| ConnectError::Other(e.to_string()))?;
        let quic_tls =
            QuicClientConfig::try_from(tls).map_err(|e| ConnectError::Other(e.to_string()))?;
        let mut cc = quinn::ClientConfig::new(Arc::new(quic_tls));
        cc.transport_config(ctx.client_transport.clone());
        let connecting = self
            .inner
            .quic
            .connect_with(cc, addr, tls::SERVER_NAME)
            .map_err(|e| ConnectError::Other(e.to_string()))?;
        let attempt = async {
            let quic = connecting
                .await
                .map_err(|e| ConnectError::from_handshake(&e, verifier.rejection(), Some(peer)))?;
            ctx.finish(quic, true, Some(peer)).await
        };
        tokio::time::timeout(ctx.cfg.handshake_timeout, attempt)
            .await
            .unwrap_or(Err(ConnectError::TimedOut))
    }

    /// The next inbound connection, or a refused one (with the presented fingerprint, so
    /// ferryd can log `unknown peer <fp>`). `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<Result<NewConnection, IncomingError>> {
        self.inner.incoming.lock().await.recv().await
    }

    /// Opens (`Some`) or closes (`None`) the pairing window. While it is open, an incoming
    /// connection is offered `ferry-pair/1` too, if no attempt is running and its source address
    /// is under [`PAIRING_BURST`](crate::PAIRING_BURST) attempts per minute (design
    /// `m1-rest-design.md` §2.2, §2.7). Each incoming connection reads it when it arrives.
    pub fn set_pairing(&self, window: Option<PairingWindow>) {
        self.inner.ctx.pairing.set_window(window);
    }

    /// Pairing handshakes turned away since the endpoint started: no window, an attempt already
    /// running, or the source address over its limit.
    pub fn pairing_refused(&self) -> u64 {
        self.inner.ctx.pairing.refused()
    }

    /// The next accepted `ferry-pair/1` connection. Pairing connections never reach
    /// [`Endpoint::accept`] or the [`Manager`](crate::Manager). `None` once the endpoint is
    /// closed.
    pub async fn accept_pairing(&self) -> Option<PairingConnection> {
        self.inner.pairings.lock().await.recv().await
    }

    /// Dials `addr` on `ferry-pair/1`. The answering certificate may carry any key but this
    /// peer's own (it is recorded: [`PairingConnection::peer_fp`]); the TLS 1.3 handshake
    /// signature is still verified. Fails with [`ConnectError::NotAccepting`] when the peer has
    /// no window open, is busy, or this address is over its limit.
    pub async fn dial_pairing(&self, addr: SocketAddr) -> Result<PairingConnection, ConnectError> {
        let ctx = &self.inner.ctx;
        let own = ctx.identity.fingerprint();
        let verifier = PinVerifier::with_rule(ctx.trust.clone(), None, Rule::Pairing { own });
        let tls = tls::client(&ctx.identity, verifier.clone(), ALPN_PAIRING)
            .map_err(|e| ConnectError::Other(e.to_string()))?;
        let quic_tls =
            QuicClientConfig::try_from(tls).map_err(|e| ConnectError::Other(e.to_string()))?;
        let mut cc = quinn::ClientConfig::new(Arc::new(quic_tls));
        cc.transport_config(ctx.client_transport.clone());
        let connecting = self
            .inner
            .quic
            .connect_with(cc, addr, tls::SERVER_NAME)
            .map_err(|e| ConnectError::Other(e.to_string()))?;
        let attempt = async {
            let quic = connecting
                .await
                .map_err(|e| ConnectError::from_handshake(&e, verifier.rejection(), None))?;
            ctx.finish_pairing(quic, true).await
        };
        let nc = tokio::time::timeout(ctx.cfg.handshake_timeout, attempt)
            .await
            .unwrap_or(Err(ConnectError::TimedOut))
            .map_err(pairing_error)?;
        Ok(PairingConnection::new(nc, None))
    }

    /// Closes every connection with [`CloseCode::SHUTDOWN`] and stops accepting.
    pub fn close(&self) {
        self.inner.accept_task.abort();
        self.inner.quic.close(CloseCode::SHUTDOWN.0.into(), b"");
    }

    /// Waits until every connection has sent its close (bounded by quinn's drain period).
    pub async fn wait_idle(&self) {
        self.inner.quic.wait_idle().await;
    }
}

async fn accept_loop(
    quic: quinn::Endpoint,
    ctx: Arc<Ctx>,
    gate: Arc<HandshakeGate>,
    tx: mpsc::Sender<Result<NewConnection, IncomingError>>,
    pair_tx: mpsc::Sender<PairingConnection>,
) {
    while let Some(incoming) = quic.accept().await {
        let remote = incoming.remote_address();
        let remote = SocketAddr::new(remote.ip().to_canonical(), remote.port());
        // Over half the slots taken: make an unproven address prove it first (A-09). The Retry
        // is stateless, so it takes no slot and no token, and a spoofed source never sees it.
        if gate.should_retry(incoming.remote_address_validated()) {
            tracing::debug!(%remote, "handshake slots over half taken: sending a Retry");
            let _ = incoming.retry();
            continue;
        }
        // Before any TLS work: a flood costs one small CONNECTION_REFUSED per attempt.
        let Some(permit) = gate.enter(remote.ip(), Instant::now()) else {
            tracing::debug!(%remote, "handshake refused: rate limit");
            incoming.refuse();
            continue;
        };
        let (ctx, tx, pair_tx) = (ctx.clone(), tx.clone(), pair_tx.clone());
        tokio::spawn(async move {
            // This connection's ALPN list: ferry-pair/1 only while a window is open, no attempt
            // runs and this address is under its pairing limit.
            let conn_alpn = ConnAlpn::new(ctx.pairing.clone(), remote.ip(), Instant::now());
            let verifier = PinVerifier::with_rule(
                ctx.trust.clone(),
                None,
                Rule::ByAlpn {
                    own: ctx.identity.fingerprint(),
                    conn: conn_alpn.clone(),
                },
            );
            let result = async {
                let alpns: &[&[u8]] = if conn_alpn.offer_pairing {
                    &[ALPN, ALPN_PAIRING]
                } else {
                    &[ALPN]
                };
                let sc = ctx
                    .server_config(
                        AlpnResolver::new(&ctx.identity, conn_alpn.clone()),
                        verifier.clone(),
                        alpns,
                    )
                    .map_err(|e| ConnectError::Other(e.to_string()))?;
                let connecting = incoming
                    .accept_with(Arc::new(sc))
                    .map_err(|e| ConnectError::from_handshake(&e, verifier.rejection(), None))?;
                let quic = connecting
                    .await
                    .map_err(|e| ConnectError::from_handshake(&e, verifier.rejection(), None))?;
                if conn_alpn.asked.get() == Some(&Asked::Pairing) {
                    let nc = ctx.finish_pairing(quic, false).await?;
                    Ok(Accepted::Pairing(PairingConnection::new(
                        nc,
                        conn_alpn.take_claim(),
                    )))
                } else {
                    ctx.finish(quic, false, None).await.map(Accepted::Session)
                }
            };
            let result = tokio::time::timeout(ctx.cfg.handshake_timeout, result)
                .await
                .unwrap_or(Err(ConnectError::TimedOut))
                // a pairing dial turned away fails as a version mismatch (alert 120) or with no
                // certificate: say what it was
                .map_err(|e| match conn_alpn.asked.get() {
                    Some(Asked::PairingRefused) => ConnectError::PairingRefused,
                    _ => e,
                });
            // the handshake is over; do not hold its slot while the consumer is slow
            drop(permit);
            // a pairing attempt whose handshake failed ends here (rustls keeps the resolver)
            drop(conn_alpn.take_claim());
            match result {
                Ok(Accepted::Session(nc)) => {
                    let _ = tx.send(Ok(nc)).await;
                }
                Ok(Accepted::Pairing(pc)) => {
                    // Nobody takes it (ferryd has no window open any more): dropping it closes
                    // it and ends the attempt.
                    if pair_tx.try_send(pc).is_err() {
                        tracing::debug!(%remote, "a pairing connection nobody takes: closed");
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(IncomingError { remote, error })).await;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::time::Duration;

    use super::*;
    use crate::limit::RETRY_ABOVE_IN_FLIGHT;

    fn endpoint() -> Endpoint {
        let cfg = EndpointConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            handshake_timeout: Duration::from_secs(3),
            ..EndpointConfig::default()
        };
        Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), cfg).unwrap()
    }

    /// A-09: with the slots over half taken by handshakes that never finish, a pinned peer
    /// still connects, after a stateless Retry.
    #[tokio::test]
    async fn a_trusted_dial_still_connects_while_over_half_the_slots_are_stalled() {
        let (a, b) = (endpoint(), endpoint());
        a.trust().insert(b.fingerprint());
        b.trust().insert(a.fingerprint());

        // stalled handshakes from other hosts
        let now = Instant::now();
        let stalled: Vec<_> = (0..=RETRY_ABOVE_IN_FLIGHT)
            .map(|n| {
                b.inner
                    .gate
                    .enter(IpAddr::from([10, 9, 0, n as u8]), now)
                    .expect("a slot")
            })
            .collect();

        let dial = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(a.dial(b.local_addr().unwrap(), b.fingerprint()), b.accept())
        })
        .await
        .expect("no timeout");
        assert!(dial.0.is_ok(), "{:?}", dial.0.err());
        assert!(dial.1.is_some_and(|r| r.is_ok()));
        assert!(b.handshakes_retried() >= 1, "the dial was sent a Retry");
        drop(stalled);
    }
}
