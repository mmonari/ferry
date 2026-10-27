//! DSCP EF on the wire. A UDP relay between two endpoints reads the TOS byte of
//! every packet with `IP_RECVTOS`, independently of ferry's socket; the socket's own counters
//! are checked too.
#![expect(
    clippy::unwrap_used,
    reason = "integration test: an unwrap that fails is the test failing"
)]

use bytes::Bytes;
use ferry_net::{DSCP_EF, Endpoint, EndpointConfig, Event, Identity, TrustSet};
use nix::sys::socket::{
    ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg, setsockopt, sockopt,
};
use std::collections::BTreeMap;
use std::io::IoSliceMut;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T: Duration = Duration::from_secs(5);

fn cfg(bind: &str, dscp_ef: bool) -> EndpointConfig {
    EndpointConfig {
        bind: bind.parse().unwrap(),
        dscp_ef,
        ..EndpointConfig::default()
    }
}

fn pair(a: EndpointConfig, b: EndpointConfig) -> (Endpoint, Endpoint) {
    let a = Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), a).unwrap();
    let b = Endpoint::bind(Identity::ephemeral().unwrap(), TrustSet::new(), b).unwrap();
    a.trust().insert(b.fingerprint());
    b.trust().insert(a.fingerprint());
    (a, b)
}

/// TOS histogram per direction, keyed by TOS byte.
type Hist = Arc<Mutex<[BTreeMap<u8, u64>; 2]>>;

/// Relays UDP between the first sender (the dialer) and `server`, recording each packet's
/// TOS (IPv4) or traffic class (IPv6) on arrival at the relay.
struct Relay {
    addr: SocketAddr,
    hist: Hist,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Relay {
    fn start(bind: &str, server: SocketAddr) -> Self {
        let sock = UdpSocket::bind(bind).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        if sock.local_addr().unwrap().is_ipv4() {
            setsockopt(&sock, sockopt::IpRecvTos, &true).unwrap();
        } else {
            setsockopt(&sock, sockopt::Ipv6RecvTClass, &true).unwrap();
        }
        let addr = sock.local_addr().unwrap();
        let hist: Hist = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s) = (hist.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut client: Option<SocketAddr> = None;
            let mut buf = [0u8; 2048];
            let mut cmsg = nix::cmsg_space!(u8, i32);
            while !s.load(Ordering::Relaxed) {
                let mut iov = [IoSliceMut::new(&mut buf)];
                let Ok(m) = recvmsg::<SockaddrStorage>(
                    sock.as_raw_fd(),
                    &mut iov,
                    Some(&mut cmsg),
                    MsgFlags::empty(),
                ) else {
                    continue;
                };
                let mut tos = None;
                for c in m.cmsgs().unwrap() {
                    match c {
                        ControlMessageOwned::Ipv4Tos(t) => tos = Some(t),
                        ControlMessageOwned::Ipv6TClass(t) => tos = Some(t as u8),
                        _ => {}
                    }
                }
                let from = m.address.unwrap();
                let from = if let Some(v4) = from.as_sockaddr_in() {
                    SocketAddr::V4((*v4).into())
                } else {
                    SocketAddr::V6((*from.as_sockaddr_in6().unwrap()).into())
                };
                let n = m.bytes;
                let (dir, to) = if from == server {
                    (1, client)
                } else {
                    client.get_or_insert(from);
                    (0, Some(server))
                };
                *h.lock().unwrap()[dir]
                    .entry(tos.unwrap_or(0xFF))
                    .or_default() += 1;
                if let Some(to) = to {
                    let _ = sock.send_to(&buf[..n], to);
                }
            }
        });
        Self {
            addr,
            hist,
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> [BTreeMap<u8, u64>; 2] {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        self.hist.lock().unwrap().clone()
    }
}

/// Dials `b` through a relay, exchanges frames and datagrams both ways, and returns the TOS
/// histograms (dialer → listener, listener → dialer).
async fn through_relay(
    a: &Endpoint,
    b: &Endpoint,
    relay_bind: &str,
    b_addr: SocketAddr,
) -> [BTreeMap<u8, u64>; 2] {
    let relay = Relay::start(relay_bind, b_addr);
    let (dialed, accepted) = tokio::time::timeout(T, async {
        tokio::join!(a.dial(relay.addr, b.fingerprint()), b.accept())
    })
    .await
    .unwrap();
    let (mut ab, mut ba) = (dialed.unwrap(), accepted.unwrap().unwrap());
    for i in 0..20u8 {
        ab.conn.send_datagram(Bytes::from(vec![i; 28])).unwrap();
        ba.conn.try_send_frame(Bytes::from(vec![i; 64])).unwrap();
        ab.conn.try_send_frame(Bytes::from(vec![i; 64])).unwrap();
    }
    tokio::time::timeout(T, async {
        let mut frames = 0;
        while frames < 20 {
            if let Some(Event::Frame(_)) = ab.events.next().await {
                frames += 1;
            }
        }
        let mut frames = 0;
        while frames < 20 {
            if let Some(Event::Frame(_)) = ba.events.next().await {
                frames += 1;
            }
        }
    })
    .await
    .unwrap();
    relay.finish()
}

fn only_ef(h: &BTreeMap<u8, u64>) -> bool {
    !h.is_empty() && h.keys().all(|&t| t == DSCP_EF)
}

fn b_addr_v4(b: &Endpoint) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], b.local_addr().unwrap().port()))
}

#[tokio::test]
async fn ipv4_socket_marks_every_packet_ef() {
    let (a, b) = pair(cfg("127.0.0.1:0", true), cfg("127.0.0.1:0", true));
    let [ab, ba] = through_relay(&a, &b, "127.0.0.1:0", b_addr_v4(&b)).await;
    assert!(only_ef(&ab), "dialer → listener TOS: {ab:x?}");
    assert!(only_ef(&ba), "listener → dialer TOS: {ba:x?}");
}

#[tokio::test]
async fn dual_stack_socket_marks_ipv4_packets_ef() {
    let (a, b) = pair(cfg("[::]:0", true), cfg("[::]:0", true));
    assert!(a.local_addr().unwrap().is_ipv6());
    let [ab, ba] = through_relay(&a, &b, "127.0.0.1:0", b_addr_v4(&b)).await;
    assert!(only_ef(&ab), "dialer → listener TOS: {ab:x?}");
    assert!(only_ef(&ba), "listener → dialer TOS: {ba:x?}");
}

#[tokio::test]
async fn dual_stack_socket_marks_ipv6_packets_ef() {
    let (a, b) = pair(cfg("[::]:0", true), cfg("[::]:0", true));
    let b6 = SocketAddr::from((
        std::net::Ipv6Addr::LOCALHOST,
        b.local_addr().unwrap().port(),
    ));
    let [ab, ba] = through_relay(&a, &b, "[::1]:0", b6).await;
    assert!(only_ef(&ab), "dialer → listener TCLASS: {ab:x?}");
    assert!(only_ef(&ba), "listener → dialer TCLASS: {ba:x?}");
}

#[tokio::test]
async fn the_relay_sees_the_difference_when_ef_is_off() {
    let (a, b) = pair(cfg("[::]:0", false), cfg("[::]:0", false));
    let [ab, ba] = through_relay(&a, &b, "127.0.0.1:0", b_addr_v4(&b)).await;
    assert!(ab.keys().all(|&t| t == 0) && !ab.is_empty(), "{ab:x?}");
    assert!(ba.keys().all(|&t| t == 0) && !ba.is_empty(), "{ba:x?}");
}

#[tokio::test]
async fn socket_counts_ef_on_receive_for_ipv4_via_dual_stack() {
    // direct, no relay: a's packets reach b's dual-stack socket as IPv4
    let (a, b) = pair(cfg("[::]:0", true), cfg("[::]:0", true));
    let (dialed, accepted) = tokio::time::timeout(T, async {
        tokio::join!(a.dial(b_addr_v4(&b), b.fingerprint()), b.accept())
    })
    .await
    .unwrap();
    let (ab, mut ba) = (dialed.unwrap(), accepted.unwrap().unwrap());
    assert!(ab.conn.remote_addr().is_ipv4());
    ab.conn.send_datagram(Bytes::from_static(&[7; 28])).unwrap();
    assert!(matches!(
        tokio::time::timeout(T, ba.events.next()).await.unwrap(),
        Some(Event::Datagram(_))
    ));
    let s = b.socket_stats();
    assert!(s.rx_packets > 0);
    assert_eq!(s.rx_ef, s.rx_packets, "{s:?}");
    assert_eq!(s.rx_last_tos, Some(DSCP_EF));
}

#[tokio::test]
async fn socket_counts_no_ef_from_an_unmarked_sender() {
    let (a, b) = pair(cfg("[::]:0", false), cfg("[::]:0", true));
    let (dialed, accepted) = tokio::time::timeout(T, async {
        tokio::join!(a.dial(b_addr_v4(&b), b.fingerprint()), b.accept())
    })
    .await
    .unwrap();
    let (ab, mut ba) = (dialed.unwrap(), accepted.unwrap().unwrap());
    ab.conn.send_datagram(Bytes::from_static(&[7; 28])).unwrap();
    assert!(matches!(
        tokio::time::timeout(T, ba.events.next()).await.unwrap(),
        Some(Event::Datagram(_))
    ));
    let s = b.socket_stats();
    assert!(s.rx_packets > 0);
    assert_eq!(s.rx_ef, 0, "{s:?}");
    assert_eq!(s.rx_last_tos, Some(0));
    // and the marked direction still counts as EF on a
    assert!(a.socket_stats().rx_ef > 0);
}
