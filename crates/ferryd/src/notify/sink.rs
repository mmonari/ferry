//! The desktop's side of a toast, behind a small trait.
//!
//! [`Sink`] is what the notify task needs from `org.freedesktop.Notifications`: show a
//! notification (or replace one), close one, and hear what the user did with it. The production
//! sink is [`DbusSink`](super::dbus::DbusSink). [`Recorder`] is the test double: it keeps the
//! calls, and a test can make it hear an action or turn the desktop off.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Notify;

use super::toast::{Toast, Urgency};

/// `app_name` of every notification.
pub const APP_NAME: &str = "Ferry";
/// `app_icon` of every notification: the md-ferry mark the bar widget draws, installed by the
/// package as `dist/icons/ferry-symbolic.svg`. The `-symbolic` name lets the shell tint it.
pub const APP_ICON: &str = "ferry-symbolic";

/// One notification to send: the arguments of `Notify`.
pub(crate) struct Request {
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    pub expire_ms: Option<u32>,
    /// `(key, label)` of the one action.
    pub action: Option<(&'static str, &'static str)>,
    /// The notification this one takes the place of.
    pub replaces: Option<u32>,
}

impl Request {
    pub(crate) fn new(toast: &Toast, replaces: Option<u32>) -> Self {
        Self {
            summary: toast.title.clone(),
            body: toast.body.clone(),
            urgency: toast.urgency,
            expire_ms: toast.expire_ms,
            action: toast.action.map(|a| (a.key, a.label)),
            replaces,
        }
    }
}

/// Why a notification did not go out. Both are the desktop's doing, not a fault of ferryd.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SinkError {
    /// No bus, no notification daemon on it, or none that answers in time.
    #[error("no notification daemon answers: {0}")]
    Unavailable(String),
    /// A daemon answered with an error.
    #[error("the notification daemon refused the call: {0}")]
    Refused(String),
}

/// What the desktop reports about the notifications ferryd sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkEvent {
    /// The user pressed an action (`ActionInvoked`).
    Action { id: u32, key: String },
    /// A notification ended: expired, dismissed or closed (`NotificationClosed`).
    Closed { id: u32 },
    /// The notification daemon went away or came back as another process: every id is void.
    Reset,
}

pub(crate) trait Sink {
    /// Shows `req` and returns its id. Connects first if need be. Bounded by the caller.
    async fn show(&mut self, req: &Request) -> Result<u32, SinkError>;
    /// `CloseNotification`, best effort.
    async fn close(&mut self, id: u32);
    /// The next report. Pends while nothing is connected. Cancel-safe.
    async fn next(&mut self) -> SinkEvent;
}

// --- the test double ----------------------------------------------------------------------------

/// A call the notify task made on a [`Recorder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Show {
        id: u32,
        summary: String,
        body: String,
        urgency: Urgency,
        expire_ms: Option<u32>,
        /// The key of the action, when it offered one.
        action: Option<String>,
        /// The id it asked to replace.
        replaces: Option<u32>,
    },
    Close {
        id: u32,
    },
}

#[derive(Default)]
struct Shared {
    calls: Mutex<Vec<Call>>,
    events: Mutex<VecDeque<SinkEvent>>,
    wake: Notify,
    last_id: AtomicU32,
    down: AtomicBool,
}

/// A stand-in for the desktop that keeps every call (tests: `notify::Mode::Recorder`). Cheap to
/// clone: the test keeps one and the daemon gets the other.
#[derive(Clone, Default)]
pub struct Recorder(Arc<Shared>);

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").finish_non_exhaustive()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.0
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The summaries of the notifications shown so far, in order.
    pub fn summaries(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Show { summary, .. } => Some(summary),
                Call::Close { .. } => None,
            })
            .collect()
    }

    /// The user presses `key` on notification `id`.
    pub fn press(&self, id: u32, key: &str) {
        self.hear(SinkEvent::Action {
            id,
            key: key.to_string(),
        });
    }

    /// The desktop ends notification `id` (the user dismissed it, or it expired).
    pub fn end(&self, id: u32) {
        self.hear(SinkEvent::Closed { id });
    }

    /// The notification daemon restarted: every id it handed out is void.
    pub fn reset(&self) {
        self.hear(SinkEvent::Reset);
    }

    /// No notification daemon answers while `down` (every `show` fails as `Unavailable`).
    pub fn set_unavailable(&self, down: bool) {
        self.0.down.store(down, Ordering::Relaxed);
    }

    fn hear(&self, ev: SinkEvent) {
        self.0
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(ev);
        self.0.wake.notify_one();
    }

    fn record(&self, call: Call) {
        self.0
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call);
    }
}

impl Sink for Recorder {
    async fn show(&mut self, req: &Request) -> Result<u32, SinkError> {
        if self.0.down.load(Ordering::Relaxed) {
            return Err(SinkError::Unavailable(
                "the recorder is switched off".into(),
            ));
        }
        // a replaced notification keeps its id, as on the servers that matter
        let id = req.replaces.unwrap_or_else(|| {
            self.0
                .last_id
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1)
        });
        self.record(Call::Show {
            id,
            summary: req.summary.clone(),
            body: req.body.clone(),
            urgency: req.urgency,
            expire_ms: req.expire_ms,
            action: req.action.map(|(key, _)| key.to_string()),
            replaces: req.replaces,
        });
        Ok(id)
    }

    async fn close(&mut self, id: u32) {
        self.record(Call::Close { id });
    }

    async fn next(&mut self) -> SinkEvent {
        loop {
            let heard = self
                .0
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            if let Some(ev) = heard {
                return ev;
            }
            self.0.wake.notified().await;
        }
    }
}
