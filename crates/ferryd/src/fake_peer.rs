//! A fake peer that speaks the real wire protocol over a real ferry-net connection, for tests
//! and for driving a lab VM from the host. **Network only**: it never captures or injects.
//!
//! - As a **fake source** it sends `Hello`, `Keymap`, `Enter`, `Key`, `Held` and motion
//!   datagrams to a ferryd that is the target.
//! - As a **fake target** it answers `Hello`, acknowledges every `Held`, and records the rest.
//!
//! `examples/fake-peer.rs` wraps it in a command-line tool for the lab.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ferry_core::geom::{MonitorInfo, WireRect};
use ferry_core::wire::{
    Control, DATAGRAM_LEN, Datagram, Decoded, Motion, PROTO_MAJOR, PROTO_MINOR, Pos,
};
use ferry_net::{
    CloseReason, ConnectError, Connection, Endpoint, EndpointConfig, Event, Events, Fingerprint,
    FrameError, Identity, TrustSet,
};

/// What arrived, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Frame(Control),
    /// A frame whose variant this build does not know.
    Unknown(u32),
    /// A frame that did not decode.
    Bad(String),
    Datagram(Datagram),
    Closed(CloseReason),
}

/// A connected fake peer: its sending half ([`Sender`], reached through `Deref`) and the
/// stream of what arrives.
pub struct FakePeer {
    pub endpoint: Endpoint,
    events: Events,
    sender: Sender,
}

/// The sending half of a fake peer, with the source-side counters. `Send`, so it can live in
/// another task than the event reader.
pub struct Sender {
    pub conn: Connection,
    buf: Vec<u8>,
    /// The fake source's session, motion seq, `Held` count and keymap serial.
    pub session: u32,
    pub motion_seq: u32,
    pub held_n: u32,
    pub keymap_serial: u32,
    /// The target-local cursor the fake source last sent.
    pub pos: (f32, f32),
}

impl std::ops::Deref for FakePeer {
    type Target = Sender;
    fn deref(&self) -> &Sender {
        &self.sender
    }
}

impl std::ops::DerefMut for FakePeer {
    fn deref_mut(&mut self) -> &mut Sender {
        &mut self.sender
    }
}

/// A `Hello` with this version and these monitors.
pub fn hello(name: &str, major: u16, minor: u16, monitors: Vec<MonitorInfo>) -> Control {
    Control::Hello {
        proto_major: major,
        proto_minor: minor,
        name: name.into(),
        features: Vec::new(),
        build: "fake-peer".into(),
        monitors,
    }
}

/// One monitor at the origin.
pub fn monitor(name: &str, w: u32, h: u32) -> MonitorInfo {
    MonitorInfo {
        name: name.into(),
        description: format!("{name} (fake)"),
        rect: WireRect { x: 0, y: 0, w, h },
        scale: 1.0,
        transform: 0,
        shift: None,
    }
}

/// The current version's `Hello`.
pub fn hello_now(name: &str, monitors: Vec<MonitorInfo>) -> Control {
    hello(name, PROTO_MAJOR, PROTO_MINOR, monitors)
}

fn endpoint_for(
    identity: Identity,
    trust: Fingerprint,
    bind: SocketAddr,
) -> Result<Endpoint, String> {
    let cfg = EndpointConfig {
        bind,
        ..EndpointConfig::default()
    };
    let trust: TrustSet = [trust].into_iter().collect();
    Endpoint::bind(identity, trust, cfg).map_err(|e| e.to_string())
}

impl FakePeer {
    /// Dial a daemon (this side is the dialer: it opens the control stream and must send
    /// `Hello` first). Call it on a tokio runtime.
    pub async fn dial(
        identity: Identity,
        daemon: SocketAddr,
        daemon_fp: Fingerprint,
    ) -> Result<Self, String> {
        let bind: SocketAddr = if daemon.is_ipv4() {
            "0.0.0.0:0".parse().expect("an address")
        } else {
            "[::]:0".parse().expect("an address")
        };
        let endpoint = endpoint_for(identity, daemon_fp, bind)?;
        let nc = endpoint
            .dial(daemon, daemon_fp)
            .await
            .map_err(|e: ConnectError| e.to_string())?;
        Ok(Self::from_parts(endpoint, nc.conn, nc.events))
    }

    /// Bind `bind` and wait for the daemon `daemon_fp` to dial in (this side is the acceptor).
    pub async fn accept(
        identity: Identity,
        bind: SocketAddr,
        daemon_fp: Fingerprint,
    ) -> Result<Self, String> {
        let endpoint = endpoint_for(identity, daemon_fp, bind)?;
        loop {
            match endpoint.accept().await {
                Some(Ok(nc)) => return Ok(Self::from_parts(endpoint, nc.conn, nc.events)),
                Some(Err(e)) => tracing::warn!(error = %e.error, "fake peer: inbound refused"),
                None => return Err("endpoint closed".into()),
            }
        }
    }

    fn from_parts(endpoint: Endpoint, conn: Connection, events: Events) -> Self {
        Self {
            endpoint,
            events,
            sender: Sender {
                conn,
                buf: Vec::with_capacity(256),
                session: 0,
                motion_seq: 0,
                held_n: 0,
                keymap_serial: 0,
                pos: (0.0, 0.0),
            },
        }
    }

    /// The endpoint (keep it alive), the sending half and the event stream, apart.
    pub fn into_parts(self) -> (Endpoint, Sender, Events) {
        (self.endpoint, self.sender, self.events)
    }

    /// The next thing that arrives, or `None` after `timeout`.
    pub async fn next(&mut self, timeout: Duration) -> Option<Incoming> {
        let ev = tokio::time::timeout(timeout, self.events.next())
            .await
            .ok()??;
        Some(decode(ev))
    }

    /// The next control frame, skipping datagrams. `None` on timeout or close.
    pub async fn next_frame(&mut self, timeout: Duration) -> Option<Control> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.checked_duration_since(Instant::now())?;
            match self.next(left).await? {
                Incoming::Frame(m) => return Some(m),
                Incoming::Closed(_) => return None,
                _ => {}
            }
        }
    }

    /// Send our `Hello` and wait for theirs (as the dialer); returns theirs.
    pub async fn exchange_hello(&mut self, ours: &Control) -> Option<Control> {
        self.send(ours).ok()?;
        match self.next_frame(Duration::from_secs(5)).await? {
            h @ Control::Hello { .. } => Some(h),
            _ => None,
        }
    }

    /// Wait for their `Hello` and answer with ours (as the acceptor); returns theirs.
    pub async fn answer_hello(&mut self, ours: &Control) -> Option<Control> {
        match self.next_frame(Duration::from_secs(5)).await? {
            h @ Control::Hello { .. } => {
                self.send(ours).ok()?;
                Some(h)
            }
            _ => None,
        }
    }
}

/// One ferry-net event, decoded.
pub fn decode(ev: Event) -> Incoming {
    match ev {
        Event::Frame(b) => match Control::decode(&b) {
            Ok(Decoded::Msg(m)) => Incoming::Frame(m),
            Ok(Decoded::Unknown(v)) => Incoming::Unknown(v),
            Err(e) => Incoming::Bad(e.to_string()),
        },
        Event::Datagram(d) => match Datagram::decode(&d) {
            Ok(d) => Incoming::Datagram(d),
            Err(e) => Incoming::Bad(e.to_string()),
        },
        Event::Closed(r) => Incoming::Closed(r),
    }
}

impl Sender {
    pub fn send(&mut self, msg: &Control) -> Result<(), FrameError> {
        msg.encode(&mut self.buf);
        self.conn.try_send_frame(Bytes::copy_from_slice(&self.buf))
    }

    pub fn send_datagram(&self, d: Datagram) {
        let mut b = [0u8; DATAGRAM_LEN];
        d.encode(&mut b);
        let _ = self.conn.send_datagram(Bytes::copy_from_slice(&b));
    }

    // --- a fake source ------------------------------------------------------------------------

    fn fence(&self) -> Pos {
        Pos {
            motion_seq: self.motion_seq,
            x: self.pos.0,
            y: self.pos.1,
        }
    }

    pub fn keymap(&mut self, text: &str) -> Result<(), FrameError> {
        self.keymap_serial += 1;
        self.send(&Control::Keymap {
            serial: self.keymap_serial,
            format: 1,
            text: text.into(),
        })
    }

    /// `Enter` at a target-local point, then `Held#1` (design §5.1 Enter).
    pub fn enter(&mut self, x: f32, y: f32) -> Result<(), FrameError> {
        self.session += 1;
        self.held_n = 0;
        self.pos = (x, y);
        self.send(&Control::Enter {
            session: self.session,
            x,
            y,
            keymap_serial: self.keymap_serial,
            motion_seq: self.motion_seq,
        })?;
        self.held(&[], &[])
    }

    /// The next `Held` with this held set.
    pub fn held(&mut self, keys: &[u16], buttons: &[u16]) -> Result<(), FrameError> {
        self.held_n += 1;
        let msg = Control::Held {
            session: self.session,
            n: self.held_n,
            keys: keys.to_vec(),
            buttons: buttons.to_vec(),
            pos: self.fence(),
        };
        self.send(&msg)
    }

    pub fn key(&mut self, code: u16, pressed: bool) -> Result<(), FrameError> {
        self.send(&Control::Key {
            session: self.session,
            code,
            pressed,
        })
    }

    pub fn button(&mut self, code: u16, pressed: bool) -> Result<(), FrameError> {
        let pos = self.fence();
        self.send(&Control::Button {
            session: self.session,
            code,
            pressed,
            pos,
        })
    }

    /// A wheel event: `value120` hi-res steps (120 per notch) on `axis` (0 vertical).
    pub fn wheel(&mut self, axis: u8, value120: i32) -> Result<(), FrameError> {
        let pos = self.fence();
        self.send(&Control::Scroll {
            session: self.session,
            axis,
            source: 0,
            value: value120 as f32 * 15.0 / 120.0,
            value120: Some(value120),
            stop: false,
            pos,
        })
    }

    pub fn leave(&mut self, reason: ferry_core::wire::LeaveReason) -> Result<(), FrameError> {
        self.send(&Control::Leave {
            session: self.session,
            reason: reason.code(),
        })
    }

    /// A motion datagram to a target-local point.
    pub fn motion(&mut self, x: f32, y: f32) {
        self.motion_seq = self.motion_seq.wrapping_add(1);
        let (dx, dy) = (x - self.pos.0, y - self.pos.1);
        self.pos = (x, y);
        self.send_datagram(Datagram::Motion(Motion {
            session_lo: self.session as u16,
            seq: self.motion_seq,
            x,
            y,
            dx,
            dy,
            t_us: crate::clock::now().low32(),
        }));
    }
}
