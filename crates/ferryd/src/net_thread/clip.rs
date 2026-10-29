//! Clipboard sharing on the net thread (FR-CLIP-01–07).
//!
//! Offers go only to the **partner**: the peer this one last crossed into, or was last entered
//! by. The clipboard is not broadcast to every trusted peer on every copy, which
//! matters because a clipboard-history watcher on a peer (Omarchy runs `wl-paste --watch`)
//! pastes every item it is offered at once.
//!
//! | Event | What happens |
//! |---|---|
//! | this peer's source crosses into a peer | it becomes the partner and gets the local offer if it missed it ([`NetCmd::ClipEntered`](crate::chan::NetCmd::ClipEntered)) |
//! | a peer enters this one | it becomes the partner |
//! | a local application copies | the shareable types ([`clip::shareable`]) go as `ClipOffer` to the partner |
//! | `ClipOffer` from a peer | ferry becomes the selection with those types, on the peer's behalf |
//! | an application pastes from that selection | `ClipRequest` to the peer; its data stream is copied into the application's pipe |
//! | `ClipRequest` from a peer | the item is read from the local selection (at most `max_bytes`), then sent on a data stream, or refused with `ClipAbort` |
//! | the offering peer goes down | ferry withdraws its selection |
//!
//! The last item received is kept (one, at most `max_bytes`), so pasting the same offer again,
//! as clipboard-history watchers and pollers do, does not cross the network again.
//!
//! Data moves only when an application pastes. No byte of it, and no pasted text, is ever
//! logged: the log names types, sizes and reasons.
//!
//! Sharing off means no bridge thread at all: nothing is offered, offers are ignored and
//! requests are refused. The bridge starts off the net thread, since it waits for the
//! compositor.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ferry_core::wire::Control;
use ferry_core::wire::clip::{self, AbortReason, DataHeader, TransferIds};
use ferry_net::{Connection, DataRecv, Fingerprint};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::Net;
use crate::clip::{ClipBridge, ClipEvent, ClipStarter, Received};
use crate::ratelimit::log_limited;

/// A paste whose data has not started within this is given up: the application reads EOF.
const PENDING_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the local selection's owner may take to hand over its pipe, and then its bytes.
const READ_WAIT: Duration = Duration::from_secs(10);
/// A data stream whose header does not come within this is dropped.
const HEADER_WAIT: Duration = Duration::from_secs(5);
/// Requests one peer may have this peer serve at once: each holds its item in memory.
const MAX_SERVING: usize = 4;
/// `stop` / `reset` code on a data stream nobody waits for.
const CANCELLED: u32 = 1;
const CHUNK: usize = 256 * 1024;

/// How clipboard sharing starts (`NetInit::clip`).
pub struct ClipInit {
    pub starter: Option<ClipStarter>,
    pub enabled: bool,
    pub max_bytes: u64,
}

impl Default for ClipInit {
    fn default() -> Self {
        Self {
            starter: None,
            enabled: false,
            max_bytes: 32 << 20,
        }
    }
}

/// Back to the loop from the spawned tasks.
pub(super) enum ClipIn {
    Started {
        starter: ClipStarter,
        result: Result<Box<dyn ClipBridge>, String>,
    },
    Data {
        peer: Fingerprint,
        conn_id: usize,
        header: DataHeader,
        recv: DataRecv,
    },
    Served {
        peer: Fingerprint,
    },
    /// A paste completed: its bytes, for the next paste of the same offer.
    Received {
        key: CacheKey,
        data: Bytes,
    },
}

/// Which offer and type a cached item is: the peer, its serial, the mime type.
pub(super) type CacheKey = (Fingerprint, u32, String);

/// The offer ferry holds the selection for.
struct Remote {
    peer: Fingerprint,
    serial: u32,
}

struct Pending {
    fd: OwnedFd,
    since: Instant,
    key: CacheKey,
}

pub(super) struct ClipState {
    starter: Option<ClipStarter>,
    bridge: Option<Box<dyn ClipBridge>>,
    starting: bool,
    enabled: bool,
    max_bytes: u64,
    /// +1 per local selection change.
    serial: u32,
    /// The local selection's shareable types, while a local application owns it.
    local: Option<(u32, Vec<String>)>,
    /// The local serial each up peer was last offered.
    sent: HashMap<Fingerprint, u32>,
    /// The peer the pointer last crossed to or came from.
    partner: Option<Fingerprint>,
    remote: Option<Remote>,
    ids: HashMap<Fingerprint, TransferIds>,
    pending: HashMap<(Fingerprint, u32), Pending>,
    serving: HashMap<Fingerprint, usize>,
    cache: Option<(CacheKey, Bytes)>,
    pub(super) ev_tx: mpsc::UnboundedSender<ClipEvent>,
    pub(super) in_tx: mpsc::UnboundedSender<ClipIn>,
}

impl ClipState {
    pub(super) fn new(
        init: ClipInit,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<ClipEvent>,
        mpsc::UnboundedReceiver<ClipIn>,
    ) {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        (
            Self {
                starter: init.starter,
                bridge: None,
                starting: false,
                enabled: init.enabled,
                max_bytes: init.max_bytes,
                serial: 0,
                local: None,
                sent: HashMap::new(),
                partner: None,
                remote: None,
                ids: HashMap::new(),
                pending: HashMap::new(),
                serving: HashMap::new(),
                cache: None,
                ev_tx,
                in_tx,
            },
            ev_rx,
            in_rx,
        )
    }

    /// Sharing is on and the bridge is up.
    fn active(&self) -> bool {
        self.enabled && self.bridge.is_some()
    }

    fn start_bridge(&mut self) {
        if !self.enabled || self.bridge.is_some() || self.starting {
            return;
        }
        let Some(mut starter) = self.starter.take() else {
            return;
        };
        self.starting = true;
        let (ev_tx, in_tx) = (self.ev_tx.clone(), self.in_tx.clone());
        tokio::task::spawn_blocking(move || {
            let result = starter(ev_tx);
            let _ = in_tx.send(ClipIn::Started { starter, result });
        });
    }

    /// Drops the bridge (its thread joins off the net thread) and everything it held.
    fn stop_bridge(&mut self) {
        if let Some(b) = self.bridge.take() {
            // A peer's item held as the selection goes with it; a local application's stays.
            if self.remote.is_some() {
                b.offer(Vec::new());
            }
            tokio::task::spawn_blocking(move || drop(b));
        }
        self.local = None;
        self.remote = None;
        self.sent.clear();
        self.pending.clear();
        self.cache = None;
    }
}

impl Net {
    /// `peer` is up and takes the clipboard messages.
    fn clip_peer(&self, peer: &Fingerprint) -> bool {
        self.peers.get(peer).is_some_and(|p| {
            p.up && p
                .features
                .iter()
                .any(|f| f == ferry_core::wire::FEATURE_CLIPBOARD)
        })
    }

    /// The local offer to the partner, unless it has it already.
    fn clip_offer_to_partner(&mut self) {
        let (Some(peer), Some((serial, mimes))) = (self.clip.partner, self.clip.local.clone())
        else {
            return;
        };
        if !self.clip.active()
            || self.clip.sent.get(&peer) == Some(&serial)
            || !self.clip_peer(&peer)
        {
            return;
        }
        tracing::debug!(peer = %peer.short(), serial, types = ?mimes, "clipboard: offering");
        self.send(&peer, &Control::ClipOffer { serial, mimes });
        self.clip.sent.insert(peer, serial);
        self.live.bump("clip_offers_sent");
    }

    /// Start-up, after the TargetMachine started.
    pub(super) fn clip_start(&mut self) {
        tracing::info!(
            enabled = self.clip.enabled,
            bridge = self.clip.starter.is_some(),
            max_bytes = self.clip.max_bytes,
            "clipboard"
        );
        self.clip.start_bridge();
    }

    /// `config.set clipboard.*`, live.
    pub(super) fn clip_config(&mut self, enabled: bool, max_bytes: u64) {
        let was = self.clip.enabled;
        self.clip.enabled = enabled;
        self.clip.max_bytes = max_bytes;
        if enabled && !was {
            tracing::info!("clipboard sharing on");
            self.clip.start_bridge();
        } else if !enabled && was {
            tracing::info!("clipboard sharing off");
            self.clip.stop_bridge();
        }
    }

    pub(super) fn clip_peer_up(&mut self, peer: Fingerprint, conn: &Connection) {
        self.clip
            .ids
            .insert(peer, TransferIds::new(conn.is_dialer()));
        tokio::spawn(accept_data(conn.clone(), self.clip.in_tx.clone()));
    }

    pub(super) fn clip_peer_down(&mut self, peer: Fingerprint) {
        self.clip.ids.remove(&peer);
        self.clip.sent.remove(&peer);
        self.clip.serving.remove(&peer);
        if self.clip.partner == Some(peer) {
            self.clip.partner = None;
        }
        self.clip.pending.retain(|(p, _), _| *p != peer);
        if self
            .clip
            .cache
            .as_ref()
            .is_some_and(|((p, _, _), _)| *p == peer)
        {
            self.clip.cache = None;
        }
        if self.clip.remote.as_ref().is_some_and(|r| r.peer == peer) {
            // Its item cannot be pasted any more.
            self.clip.remote = None;
            if let Some(b) = &self.clip.bridge {
                b.offer(Vec::new());
            }
        }
    }

    /// The pointer crossed between this peer and `peer`, either way: `peer` is the partner now.
    /// When this peer's source crossed into it, it also gets the offer it missed (ADR 0013: "on
    /// `Enter` the source sends its current offer").
    pub(super) fn clip_partner(&mut self, peer: Fingerprint, entered_it: bool) {
        if self.clip.partner != Some(peer) {
            tracing::debug!(peer = %peer.short(), "clipboard: partner");
        }
        self.clip.partner = Some(peer);
        if entered_it {
            self.clip_offer_to_partner();
        }
    }

    pub(super) fn clip_tick(&mut self) {
        let now = Instant::now();
        let before = self.clip.pending.len();
        self.clip
            .pending
            .retain(|_, p| now.duration_since(p.since) < PENDING_TIMEOUT);
        let gone = before - self.clip.pending.len();
        if gone > 0 {
            tracing::info!(
                pastes = gone,
                "clipboard: no data came in time; the paste is empty"
            );
            self.live.add("clip_paste_timeouts", gone as u64);
        }
    }

    pub(super) fn clip_event(&mut self, ev: ClipEvent) {
        match ev {
            ClipEvent::Selection(mimes) => {
                // A local application owns the selection now, or nobody does.
                self.clip.remote = None;
                self.clip.cache = None;
                self.clip.serial = self.clip.serial.wrapping_add(1);
                let serial = self.clip.serial;
                self.clip.local = clip::shareable(&mimes).map(|m| (serial, m));
                if self.clip.local.is_none() {
                    tracing::debug!(types = mimes.len(), "clipboard: local selection not shared");
                    return;
                }
                self.clip_offer_to_partner();
            }
            ClipEvent::Paste { mime, fd } => {
                let Some(r) = self.clip.remote.as_ref() else {
                    // Dropping `fd` gives the application EOF.
                    return;
                };
                let (peer, serial) = (r.peer, r.serial);
                let key = (peer, serial, mime);
                if let Some((_, data)) = self.clip.cache.as_ref().filter(|(k, _)| *k == key) {
                    let data = data.clone();
                    self.live.bump("clip_pastes_cached");
                    tokio::spawn(async move {
                        if let Err(e) = write_all_to(fd, data).await {
                            tracing::debug!(error = %e, "clipboard: cached paste cut short");
                        }
                    });
                    return;
                }
                let mime = key.2.clone();
                let Some(id) = self.clip.ids.get_mut(&peer).map(TransferIds::next_id) else {
                    return;
                };
                tracing::debug!(peer = %peer.short(), id, %mime, "clipboard: paste, requesting");
                self.send(&peer, &Control::ClipRequest { serial, id, mime });
                self.clip.pending.insert(
                    (peer, id),
                    Pending {
                        fd,
                        since: Instant::now(),
                        key,
                    },
                );
                self.live.bump("clip_requests_sent");
            }
            ClipEvent::Finished => {
                tracing::warn!(
                    "clipboard: the compositor ended the data device; sharing stops until ferryd restarts"
                );
                self.clip.bridge = None;
                self.clip.stop_bridge();
            }
        }
    }

    pub(super) fn clip_in(&mut self, m: ClipIn) {
        match m {
            ClipIn::Started { starter, result } => {
                self.clip.starting = false;
                self.clip.starter = Some(starter);
                match result {
                    Ok(b) if self.clip.enabled => {
                        tracing::info!("clipboard bridge up");
                        self.clip.bridge = Some(b);
                    }
                    // turned off while it started
                    Ok(b) => drop(tokio::task::spawn_blocking(move || drop(b))),
                    Err(e) => {
                        tracing::warn!(error = %e, "clipboard bridge failed to start; nothing is shared");
                        self.live.bump("clip_bridge_failed");
                    }
                }
            }
            ClipIn::Data {
                peer,
                conn_id,
                header,
                recv,
            } => {
                let live_conn = self
                    .peers
                    .get(&peer)
                    .is_some_and(|p| p.conn.id() == conn_id);
                let Some(p) = self
                    .clip
                    .pending
                    .remove(&(peer, header.id))
                    .filter(|_| live_conn)
                else {
                    recv.stop(CANCELLED);
                    return;
                };
                if header.len > self.clip.max_bytes {
                    tracing::warn!(
                        peer = %peer.short(),
                        bytes = header.len,
                        max_bytes = self.clip.max_bytes,
                        "clipboard: the item is too large to share (clipboard.max_bytes); the paste is empty"
                    );
                    self.live.bump("clip_too_large");
                    recv.stop(CANCELLED);
                    return;
                }
                let (live, tx, key) = (self.live.clone(), self.clip.in_tx.clone(), p.key);
                tokio::spawn(async move {
                    match deliver(recv, header.len, p.fd).await {
                        Ok(data) => {
                            tracing::debug!(peer = %peer.short(), id = header.id, bytes = header.len, "clipboard: pasted");
                            live.bump("clip_pastes");
                            let _ = tx.send(ClipIn::Received { key, data });
                        }
                        Err(e) => {
                            tracing::info!(peer = %peer.short(), id = header.id, error = %e, "clipboard: paste cut short");
                            live.bump("clip_pastes_failed");
                        }
                    }
                });
            }
            ClipIn::Served { peer } => {
                if let Some(n) = self.clip.serving.get_mut(&peer) {
                    *n = n.saturating_sub(1);
                }
            }
            ClipIn::Received { key, data } => {
                // only while it is still the item ferry holds the selection for
                if self
                    .clip
                    .remote
                    .as_ref()
                    .is_some_and(|r| r.peer == key.0 && r.serial == key.1)
                {
                    self.clip.cache = Some((key, data));
                }
            }
        }
    }

    pub(super) fn clip_frame(&mut self, peer: Fingerprint, msg: Control) {
        match msg {
            Control::ClipOffer { serial, mimes } => {
                self.live.bump("clip_offers_received");
                if !self.clip.active() {
                    return;
                }
                // The peer filtered them; this side holds them to its own rule too.
                let Some(mimes) = clip::shareable(&mimes) else {
                    return;
                };
                tracing::debug!(peer = %peer.short(), serial, types = ?mimes, "clipboard: peer offers");
                if let Some(b) = &self.clip.bridge {
                    b.offer(mimes);
                }
                self.clip.local = None;
                self.clip.cache = None;
                self.clip.remote = Some(Remote { peer, serial });
            }
            Control::ClipRequest { serial, id, mime } => {
                self.live.bump("clip_requests_received");
                let abort = |net: &mut Self, reason: AbortReason| {
                    tracing::debug!(peer = %peer.short(), id, reason = reason.as_str(), "clipboard: request refused");
                    net.send(
                        &peer,
                        &Control::ClipAbort {
                            id,
                            reason: reason.code(),
                        },
                    );
                };
                let Some(bridge) = self.clip.bridge.as_ref().filter(|_| self.clip.enabled) else {
                    return abort(self, AbortReason::Unavailable);
                };
                match &self.clip.local {
                    Some((s, m)) if *s == serial && m.contains(&mime) => {}
                    _ => return abort(self, AbortReason::Stale),
                }
                let serving = self.clip.serving.entry(peer).or_default();
                if *serving >= MAX_SERVING {
                    return abort(self, AbortReason::Unavailable);
                }
                let Some(conn) = self.peers.get(&peer).map(|p| p.conn.clone()) else {
                    return;
                };
                *serving += 1;
                let received = bridge.receive(mime);
                let (max, tx, live) = (
                    self.clip.max_bytes,
                    self.clip.in_tx.clone(),
                    self.live.clone(),
                );
                tokio::spawn(async move {
                    serve(&conn, id, received, max, &live).await;
                    let _ = tx.send(ClipIn::Served { peer });
                });
            }
            Control::ClipAbort { id, reason } => {
                let dialer = self.peers.get(&peer).is_some_and(|p| p.conn.is_dialer());
                if !TransferIds::is_ours(dialer, id) {
                    return;
                }
                if self.clip.pending.remove(&(peer, id)).is_none() {
                    return;
                }
                let reason = AbortReason::from_code(reason);
                self.live.bump("clip_pastes_refused");
                if reason == AbortReason::TooLarge {
                    log_limited!(warn, peer = %peer.short(), "clipboard: the item is too large to share (the other machine's clipboard.max_bytes); the paste is empty");
                } else {
                    tracing::info!(peer = %peer.short(), id, reason = reason.as_str(), "clipboard: the peer refused the paste");
                }
            }
            _ => {}
        }
    }
}

/// Every data stream the peer opens on `conn`, with its header, back to the loop.
async fn accept_data(conn: Connection, tx: mpsc::UnboundedSender<ClipIn>) {
    let (peer, conn_id) = (conn.peer_fp(), conn.id());
    while let Ok(mut recv) = conn.accept_data().await {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut h = [0u8; DataHeader::LEN];
            match tokio::time::timeout(HEADER_WAIT, recv.read_exact(&mut h)).await {
                Ok(Ok(())) => {
                    let _ = tx.send(ClipIn::Data {
                        peer,
                        conn_id,
                        header: DataHeader::decode(&h),
                        recv,
                    });
                }
                _ => recv.stop(CANCELLED),
            }
        });
    }
}

enum ServeError {
    /// Refused before any data went: the requester gets `ClipAbort`.
    Abort(AbortReason, String),
    /// The data stream failed; the requester sees that on the stream.
    Stream(String),
}

/// Serves request `id`: the item from the local selection, then a data stream.
async fn serve(
    conn: &Connection,
    id: u32,
    received: Received,
    max: u64,
    live: &crate::live::SharedLive,
) {
    let peer = conn.peer_fp();
    match serve_inner(conn, id, received, max).await {
        Ok(len) => {
            tracing::debug!(peer = %peer.short(), id, bytes = len, "clipboard: served");
            live.bump("clip_served");
        }
        Err(ServeError::Abort(reason, why)) => {
            if reason == AbortReason::TooLarge {
                log_limited!(warn, peer = %peer.short(), max_bytes = max, "clipboard: the item is too large to share (clipboard.max_bytes)");
            } else {
                tracing::info!(peer = %peer.short(), id, reason = reason.as_str(), why, "clipboard: cannot serve the paste");
            }
            live.bump("clip_serve_refused");
            let mut buf = Vec::new();
            Control::ClipAbort {
                id,
                reason: reason.code(),
            }
            .encode(&mut buf);
            let _ = conn.try_send_frame(buf.into());
        }
        Err(ServeError::Stream(why)) => {
            tracing::info!(peer = %peer.short(), id, why, "clipboard: sending cut short");
            live.bump("clip_serve_failed");
        }
    }
}

async fn serve_inner(
    conn: &Connection,
    id: u32,
    received: Received,
    max: u64,
) -> Result<u64, ServeError> {
    let unavailable = |why: String| ServeError::Abort(AbortReason::Unavailable, why);
    let fd = tokio::time::timeout(READ_WAIT, received)
        .await
        .map_err(|_| unavailable("the selection's owner did not answer".into()))?
        .map_err(unavailable)?;
    let data = tokio::time::timeout(READ_WAIT, read_capped(fd, max))
        .await
        .map_err(|_| unavailable("the selection's owner sent too slowly".into()))?
        .map_err(|e| unavailable(e.to_string()))?
        .ok_or_else(|| ServeError::Abort(AbortReason::TooLarge, String::new()))?;
    let len = data.len() as u64;
    let stream = |e: ferry_net::StreamError| ServeError::Stream(e.to_string());
    let mut s = conn.open_data().await.map_err(stream)?;
    s.write_all(&DataHeader { id, len }.encode())
        .await
        .map_err(stream)?;
    s.write_chunk(Bytes::from(data)).await.map_err(stream)?;
    s.finish();
    Ok(len)
}

/// The whole item from a pipe, or `None` when it is larger than `max`.
async fn read_capped(fd: OwnedFd, max: u64) -> std::io::Result<Option<Vec<u8>>> {
    let pipe = tokio::net::unix::pipe::Receiver::from_owned_fd(fd)?;
    let mut data = Vec::new();
    pipe.take(max.saturating_add(1))
        .read_to_end(&mut data)
        .await?;
    Ok((data.len() as u64 <= max).then_some(data))
}

/// Copies a data stream of `len` bytes into the pasting application's `fd`, then closes it.
/// Returns the bytes, for the cache.
async fn deliver(mut recv: DataRecv, len: u64, fd: OwnedFd) -> Result<Bytes, String> {
    let file = std::fs::File::from(fd);
    let fifo = file.metadata().is_ok_and(|m| m.file_type().is_fifo());
    let (mut w, file) = if fifo {
        let w = tokio::net::unix::pipe::Sender::from_owned_fd(file.into())
            .map_err(|e| e.to_string())?;
        (Some(w), None)
    } else {
        (None, Some(file))
    };
    // `len` was checked against max_bytes
    let mut data = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    while let Some(c) = recv.read_chunk(CHUNK).await.map_err(|e| e.to_string())? {
        if (data.len() + c.len()) as u64 > len {
            recv.stop(CANCELLED);
            return Err("more bytes than the header said".into());
        }
        if let Some(w) = w.as_mut()
            && let Err(e) = w.write_all(&c).await
        {
            // the application stopped reading
            recv.stop(CANCELLED);
            return Err(e.to_string());
        }
        data.extend_from_slice(&c);
    }
    if data.len() as u64 != len {
        return Err(format!("{} of {len} bytes", data.len()));
    }
    let data = Bytes::from(data);
    if let Some(file) = file {
        // Not a pipe (a file, a socket): write it off the runtime.
        write_blocking(file, data.clone()).await?;
    }
    Ok(data)
}

/// Writes a cached item into a pasting application's `fd`, then closes it.
async fn write_all_to(fd: OwnedFd, data: Bytes) -> Result<(), String> {
    let file = std::fs::File::from(fd);
    if file.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
        let mut w = tokio::net::unix::pipe::Sender::from_owned_fd(file.into())
            .map_err(|e| e.to_string())?;
        w.write_all(&data).await.map_err(|e| e.to_string())
    } else {
        write_blocking(file, data).await
    }
}

async fn write_blocking(mut file: std::fs::File, data: Bytes) -> Result<(), String> {
    tokio::task::spawn_blocking(move || std::io::Write::write_all(&mut file, &data))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
