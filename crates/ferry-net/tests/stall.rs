//! A link stall on loopback: a relay between the two endpoints drops every packet for
//! a while, the way a Wi-Fi burst or a cut does, and the test measures how long after the link
//! is back a frame queued during the stall arrives.
#![expect(
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "integration test: diagnostics for a failing run, which the harness captures; an unwrap that fails is the test failing"
)]

use bytes::Bytes;
use ferry_core::Micros;
use ferry_core::source::{MotionPacer, Pace};
use ferry_net::{Endpoint, EndpointConfig, Event, Identity, NewConnection, PathInfo, TrustSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const T: Duration = Duration::from_secs(10);

fn lo(min_window: u64) -> EndpointConfig {
    EndpointConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        min_window,
        ..EndpointConfig::default()
    }
}

fn endpoint(cfg: EndpointConfig) -> Endpoint {
    Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), cfg).unwrap()
}

/// A UDP relay in front of `server`: the dialer talks to [`Relay::addr`]. While `cut` is set,
/// every packet in either direction is dropped.
struct Relay {
    addr: SocketAddr,
    cut: Arc<AtomicBool>,
}

async fn relay(server: SocketAddr) -> Relay {
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let cut = Arc::new(AtomicBool::new(false));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    let addr = front.local_addr().unwrap();
    {
        let (front, back, cut, client) = (front.clone(), back.clone(), cut.clone(), client.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, from)) = front.recv_from(&mut buf).await {
                *client.lock().unwrap() = Some(from);
                if !cut.load(Ordering::SeqCst) {
                    let _ = back.send_to(&buf[..n], server).await;
                }
            }
        });
    }
    {
        let cut = cut.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, _)) = back.recv_from(&mut buf).await {
                let to = *client.lock().unwrap();
                if let Some(to) = to
                    && !cut.load(Ordering::SeqCst)
                {
                    let _ = front.send_to(&buf[..n], to).await;
                }
            }
        });
    }
    Relay { addr, cut }
}

/// a dials b through a relay; returns (a's side, b's side, the relay).
async fn connect_via_relay(
    min_window: u64,
) -> (NewConnection, NewConnection, Relay, Endpoint, Endpoint) {
    let (a, b) = (endpoint(lo(min_window)), endpoint(lo(min_window)));
    a.trust().insert(b.fingerprint());
    b.trust().insert(a.fingerprint());
    let r = relay(b.local_addr().unwrap()).await;
    let (dialed, accepted) = tokio::time::timeout(T, async {
        tokio::join!(a.dial(r.addr, b.fingerprint()), b.accept())
    })
    .await
    .expect("connect timed out");
    (dialed.unwrap(), accepted.unwrap().unwrap(), r, a, b)
}

const KEY: &[u8] = b"key";
const PING: [u8; 28] = [2; 28];
const HELD: &[u8] = b"held";
const ACK: &[u8] = b"ack";

/// What the source puts into the stall: `per_ms` motions every 1 ms tick (a 1, 2, 4 or 8 kHz
/// mouse; EIS gives one `Motion` per pointer event, A-04). Unpaced, each is a datagram; paced,
/// the source's [`MotionPacer`] decides, with the acks of b as they arrive. `spread` lets
/// quinn's driver run between two datagrams, so each leaves in its own packet, as from a real
/// mouse whose events are 125-500 µs apart; otherwise a tick's datagrams share packets.
#[derive(Debug, Clone, Copy)]
struct Load {
    per_ms: u32,
    spread: bool,
    paced: bool,
}

const KHZ1: Load = Load {
    per_ms: 1,
    spread: false,
    paced: false,
};

/// One stall: `cut` long, with motion datagrams at `load` for the first 650 ms (a source until
/// its silence release), a key frame queued 300 ms in, and, when `probe`, a 28-byte Ping
/// datagram every 100 ms from the release on (ferryd's stall probe). Until the release a `Held`
/// frame leaves every 100 ms once the one before it is acknowledged (stop-and-wait),
/// and b acknowledges each. Returns how long after the relay passes packets again the key frame
/// reached b, and a's path afterwards.
async fn key_after_stall(
    min_window: u64,
    cut: Duration,
    probe: bool,
    load: Load,
) -> (Duration, PathInfo) {
    let (ab, ba, r, _a, _b) = connect_via_relay(min_window).await;
    let NewConnection {
        conn: ab,
        events: mut ab_events,
    } = ab;
    let NewConnection {
        conn: ba,
        events: mut ba_events,
    } = ba;
    // Warm up: RTT samples and an open control stream.
    for i in 0..20u8 {
        ab.try_send_frame(Bytes::from(vec![i + 1])).unwrap();
        ab.send_datagram(Bytes::from_static(&PING)).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut frames = 0;
    while frames < 20 {
        match tokio::time::timeout(T, ba_events.next()).await.unwrap() {
            Some(Event::Frame(_)) => frames += 1,
            Some(Event::Datagram(_)) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<Instant>();
    tokio::spawn(async move {
        let mut tx = Some(tx);
        while let Some(ev) = ba_events.next().await {
            match ev {
                Event::Frame(f) if f == KEY => {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(Instant::now());
                    }
                }
                Event::Frame(f) if f == HELD => {
                    let _ = ba.try_send_frame(Bytes::from_static(ACK));
                }
                _ => {}
            }
        }
    });
    let (ack_tx, mut acks) = tokio::sync::mpsc::unbounded_channel::<Instant>();
    tokio::spawn(async move {
        while let Some(ev) = ab_events.next().await {
            if ev == Event::Frame(Bytes::from_static(ACK)) {
                let _ = ack_tx.send(Instant::now());
            }
        }
    });

    let start = Instant::now();
    r.cut.store(true, Ordering::SeqCst);
    let mut key_sent = false;
    let mut held_out = false;
    let mut next_held = Duration::ZERO;
    let mut next_probe = Duration::from_millis(650);
    let mut back_at = None;
    let micros = |at: Instant| Micros(at.saturating_duration_since(start).as_micros() as u64);
    // The cut comes right after an ack: the most motion the pacer lets into it.
    let mut pacer = MotionPacer::new(Micros::ZERO);
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    let arrived = loop {
        tick.tick().await;
        let t = start.elapsed();
        while let Ok(at) = acks.try_recv() {
            held_out = false;
            pacer.acked(micros(at));
        }
        if t < Duration::from_millis(650) {
            if !held_out && t >= next_held {
                ab.try_send_frame(Bytes::from_static(HELD)).unwrap();
                held_out = true;
                next_held = t + Duration::from_millis(100);
            }
            let mut n = 0;
            if load.paced {
                n += u32::from(pacer.flush(micros(Instant::now())).is_some());
            }
            for _ in 0..load.per_ms {
                let now = micros(Instant::now());
                n += u32::from(
                    !load.paced || matches!(pacer.motion(1.0, 0.0, now), Pace::Send { .. }),
                );
            }
            for _ in 0..n {
                let _ = ab.send_datagram(Bytes::from_static(&[1; 28]));
                if load.spread {
                    tokio::task::yield_now().await;
                }
            }
        }
        if !key_sent && t >= Duration::from_millis(300) {
            ab.try_send_frame(Bytes::from_static(KEY)).unwrap();
            key_sent = true;
        }
        if probe && t >= next_probe {
            let _ = ab.send_datagram(Bytes::from_static(&PING));
            next_probe += Duration::from_millis(100);
        }
        if back_at.is_none() && t >= cut {
            r.cut.store(false, Ordering::SeqCst);
            back_at = Some(Instant::now());
            break rx;
        }
    };
    let back_at = back_at.unwrap();
    // Keep probing until the key is in.
    let got = tokio::time::timeout(T, async {
        let mut arrived = std::pin::pin!(arrived);
        loop {
            tokio::select! {
                t = &mut arrived => break t.unwrap(),
                () = tokio::time::sleep_until((start + next_probe).into()), if probe => {
                    let _ = ab.send_datagram(Bytes::from_static(&PING));
                    next_probe += Duration::from_millis(100);
                }
            }
        }
    })
    .await
    .expect("the key never arrived");
    let late = got.saturating_duration_since(back_at);
    let path = ab.path();
    eprintln!(
        "min_window={min_window} cut={cut:?} probe={probe} {load:?}: key {late:?} after the link was back; {path:?}"
    );
    (late, path)
}

/// The F1 mechanism (exit check 2026-10-19): with quinn's stock window, the stall's in-flight
/// bytes fill the congestion window, so nothing leaves but quinn's own loss probes, whose
/// timeout doubles at each one; the key waits for the next probe after the link is back, and
/// persistent congestion leaves the window at two packets. How late depends on where the link
/// comes back in the probe schedule (0.25-4.3 s in `stall_matrix`, 0.2 s under load), so only
/// the window is asserted.
#[tokio::test]
async fn with_the_stock_window_a_stall_collapses_the_window() {
    let (_, path) = key_after_stall(0, Duration::from_millis(1200), true, KHZ1).await;
    assert!(path.congestion_events >= 1, "{path:?}");
    assert!(path.cwnd < 12_000, "below CUBIC's initial window: {path:?}");
}

/// The fix: a window floor lets the stall probe leave, and its acknowledgement makes quinn
/// declare the stall's packets lost and resend the key at once.
#[tokio::test]
async fn with_the_window_floor_a_stall_probe_brings_the_key_within_one_probe_period() {
    let (late, path) = key_after_stall(
        EndpointConfig::default().min_window,
        Duration::from_millis(1200),
        true,
        KHZ1,
    )
    .await;
    assert!(late <= Duration::from_millis(250), "{late:?}");
    assert_eq!(path.cwnd, ferry_net::MIN_WINDOW, "{path:?}");
}

/// The numbers behind BL-008's analysis, printed (not a gate): every combination of floor and
/// probe over a few stall lengths. `cargo test -p ferry-net --test stall -- --ignored --nocapture`
#[tokio::test]
#[ignore = "prints a matrix for the analysis; slow"]
async fn stall_matrix() {
    for cut_ms in [800u64, 1200, 1500, 2500, 5000] {
        for (floor, probe) in [
            (0, false),
            (0, true),
            (ferry_net::MIN_WINDOW, false),
            (ferry_net::MIN_WINDOW, true),
        ] {
            key_after_stall(floor, Duration::from_millis(cut_ms), probe, KHZ1).await;
        }
    }
}

/// A-04's measurement: a 1.5 s cut with the floor and the probes, at 1, 2, 4 and 8 motions per
/// ms, unpaced and paced, 3 runs each. `cargo test -p ferry-net --test stall rate_matrix -- --ignored --nocapture`
#[tokio::test]
#[ignore = "prints a matrix for the analysis; slow"]
async fn rate_matrix() {
    for paced in [false, true] {
        for spread in [false, true] {
            for per_ms in [1, 2, 4, 8] {
                for _ in 0..3 {
                    key_after_stall(
                        ferry_net::MIN_WINDOW,
                        Duration::from_millis(1500),
                        true,
                        Load {
                            per_ms,
                            spread,
                            paced,
                        },
                    )
                    .await;
                }
            }
        }
    }
}

/// A-04: with the source's motion pacing, an 8 kHz mouse moving into a 1.5 s cut
/// leaves room under the floor, so a key still arrives within BL-008's 250 ms of the link
/// coming back. Unpaced, the same load gave 0.4-0.5 s (`rate_matrix`). Each datagram in its own
/// packet is the worst case.
#[tokio::test]
async fn at_8_khz_a_paced_source_brings_the_key_within_250_ms() {
    for spread in [true, false] {
        let (late, path) = key_after_stall(
            ferry_net::MIN_WINDOW,
            Duration::from_millis(1500),
            true,
            Load {
                per_ms: 8,
                spread,
                paced: true,
            },
        )
        .await;
        assert!(late <= Duration::from_millis(250), "{late:?} {path:?}");
    }
}
