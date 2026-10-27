//! Real-wire probe for ferry-net: RTT/2 of timestamped datagrams echoed over a pinned
//! connection (the S6 method), plus the DSCP the receiving socket saw.
//!
//! ```text
//! net-probe id     <dir>                                   print (and create) the identity
//! net-probe serve  <dir> <peer-fp> [port] [secs]           echo datagrams for secs (default 60)
//! net-probe client <dir> <peer-fp> <addr:port> [n] [hz]    send n datagrams at hz (10000, 1000)
//! ```
//!
//! The payload is 24 bytes, `seq u64 LE | t_send_ns u64 LE | 0u64`, as in S6/S7. The first
//! second of samples is dropped as warm-up.
#![expect(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "a lab probe: its output is the product, and a bad argument or a failed call ends it with a panic"
)]

use bytes::Bytes;
use ferry_net::{
    CloseCode, Connection, Endpoint, EndpointConfig, Event, Fingerprint, Identity, TrustSet,
};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn die(msg: &str) -> ! {
    eprintln!("net-probe: {msg}");
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("id") => {
            let id = Identity::load_or_create(Path::new(arg(1).unwrap_or_else(|| die("id <dir>"))))
                .unwrap_or_else(|e| die(&e.to_string()));
            println!("{}", id.fingerprint());
        }
        Some("serve") => {
            let (dir, peer) = id_and_peer(arg(1), arg(2));
            let port = arg(3).map_or(ferry_net::DEFAULT_PORT, |p| p.parse().unwrap());
            let secs = arg(4).map_or(60, |s| s.parse().unwrap());
            serve(dir, peer, port, Duration::from_secs(secs));
        }
        Some("client") => {
            let (dir, peer) = id_and_peer(arg(1), arg(2));
            let addr: SocketAddr = arg(3)
                .unwrap_or_else(|| die("client <dir> <fp> <addr:port>"))
                .parse()
                .unwrap_or_else(|_| die("bad address"));
            let n = arg(4).map_or(10_000, |s| s.parse().unwrap());
            let hz = arg(5).map_or(1000, |s| s.parse().unwrap());
            client(dir, peer, addr, n, hz);
        }
        _ => die("usage: net-probe id|serve|client …"),
    }
}

fn id_and_peer(dir: Option<&str>, fp: Option<&str>) -> (Identity, Fingerprint) {
    let id = Identity::load_or_create(Path::new(dir.unwrap_or_else(|| die("missing <dir>"))))
        .unwrap_or_else(|e| die(&e.to_string()));
    let fp = fp
        .unwrap_or_else(|| die("missing <peer-fp>"))
        .parse()
        .unwrap_or_else(|e: ferry_net::ParseFingerprintError| die(&e.to_string()));
    (id, fp)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn print_stats(who: &str, ep: &Endpoint) {
    let s = ep.socket_stats();
    println!(
        "{who}: socket rx_packets={} rx_ef={} ({:.1}%) last_tos={} tx_errors={}",
        s.rx_packets,
        s.rx_ef,
        100.0 * s.rx_ef as f64 / s.rx_packets.max(1) as f64,
        s.rx_last_tos.map_or("none".into(), |t| format!("{t:#04x}")),
        s.tx_errors,
    );
}

fn serve(id: Identity, peer: Fingerprint, port: u16, secs: Duration) {
    runtime().block_on(async move {
        let cfg = EndpointConfig {
            bind: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
            ..EndpointConfig::default()
        };
        let ep = Endpoint::bind(id, TrustSet::from_iter([peer]), cfg)
            .unwrap_or_else(|e| die(&e.to_string()));
        println!(
            "serving on {} as {}",
            ep.local_addr().unwrap(),
            ep.fingerprint().short()
        );
        let deadline = tokio::time::sleep(secs);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                inc = ep.accept() => match inc {
                    None => break,
                    Some(Err(e)) => println!("refused {e}"),
                    Some(Ok(nc)) => {
                        println!("connected {} from {}", nc.conn.peer_fp().short(), nc.conn.remote_addr());
                        let (conn, mut events) = (nc.conn, nc.events);
                        let ep = ep.clone();
                        tokio::spawn(async move {
                            let mut echoed = 0u64;
                            while let Some(ev) = events.next().await {
                                match ev {
                                    Event::Datagram(d) => {
                                        let _ = conn.send_datagram(d);
                                        echoed += 1;
                                    }
                                    Event::Frame(f) => { let _ = conn.try_send_frame(f); }
                                    Event::Closed(r) => println!("closed: {r}; echoed {echoed}"),
                                }
                            }
                            print_stats("server", &ep);
                        });
                    }
                },
            }
        }
        print_stats("server", &ep);
        ep.close();
        ep.wait_idle().await;
    });
}

fn client(id: Identity, peer: Fingerprint, addr: SocketAddr, n: u64, hz: u64) {
    let t0 = Instant::now();
    let rtt: Arc<Vec<AtomicU64>> = Arc::new((0..n).map(|_| AtomicU64::new(0)).collect());
    let (conn_tx, conn_rx) = std::sync::mpsc::channel::<Connection>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let r = rtt.clone();
    // the `net` thread: its own current-thread runtime, as in ferryd
    let net = std::thread::spawn(move || {
        runtime().block_on(async move {
            let cfg = EndpointConfig {
                bind: if addr.is_ipv4() {
                    "0.0.0.0:0".parse().unwrap()
                } else {
                    "[::]:0".parse().unwrap()
                },
                ..EndpointConfig::default()
            };
            let ep = Endpoint::bind(id, TrustSet::from_iter([peer]), cfg)
                .unwrap_or_else(|e| die(&e.to_string()));
            let nc = ep
                .dial(addr, peer)
                .await
                .unwrap_or_else(|e| die(&format!("dial {addr}: {e}")));
            println!(
                "connected to {} at {} (rtt {:?})",
                nc.conn.peer_fp().short(),
                nc.conn.remote_addr(),
                nc.conn.rtt()
            );
            conn_tx.send(nc.conn.clone()).unwrap();
            let mut events = nc.events;
            let mut stop = stop_rx;
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    ev = events.next() => match ev {
                        Some(Event::Datagram(d)) if d.len() == 24 => {
                            let seq = u64::from_le_bytes(d[0..8].try_into().unwrap());
                            let sent = u64::from_le_bytes(d[8..16].try_into().unwrap());
                            let now = t0.elapsed().as_nanos() as u64;
                            if let Some(slot) = r.get(seq as usize) {
                                let _ = slot.compare_exchange(0, now.saturating_sub(sent).max(1), Ordering::Relaxed, Ordering::Relaxed);
                            }
                        }
                        Some(Event::Closed(reason)) => { println!("closed: {reason}"); break }
                        None => break,
                        _ => {}
                    }
                }
            }
            println!("final rtt estimate {:?}", nc.conn.rtt());
            print_stats("client", &ep);
            nc.conn.close(CloseCode::SHUTDOWN, "done");
            ep.wait_idle().await;
        });
    });
    let conn = conn_rx.recv().unwrap_or_else(|_| die("connection failed"));
    std::thread::sleep(Duration::from_millis(200));

    // paced sender on this thread, like the capture thread calling send_datagram
    let period = Duration::from_nanos(1_000_000_000 / hz);
    let start = Instant::now() + Duration::from_millis(20);
    let mut errors = 0u64;
    for seq in 0..n {
        let deadline = start + period * seq as u32;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let left = deadline - now;
            if left > Duration::from_micros(250) {
                std::thread::sleep(left - Duration::from_micros(150));
            } else {
                std::hint::spin_loop();
            }
        }
        let mut b = [0u8; 24];
        b[0..8].copy_from_slice(&seq.to_le_bytes());
        b[8..16].copy_from_slice(&(t0.elapsed().as_nanos() as u64).to_le_bytes());
        if conn.send_datagram(Bytes::copy_from_slice(&b)).is_err() {
            errors += 1;
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    let _ = stop_tx.send(());
    net.join().unwrap();

    let warmup = hz.min(n / 10);
    let mut half_us: Vec<f64> = rtt[warmup as usize..]
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .filter(|&v| v > 0)
        .map(|v| v as f64 / 2000.0)
        .collect();
    let counted = n - warmup;
    let lost = counted - half_us.len() as u64;
    half_us.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        if half_us.is_empty() {
            f64::NAN
        } else {
            half_us[((half_us.len() - 1) as f64 * p).round() as usize]
        }
    };
    println!(
        "RTT/2 µs over {} samples ({} warm-up dropped, {} lost, {} send errors): p50 {:.0}  p90 {:.0}  p99 {:.0}  p99.9 {:.0}  max {:.0}",
        half_us.len(),
        warmup,
        lost,
        errors,
        pct(0.50),
        pct(0.90),
        pct(0.99),
        pct(0.999),
        pct(1.0),
    );
}
