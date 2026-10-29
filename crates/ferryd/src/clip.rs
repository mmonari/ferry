//! The seam between the net thread's clipboard sharing (`net_thread/clip.rs`) and the
//! compositor's clipboard.
//!
//! The production bridge is ferry-clip's `ext-data-control-v1` thread ([`ClipBackend::Wayland`]).
//! Tests put a [`FakeClipboard`] in its place, which touches no compositor.

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// What the compositor's clipboard reports.
#[derive(Debug)]
pub enum ClipEvent {
    /// The selection changed to one ferry did not set; empty when it was cleared.
    Selection(Vec<String>),
    /// An application pastes from the selection ferry set: write the item to `fd`, then close it.
    Paste { mime: String, fd: OwnedFd },
    /// The bridge stopped (the compositor ended the data device).
    Finished,
}

pub type Received = Pin<Box<dyn Future<Output = Result<OwnedFd, String>> + Send>>;

/// The compositor's clipboard, as the net thread uses it. Both calls return at once.
pub trait ClipBridge: Send {
    /// Become the selection with these types on behalf of a peer; empty withdraws it.
    fn offer(&self, mimes: Vec<String>);
    /// A pipe that yields the current selection in `mime`.
    fn receive(&self, mime: String) -> Received;
}

/// Starts a bridge whose events go to the sender. Called off the net thread (it may wait for
/// the compositor), each time sharing is turned on.
pub type ClipStarter =
    Box<dyn FnMut(mpsc::UnboundedSender<ClipEvent>) -> Result<Box<dyn ClipBridge>, String> + Send>;

/// Where the clipboard is.
pub enum ClipBackend {
    /// None: nothing is shared. The default for an in-process daemon, so a test never touches
    /// the live session's clipboard.
    Off,
    /// ferry-clip on the session's Wayland display (the `ferryd` binary).
    Wayland,
    /// A replacement, for tests.
    Custom(ClipStarter),
}

impl std::fmt::Debug for ClipBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "Off",
            Self::Wayland => "Wayland",
            Self::Custom(_) => "Custom",
        })
    }
}

impl ClipBackend {
    /// The starter, or `None` for [`ClipBackend::Off`].
    pub fn starter(self) -> Option<ClipStarter> {
        match self {
            Self::Off => None,
            Self::Wayland => Some(Box::new(|tx| start_wayland(tx).map_err(|e| e.to_string()))),
            Self::Custom(s) => Some(s),
        }
    }
}

struct WaylandBridge(ferry_clip::Clipboard);

impl ClipBridge for WaylandBridge {
    fn offer(&self, mimes: Vec<String>) {
        self.0.offer(mimes);
    }

    fn receive(&self, mime: String) -> Received {
        let rx = self.0.receive(mime);
        Box::pin(async move {
            match rx.await {
                Ok(Ok(fd)) => Ok(fd),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("the clipboard thread is gone".into()),
            }
        })
    }
}

fn start_wayland(tx: mpsc::UnboundedSender<ClipEvent>) -> ferry_clip::Result<Box<dyn ClipBridge>> {
    let sink = move |ev: ferry_clip::ClipboardEvent| {
        let ev = match ev {
            ferry_clip::ClipboardEvent::Selection { mime_types } => {
                ClipEvent::Selection(mime_types)
            }
            ferry_clip::ClipboardEvent::Paste { mime_type, fd } => ClipEvent::Paste {
                mime: mime_type,
                fd,
            },
            ferry_clip::ClipboardEvent::Finished => ClipEvent::Finished,
        };
        let _ = tx.send(ev);
    };
    let c = ferry_clip::Clipboard::start(ferry_wl::Target::default(), Box::new(sink))?;
    Ok(Box::new(WaylandBridge(c)))
}

/// A clipboard for tests: [`FakeClipboard::copy`] is an application copying,
/// [`FakeClipboard::paste`] one pasting. Clones share one clipboard.
#[derive(Clone, Default)]
pub struct FakeClipboard(Arc<Mutex<Fake>>);

#[derive(Default)]
struct Fake {
    tx: Option<mpsc::UnboundedSender<ClipEvent>>,
    /// What a local application copied, by type.
    local: HashMap<String, Vec<u8>>,
    /// The types ferry offers on a peer's behalf, while it holds the selection.
    offered: Option<Vec<String>>,
    starts: u32,
}

impl FakeClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The backend to hand to [`crate::Options::clipboard`].
    pub fn backend(&self) -> ClipBackend {
        let me = self.clone();
        ClipBackend::Custom(Box::new(move |tx| {
            let mut f = me.lock();
            f.tx = Some(tx);
            f.starts += 1;
            Ok(Box::new(me.clone()) as Box<dyn ClipBridge>)
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Fake> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An application copies `items` (type, bytes), in that order.
    pub fn copy(&self, items: &[(&str, &[u8])]) {
        let mut f = self.lock();
        f.local = items
            .iter()
            .map(|(m, b)| ((*m).to_string(), b.to_vec()))
            .collect();
        f.offered = None;
        let mimes = items.iter().map(|(m, _)| (*m).to_string()).collect();
        if let Some(tx) = &f.tx {
            let _ = tx.send(ClipEvent::Selection(mimes));
        }
    }

    /// The types ferry offers for a peer, while it holds the selection.
    pub fn offered(&self) -> Option<Vec<String>> {
        self.lock().offered.clone()
    }

    /// How often the bridge was started.
    pub fn starts(&self) -> u32 {
        self.lock().starts
    }

    /// An application pastes `mime` from the selection ferry holds: the bytes it reads, up to
    /// EOF. Blocks; call it off the daemon's threads.
    pub fn paste(&self, mime: &str) -> std::io::Result<Vec<u8>> {
        let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        {
            let f = self.lock();
            let tx =
                f.tx.as_ref()
                    .ok_or_else(|| std::io::Error::other("not started"))?;
            tx.send(ClipEvent::Paste {
                mime: mime.to_string(),
                fd: write,
            })
            .map_err(|_| std::io::Error::other("stopped"))?;
        }
        let mut out = Vec::new();
        std::fs::File::from(read).read_to_end(&mut out)?;
        Ok(out)
    }
}

impl ClipBridge for FakeClipboard {
    fn offer(&self, mimes: Vec<String>) {
        let mut f = self.lock();
        if mimes.is_empty() {
            f.offered = None;
        } else {
            f.local.clear();
            f.offered = Some(mimes);
        }
    }

    fn receive(&self, mime: String) -> Received {
        let data = self.lock().local.get(&mime).cloned();
        Box::pin(async move {
            let data = data.ok_or_else(|| format!("the selection has no {mime}"))?;
            let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
                .map_err(|e| e.to_string())?;
            std::thread::spawn(move || {
                let _ = std::fs::File::from(write).write_all(&data);
            });
            Ok(read)
        })
    }
}
