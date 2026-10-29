// from gliff crates/hypr-input/src/clipboard.rs @ 089662f
//! Clipboard bridge via `ext-data-control-v1`, on its own thread and Wayland
//! connection. It moves no data itself: it reports which mime types the compositor's selection
//! offers and hands out a pipe to read one of them; in the other direction it advertises a set
//! of mime types on behalf of a peer and hands the owner the pipe of every application that
//! pastes. Every source it sets carries [`OWN_SOURCE_MIME`], so a selection of ferry's is never
//! reported back as new and never read back into itself.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc;

use calloop::channel::{self, Sender};
use rustix::pipe::{PipeFlags, pipe_with};
use tokio::sync::oneshot;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self as device, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self as offer, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self as source, ExtDataControlSourceV1,
};

use ferry_wl::{LoopState, Seat, Target};

/// Marks a selection as set by [`Clipboard::offer`]. It carries no data. A clipboard manager
/// that republishes ferry's selection keeps the marker, so its copy of the peer's item is not
/// reported as new either.
pub const OWN_SOURCE_MIME: &str = "application/x-ferry-source";

/// The global this crate needs (`ferry doctor`).
pub const GLOBAL: &str = "ext_data_control_manager_v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Wayland(#[from] ferry_wl::Error),
    #[error("the clipboard thread is gone")]
    ThreadGone,
    #[error("there is no selection")]
    NoSelection,
    #[error("the selection is ferry's own")]
    OwnSelection,
    #[error("the selection has no {0}")]
    NotOffered(String),
    #[error("pipe: {0}")]
    Pipe(std::io::Error),
    #[error("thread: {0}")]
    Thread(std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum ClipboardEvent {
    /// The compositor's selection changed to one ferry did not set, offering these mime types
    /// (empty when it was cleared). Read one with [`Clipboard::receive`].
    Selection { mime_types: Vec<String> },
    /// An application pastes from the selection set with [`Clipboard::offer`]: write the item
    /// in `mime_type` to `fd` and close it.
    Paste { mime_type: String, fd: OwnedFd },
    /// The compositor ended the data device; the bridge has stopped.
    Finished,
}

pub type ClipboardSink = Box<dyn FnMut(ClipboardEvent) + Send>;

enum Cmd {
    Offer(Vec<String>),
    Receive {
        mime_type: String,
        reply: oneshot::Sender<Result<OwnedFd>>,
    },
    Stop,
}

/// Handle to the clipboard thread. Dropping it stops the thread.
pub struct Clipboard {
    cmd: Sender<Cmd>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Clipboard {
    /// Connects to the compositor, binds the data-control manager and starts the thread.
    /// Fails when the compositor does not offer [`GLOBAL`].
    pub fn start(target: Target, sink: ClipboardSink) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (cmd, rx) = channel::channel::<Cmd>();
        let join = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut sink = sink;
                if let Err(e) = run(target, &mut sink, rx, &ready_tx) {
                    tracing::warn!(error = %e, "clipboard bridge ended");
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(Error::Thread)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(Error::ThreadGone),
        }
    }

    /// Become the compositor's selection, offering `mime_types`; each paste arrives as
    /// [`ClipboardEvent::Paste`]. An empty list withdraws ferry's selection if it still holds it.
    pub fn offer(&self, mime_types: Vec<String>) {
        let _ = self.cmd.send(Cmd::Offer(mime_types));
    }

    /// Ask the current selection's owner for `mime_type`; the pipe yields the bytes, then EOF.
    /// The answer comes from the clipboard thread, so awaiting it never blocks a runtime.
    pub fn receive(&self, mime_type: String) -> oneshot::Receiver<Result<OwnedFd>> {
        let (reply, rx) = oneshot::channel();
        if let Err(mpsc::SendError(Cmd::Receive { reply, .. })) =
            self.cmd.send(Cmd::Receive { mime_type, reply })
        {
            let _ = reply.send(Err(Error::ThreadGone));
        }
        rx
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct State {
    sink: ClipboardSink,
    conn: Connection,
    qh: QueueHandle<State>,
    seat: Seat,
    manager: ExtDataControlManagerV1,
    device: Option<ExtDataControlDeviceV1>,
    /// mimes advertised by each live incoming offer.
    offers: HashMap<ExtDataControlOfferV1, Vec<String>>,
    /// the selection's current offer, kept until the selection changes.
    current: Option<ExtDataControlOfferV1>,
    our_source: Option<ExtDataControlSourceV1>,
    echo: EchoGuard,
    quit: bool,
}

fn run(
    target: Target,
    sink: &mut ClipboardSink,
    rx: channel::Channel<Cmd>,
    ready_tx: &mpsc::Sender<Result<()>>,
) -> Result<()> {
    let (conn, globals, mut queue) = ferry_wl::init::<State>(&target)?;
    let qh = queue.handle();
    let manager: ExtDataControlManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| ferry_wl::Error::MissingGlobal(GLOBAL))?;
    let seat = Seat::bind(&globals, &qh)?;

    let mut state = State {
        sink: Box::new(|_| {}),
        conn: conn.clone(),
        qh: qh.clone(),
        seat,
        manager,
        device: None,
        offers: HashMap::new(),
        current: None,
        our_source: None,
        echo: EchoGuard::default(),
        quit: false,
    };
    std::mem::swap(&mut state.sink, sink);
    let result = (|| {
        queue
            .roundtrip(&mut state)
            .map_err(|e| ferry_wl::Error::Wayland(e.to_string()))?;
        let seat = state.seat.wl_seat()?.clone();
        let device = state.manager.get_data_device(&seat, &qh, ());
        state.device = Some(device);
        let _ = ready_tx.send(Ok(()));
        ferry_wl::run_loop(conn, queue, rx, &mut state)
    })();
    std::mem::swap(&mut state.sink, sink);
    Ok(result?)
}

impl LoopState for State {
    type Cmd = Cmd;

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Offer(mimes) if mimes.is_empty() => self.withdraw(),
            Cmd::Offer(mimes) => self.set_selection(mimes),
            Cmd::Receive { mime_type, reply } => {
                let _ = reply.send(self.receive(&mime_type));
            }
            Cmd::Stop => self.stop(),
        }
        let _ = self.conn.flush();
    }

    fn stop(&mut self) {
        self.quit = true;
    }

    fn stop_requested(&self) -> bool {
        self.quit
    }
}

impl State {
    fn set_selection(&mut self, mimes: Vec<String>) {
        let src = self.manager.create_data_source(&self.qh, ());
        for m in &mimes {
            src.offer(m.clone());
        }
        src.offer(OWN_SOURCE_MIME.to_string());
        if let Some(device) = &self.device {
            device.set_selection(Some(&src));
        }
        // The old source goes only once it is replaced: destroying the current selection's
        // source clears the selection first.
        if let Some(old) = self.our_source.replace(src) {
            old.destroy();
        }
    }

    fn withdraw(&mut self) {
        if let Some(old) = self.our_source.take() {
            if let Some(device) = &self.device {
                device.set_selection(None);
            }
            old.destroy();
            let withdrawal = self.echo.withdrawn();
            self.conn.display().sync(&self.qh, withdrawal);
        }
    }

    fn receive(&mut self, mime: &str) -> Result<OwnedFd> {
        let off = self.current.as_ref().ok_or(Error::NoSelection)?;
        let mimes = self.offers.get(off).map(Vec::as_slice).unwrap_or_default();
        if is_own_selection(mimes) {
            return Err(Error::OwnSelection);
        }
        if !mimes.iter().any(|a| a == mime) {
            return Err(Error::NotOffered(mime.to_string()));
        }
        let (read_fd, write_fd) =
            pipe_with(PipeFlags::CLOEXEC).map_err(|e| Error::Pipe(e.into()))?;
        off.receive(mime.to_string(), write_fd.as_fd());
        drop(write_fd);
        let _ = self.conn.flush();
        Ok(read_fd)
    }

    fn on_selection(&mut self, id: Option<ExtDataControlOfferV1>) {
        // Every offer but the new selection's is stale; destroy them so offer proxies and map
        // entries do not accumulate.
        let stale: Vec<_> = self
            .offers
            .keys()
            .filter(|o| Some(*o) != id.as_ref())
            .cloned()
            .collect();
        for off in stale {
            self.offers.remove(&off);
            off.destroy();
        }
        self.current = id.clone();
        let mimes = id
            .as_ref()
            .and_then(|o| self.offers.get(o))
            .cloned()
            .unwrap_or_default();
        if !self.echo.is_echo(&mimes) {
            (self.sink)(ClipboardEvent::Selection { mime_types: mimes });
        }
    }

    fn clear_offers(&mut self) {
        for (off, _) in self.offers.drain() {
            off.destroy();
        }
        self.current = None;
    }
}

fn is_own_selection(mimes: &[String]) -> bool {
    mimes.iter().any(|m| m == OWN_SOURCE_MIME)
}

/// Tells the compositor's reports of ferry's own changes from changes made by others: ferry's
/// source comes back as a selection that lists the marker, and a withdrawal as one empty
/// selection. A withdrawal of a selection that was already cleared has no echo, so each one
/// expires at the display sync that follows it.
#[derive(Default)]
struct EchoGuard {
    withdrawn: u64,
    settled: u64,
}

impl EchoGuard {
    fn withdrawn(&mut self) -> u64 {
        self.withdrawn += 1;
        self.withdrawn
    }

    fn settle(&mut self, withdrawal: u64) {
        self.settled = self.settled.max(withdrawal);
    }

    fn is_echo(&mut self, mimes: &[String]) -> bool {
        if !mimes.is_empty() {
            return is_own_selection(mimes);
        }
        let pending = self.settled < self.withdrawn;
        if pending {
            self.settled += 1;
        }
        pending
    }
}

impl Dispatch<WlCallback, u64> for State {
    fn event(
        s: &mut Self,
        _: &WlCallback,
        e: wl_callback::Event,
        withdrawal: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = e {
            s.echo.settle(*withdrawal);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        s: &mut Self,
        _: &WlSeat,
        e: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.seat.handle(e);
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &ExtDataControlDeviceV1,
        e: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            device::Event::DataOffer { id } => {
                s.offers.insert(id, Vec::new());
            }
            device::Event::Selection { id } => s.on_selection(id),
            device::Event::Finished => {
                s.clear_offers();
                (s.sink)(ClipboardEvent::Finished);
                s.quit = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ExtDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(
        s: &mut Self,
        off: &ExtDataControlOfferV1,
        e: offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let offer::Event::Offer { mime_type } = e {
            s.offers.entry(off.clone()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(
        s: &mut Self,
        src: &ExtDataControlSourceV1,
        e: source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ours = s.our_source.as_ref() == Some(src);
        match e {
            source::Event::Send { mime_type, fd } if ours => {
                if mime_type != OWN_SOURCE_MIME {
                    (s.sink)(ClipboardEvent::Paste { mime_type, fd });
                }
            }
            source::Event::Cancelled if ours => {
                s.our_source = None;
            }
            source::Event::Cancelled => src.destroy(),
            _ => {}
        }
    }
}

delegate_noop!(State: ExtDataControlManagerV1);

#[cfg(test)]
mod tests {
    use super::{EchoGuard, OWN_SOURCE_MIME};

    fn mimes(list: &[&str]) -> Vec<String> {
        list.iter().map(|m| m.to_string()).collect()
    }

    #[test]
    fn a_selection_with_our_marker_is_an_echo() {
        let mut guard = EchoGuard::default();
        let ours = mimes(&["image/png", OWN_SOURCE_MIME]);
        assert!(guard.is_echo(&ours));
        assert!(guard.is_echo(&ours));
    }

    #[test]
    fn a_selection_without_our_marker_is_new() {
        let mut guard = EchoGuard::default();
        assert!(!guard.is_echo(&mimes(&["image/png"])));
    }

    #[test]
    fn an_empty_selection_is_an_echo_only_after_a_withdrawal() {
        let mut guard = EchoGuard::default();
        assert!(!guard.is_echo(&[]));
        guard.withdrawn();
        assert!(guard.is_echo(&[]));
        assert!(!guard.is_echo(&[]));
    }

    #[test]
    fn a_withdrawal_queued_behind_our_selection_is_still_an_echo() {
        let mut guard = EchoGuard::default();
        guard.withdrawn();
        assert!(guard.is_echo(&mimes(&["text/plain", OWN_SOURCE_MIME])));
        assert!(guard.is_echo(&[]));
    }

    #[test]
    fn a_withdrawal_without_an_echo_expires_at_its_sync() {
        let mut guard = EchoGuard::default();
        let withdrawal = guard.withdrawn();
        guard.settle(withdrawal);
        assert!(!guard.is_echo(&[]));
    }

    #[test]
    fn a_sync_after_the_echo_leaves_a_later_withdrawal_pending() {
        let mut guard = EchoGuard::default();
        let first = guard.withdrawn();
        assert!(guard.is_echo(&[]));
        guard.withdrawn();
        guard.settle(first);
        assert!(guard.is_echo(&[]));
        assert!(!guard.is_echo(&[]));
    }

    #[test]
    fn a_new_selection_does_not_spend_a_pending_withdrawal() {
        let mut guard = EchoGuard::default();
        guard.withdrawn();
        assert!(!guard.is_echo(&mimes(&["text/plain"])));
        assert!(guard.is_echo(&[]));
    }
}
