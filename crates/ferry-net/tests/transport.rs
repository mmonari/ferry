//! Loopback integration tests: pinned handshakes, refusals, lanes, close codes, dedupe.
#![expect(
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "integration test: diagnostics for a failing run, which the harness captures; an unwrap that fails is the test failing"
)]

use bytes::Bytes;
use ferry_net::{
    CloseCode, CloseReason, ConnectError, Connection, DatagramError, Endpoint, EndpointConfig,
    Event, Events, FrameError, Identity, Manager, ManagerEvent, NewConnection, REFUSED_BACKOFF_MIN,
    TrustSet,
};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

const T: Duration = Duration::from_secs(5);

async fn within<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(T, f).await.expect("timed out")
}

fn lo() -> EndpointConfig {
    EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        handshake_timeout: Duration::from_secs(3),
        ..EndpointConfig::default()
    }
}

/// An endpoint with a fresh identity and an empty trust store.
fn endpoint(cfg: EndpointConfig) -> Endpoint {
    Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), cfg).unwrap()
}

fn addr(ep: &Endpoint) -> SocketAddr {
    ep.local_addr().unwrap()
}

fn trust_each_other(a: &Endpoint, b: &Endpoint) {
    a.trust().insert(b.fingerprint());
    b.trust().insert(a.fingerprint());
}

/// a dials b; returns (a's side, b's side).
async fn connect(a: &Endpoint, b: &Endpoint) -> (NewConnection, NewConnection) {
    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(addr(b), b.fingerprint()), b.accept()) }).await;
    (dialed.unwrap(), accepted.unwrap().unwrap())
}

async fn next(ev: &mut Events) -> Event {
    within(ev.next()).await.expect("events ended")
}

/// Asserts that nothing at all arrives on `ep.accept()` for a while.
async fn no_incoming(ep: &Endpoint) {
    assert!(
        tokio::time::timeout(Duration::from_millis(300), ep.accept())
            .await
            .is_err(),
        "unexpected inbound connection"
    );
}

#[tokio::test]
async fn pinned_handshake_succeeds_both_ways() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);

    let (ab, ba) = connect(&a, &b).await;
    assert_eq!(ab.conn.peer_fp(), b.fingerprint());
    assert_eq!(ba.conn.peer_fp(), a.fingerprint());
    assert!(ab.conn.is_dialer() && !ba.conn.is_dialer());
    assert_eq!(ab.conn.remote_addr(), addr(&b));

    let (ba2, ab2) = connect(&b, &a).await;
    assert_eq!(ba2.conn.peer_fp(), a.fingerprint());
    assert_eq!(ab2.conn.peer_fp(), b.fingerprint());
    assert!(ab.conn.rtt() < Duration::from_secs(1));
}

#[tokio::test]
async fn untrusted_client_is_refused_in_the_handshake() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    a.trust().insert(b.fingerprint()); // a trusts b, b does not trust a

    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    // the dialer learns which peer refused it, in full
    let not_trusted = dialed.unwrap_err();
    assert_eq!(not_trusted, ConnectError::NotTrustedByPeer(b.fingerprint()));
    assert_eq!(
        not_trusted.to_string(),
        format!("peer {} does not trust this machine", b.fingerprint())
    );
    let refused = accepted.unwrap().unwrap_err();
    assert_eq!(refused.error, ConnectError::UnknownPeer(a.fingerprint()));
    assert_eq!(refused.remote, addr(&a));
    // the full fingerprint, the form `ferry id` prints, so the user can compare it
    assert!(refused.to_string().contains(&a.fingerprint().to_string()));
    // b never sees a connection, so no frame or datagram from a can reach b's consumer
    no_incoming(&b).await;
}

#[tokio::test]
async fn untrusted_server_is_refused_in_the_handshake() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    b.trust().insert(a.fingerprint()); // b trusts a, a does not trust b

    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::UnknownPeer(b.fingerprint())
    );
    assert_eq!(
        accepted.unwrap().unwrap_err().error,
        ConnectError::RefusedByDialer
    );
    no_incoming(&b).await;
}

#[tokio::test]
async fn dial_reports_a_changed_fingerprint_trusted_or_not() {
    let (a, b, c) = (endpoint(lo()), endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    a.trust().insert(c.fingerprint());

    // a trusted peer answers where a expects c
    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(addr(&b), c.fingerprint()), b.accept()) }).await;
    let changed = ConnectError::FingerprintChanged {
        expected: c.fingerprint(),
        got: b.fingerprint(),
    };
    let text = changed.to_string();
    assert!(text.contains(&c.fingerprint().to_string()), "{text}");
    assert!(text.contains(&b.fingerprint().to_string()), "{text}");
    assert_eq!(dialed.unwrap_err(), changed);
    assert_eq!(
        accepted.unwrap().unwrap_err().error,
        ConnectError::RefusedByDialer
    );

    // an unknown key answers where a expects c (c regenerated its key)
    let d = endpoint(lo());
    let (dialed, _) =
        within(async { tokio::join!(a.dial(addr(&d), c.fingerprint()), d.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::FingerprintChanged {
            expected: c.fingerprint(),
            got: d.fingerprint()
        }
    );
}

#[tokio::test]
async fn pins_added_and_removed_at_runtime_apply_to_the_next_handshake() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    a.trust().insert(b.fingerprint());

    let (dialed, _) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::NotTrustedByPeer(b.fingerprint())
    );

    b.trust().insert(a.fingerprint()); // `ferry trust` on b
    let (ab, _ba) = connect(&a, &b).await;
    assert_eq!(ab.conn.peer_fp(), b.fingerprint());

    b.trust().remove(&a.fingerprint()); // unpair: the next handshake is refused
    let (dialed, _) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::NotTrustedByPeer(b.fingerprint())
    );

    b.set_trust([a.fingerprint()]); // swap the whole set
    let (ab, _ba) = connect(&a, &b).await;
    assert_eq!(ab.conn.peer_fp(), b.fingerprint());
    b.set_trust([]);
    let (dialed, _) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::NotTrustedByPeer(b.fingerprint())
    );
}

#[tokio::test]
async fn version_mismatch_is_named() {
    // A raw quinn client offering another ALPN, with a valid trusted certificate.
    let b = endpoint(lo());
    let id = Identity::ephemeral().unwrap();
    b.trust().insert(id.fingerprint());
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAny))
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"ferry/2".to_vec()];
    let cc = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let attempt = client.connect_with(cc, addr(&b), "ferry").unwrap();
    let (res, accepted) = within(async { tokio::join!(attempt, b.accept()) }).await;
    assert!(res.is_err());
    assert_eq!(
        accepted.unwrap().unwrap_err().error,
        ConnectError::VersionMismatch
    );
}

/// Test-only verifier for the raw client above (it never reaches app data).
#[derive(Debug)]
struct AcceptAny;
impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![rustls::SignatureScheme::ED25519]
    }
}

fn pattern(len: usize, seed: u8) -> Bytes {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

#[tokio::test]
async fn datagrams_and_frames_round_trip() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (mut ab, mut ba) = connect(&a, &b).await;

    // frames, both directions, in order, including 64 KiB and exactly the 1 MiB cap
    let sizes = [1usize, 28, 4096, 64 * 1024, ferry_net::MAX_FRAME];
    for (i, &n) in sizes.iter().enumerate() {
        ab.conn.try_send_frame(pattern(n, i as u8)).unwrap();
        ba.conn.try_send_frame(pattern(n, 0x80 | i as u8)).unwrap();
    }
    for (i, &n) in sizes.iter().enumerate() {
        assert_eq!(
            next(&mut ba.events).await,
            Event::Frame(pattern(n, i as u8))
        );
        assert_eq!(
            next(&mut ab.events).await,
            Event::Frame(pattern(n, 0x80 | i as u8))
        );
    }

    // many small frames and a few over the 4 KiB read buffer: every one keeps its bytes while
    // later ones are read (A-11, the frames share one buffer); all are read before any is checked
    let sizes: Vec<usize> = (0..600)
        .map(|i| if i % 97 == 0 { 9000 } else { 1 + i % 300 })
        .collect();
    let send = async {
        for (i, &n) in sizes.iter().enumerate() {
            while let Err(FrameError::Full) = ab.conn.try_send_frame(pattern(n, i as u8)) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    };
    let recv = async {
        let mut got = Vec::new();
        for _ in &sizes {
            got.push(next(&mut ba.events).await);
        }
        got
    };
    let ((), got) = tokio::join!(send, recv);
    for (i, (&n, ev)) in sizes.iter().zip(got).enumerate() {
        assert_eq!(ev, Event::Frame(pattern(n, i as u8)), "frame {i}");
    }

    // datagrams, both directions (a 28-byte motion datagram)
    ab.conn.send_datagram(pattern(28, 1)).unwrap();
    assert_eq!(next(&mut ba.events).await, Event::Datagram(pattern(28, 1)));
    ba.conn.send_datagram(pattern(28, 2)).unwrap();
    assert_eq!(next(&mut ab.events).await, Event::Datagram(pattern(28, 2)));

    // local limits: nothing is queued
    assert_eq!(ab.conn.try_send_frame(Bytes::new()), Err(FrameError::Empty));
    assert!(matches!(
        ab.conn.try_send_frame(pattern(ferry_net::MAX_FRAME + 1, 0)),
        Err(FrameError::TooLarge { .. })
    ));
    let max = ab.conn.max_datagram_size().unwrap();
    assert!(matches!(
        ab.conn.send_datagram(pattern(max + 1, 0)),
        Err(DatagramError::TooLarge { .. })
    ));
}

#[tokio::test]
async fn an_over_cap_frame_from_the_peer_closes_the_connection() {
    // a may send up to 2 MiB; b accepts at most 1 MiB
    let big = EndpointConfig {
        max_frame: 2 << 20,
        ..lo()
    };
    let (a, b) = (endpoint(big), endpoint(lo()));
    trust_each_other(&a, &b);
    let (mut ab, mut ba) = connect(&a, &b).await;

    ab.conn.try_send_frame(pattern(16, 0)).unwrap();
    ab.conn
        .try_send_frame(pattern(ferry_net::MAX_FRAME + 1, 0))
        .unwrap();
    assert_eq!(next(&mut ba.events).await, Event::Frame(pattern(16, 0)));
    assert_eq!(
        next(&mut ba.events).await,
        Event::Closed(CloseReason::Local {
            code: CloseCode::FRAME_TOO_LARGE,
            reason: String::new()
        })
    );
    assert_eq!(CloseCode::FRAME_TOO_LARGE.0, 3);
    assert_eq!(within(ba.events.next()).await, None);
    assert_eq!(
        next(&mut ab.events).await,
        Event::Closed(CloseReason::Peer {
            code: CloseCode::FRAME_TOO_LARGE,
            reason: String::new()
        })
    );
}

#[tokio::test]
async fn the_close_code_and_reason_reach_the_other_side() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (mut ab, mut ba) = connect(&a, &b).await;

    ab.conn.close(CloseCode::UNPAIRED, "unpaired by user");
    let Event::Closed(seen) = next(&mut ba.events).await else {
        panic!("expected Closed")
    };
    assert_eq!(seen.code(), Some(CloseCode::UNPAIRED));
    assert_eq!(seen.reason(), "unpaired by user");
    assert!(seen.by_peer());
    let Event::Closed(own) = next(&mut ab.events).await else {
        panic!("expected Closed")
    };
    assert_eq!(own.code(), Some(CloseCode::UNPAIRED));
    assert!(!own.by_peer());
    assert_eq!(ab.conn.close_reason(), Some(own));
    assert_eq!(
        ba.conn.try_send_frame(pattern(4, 0)),
        Err(FrameError::Closed)
    );
}

#[tokio::test]
async fn close_graceful_delivers_every_queued_frame_first() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (ab, mut ba) = connect(&a, &b).await;
    // fill the queue (255 small frames and a keymap-sized one), then close at once
    for i in 0..255u8 {
        ab.conn.try_send_frame(pattern(100, i)).unwrap();
    }
    ab.conn.try_send_frame(pattern(64 * 1024, 0xEE)).unwrap();

    let consumer = async {
        let mut got = Vec::new();
        while let Some(ev) = ba.events.next().await {
            got.push(ev);
        }
        got
    };
    let (answered, got) = within(async {
        tokio::join!(
            ab.conn
                .close_graceful(CloseCode::SHUTDOWN, "bye", Duration::from_secs(2)),
            consumer
        )
    })
    .await;
    assert!(answered, "the peer finished its side in time");
    assert_eq!(got.len(), 257);
    for (i, ev) in got[..255].iter().enumerate() {
        assert_eq!(*ev, Event::Frame(pattern(100, i as u8)));
    }
    assert_eq!(got[255], Event::Frame(pattern(64 * 1024, 0xEE)));
    assert_eq!(
        got[256],
        Event::Closed(CloseReason::Peer {
            code: CloseCode::SHUTDOWN,
            reason: "bye".into()
        })
    );
    assert_eq!(
        ab.conn.try_send_frame(pattern(1, 0)),
        Err(FrameError::Closed)
    );
}

#[tokio::test]
async fn the_frame_queue_holds_256_and_then_reports_full() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (ab, _ba) = connect(&a, &b).await;
    // nothing drains while this test does not yield
    for i in 0..256u32 {
        ab.conn.try_send_frame(pattern(8, i as u8)).unwrap();
    }
    assert_eq!(ab.conn.try_send_frame(pattern(8, 0)), Err(FrameError::Full));
}

#[tokio::test]
async fn the_handle_works_from_a_plain_thread() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (ab, mut ba) = connect(&a, &b).await;
    let conn = ab.conn.clone();
    // like the calloop capture thread: no tokio context at all
    std::thread::spawn(move || {
        conn.try_send_frame(pattern(10, 1)).unwrap();
        conn.send_datagram(pattern(28, 2)).unwrap();
        let _ = (conn.rtt(), conn.peer_fp(), conn.remote_addr());
    })
    .join()
    .unwrap();
    let mut got = vec![next(&mut ba.events).await, next(&mut ba.events).await];
    got.sort_by_key(|e| matches!(e, Event::Datagram(_)));
    assert_eq!(
        got,
        [
            Event::Frame(pattern(10, 1)),
            Event::Datagram(pattern(28, 2))
        ]
    );
}

#[tokio::test]
async fn a_peer_without_datagrams_is_refused() {
    // a raw quinn server with ferry's ALPN and a trusted key, but datagrams off
    let a = endpoint(lo());
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["ferry".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let fp = ferry_net::Fingerprint::of_cert(cert.der()).unwrap();
    a.trust().insert(fp);
    let mut tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    )
    .unwrap();
    tls.alpn_protocols = vec![ferry_net::ALPN.to_vec()];
    let mut sc = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
    ));
    let mut t = quinn::TransportConfig::default();
    t.datagram_receive_buffer_size(None);
    sc.transport_config(std::sync::Arc::new(t));
    let server = quinn::Endpoint::server(sc, "127.0.0.1:0".parse().unwrap()).unwrap();
    let server_addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        while let Some(inc) = server.accept().await {
            if let Ok(conn) = inc.await {
                conn.closed().await;
            }
        }
    });
    let err = within(a.dial(server_addr, fp)).await.unwrap_err();
    assert_eq!(err, ConnectError::DatagramsUnsupported);
}

#[tokio::test]
async fn simultaneous_dials_converge_on_the_connection_dialled_by_the_lower_fingerprint() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (fa, fb) = (a.fingerprint(), b.fingerprint());
    let (addr_a, addr_b) = (addr(&a), addr(&b));
    let (mut ma, mut mb) = (Manager::new(a), Manager::new(b));

    // Both dial at once. Neither manager is polled until both connections are open on both
    // endpoints, so each manager sees two connections and must pick one. The handshakes finish on
    // the endpoints' own tasks: wait for that state, not for a time.
    ma.dial(fb, addr_b);
    mb.dial(fa, addr_a);
    within(async {
        while ma.endpoint().open_connections() != 2 || mb.endpoint().open_connections() != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    // Drive both managers until the loser is gone on both endpoints and each manager reports the
    // survivor. The close of the loser arrives whenever the host schedules it, so the deadline is
    // generous and a quiet spell proves nothing.
    let mut connected_a = Vec::new();
    let mut connected_b = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let settled = ma.endpoint().open_connections() == 1
                && mb.endpoint().open_connections() == 1
                && ma
                    .connection(&fb)
                    .is_some_and(|c| c.close_reason().is_none())
                && mb.connection(&fa).is_some_and(|live_b| {
                    live_b.close_reason().is_none()
                        && connected_b.iter().any(|nc: &NewConnection| nc.conn.id() == live_b.id())
                });
            if settled {
                break;
            }
            tokio::select! {
                Some(ev) = ma.next() => if let ManagerEvent::Connected(nc) = ev { connected_a.push(nc) },
                Some(ev) = mb.next() => if let ManagerEvent::Connected(nc) = ev { connected_b.push(nc) },
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    })
    .await
    .expect("the duplicate was not closed on both endpoints");

    let lower = fa.min(fb);
    let dialer = |me, nc: &NewConnection| {
        if nc.conn.is_dialer() {
            me
        } else {
            nc.conn.peer_fp()
        }
    };
    let live_a = ma.connection(&fb).expect("a has a connection").clone();
    let live_b = mb.connection(&fa).expect("b has a connection").clone();
    assert!(live_a.close_reason().is_none() && live_b.close_reason().is_none());
    assert_eq!(live_a.is_dialer(), fa == lower);
    assert_eq!(live_b.is_dialer(), fb == lower);
    // the loser is gone on both sides; any reported loser was dialled by the higher one
    assert_eq!(ma.endpoint().open_connections(), 1);
    assert_eq!(mb.endpoint().open_connections(), 1);
    for nc in connected_a
        .iter_mut()
        .filter(|nc| nc.conn.id() != live_a.id())
    {
        assert_eq!(dialer(fa, nc), fa.max(fb));
        assert!(matches!(
            next(&mut nc.events).await,
            Event::Closed(
                CloseReason::Local {
                    code: CloseCode::DUPLICATE,
                    ..
                } | CloseReason::Peer {
                    code: CloseCode::DUPLICATE,
                    ..
                }
            )
        ));
    }

    // the survivor carries traffic end to end
    let mut events_b = connected_b
        .into_iter()
        .find(|nc| nc.conn.id() == live_b.id())
        .unwrap()
        .events;
    live_a.try_send_frame(Bytes::from_static(b"hello")).unwrap();
    loop {
        tokio::select! {
            ev = within(events_b.next()) => {
                assert_eq!(ev, Some(Event::Frame(Bytes::from_static(b"hello"))));
                break;
            }
            _ = ma.next() => {}
            _ = mb.next() => {}
        }
    }
}

#[tokio::test]
async fn manager_reports_a_refused_dial_once_and_redials_until_the_peer_trusts_it() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    a.trust().insert(b.fingerprint());
    let (fa, fb) = (a.fingerprint(), b.fingerprint());
    let addr_b = addr(&b);
    let mut ma = Manager::new(a);
    let mut mb = Manager::new(b);
    ma.dial(fb, addr_b);

    // b's refusals count a's attempts; a reports only the first failure
    let (mut reported, mut refusals) = (Vec::new(), 0);
    within(async {
        loop {
            tokio::select! {
                Some(ev) = ma.next() => match ev {
                    ManagerEvent::DialFailed { error, attempts, retry_in, .. } => {
                        reported.push((error, attempts, retry_in));
                    }
                    ManagerEvent::Connected(nc) => {
                        assert_eq!(nc.conn.peer_fp(), fb);
                        break;
                    }
                    ManagerEvent::InboundFailed(e) => panic!("{e}"),
                },
                Some(ev) = mb.next() => if let ManagerEvent::InboundFailed(e) = ev {
                    assert_eq!(e.error, ConnectError::UnknownPeer(fa));
                    refusals += 1;
                    if refusals == 2 {
                        // the refused backoff now waits 2 s; a trust change dials at once
                        mb.endpoint().trust().insert(fa);
                        ma.retry_now(&fb);
                    }
                },
            }
        }
    })
    .await;
    assert_eq!(
        reported,
        [(ConnectError::NotTrustedByPeer(fb), 1, REFUSED_BACKOFF_MIN)]
    );
    assert_eq!(refusals, 2);
}

/// The next `DialFailed` from `m`, as (error, attempts, retry_in); any other event fails.
async fn next_dial_failure(m: &mut Manager) -> (ConnectError, u32, Duration) {
    match m.next().await.expect("the manager ended") {
        ManagerEvent::DialFailed {
            error,
            attempts,
            retry_in,
            ..
        } => (error, attempts, retry_in),
        other => panic!("unexpected {other:?}"),
    }
}

/// A dial attempt's handshake timeout, short enough that quinn sends no second Initial: quinn's
/// own 333 ms initial RTT, since the default 50 ms would resend inside the 300 ms.
fn short_handshake() -> EndpointConfig {
    EndpointConfig {
        handshake_timeout: Duration::from_millis(300),
        initial_rtt: Duration::from_millis(333),
        ..lo()
    }
}

#[tokio::test]
async fn a_failing_dial_is_reported_again_when_its_error_changes_and_after_a_connection() {
    // a port where nothing answers, until b binds it
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = silent.local_addr().unwrap();
    let id_b = Identity::ephemeral().unwrap();
    let fb = id_b.fingerprint();
    let a = endpoint(short_handshake());
    a.trust().insert(fb);
    let fa = a.fingerprint();
    let mut ma = Manager::new(a);
    ma.dial(fb, addr_b);

    // 1. nothing answers: reported once, and retried at once, since the attempt took the
    //    whole handshake timeout
    assert_eq!(
        within(next_dial_failure(&mut ma)).await,
        (ConnectError::TimedOut, 1, Duration::ZERO)
    );

    // 2. b comes up there and does not trust a: a new error, so a new report. The count goes
    //    on, and timeouts did not grow the backoff.
    drop(silent);
    let b = Endpoint::bind(
        id_b,
        TrustSet::new(),
        EndpointConfig {
            bind: addr_b,
            ..lo()
        },
    )
    .unwrap();
    let mut mb = Manager::new(b);
    let (error, attempts, retry_in) = within(next_dial_failure(&mut ma)).await;
    assert_eq!(error, ConnectError::NotTrustedByPeer(fb));
    assert!(attempts >= 2, "{attempts}");
    assert_eq!(retry_in, REFUSED_BACKOFF_MIN);

    // 3. b trusts a: connected
    mb.endpoint().trust().insert(fa);
    let (mut up_a, mut at_b) = (false, None);
    within(async {
        while !up_a || at_b.is_none() {
            tokio::select! {
                Some(ev) = ma.next() => match ev {
                    ManagerEvent::Connected(_) => up_a = true,
                    other => panic!("unexpected {other:?}"),
                },
                Some(ev) = mb.next() => if let ManagerEvent::Connected(nc) = ev {
                    at_b = Some(nc);
                },
            }
        }
    })
    .await;

    // 4. b unpairs a and closes: the first failure after a connection is reported again
    mb.endpoint().set_trust([]);
    at_b.unwrap().conn.close(CloseCode::UNPAIRED, "");
    assert_eq!(
        within(next_dial_failure(&mut ma)).await,
        (ConnectError::NotTrustedByPeer(fb), 1, REFUSED_BACKOFF_MIN)
    );
}

#[tokio::test]
async fn a_dial_that_times_out_as_the_peer_restarts_is_abandoned_not_refused_and_redialled_at_once()
{
    // The netbook case of 2026-10-16: a dial is in flight while the peer is down; the peer comes
    // back; the dial's timeout fires, and its close is the first packet the new endpoint sees.
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = silent.local_addr().unwrap();
    let id_b = Identity::ephemeral().unwrap();
    let fb = id_b.fingerprint();
    let a = endpoint(short_handshake());
    a.trust().insert(fb);
    let (fa, addr_a) = (a.fingerprint(), addr(&a));
    let mut ma = Manager::new(a);
    ma.dial(fb, addr_b);

    // the first attempt's Initial reaches the dead port; then the peer restarts there
    let mut buf = [0u8; 2048];
    tokio::select! {
        r = within(silent.recv(&mut buf)) => { r.unwrap(); }
        ev = ma.next() => panic!("before the Initial: {ev:?}"),
    }
    drop(silent);
    let b = Endpoint::bind(
        id_b,
        [fa].into_iter().collect(),
        EndpointConfig {
            bind: addr_b,
            ..lo()
        },
    )
    .unwrap();
    let mut mb = Manager::new(b);

    // a: one timeout, retried at once, then connected. b: the abandoned attempt, then the
    // connection. Nothing reads as a refusal.
    let (mut seen_a, mut seen_b) = (Vec::new(), Vec::new());
    within(async {
        while !(seen_a.contains(&"connected") && seen_b.contains(&"connected")) {
            tokio::select! {
                Some(ev) = ma.next() => seen_a.push(match ev {
                    ManagerEvent::DialFailed { error: ConnectError::TimedOut, attempts: 1, retry_in, .. } => {
                        assert_eq!(retry_in, Duration::ZERO);
                        "timed out"
                    }
                    ManagerEvent::Connected(_) => "connected",
                    other => panic!("a: {other:?}"),
                }),
                Some(ev) = mb.next() => seen_b.push(match ev {
                    ManagerEvent::InboundFailed(e) => {
                        assert_eq!(e.error, ConnectError::Abandoned, "{e}");
                        assert_eq!(e.remote, addr_a);
                        "abandoned"
                    }
                    ManagerEvent::Connected(_) => "connected",
                    other => panic!("b: {other:?}"),
                }),
            }
        }
    })
    .await;
    assert_eq!(seen_a, ["timed out", "connected"]);
    assert_eq!(seen_b, ["abandoned", "connected"]);
}

#[tokio::test]
async fn manager_redials_after_the_connection_drops() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let fb = b.fingerprint();
    let addr_b = addr(&b);
    let (mut ma, mut mb) = (Manager::new(a), Manager::new(b));
    ma.dial(fb, addr_b);

    let mut seen_by_a = Vec::new();
    let mut dropped = false;
    within(async {
        loop {
            tokio::select! {
                Some(ev) = ma.next() => if let ManagerEvent::Connected(nc) = ev {
                    seen_by_a.push(nc.conn.id());
                    if seen_by_a.len() == 2 { break; }
                },
                Some(ev) = mb.next() => if let ManagerEvent::Connected(nc) = ev && !dropped {
                    // b drops the first connection, as after a restart
                    dropped = true;
                    nc.conn.close(CloseCode::SHUTDOWN, "restart");
                },
            }
        }
    })
    .await;
    assert_ne!(seen_by_a[0], seen_by_a[1]);
}

/// Brings a and b up under managers: a dials b, both managers are polled until both report it.
async fn managed_pair(
    ma: &mut Manager,
    mb: &mut Manager,
    fb: ferry_net::Fingerprint,
    addr_b: SocketAddr,
) -> (NewConnection, NewConnection) {
    ma.dial(fb, addr_b);
    let (mut up_a, mut up_b) = (None, None);
    within(async {
        while up_a.is_none() || up_b.is_none() {
            tokio::select! {
                Some(ev) = ma.next() => if let ManagerEvent::Connected(nc) = ev { up_a = Some(nc) },
                Some(ev) = mb.next() => if let ManagerEvent::Connected(nc) = ev { up_b = Some(nc) },
            }
        }
    })
    .await;
    (up_a.unwrap(), up_b.unwrap())
}

#[tokio::test]
async fn forget_closes_with_shutdown_and_forget_with_closes_with_the_given_code() {
    for (code, via_with) in [
        (CloseCode::SHUTDOWN, false),
        (CloseCode::UNPAIRED, true),
        (CloseCode(9), true),
    ] {
        let (a, b) = (endpoint(lo()), endpoint(lo()));
        trust_each_other(&a, &b);
        let (fb, addr_b) = (b.fingerprint(), addr(&b));
        let (mut ma, mut mb) = (Manager::new(a), Manager::new(b));
        let (mut at_a, mut at_b) = managed_pair(&mut ma, &mut mb, fb, addr_b).await;

        if via_with {
            ma.forget_with(&fb, code);
        } else {
            ma.forget(&fb);
        }
        assert!(ma.connection(&fb).is_none(), "forgotten at once");

        // the peer sees the code, the reason it was sent with, and that the peer closed it
        let Event::Closed(reason) = within(async {
            loop {
                match at_b.events.next().await {
                    Some(Event::Closed(r)) => break Event::Closed(r),
                    Some(_) => {}
                    None => panic!("events ended without Closed"),
                }
            }
        })
        .await
        else {
            unreachable!()
        };
        assert_eq!(reason.code(), Some(code), "{reason}");
        assert!(reason.by_peer());
        // and this side's own events say it closed locally with the same code
        let Event::Closed(local) = next(&mut at_a.events).await else {
            panic!("expected Closed")
        };
        assert_eq!(local.code(), Some(code), "{local}");
        assert!(!local.by_peer());
    }
}

#[tokio::test]
async fn forget_stops_the_dialler() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (fb, addr_b) = (b.fingerprint(), addr(&b));
    let (mut ma, mut mb) = (Manager::new(a), Manager::new(b));
    let (at_a, _at_b) = managed_pair(&mut ma, &mut mb, fb, addr_b).await;
    ma.forget_with(&fb, CloseCode::UNPAIRED);
    // no redial follows: nothing new connects while both managers keep running
    let again = tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            tokio::select! {
                Some(ev) = ma.next() => if let ManagerEvent::Connected(nc) = ev { return nc.conn.id() },
                Some(_) = mb.next() => {}
            }
        }
    })
    .await;
    assert!(again.is_err(), "forgotten peer was dialled again");
    assert!(at_a.conn.close_reason().is_some());
}

/// What a dial from `client` to `server` ended as: `true` if the server ran the TLS handshake
/// (it refused the unpinned client with `access_denied`), `false` if it never got that far.
async fn reached_tls(client: &Endpoint, server: &Endpoint) -> bool {
    matches!(
        within(client.dial(addr(server), server.fingerprint())).await,
        Err(ConnectError::NotTrustedByPeer(_))
    )
}

#[tokio::test]
async fn a_flood_from_one_address_is_refused_before_tls_and_another_address_is_unaffected() {
    use ferry_net::{HANDSHAKE_BURST, HANDSHAKE_REFILL_PER_SEC};
    let server = endpoint(lo()); // pins nobody: every handshake that starts ends in a refusal
    let from = |ip: &str| {
        let c = endpoint(EndpointConfig {
            bind: format!("{ip}:0").parse().unwrap(),
            ..lo()
        });
        c.trust().insert(server.fingerprint());
        c
    };
    let (flooder, bystander) = (from("127.0.0.1"), from("127.0.0.2"));

    let attempts = 3 * HANDSHAKE_BURST as usize;
    let started = std::time::Instant::now();
    let dials: Vec<_> = (0..attempts)
        .map(|_| {
            let (c, s) = (flooder.clone(), server.clone());
            tokio::spawn(async move { reached_tls(&c, &s).await })
        })
        .collect();
    let mut reached = 0;
    for d in dials {
        reached += usize::from(d.await.unwrap());
    }
    // Debug-build handshakes run on this one thread and delay the accept loop, so tokens may
    // refill meanwhile. A bucket can never hand out more than the burst plus what the elapsed
    // time earned, and the flood is well past the burst.
    let earned = (started.elapsed().as_secs_f64() * f64::from(HANDSHAKE_REFILL_PER_SEC)) as usize;
    let burst = HANDSHAKE_BURST as usize;
    assert!(
        (burst..=burst + earned).contains(&reached),
        "{reached} of {attempts} reached TLS; burst {burst}, earned {earned}"
    );
    // a client that has not heard back retransmits its Initial, and each copy is counted
    assert!(server.handshakes_refused() >= (attempts - reached) as u64);
    assert!(reached < attempts);

    // the refused attempts cost the server no handshake: only the admitted ones reach accept()
    for _ in 0..reached {
        let refused = within(server.accept()).await.unwrap().unwrap_err();
        assert!(matches!(refused.error, ConnectError::UnknownPeer(_)));
    }
    no_incoming(&server).await;

    // another source address has its own bucket
    let before = server.handshakes_refused();
    assert!(reached_tls(&bystander, &server).await);
    assert_eq!(
        server.handshakes_refused(),
        before,
        "nothing of the bystander refused"
    );

    // and the flooder earns tokens back: 600 ms at 2 a second is more than one
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(reached_tls(&flooder, &server).await);
}

/// Two trusting endpoints, the first with the lower fingerprint.
fn ordered_pair() -> (Endpoint, Endpoint) {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    if a.fingerprint() < b.fingerprint() {
        (a, b)
    } else {
        (b, a)
    }
}

/// The hold here outlasts any dial, so the own dial ends it and not the clock: with the default
/// 300 ms, a host loaded enough to slow the own handshake past it makes the lower side report the
/// held connection and then its replacement, as [`ManagerEvent::Connected`] documents.
#[tokio::test]
async fn a_simultaneous_dial_reports_the_surviving_connection_once() {
    for round in 0..6 {
        let (a, b) = ordered_pair(); // a is the lower fingerprint: its dial wins
        let (fa, fb) = (a.fingerprint(), b.fingerprint());
        let (addr_a, addr_b) = (addr(&a), addr(&b));
        let (mut ma, mut mb) = (Manager::with_hold(a, T), Manager::new(b));
        ma.dial(fb, addr_b);
        mb.dial(fa, addr_a);

        // Polled continuously, as ferryd does, until both sides keep the connection a dialled
        // and the other one has closed on both endpoints. The close needs no event here, so
        // the endpoints are looked at again every 10 ms.
        let (mut at_a, mut at_b) = (Vec::new(), Vec::new());
        within(async {
            loop {
                let settled = !at_a.is_empty()
                    && at_b
                        .last()
                        .is_some_and(|nc: &NewConnection| !nc.conn.is_dialer())
                    && ma.endpoint().open_connections() == 1
                    && mb.endpoint().open_connections() == 1;
                if settled {
                    break;
                }
                tokio::select! {
                    Some(ev) = ma.next() => match ev {
                        ManagerEvent::Connected(nc) => at_a.push(nc),
                        other => panic!("round {round}: a got {other:?}"),
                    },
                    Some(ev) = mb.next() => match ev {
                        ManagerEvent::Connected(nc) => at_b.push(nc),
                        other => panic!("round {round}: b got {other:?}"),
                    },
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
        .await;

        // the lower side held back the loser that reached it first: one report, the survivor
        let dialers: Vec<_> = at_a.iter().map(|nc| nc.conn.is_dialer()).collect();
        assert_eq!(
            at_a.len(),
            1,
            "round {round}: a reported {} connections, is_dialer {dialers:?}",
            at_a.len()
        );
        assert!(
            at_a[0].conn.is_dialer(),
            "the connection a dialled survives"
        );
        assert_eq!(
            ma.connection(&fb).map(Connection::id),
            Some(at_a[0].conn.id())
        );
        // the higher side's last report is the same connection, from the other end
        let last_b = at_b.last().expect("b reported");
        assert!(!last_b.conn.is_dialer());
        assert_eq!(
            mb.connection(&fa).map(Connection::id),
            Some(last_b.conn.id())
        );
        assert!(at_a[0].conn.close_reason().is_none() && last_b.conn.close_reason().is_none());
        assert_eq!(ma.endpoint().open_connections(), 1);
        assert_eq!(mb.endpoint().open_connections(), 1);

        // the survivor carries traffic, and nothing was closed under a reported connection
        at_a[0]
            .conn
            .try_send_frame(Bytes::from_static(b"hello"))
            .unwrap();
        let mut events = at_b.pop().unwrap().events;
        loop {
            tokio::select! {
                ev = within(events.next()) => {
                    assert_eq!(ev, Some(Event::Frame(Bytes::from_static(b"hello"))));
                    break;
                }
                ev = ma.next() => assert!(
                    !matches!(ev, Some(ManagerEvent::Connected(_))),
                    "round {round}: a reported a second connection"
                ),
                _ = mb.next() => {}
            }
        }
    }
}

#[tokio::test]
async fn an_inbound_connection_held_for_a_slow_own_dial_is_reported_after_the_hold() {
    let (a, b) = ordered_pair(); // a is lower: it holds b's inbound connection while it dials
    let (fa, fb) = (a.fingerprint(), b.fingerprint());
    let addr_a = addr(&a);
    // a dials an address where nothing answers, so its own attempt stays in flight
    let black_hole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let (mut ma, mut mb) = (Manager::new(a), Manager::new(b));
    ma.dial(fb, black_hole.local_addr().unwrap());
    mb.dial(fa, addr_a);

    let started = tokio::time::Instant::now();
    let mut reported = Vec::new();
    within(async {
        loop {
            tokio::select! {
                Some(ev) = ma.next() => match ev {
                    ManagerEvent::Connected(nc) => reported.push((started.elapsed(), nc)),
                    other => panic!("a got {other:?}"),
                },
                Some(_) = mb.next() => {}
                () = tokio::time::sleep(Duration::from_millis(900)) => break,
            }
        }
    })
    .await;
    assert_eq!(reported.len(), 1, "reported once");
    let (after, nc) = &reported[0];
    assert!(
        *after >= Duration::from_millis(250),
        "held back while the own dial was in flight: {after:?}"
    );
    assert!(!nc.conn.is_dialer());
    assert_eq!(ma.connection(&fb).map(Connection::id), Some(nc.conn.id()));
}

// --- pairing -----------------------------------------------

fn window() -> ferry_net::PairingWindow {
    ferry_net::PairingWindow {
        until: std::time::Instant::now() + Duration::from_secs(120),
    }
}

/// a dials b on ferry-pair/1 and b accepts it; returns (a's side, b's side).
async fn pair_connect(
    a: &Endpoint,
    b: &Endpoint,
) -> (ferry_net::PairingConnection, ferry_net::PairingConnection) {
    let (dialed, accepted) =
        within(async { tokio::join!(a.dial_pairing(addr(b)), b.accept_pairing()) }).await;
    (dialed.unwrap(), accepted.unwrap())
}

#[tokio::test]
async fn pairing_alpn_refused_when_closed() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    // no window: not accepting, refused in the handshake, nothing reaches either accept
    let started = std::time::Instant::now();
    assert_eq!(
        within(a.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::NotAccepting
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(b.pairing_refused(), 1);
    // the refused attempt is an inbound failure on b, never a pairing connection
    let refused = within(b.accept()).await.unwrap().unwrap_err();
    assert_eq!(refused.error, ConnectError::PairingRefused);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), b.accept_pairing())
            .await
            .is_err()
    );

    // a window that has ended is closed too
    b.set_pairing(Some(ferry_net::PairingWindow {
        until: std::time::Instant::now(),
    }));
    assert_eq!(
        within(a.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::NotAccepting
    );
    // open: it works, and closing the window again refuses the next one
    b.set_pairing(Some(window()));
    let (ab, ba) = pair_connect(&a, &b).await;
    drop((ab, ba));
    b.set_pairing(None);
    assert_eq!(
        within(a.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::NotAccepting
    );
}

#[tokio::test]
async fn unknown_key_accepted_only_on_pair_alpn() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    b.set_pairing(Some(window()));
    // a knows b, b does not know a: ferry/1 is still refused in the handshake, window or not
    a.trust().insert(b.fingerprint());
    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(addr(&b), b.fingerprint()), b.accept()) }).await;
    assert_eq!(
        dialed.unwrap_err(),
        ConnectError::NotTrustedByPeer(b.fingerprint())
    );
    assert_eq!(
        accepted.unwrap().unwrap_err().error,
        ConnectError::UnknownPeer(a.fingerprint())
    );
    // and a key nobody pinned dials ferry-pair/1 fine: each side records the other's key
    a.trust().remove(&b.fingerprint());
    let (mut ab, mut ba) = pair_connect(&a, &b).await;
    assert_eq!(ab.peer_fp(), b.fingerprint());
    assert_eq!(ba.peer_fp(), a.fingerprint());
    assert_eq!(ba.remote, addr(&a), "the dialer's bound port: dialable");
    assert!(!a.trust().contains(&b.fingerprint()) && !b.trust().contains(&a.fingerprint()));
    // frames flow both ways, in order
    ab.conn
        .try_send_frame(Bytes::from_static(b"commit"))
        .unwrap();
    assert_eq!(
        next(&mut ba.events).await,
        Event::Frame(Bytes::from_static(b"commit"))
    );
    ba.conn
        .try_send_frame(Bytes::from_static(b"nonce"))
        .unwrap();
    assert_eq!(
        next(&mut ab.events).await,
        Event::Frame(Bytes::from_static(b"nonce"))
    );
    // the pairing connection never reached b's session accept
    no_incoming(&b).await;
    // a graceful close is acknowledged, as the final round trip needs
    let (ok_a, ok_b) = tokio::join!(
        ab.conn.close_graceful(CloseCode::SHUTDOWN, "paired", T),
        ba.conn.close_graceful(CloseCode::SHUTDOWN, "paired", T)
    );
    assert!(ok_a && ok_b);
}

#[tokio::test]
async fn one_pairing_attempt_at_a_time() {
    let (a, b, c) = (endpoint(lo()), endpoint(lo()), endpoint(lo()));
    b.set_pairing(Some(window()));
    let (ab, ba) = pair_connect(&a, &b).await;
    // while a's attempt runs, another dialer is not accepted
    assert_eq!(
        within(c.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::NotAccepting
    );
    // the attempt ends when b drops it; then c gets in
    drop(ba);
    drop(ab);
    let (cb, bc) = pair_connect(&c, &b).await;
    assert_eq!(bc.peer_fp(), c.fingerprint());
    drop((cb, bc));
}

/// Any LAN host reaches a pairing connection, so its frames are capped far below a session's
/// (A-05): a 1 MiB `Hello` would decode into tens of MiB before ferry-core's caps refuse it.
#[tokio::test]
async fn a_pairing_frame_over_4_kib_closes_the_connection() {
    // a may send up to 2 MiB on ferry-pair/1; b accepts the default
    let big = EndpointConfig {
        pairing_max_frame: 2 << 20,
        ..lo()
    };
    let (a, b) = (endpoint(big), endpoint(lo()));
    assert_eq!(ferry_net::MAX_PAIRING_FRAME, 4096);
    b.set_pairing(Some(window()));
    let (mut ab, mut ba) = pair_connect(&a, &b).await;
    // b's own sends are capped the same way
    assert!(
        ba.conn
            .try_send_frame(pattern(ferry_net::MAX_PAIRING_FRAME + 1, 0))
            .is_err()
    );
    ab.conn
        .try_send_frame(pattern(ferry_net::MAX_PAIRING_FRAME, 0))
        .unwrap();
    ab.conn.try_send_frame(pattern(1 << 20, 0)).unwrap();
    assert_eq!(
        next(&mut ba.events).await,
        Event::Frame(pattern(ferry_net::MAX_PAIRING_FRAME, 0))
    );
    assert_eq!(
        next(&mut ba.events).await,
        Event::Closed(CloseReason::Local {
            code: CloseCode::FRAME_TOO_LARGE,
            reason: String::new()
        })
    );
    assert_eq!(
        next(&mut ab.events).await,
        Event::Closed(CloseReason::Peer {
            code: CloseCode::FRAME_TOO_LARGE,
            reason: String::new()
        })
    );
}

#[tokio::test]
async fn two_alpns_or_own_key_refused() {
    // Both ALPNs offered at once: b's resolver refuses, whatever the window says.
    let b = endpoint(lo());
    b.set_pairing(Some(window()));
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAny))
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"ferry/1".to_vec(), b"ferry-pair/1".to_vec()];
    let cc = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let attempt = client.connect_with(cc, addr(&b), "ferry").unwrap();
    let (res, accepted) = within(async { tokio::join!(attempt, b.accept()) }).await;
    assert!(res.is_err(), "two ALPNs must fail the handshake");
    assert!(accepted.unwrap().is_err());

    // The acceptor's own key, presented by a dialer: refused on both sides of the rule.
    let (_scratch, dir) = scratch_dir();
    let id = Identity::load_or_create(&dir).unwrap();
    let b = Endpoint::bind(id, TrustSet::new(), lo()).unwrap();
    b.set_pairing(Some(window()));
    // a dialer with the same identity refuses b's (its own) key in its verifier
    let twin = Endpoint::bind(
        Identity::load_or_create(&dir).unwrap(),
        TrustSet::new(),
        lo(),
    )
    .unwrap();
    assert_eq!(
        within(twin.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::OwnKey
    );
    assert_eq!(
        within(b.accept()).await.unwrap().unwrap_err().error,
        ConnectError::RefusedByDialer
    );
    // and b refuses a raw client presenting b's own certificate
    let cert =
        rustls::pki_types::CertificateDer::from(std::fs::read(dir.join("cert.der")).unwrap());
    let key =
        rustls::pki_types::PrivateKeyDer::Pkcs8(std::fs::read(dir.join("key.der")).unwrap().into());
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAny))
    .with_client_auth_cert(vec![cert], key)
    .unwrap();
    tls.alpn_protocols = vec![b"ferry-pair/1".to_vec()];
    let cc = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let attempt = client.connect_with(cc, addr(&b), "ferry").unwrap();
    let (_res, accepted) = within(async { tokio::join!(attempt, b.accept()) }).await;
    assert_eq!(accepted.unwrap().unwrap_err().error, ConnectError::OwnKey);
}

#[tokio::test]
async fn pairing_limit_per_address() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    b.set_pairing(Some(window()));
    // three attempts within the minute: each one runs, then ends
    for _ in 0..ferry_net::PAIRING_BURST {
        let (ab, ba) = pair_connect(&a, &b).await;
        drop((ab, ba));
    }
    // the fourth from the same address is not accepted, and counted
    let before = b.pairing_refused();
    assert_eq!(
        within(a.dial_pairing(addr(&b))).await.unwrap_err(),
        ConnectError::NotAccepting
    );
    assert_eq!(b.pairing_refused(), before + 1);
    assert_eq!(
        within(b.accept()).await.unwrap().unwrap_err().error,
        ConnectError::PairingRefused
    );
    // ferry/1 from that address is unaffected
    trust_each_other(&a, &b);
    connect(&a, &b).await;
}

/// A private scratch directory, removed when the returned guard drops (a failed assertion
/// included), and the `id` directory inside it for `Identity::load_or_create`.
fn scratch_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ferry-net-t-")
        .tempdir()
        .unwrap();
    let dir = tmp.path().join("id");
    (tmp, dir)
}

// --- stateless reset ("Restart seen late") ----------------------------------------------

/// A UDP relay for one client: what the client sends goes to the current target, and the
/// target's answers go back. With no target, everything is dropped, as if the target's host
/// had vanished: a killed peer sends no close.
struct Relay {
    addr: SocketAddr,
    target: std::sync::Arc<std::sync::Mutex<Option<SocketAddr>>>,
}

impl Relay {
    async fn start(target: SocketAddr) -> Self {
        let front = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let back = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = front.local_addr().unwrap();
        let target = std::sync::Arc::new(std::sync::Mutex::new(Some(target)));
        let t = target.clone();
        tokio::spawn(async move {
            let (mut fb, mut bb) = ([0u8; 2048], [0u8; 2048]);
            let mut client = None;
            loop {
                tokio::select! {
                    Ok((n, from)) = front.recv_from(&mut fb) => {
                        client = Some(from);
                        let to = *t.lock().unwrap();
                        if let Some(to) = to {
                            let _ = back.send_to(&fb[..n], to).await;
                        }
                    }
                    Ok((n, from)) = back.recv_from(&mut bb) => {
                        let current = *t.lock().unwrap();
                        if let (Some(c), Some(to)) = (client, current)
                            && from == to
                        {
                            let _ = front.send_to(&bb[..n], c).await;
                        }
                    }
                }
            }
        });
        Self { addr, target }
    }

    fn set(&self, target: Option<SocketAddr>) {
        *self.target.lock().unwrap() = target;
    }
}

/// b restarts behind the relay with `next` as its identity; a pokes the old connection with a
/// datagram every 50 ms. Returns how long until a's connection ended, and why, or `None` if it
/// was still open after `wait`.
async fn restart_and_poke(same_identity: bool, wait: Duration) -> Option<(Duration, CloseReason)> {
    let (_scratch, dir) = scratch_dir();
    let b = Endpoint::bind(
        Identity::load_or_create(&dir).unwrap(),
        TrustSet::new(),
        lo(),
    )
    .unwrap();
    let a = endpoint(lo());
    let fb = b.fingerprint();
    a.trust().insert(fb);
    b.trust().insert(a.fingerprint());
    let relay = Relay::start(addr(&b)).await;
    let (dialed, accepted) =
        within(async { tokio::join!(a.dial(relay.addr, fb), b.accept()) }).await;
    let (mut ab, _ba) = (dialed.unwrap(), accepted.unwrap().unwrap());

    // b "crashes": the relay drops everything, so b's close never reaches a
    relay.set(None);
    drop(_ba);
    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let next_id = if same_identity {
        Identity::load_or_create(&dir).unwrap()
    } else {
        Identity::ephemeral().unwrap()
    };
    let b2 = Endpoint::bind(next_id, TrustSet::new(), lo()).unwrap();
    relay.set(Some(addr(&b2)));

    let started = tokio::time::Instant::now();
    let poke = Bytes::from_static(&[1u8; 28]);
    let outcome = tokio::time::timeout(wait, async {
        loop {
            let _ = ab.conn.send_datagram(poke.clone());
            tokio::select! {
                ev = ab.events.next() => match ev {
                    Some(Event::Closed(reason)) => return (started.elapsed(), reason),
                    Some(_) => {}
                    None => panic!("events ended without a close"),
                },
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    })
    .await
    .ok();
    drop(b2);
    outcome
}

#[tokio::test]
async fn a_restarted_peer_with_the_same_identity_resets_the_old_connection_at_once() {
    // the reset key comes from the identity: the first packet after the restart ends it
    let (after, reason) = restart_and_poke(true, Duration::from_secs(3))
        .await
        .expect("a's connection must end long before its 10 s idle timeout");
    assert_eq!(reason, CloseReason::Reset);
    eprintln!("a's old connection ended {after:?} after the first packet to the new b");
    assert!(after < Duration::from_millis(500), "{after:?}");

    // control: another key's resets do not match a's tokens and are ignored
    assert_eq!(
        restart_and_poke(false, Duration::from_millis(1500)).await,
        None,
        "a reset from another identity must not end the connection"
    );
}

/// SAS pairing's last round trip: one side finishes first; the other
/// reads that side's last frame, then closes gracefully too, at once. Both must learn that the
/// other got everything: the first one's close waits for the second one's finish, which the
/// second one's close must deliver before it closes.
#[tokio::test]
async fn both_sides_close_gracefully_whoever_finishes_first() {
    for round in 0..20 {
        // fresh endpoints: the handshake gate allows a burst of 8 from one address
        let (a, b) = (endpoint(lo()), endpoint(lo()));
        trust_each_other(&a, &b);
        let (mut ab, ba) = connect(&a, &b).await;
        let last = Bytes::from_static(b"confirm");
        ba.conn.try_send_frame(last.clone()).unwrap();
        let first = tokio::spawn({
            let conn = ba.conn.clone();
            async move { conn.close_graceful(CloseCode::SHUTDOWN, "paired", T).await }
        });
        // a reads b's last frame, then closes as soon as it can
        assert_eq!(next(&mut ab.events).await, Event::Frame(last));
        let second = ab
            .conn
            .close_graceful(CloseCode::SHUTDOWN, "paired", T)
            .await;
        assert!(second, "round {round}: a");
        assert!(
            first.await.unwrap(),
            "round {round}: b never got a's finish"
        );
        drop(ba);
    }
}

#[tokio::test]
async fn data_streams_carry_bytes_both_ways_beside_the_frames() {
    let (a, b) = (endpoint(lo()), endpoint(lo()));
    trust_each_other(&a, &b);
    let (mut ab, ba) = connect(&a, &b).await;

    // 3 MiB from the acceptor (it opens streams too), a frame queued behind its first bytes
    let big = pattern(3 << 20, 7);
    let mut s = within(ba.conn.open_data()).await.unwrap();
    s.write_all(b"head").await.unwrap();
    ba.conn
        .try_send_frame(Bytes::from_static(b"frame"))
        .unwrap();
    let sender = {
        let big = big.clone();
        tokio::spawn(async move {
            s.write_chunk(big).await.unwrap();
            s.finish();
        })
    };
    let mut r = within(ab.conn.accept_data()).await.unwrap();
    assert_eq!(
        next(&mut ab.events).await,
        Event::Frame(Bytes::from_static(b"frame"))
    );
    let mut head = [0u8; 4];
    within(r.read_exact(&mut head)).await.unwrap();
    assert_eq!(&head, b"head");
    let mut got = Vec::new();
    while let Some(c) = within(r.read_chunk(64 << 10)).await.unwrap() {
        got.extend_from_slice(&c);
    }
    assert_eq!(got.len(), big.len());
    assert!(got == big[..]);
    sender.await.unwrap();

    // the dialer's way, and a reset the reader sees as one
    let mut s = within(ab.conn.open_data()).await.unwrap();
    s.write_all(b"partial").await.unwrap();
    let mut r = within(ba.conn.accept_data()).await.unwrap();
    let mut first = [0u8; 7];
    within(r.read_exact(&mut first)).await.unwrap();
    s.reset(9);
    assert_eq!(
        within(r.read_chunk(1024)).await,
        Err(ferry_net::StreamError::Reset(9))
    );

    // a reader that stops: the writer learns it
    let mut s = within(ab.conn.open_data()).await.unwrap();
    s.write_all(b"x").await.unwrap();
    let r = within(ba.conn.accept_data()).await.unwrap();
    r.stop(4);
    let err = within(async {
        loop {
            if let Err(e) = s.write_all(&[0u8; 65536]).await {
                return e;
            }
        }
    })
    .await;
    assert_eq!(err, ferry_net::StreamError::Stopped(4));
}
