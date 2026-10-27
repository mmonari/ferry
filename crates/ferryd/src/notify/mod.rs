//! Toasts: ferryd talks to `org.freedesktop.Notifications` itself (M3 design §3, FR-UI-09,
//! FR-PAIR-08, FR-PAIR-13, FR-LAYOUT-10).
//!
//! | Part | What |
//! |---|---|
//! | [`toast`] | [`Toast`], its [`Kind`], and the words |
//! | [`policy`] | which toasts show and when: the rate limits, pure, under a synthetic clock |
//! | [`sink`] | the desktop behind a small trait; [`Recorder`] is the test double |
//! | `dbus` | the production sink, zbus (the version ashpd already brings) |
//! | here | the hooks `ctl` calls ([`Notifier`]), and the task that runs the policy |
//!
//! # Threads
//!
//! Nothing here runs on the input path. `ctl`'s hooks make an [`Event`] and `try_send` it into a
//! queue of [`QUEUE_CAP`]: a full queue means the toast is dropped and counted
//! (`toast_queue_full`), and `ctl` never waits. One local task on the `ctl` runtime, beside the
//! portal driver (RG-CONC-03 puts D-Bus on `ctl`), owns the [`Policy`] and the sink. Every
//! D-Bus call is bounded, so a notification daemon that hangs delays the next toasts by a
//! second and costs nothing else.
//!
//! # Every toast is also a `notice`
//!
//! The policy decides first, and [`on_toast_shown`] sees every toast it lets through, whether or
//! not a notification daemon answers or `[notify] enabled` is on: it publishes the
//! toast as a `notice` event to the subscribers of that topic. Only the desktop delivery depends
//! on those. A button on a toast (`open` on the pairing code) comes back as
//! [`on_notification_action`], which publishes `ui {"open": …}`.
//!
//! # `attention`
//!
//! A new entry in `status.attention` is a critical toast. The task reads the list itself, so it
//! works with no frontend open: `ctl` wakes it where it wakes the event hub
//! ([`Notifier::poke`]), and any thread that changes what `status` shows wakes it through
//! [`Changed`](crate::live::Changed). What was in the list at the last look is not new, so a
//! problem that stays is one toast, and one that comes back is another (at most one per code and
//! peer per 5 minutes).
//!
//! # The pairing code
//!
//! A pair-code toast carries the code in its title. It is never logged: the
//! [`Secret`], the [`Toast`] and the `Notice` hide it from `Debug`, and neither
//! [`on_toast_shown`] nor the sinks log a text.
//!
//! # Live-session safety
//!
//! [`Mode::Off`] is the default: a library user, and every test, sends nothing to any bus. Only
//! the `ferryd` binary chooses [`Mode::Session`], and `--no-input` (the dev daemon on a
//! desktop) leaves it off unless `--notify-bus` names a bus of its own.

mod dbus;
pub mod policy;
pub mod sink;
pub mod toast;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use ferry_core::api::{Attention, AttentionCode, LinkState, PairingRole, UiView};
use ferry_core::pair::PairCode;
use ferry_net::Fingerprint;
use tokio::sync::{mpsc, oneshot};

use crate::clock;
use crate::ctl::Ctl;
use crate::live::SharedLive;
use crate::ratelimit::log_limited;

use dbus::{DbusSink, Target};
pub use policy::{Action, Event, Limits, Policy, Secret};
pub use sink::{Call, Recorder, SinkEvent};
use sink::{Request, Sink, SinkError};
pub use toast::{Kind, Toast, ToastAction, Urgency};

/// The hooks' queue to the notify task. Full means the toast is dropped and counted
/// (`toast_queue_full`): the task reads at once, so only a hung notification daemon fills it.
pub const QUEUE_CAP: usize = 32;
/// How long `ctl`'s shutdown waits for the notify task to close the pair-code toast.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(400);

/// Where the toasts go.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    /// Nowhere: the policy still runs and [`on_toast_shown`] still hears every toast. The
    /// default, so that tests and library users never reach a bus.
    #[default]
    Off,
    /// The session bus of the environment: the real desktop. The `ferryd` binary's choice.
    Session,
    /// A bus at this address (D-Bus address syntax): tests, and `ferryd --notify-bus`.
    Bus(String),
    /// Tests: the calls are kept in the [`Recorder`].
    Recorder(Recorder),
}

/// How the daemon notifies. [`Options::default`] is [`Mode::Off`] and the production limits.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub mode: Mode,
    /// Tests shorten the windows.
    pub limits: Limits,
}

/// What the hooks send to the task.
#[derive(Debug)]
pub(crate) enum Cmd {
    Event(Event),
    /// `[notify] enabled` changed.
    SetEnabled(bool),
    /// Close the pair-code toast, answer, and stop.
    Shutdown(oneshot::Sender<()>),
}

/// What `ctl` calls. One per daemon, in [`Ctl`]. Every method is synchronous and never waits.
pub struct Notifier {
    tx: mpsc::Sender<Cmd>,
    live: SharedLive,
    /// Per trusted peer, the link state last sent: a refreshed RTT every 2 s is not an event.
    links: RefCell<HashMap<Fingerprint, bool>>,
}

/// The hooks' end and the task's end of the queue.
pub(crate) fn channel(live: SharedLive) -> (Notifier, mpsc::Receiver<Cmd>) {
    let (tx, rx) = mpsc::channel(QUEUE_CAP);
    (
        Notifier {
            tx,
            live,
            links: RefCell::new(HashMap::new()),
        },
        rx,
    )
}

impl Notifier {
    fn send(&self, cmd: Cmd) -> bool {
        match self.tx.try_send(cmd) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.live.bump("toast_queue_full");
                log_limited!(warn, "the toast queue is full: a toast is dropped");
                false
            }
            // shutting down
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn event(&self, event: Event) -> bool {
        self.send(Cmd::Event(event))
    }

    /// A pairing reached its code (`on_note` for `PairNote::Code`).
    pub(crate) fn pair_code(&self, role: PairingRole, peer: &str, code: &PairCode, window_ms: u64) {
        self.event(Event::PairCode {
            role,
            peer: peer.to_string(),
            code: Secret::new(code.to_string()),
            window_ms,
        });
    }

    /// The pairing moved on without a result of its own: the local user answered, the acceptor
    /// listens again, or its window ended.
    pub(crate) fn pairing_moved(&self) {
        self.event(Event::PairMoved);
    }

    /// The pin is written (`pairing::pinned`). `note` says why it has another name.
    pub(crate) fn paired(&self, peer: &str, note: Option<&str>) {
        self.event(Event::Paired {
            peer: peer.to_string(),
            note: note.map(str::to_string),
        });
    }

    pub(crate) fn pair_failed(&self, reason: &str, message: &str) {
        self.event(Event::PairFailed {
            reason: reason.to_string(),
            message: message.to_string(),
        });
    }

    /// A pinned peer's link changed (`apply` for `CtlMsg::Link`). `Connecting` and a state that
    /// is the one already sent are not events.
    pub(crate) fn link(&self, peer: Fingerprint, name: &str, state: &LinkState) {
        let up = match state {
            LinkState::Connected => true,
            LinkState::Offline | LinkState::Refused => false,
            LinkState::Connecting => return,
        };
        if self.links.borrow().get(&peer) == Some(&up) {
            return;
        }
        if self.event(Event::Link {
            peer,
            name: name.to_string(),
            up,
        }) {
            self.links.borrow_mut().insert(peer, up);
        }
    }

    /// The peer was unpaired here, or paired here: what is known of its link is void, and its
    /// next connect is a first one.
    pub(crate) fn forget_peer(&self, peer: Fingerprint) {
        self.links.borrow_mut().remove(&peer);
        self.event(Event::Forget { peer });
    }

    /// The other side unpaired this machine.
    pub(crate) fn unpaired(&self, peer: Fingerprint, name: &str) {
        self.links.borrow_mut().remove(&peer);
        self.event(Event::Unpaired {
            peer,
            name: name.to_string(),
        });
    }

    pub(crate) fn layout_replaced(&self, winner: &str) {
        self.event(Event::LayoutReplaced {
            winner: winner.to_string(),
        });
    }

    /// `status.attention` may have changed: the task looks. Where `ctl` pokes the event hub, it
    /// pokes this.
    pub(crate) fn poke(&self) {
        self.live.signal().toast_wake();
    }

    pub(crate) fn set_enabled(&self, on: bool) {
        self.send(Cmd::SetEnabled(on));
    }

    /// Closes the pair-code toast, if one is open, and stops the task. Waits at most
    /// [`SHUTDOWN_WAIT`]: it is the last step of the §9 shutdown, after every input-safety one.
    pub(crate) async fn shutdown(&self) {
        let (ack, acked) = oneshot::channel();
        if self.tx.try_send(Cmd::Shutdown(ack)).is_ok() {
            let _ = tokio::time::timeout(SHUTDOWN_WAIT, acked).await;
        }
    }
}

// --- the hooks ------------------------------------------------------------------------------------

/// This peer's own edit of the layout lost to a higher revision; `winner` is the
/// display name of the peer whose revision won. Stream A's `ctl::on_layout_replaced` calls it.
pub fn layout_replaced(ctl: &Ctl, winner: &str) {
    ctl.notify.layout_replaced(winner);
}

/// `config.set notify.enabled`, applied live.
pub fn set_enabled(ctl: &Ctl, on: bool) {
    ctl.notify.set_enabled(on);
}

/// Every toast the policy lets through, once, in the order they are shown: logged, and published
/// to the `notice` subscribers. Never log `toast.title` or `toast.body` here: a pair-code
/// toast's title is the pairing code.
pub fn on_toast_shown(ctl: &Ctl, toast: &Toast) {
    tracing::info!(
        kind = toast.kind.as_str(),
        peer = ?toast.peer,
        urgency = toast.urgency.as_str(),
        "toast"
    );
    ctl.hub.publish_notice(&toast.notice());
}

/// The user pressed a toast's action: `view` is what a frontend opens. Published to the `ui`
/// subscribers.
pub fn on_notification_action(ctl: &Ctl, view: UiView) {
    tracing::info!(
        view = view.as_str(),
        "a notification asks a frontend to open a view"
    );
    ctl.hub.publish_ui(view);
}

// --- the task --------------------------------------------------------------------------------------

/// Starts the notify task on the `ctl` runtime (call from inside the `LocalSet`).
pub(crate) fn spawn(ctl: &Rc<Ctl>, rx: mpsc::Receiver<Cmd>, opts: Options, enabled: bool) {
    let Options { mode, limits } = opts;
    match mode {
        Mode::Off => start(ctl, rx, limits, enabled, None::<Recorder>),
        Mode::Session => start(
            ctl,
            rx,
            limits,
            enabled,
            Some(DbusSink::new(Target::Session)),
        ),
        Mode::Bus(address) => start(
            ctl,
            rx,
            limits,
            enabled,
            Some(DbusSink::new(Target::Address(address))),
        ),
        Mode::Recorder(recorder) => start(ctl, rx, limits, enabled, Some(recorder)),
    }
}

fn start<S: Sink + 'static>(
    ctl: &Rc<Ctl>,
    rx: mpsc::Receiver<Cmd>,
    limits: Limits,
    enabled: bool,
    sink: Option<S>,
) {
    // The task holds `ctl` weakly: it must not keep the daemon's state alive, and it ends when
    // the last hook is gone.
    let (for_toasts, for_actions, for_attention) =
        (Rc::downgrade(ctl), Rc::downgrade(ctl), Rc::downgrade(ctl));
    let actor = Actor {
        policy: Policy::new(limits),
        sink,
        enabled,
        slot: None,
        reported: false,
        seen: Vec::new(),
        live: ctl.live.clone(),
        on_shown: move |toast: &Toast| {
            if let Some(ctl) = for_toasts.upgrade() {
                on_toast_shown(&ctl, toast);
            }
        },
        on_action: move |view: UiView| {
            if let Some(ctl) = for_actions.upgrade() {
                on_notification_action(&ctl, view);
            }
        },
        // What a frontend would be shown (`status.attention`). `None`, if a handler holds the
        // state mid-change (none does across an await) or ctl is gone: that look is skipped.
        attention: move || {
            for_attention
                .upgrade()
                .and_then(|ctl| ctl.state.try_borrow().ok().map(|s| s.status().attention))
        },
    };
    tokio::task::spawn_local(actor.run(rx));
}

struct Actor<S, F, G, A> {
    policy: Policy,
    sink: Option<S>,
    /// `[notify] enabled`. Off: toasts still pass through `on_shown`, and nothing is sent.
    enabled: bool,
    /// The id of the pair-code notification while it is open on the desktop.
    slot: Option<u32>,
    /// A failure was logged and nothing has worked since (the log says it once).
    reported: bool,
    /// The `attention` entries at the last look, as (code, peer): what is in the list again is
    /// not new.
    seen: Vec<(AttentionCode, Option<String>)>,
    live: SharedLive,
    on_shown: F,
    on_action: G,
    attention: A,
}

impl<S: Sink, F: Fn(&Toast), G: Fn(UiView), A: Fn() -> Option<Vec<Attention>>> Actor<S, F, G, A> {
    async fn run(mut self, mut rx: mpsc::Receiver<Cmd>) {
        // From here on a change of what `status` shows wakes this task, subscribers or not; the
        // first look is the state ferryd starts with (a config file it could not read in full).
        self.live.signal().toast_on();
        self.look_at_attention().await;
        loop {
            // A deadline that is due fires before anything else is read.
            let due = self.policy.tick(clock::now());
            self.apply(due).await;
            let wake = self.policy.next_deadline();
            let live = self.live.clone();
            tokio::select! {
                // What the desktop reports first: a command must not act on an id it has just
                // voided.
                biased;
                heard = next_signal(&mut self.sink) => self.heard(heard),
                () = live.signal().toast_woken() => self.look_at_attention().await,
                cmd = rx.recv() => match cmd {
                    None => break,
                    Some(Cmd::Event(event)) => {
                        let out = self.policy.handle(clock::now(), event);
                        self.apply(out).await;
                    }
                    Some(Cmd::SetEnabled(on)) => self.enabled = on,
                    Some(Cmd::Shutdown(ack)) => {
                        self.close_pairing().await;
                        let _ = ack.send(());
                        break;
                    }
                },
                () = sleep_until(wake) => {}
            }
        }
    }

    async fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Show(toast) => self.show(toast).await,
                Action::ClosePairing => self.close_pairing().await,
            }
        }
    }

    async fn show(&mut self, toast: Toast) {
        self.live.bump("toasts_shown");
        (self.on_shown)(&toast);
        if !self.enabled {
            return;
        }
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        let pairing = toast.kind.shares_pairing_toast();
        let replaces = if pairing { self.slot } else { None };
        match sink.show(&Request::new(&toast, replaces)).await {
            Ok(id) => {
                if std::mem::take(&mut self.reported) {
                    tracing::debug!("the notification daemon answers again");
                }
                if pairing {
                    // the result replaced the code toast, which is then not ours to close
                    self.slot = (toast.kind == Kind::PairCode).then_some(id);
                }
            }
            Err(e) => {
                if pairing || matches!(e, SinkError::Unavailable(_)) {
                    self.slot = None;
                }
                self.live.bump("toasts_undelivered");
                if std::mem::replace(&mut self.reported, true) {
                    tracing::debug!(error = %e, "a toast did not reach the desktop");
                } else {
                    tracing::info!(
                        error = %e,
                        "toasts do not reach the desktop; they go on as notice events until a \
                         notification daemon answers"
                    );
                }
            }
        }
    }

    async fn close_pairing(&mut self) {
        let Some(id) = self.slot.take() else {
            return;
        };
        if let Some(sink) = self.sink.as_mut() {
            sink.close(id).await;
        }
    }

    /// Reads `status.attention` and toasts the entries that were not in it at the last look.
    async fn look_at_attention(&mut self) {
        let Some(now) = (self.attention)() else {
            return;
        };
        let fresh: Vec<Event> = now
            .iter()
            .filter(|a| {
                !self
                    .seen
                    .iter()
                    .any(|(code, peer)| *code == a.code && *peer == a.peer)
            })
            .map(|a| Event::Attention {
                code: code_name(a.code).to_string(),
                message: a.message.clone(),
                hint: a.hint.clone(),
                peer: a.peer.clone(),
            })
            .collect();
        self.seen = now.into_iter().map(|a| (a.code, a.peer)).collect();
        for event in fresh {
            let out = self.policy.handle(clock::now(), event);
            self.apply(out).await;
        }
    }

    /// What the desktop reports. Only the pair-code notification has an id worth keeping.
    fn heard(&mut self, event: SinkEvent) {
        match event {
            SinkEvent::Action { id, key } => {
                if Some(id) == self.slot && key == toast::OPEN_PAIR.key {
                    self.live.bump("notification_actions");
                    (self.on_action)(toast::OPEN_PAIR.opens);
                }
            }
            SinkEvent::Closed { id } => {
                if Some(id) == self.slot {
                    self.slot = None;
                }
            }
            SinkEvent::Reset => self.slot = None,
        }
    }
}

/// The name of an `attention` code, as `status` writes it.
fn code_name(code: AttentionCode) -> &'static str {
    match code {
        AttentionCode::PeerRefused => "peer_refused",
        AttentionCode::BarriersFailed => "barriers_failed",
        AttentionCode::CaptureDenied => "capture_denied",
        AttentionCode::LayoutUnplaced => "layout_unplaced",
        AttentionCode::ConfigInvalid => "config_invalid",
        AttentionCode::Unknown => "unknown",
    }
}

/// The sink's next report; never, while there is no sink.
async fn next_signal<S: Sink>(sink: &mut Option<S>) -> SinkEvent {
    match sink {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<ferry_core::Micros>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(clock::instant_at(d).into()).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests;
