//! One peer connection: datagrams, the framed control stream, and its event stream.

use crate::error::{LocalClose, close_reason};
use crate::{CloseCode, CloseReason, ConnectError, DatagramError, Fingerprint, FrameError};
use bytes::{Bytes, BytesMut};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// What a peer connection delivers, in [`Events::next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An unreliable datagram (motion lane).
    Datagram(Bytes),
    /// One frame from the control stream, in order, never empty, at most the frame cap.
    Frame(Bytes),
    /// The connection ended. Delivered once, after every frame received before the end.
    Closed(CloseReason),
}

/// A fresh connection: the handle to send on and the stream of what arrives.
#[derive(Debug)]
pub struct NewConnection {
    pub conn: Connection,
    pub events: Events,
}

/// Handle to one peer connection: `Clone + Send + Sync`. Every method except
/// [`Connection::close_graceful`] and [`Connection::closed`] is synchronous and non-blocking,
/// so a non-tokio thread (the calloop capture thread) can call it directly.
/// The connection's I/O runs on the tokio runtime that created it.
///
/// Dropping every clone and the [`Events`] closes the connection with
/// [`CloseCode::SHUTDOWN`].
#[derive(Clone, Debug)]
pub struct Connection {
    quic: quinn::Connection,
    shared: Arc<Shared>,
}

const _: () = {
    const fn handle<T: Clone + Send + Sync + 'static>() {}
    handle::<Connection>();
};

/// What the writer task takes, in order.
#[derive(Debug)]
enum Out {
    Frame(Bytes),
    /// Finish the control stream (graceful close, or the answer to the peer's finish).
    Finish,
}

#[derive(Debug)]
struct Shared {
    peer: Fingerprint,
    dialed: bool,
    frames: mpsc::Sender<Out>,
    max_frame: usize,
    local_close: Arc<OnceLock<LocalClose>>,
    /// Set by the reader when the peer finished its side of the control stream.
    peer_finished: watch::Receiver<bool>,
    /// Set by the writer when the peer acknowledged everything this side sent on the control
    /// stream, its finish included.
    own_acked: watch::Receiver<bool>,
}

impl Connection {
    /// The peer's fingerprint, taken from the certificate the verifier accepted.
    /// `peer_fp().0` is the `[u8; 32]`.
    pub fn peer_fp(&self) -> Fingerprint {
        self.shared.peer
    }

    /// `true` if this side dialled the connection.
    pub fn is_dialer(&self) -> bool {
        self.shared.dialed
    }

    /// The peer's address; an IPv4 peer on the dual-stack socket shows as IPv4.
    pub fn remote_addr(&self) -> SocketAddr {
        let a = self.quic.remote_address();
        SocketAddr::new(a.ip().to_canonical(), a.port())
    }

    /// Identifies this connection among others to the same peer (local, not on the wire).
    pub fn id(&self) -> usize {
        self.quic.stable_id()
    }

    /// Queues an unreliable datagram and returns at once. When the small send buffer is full,
    /// quinn drops the **oldest** queued datagram, so the newest position always goes out.
    pub fn send_datagram(&self, data: Bytes) -> Result<(), DatagramError> {
        use quinn::SendDatagramError as E;
        let len = data.len();
        self.quic.send_datagram(data).map_err(|e| match e {
            E::UnsupportedByPeer | E::Disabled => DatagramError::Unsupported,
            E::TooLarge => DatagramError::TooLarge {
                len,
                max: self.quic.max_datagram_size().unwrap_or(0),
            },
            E::ConnectionLost(_) => DatagramError::Closed,
        })
    }

    /// Largest datagram the path takes now, or `None` if the peer refuses datagrams.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.quic.max_datagram_size()
    }

    /// Queues one frame on the control stream without waiting. Frames arrive in order, and
    /// each is delivered whole. Fails with [`FrameError::Full`] when the bounded queue
    /// (`EndpointConfig::frame_queue`, 256) is full, instead of blocking the caller.
    pub fn try_send_frame(&self, frame: Bytes) -> Result<(), FrameError> {
        if frame.is_empty() {
            return Err(FrameError::Empty);
        }
        if frame.len() > self.shared.max_frame {
            return Err(FrameError::TooLarge {
                len: frame.len(),
                max: self.shared.max_frame,
            });
        }
        if self.quic.close_reason().is_some() {
            return Err(FrameError::Closed);
        }
        self.shared
            .frames
            .try_send(Out::Frame(frame))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => FrameError::Full,
                mpsc::error::TrySendError::Closed(_) => FrameError::Closed,
            })
    }

    /// Closes the connection at once; the peer sees [`CloseReason::Peer`] with `code` and
    /// `reason`. Queued frames and datagrams that have not left are dropped; use
    /// [`Connection::close_graceful`] to deliver them first.
    pub fn close(&self, code: CloseCode, reason: &str) {
        let _ = self.shared.local_close.set((code, reason.to_string()));
        self.quic.close(code.0.into(), reason.as_bytes());
    }

    /// Delivers every queued frame, then closes with `code` and `reason`.
    ///
    /// It finishes the control stream after the queued frames and waits, up to `timeout`, for
    /// two things: the peer finishes its side in return (the peer's ferry-net does that as soon
    /// as its reader has taken every frame), and the peer acknowledges everything this side sent,
    /// the finish included. Only then does it close, so the peer always gets this side's finish
    /// before the close (SAS pairing's final round trip, relies on it).
    /// Returns `true` if both came in time, `false` if the timeout closed the connection anyway.
    /// Frames sent after this call fail with [`FrameError::Closed`].
    ///
    /// Await it on the endpoint's runtime, or from another thread with
    /// `runtime_handle.block_on(...)` while that runtime runs.
    pub async fn close_graceful(&self, code: CloseCode, reason: &str, timeout: Duration) -> bool {
        let mut peer_finished = self.shared.peer_finished.clone();
        let mut own_acked = self.shared.own_acked.clone();
        let answered = tokio::time::timeout(timeout, async {
            // The writer may have finished already, in answer to the peer's finish; then this
            // send finds it gone and the acknowledgement below still tells.
            let _ = self.shared.frames.send(Out::Finish).await;
            peer_finished.wait_for(|f| *f).await.is_ok() && own_acked.wait_for(|a| *a).await.is_ok()
        })
        .await
        .unwrap_or(false);
        self.close(code, reason);
        answered
    }

    /// Why the connection ended, or `None` while it is open.
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.quic
            .close_reason()
            .map(|e| close_reason(&e, self.shared.local_close.get()))
    }

    /// Resolves when the connection has ended.
    pub async fn closed(&self) -> CloseReason {
        let e = self.quic.closed().await;
        close_reason(&e, self.shared.local_close.get())
    }

    /// quinn's smoothed round-trip estimate.
    pub fn rtt(&self) -> Duration {
        self.quic.rtt()
    }

    /// quinn's view of the path, for diagnostics.
    pub fn path(&self) -> PathInfo {
        let p = self.quic.stats().path;
        PathInfo {
            rtt: p.rtt,
            cwnd: p.cwnd,
            congestion_events: p.congestion_events,
            lost_packets: p.lost_packets,
            sent_packets: p.sent_packets,
            current_mtu: p.current_mtu,
        }
    }
}

/// What [`Connection::path`] reports: quinn's path statistics, without quinn's types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathInfo {
    /// Smoothed round-trip estimate.
    pub rtt: Duration,
    /// Congestion window in bytes (after ferry's floor, `EndpointConfig::min_window`).
    pub cwnd: u64,
    /// Loss episodes that cut the window.
    pub congestion_events: u64,
    pub lost_packets: u64,
    pub sent_packets: u64,
    /// Largest UDP payload the path takes now.
    pub current_mtu: u16,
}

/// The inbound side of a connection. Owned by one consumer.
#[derive(Debug)]
pub struct Events {
    quic: quinn::Connection,
    frames: mpsc::Receiver<Bytes>,
    frames_done: bool,
    local_close: Arc<OnceLock<LocalClose>>,
    done: bool,
}

impl Events {
    /// The next datagram, frame or close. Returns `None` after [`Event::Closed`].
    ///
    /// Cancel-safe: dropping the future loses nothing, so it can sit in a `select!`.
    pub async fn next(&mut self) -> Option<Event> {
        if self.done {
            return None;
        }
        loop {
            tokio::select! {
                biased;
                f = self.frames.recv(), if !self.frames_done => match f {
                    Some(b) => return Some(Event::Frame(b)),
                    None => self.frames_done = true,
                },
                d = self.quic.read_datagram() => match d {
                    Ok(b) => return Some(Event::Datagram(b)),
                    Err(e) => {
                        // frames received before the close still come first
                        if !self.frames_done {
                            if let Some(b) = self.frames.recv().await {
                                return Some(Event::Frame(b));
                            }
                            self.frames_done = true;
                        }
                        self.done = true;
                        let local = self.local_close.get();
                        return Some(Event::Closed(close_reason(&e, local)));
                    }
                },
            }
        }
    }
}

/// The open exchange: the dialer opens the control stream and sends an empty frame; the
/// acceptor answers with an empty frame. Each side reports the connection only after it, so a
/// dialer whose certificate the peer refused never sees an open connection (a QUIC client
/// finishes its handshake before the server has checked the client's certificate).
const OPEN: [u8; 4] = 0u32.to_le_bytes();

pub(crate) struct Params {
    pub max_frame: usize,
    pub frame_queue: usize,
}

pub(crate) async fn establish(
    quic: quinn::Connection,
    peer: Fingerprint,
    dialed: bool,
    p: &Params,
) -> Result<NewConnection, ConnectError> {
    let local_close: Arc<OnceLock<LocalClose>> = Arc::new(OnceLock::new());
    let fail = |code: CloseCode, err: ConnectError| {
        let _ = local_close.set((code, String::new()));
        quic.close(code.0.into(), b"");
        err
    };
    // a dialer learns here that the peer refused its certificate (see OPEN)
    let dialed_fp = dialed.then_some(peer);
    let lost = |e: quinn::ConnectionError| ConnectError::from_handshake(&e, None, dialed_fp);

    let (mut send, mut recv) = if dialed {
        let (mut s, r) = quic.open_bi().await.map_err(lost)?;
        let _ = s.set_priority(0);
        s.write_all(&OPEN)
            .await
            .map_err(|e| write_err(&quic, e, dialed_fp))?;
        (s, r)
    } else {
        quic.accept_bi().await.map_err(lost)?
    };
    let mut hdr = [0u8; 4];
    if let Err(e) = recv.read_exact(&mut hdr).await {
        return Err(match quic.close_reason() {
            Some(c) => lost(c),
            None => fail(
                CloseCode::PROTOCOL,
                ConnectError::Other(format!("open: {e}")),
            ),
        });
    }
    if hdr != OPEN {
        return Err(fail(
            CloseCode::PROTOCOL,
            ConnectError::Other("open: unexpected first frame".into()),
        ));
    }
    if !dialed {
        let _ = send.set_priority(0);
        send.write_all(&OPEN)
            .await
            .map_err(|e| write_err(&quic, e, dialed_fp))?;
    }

    let (out_tx, out_rx) = mpsc::channel(p.frame_queue);
    let (in_tx, in_rx) = mpsc::channel(p.frame_queue.min(64));
    let (finished_tx, finished_rx) = watch::channel(false);
    let (acked_tx, acked_rx) = watch::channel(false);
    tokio::spawn(write_frames(send, out_rx, acked_tx));
    tokio::spawn(read_frames(Reader {
        quic: quic.clone(),
        recv,
        tx: in_tx,
        max_frame: p.max_frame,
        local_close: local_close.clone(),
        finished: finished_tx,
        answer: out_tx.downgrade(),
    }));
    Ok(NewConnection {
        conn: Connection {
            quic: quic.clone(),
            shared: Arc::new(Shared {
                peer,
                dialed,
                frames: out_tx,
                max_frame: p.max_frame,
                local_close: local_close.clone(),
                peer_finished: finished_rx,
                own_acked: acked_rx,
            }),
        },
        events: Events {
            quic,
            frames: in_rx,
            frames_done: false,
            local_close,
            done: false,
        },
    })
}

fn write_err(
    quic: &quinn::Connection,
    e: quinn::WriteError,
    dialed: Option<Fingerprint>,
) -> ConnectError {
    match quic.close_reason() {
        Some(c) => ConnectError::from_handshake(&c, None, dialed),
        None => ConnectError::Other(format!("open: {e}")),
    }
}

/// The control stream's length headers, and the frames read, are cut from buffers of this size
/// (A-11): one allocation per 1,024 headers sent, or per 4 KiB of frames read.
const SLAB: usize = 4096;

async fn write_frames(
    mut send: quinn::SendStream,
    mut rx: mpsc::Receiver<Out>,
    acked: watch::Sender<bool>,
) {
    // quinn takes a `Bytes` as it is, but copies a `&[u8]` into a new allocation (quinn-proto
    // 0.11.19 `ByteSlice::pop_chunk`), so a header written with `write_all` would still
    // allocate: it is cut from a slab instead.
    let mut hdrs = BytesMut::with_capacity(SLAB);
    while let Some(out) = rx.recv().await {
        match out {
            Out::Frame(body) => {
                if hdrs.capacity() < 4 {
                    hdrs.reserve(SLAB);
                }
                // the length was checked against the cap, which is far below u32::MAX
                hdrs.extend_from_slice(&(body.len() as u32).to_le_bytes());
                let hdr = hdrs.split_to(4).freeze();
                if send.write_all_chunks(&mut [hdr, body]).await.is_err() {
                    return;
                }
            }
            Out::Finish => break,
        }
    }
    drop(rx);
    if send.finish().is_ok() {
        // `None`: the peer acknowledged every byte and the finish (quinn's `stopped`)
        if let Ok(None) = send.stopped().await {
            acked.send_replace(true);
        }
    }
}

struct Reader {
    quic: quinn::Connection,
    recv: quinn::RecvStream,
    tx: mpsc::Sender<Bytes>,
    max_frame: usize,
    local_close: Arc<OnceLock<LocalClose>>,
    finished: watch::Sender<bool>,
    /// Weak, so the reader does not keep the writer alive after every handle is gone.
    answer: mpsc::WeakSender<Out>,
}

async fn read_frames(mut r: Reader) {
    let violation = |r: &Reader, code: CloseCode| {
        let _ = r.local_close.set((code, String::new()));
        r.quic.close(code.0.into(), b"");
    };
    // Every frame is read into this and split off it; a frame still held elsewhere keeps its
    // piece, and the buffer is reused in place once all of them are dropped.
    let mut buf = BytesMut::with_capacity(SLAB);
    loop {
        let mut hdr = [0u8; 4];
        let read = tokio::select! {
            _ = r.tx.closed() => return,
            read = r.recv.read_exact(&mut hdr) => read,
        };
        match read {
            Ok(()) => {}
            // The peer finished at a frame boundary: a graceful close. Every frame it sent is
            // already in the channel; finish our side in answer (Connection::close_graceful).
            Err(quinn::ReadExactError::FinishedEarly(0)) => {
                r.finished.send_replace(true);
                if let Some(answer) = r.answer.upgrade() {
                    let _ = answer.send(Out::Finish).await;
                }
                return;
            }
            Err(quinn::ReadExactError::FinishedEarly(_)) => {
                return violation(&r, CloseCode::PROTOCOL);
            }
            Err(_) => return,
        }
        let len = u32::from_le_bytes(hdr) as usize;
        if len == 0 {
            return violation(&r, CloseCode::PROTOCOL);
        }
        if len > r.max_frame {
            tracing::debug!(len, max_frame = r.max_frame, "peer sent an oversized frame");
            return violation(&r, CloseCode::FRAME_TOO_LARGE);
        }
        if buf.capacity() < len {
            buf.reserve(SLAB.max(len));
        }
        buf.resize(len, 0);
        tokio::select! {
            _ = r.tx.closed() => return,
            read = r.recv.read_exact(&mut buf) => match read {
                Ok(()) => {}
                Err(quinn::ReadExactError::FinishedEarly(_)) => {
                    return violation(&r, CloseCode::PROTOCOL);
                }
                Err(_) => return,
            },
        }
        if r.tx.send(buf.split_to(len).freeze()).await.is_err() {
            return;
        }
    }
}
