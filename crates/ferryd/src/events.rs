//! Events for frontends.
//!
//! A connection that sent `subscribe` gets one **snapshot** of each topic at once, and then one
//! event whenever a topic's snapshot changes. The `hub` runs inside ferryd's `ctl` loop and does
//! its work only for topics somebody subscribed to.
//!
//! # What triggers a look
//!
//! [`Hub::poke`] says "something may have changed". ctl pokes after every `CtlMsg` it applied and
//! after every request that is not a read
//! ([`ferry_core::api::Request::is_read_only`]); the other threads wake the
//! hub through [`Changed`] when they publish a new machine status. Nothing pokes per input event.
//! A poke costs one flag while nobody subscribes.
//!
//! # What goes out
//!
//! - **Change detection.** The hub keeps, per connection and topic, the snapshot it sent last and
//!   sends the new one only when it differs. For `state` the counters, the clock of the last
//!   release, a peer's round trip and the pairing countdown do not count
//!   ([`ferry_core::api::StatusResult::without_noise`]); a 2 s refresh sends them anyway.
//! - **Coalescing.** A topic is looked at at most once per [`COALESCE`] (50 ms): the first change
//!   goes out at once, the rest of a burst goes out at the end of the window, so the last change
//!   always arrives.
//! - **Slow readers.** A connection's events wait in an [`EventQueue`] that holds at most one
//!   unsent snapshot per topic: a newer one replaces an older one that the writer has not taken.
//!   A client that stops reading therefore costs a few kilobytes, never stalls ferryd, and finds
//!   the current state first when it reads again. Responses use the bounded queue of
//!   `server.rs` and are never replaced.
//! - **Events that are not snapshots** (`pairing`, `notice`, `ui`) are queued in order, at most
//!   [`EVENT_BACKLOG`] per topic; the oldest goes first when a reader is that far behind.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_core::api::{EdgeEvent, Notice, PairingEvent, Topic, UiEvent, UiView};
use serde::Serialize;

use crate::live::Changed;

/// A topic is looked at at most once per this long (FR-UI-02's 100 ms budget is the ceiling).
pub const COALESCE: Duration = Duration::from_millis(50);

/// While a frontend watches `state`, the noise fields it hides (counters, round trips) reach it
/// at least this often.
pub const REFRESH: Duration = Duration::from_secs(2);

/// Events of a stream topic (`pairing`, `notice`, `ui`) one connection may have waiting.
pub const EVENT_BACKLOG: usize = 32;

/// The topics whose events are snapshots, which the hub can compare, coalesce and replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    State,
    Peers,
    Layout,
    Config,
}

impl Snap {
    pub const ALL: [Snap; 4] = [Snap::State, Snap::Peers, Snap::Layout, Snap::Config];

    pub const fn topic(self) -> Topic {
        match self {
            Snap::State => Topic::State,
            Snap::Peers => Topic::Peers,
            Snap::Layout => Topic::Layout,
            Snap::Config => Topic::Config,
        }
    }

    pub const fn of(topic: Topic) -> Option<Snap> {
        match topic {
            Topic::State => Some(Snap::State),
            Topic::Peers => Some(Snap::Peers),
            Topic::Layout => Some(Snap::Layout),
            Topic::Config => Some(Snap::Config),
            Topic::Pairing | Topic::Notice | Topic::Ui | Topic::Edge => None,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// One serialised event line `{"event":…,"data":…}\n`, shared by every connection it goes to.
pub fn event_line(topic: Topic, data: &str) -> Rc<[u8]> {
    let mut line = String::with_capacity(data.len() + 32);
    line.push_str("{\"event\":\"");
    line.push_str(topic.as_str());
    line.push_str("\",\"data\":");
    line.push_str(data);
    line.push_str("}\n");
    Rc::from(line.into_bytes())
}

fn data_of<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("a snapshot serialises")
}

/// The current value of a snapshot topic, ready to send and to compare.
#[derive(Debug)]
pub struct Snapshot {
    line: Rc<[u8]>,
    /// The `data` without the noise, when the topic has some: the part that decides whether the
    /// snapshot is news. `None`: the whole line is.
    news: Option<String>,
}

impl Snapshot {
    /// A snapshot of `data`, all of it news.
    pub fn new<T: Serialize>(snap: Snap, data: &T) -> Snapshot {
        Snapshot {
            line: event_line(snap.topic(), &data_of(data)),
            news: None,
        }
    }

    /// A snapshot whose news is `quiet`: `data` with the noise taken out.
    pub fn with_noise<T: Serialize>(snap: Snap, data: &T, quiet: &T) -> Snapshot {
        Snapshot {
            line: event_line(snap.topic(), &data_of(data)),
            news: Some(data_of(quiet)),
        }
    }

    fn same_news(&self, other: &Snapshot) -> bool {
        match (&self.news, &other.news) {
            (Some(a), Some(b)) => a == b,
            _ => self.line == other.line,
        }
    }

    fn same_bytes(&self, other: &Snapshot) -> bool {
        self.line == other.line
    }

    /// The event line, with its newline.
    pub fn line(&self) -> &Rc<[u8]> {
        &self.line
    }
}

/// Where the hub reads the current state. [`crate::ctl::Ctl`] is the real one.
pub trait Source {
    /// The current value of a snapshot topic.
    fn snapshot(&self, snap: Snap) -> Snapshot;
    /// The pairing that runs now, as the event of its current stage (with its code at stage
    /// `code`), for a frontend that subscribes in the middle of it.
    fn pairing(&self) -> Option<PairingEvent>;
}

// --- the queue of one connection -----------------------------------------------------------------

struct Waiting {
    topic: Topic,
    line: Rc<[u8]>,
}

/// The events waiting for one connection's writer (`server.rs`).
#[derive(Default)]
pub struct EventQueue {
    waiting: RefCell<VecDeque<Waiting>>,
    ready: tokio::sync::Notify,
}

impl EventQueue {
    /// Queues an event. A snapshot replaces the unsent snapshot of its topic, so nothing waits
    /// behind a reader that is slow; an event of a stream topic waits its turn, and the oldest
    /// of its topic goes when [`EVENT_BACKLOG`] are waiting. Returns whether something unsent
    /// was dropped.
    pub fn push(&self, topic: Topic, line: Rc<[u8]>) -> bool {
        let mut waiting = self.waiting.borrow_mut();
        let mut dropped = false;
        if topic.is_snapshot() {
            let before = waiting.len();
            waiting.retain(|w| w.topic != topic);
            dropped = waiting.len() != before;
        } else if waiting.iter().filter(|w| w.topic == topic).count() >= EVENT_BACKLOG {
            if let Some(i) = waiting.iter().position(|w| w.topic == topic) {
                waiting.remove(i);
            }
            dropped = true;
        }
        waiting.push_back(Waiting { topic, line });
        drop(waiting);
        self.ready.notify_one();
        dropped
    }

    fn pop(&self) -> Option<Rc<[u8]>> {
        self.waiting.borrow_mut().pop_front().map(|w| w.line)
    }

    /// The next event line, waiting for one. Cancel-safe: an event is taken only when it is
    /// returned.
    pub async fn next(&self) -> Rc<[u8]> {
        loop {
            if let Some(line) = self.pop() {
                return line;
            }
            self.ready.notified().await;
        }
    }

    /// Forgets everything waiting (the connection unsubscribed, or subscribed anew).
    pub fn clear(&self) {
        self.waiting.borrow_mut().clear();
    }

    /// How many events wait.
    pub fn len(&self) -> usize {
        self.waiting.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// --- the hub --------------------------------------------------------------------------------------

struct Sub {
    conn: u64,
    queue: Rc<EventQueue>,
    topics: Vec<Topic>,
    /// The snapshot this connection was last sent, per snapshot topic.
    last: [Option<Rc<Snapshot>>; 4],
}

impl Sub {
    fn wants(&self, topic: Topic) -> bool {
        self.topics.contains(&topic)
    }
}

/// When a snapshot topic was last looked at, and whether it is due.
#[derive(Default)]
struct Sched {
    /// A poke came and the topic has not been looked at since.
    pending: Cell<bool>,
    evaluated: Cell<Option<Instant>>,
    /// `state` only: the next refresh of the noise.
    refresh_at: Cell<Option<Instant>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Send to a connection whose last snapshot differs in the news.
    News,
    /// Send to a connection whose last snapshot differs at all.
    Refresh,
}

pub struct Hub {
    signal: Arc<Changed>,
    subs: RefCell<Vec<Sub>>,
    dirty: Cell<bool>,
    sched: [Sched; 4],
    superseded: Cell<u64>,
}

impl Hub {
    pub fn new(signal: Arc<Changed>) -> Self {
        Self {
            signal,
            subs: RefCell::new(Vec::new()),
            dirty: Cell::new(false),
            sched: Default::default(),
            superseded: Cell::new(0),
        }
    }

    /// Events dropped because a reader was behind (counter `events_superseded`).
    pub fn superseded(&self) -> u64 {
        self.superseded.get()
    }

    fn drop_counted(&self, dropped: bool) {
        if dropped {
            self.superseded.set(self.superseded.get().saturating_add(1));
        }
    }

    /// Whether any connection is subscribed to `topic`.
    pub fn wants(&self, topic: Topic) -> bool {
        self.subs.borrow().iter().any(|s| s.wants(topic))
    }

    /// Whether connection `conn` is subscribed to `topic`.
    pub fn has(&self, conn: u64, topic: Topic) -> bool {
        self.subs
            .borrow()
            .iter()
            .any(|s| s.conn == conn && s.wants(topic))
    }

    /// Something may have changed: look at the topics somebody subscribed to. One flag while
    /// nobody did.
    pub fn poke(&self) {
        if !self.subs.borrow().is_empty() {
            self.dirty.set(true);
            self.signal.wake();
        }
    }

    /// Starts events for `conn` on exactly `topics` (a second call replaces the set): one
    /// snapshot of each snapshot topic at once, and the pairing that runs now, if any. `queue` is
    /// the connection's.
    pub fn subscribe(
        &self,
        src: &dyn Source,
        conn: u64,
        queue: &Rc<EventQueue>,
        topics: &[Topic],
        now: Instant,
    ) {
        // Before the snapshots: a change another thread makes from here on either shows in them or
        // wakes the hub.
        self.signal.watch(true);
        self.remove(conn);
        queue.clear();
        let mut sub = Sub {
            conn,
            queue: queue.clone(),
            topics: topics.to_vec(),
            last: Default::default(),
        };
        let wanted: Vec<Topic> = Topic::ALL.into_iter().filter(|t| sub.wants(*t)).collect();
        for topic in wanted {
            if let Some(snap) = Snap::of(topic) {
                let snapshot = Rc::new(src.snapshot(snap));
                self.drop_counted(queue.push(topic, snapshot.line.clone()));
                sub.last[snap.index()] = Some(snapshot);
            } else if topic == Topic::Pairing
                && let Some(ev) = src.pairing()
            {
                queue.push(topic, event_line(topic, &data_of(&ev)));
            }
        }
        if sub.wants(Topic::State) && self.sched[Snap::State.index()].refresh_at.get().is_none() {
            self.sched[Snap::State.index()]
                .refresh_at
                .set(Some(now + REFRESH));
        }
        self.subs.borrow_mut().push(sub);
        // the run loop arms its timers from what flush returns
        self.signal.wake();
    }

    /// Ends the events of `conn` and forgets what waited for it.
    pub fn unsubscribe(&self, conn: u64) {
        if let Some(sub) = self.remove(conn) {
            sub.queue.clear();
        }
        if self.subs.borrow().is_empty() {
            self.signal.watch(false);
            self.dirty.set(false);
        }
    }

    fn remove(&self, conn: u64) -> Option<Sub> {
        let mut subs = self.subs.borrow_mut();
        let i = subs.iter().position(|s| s.conn == conn)?;
        Some(subs.remove(i))
    }

    /// The hub's turn: send what is due, and return when it should run next (`None`: only a
    /// poke will need it).
    pub fn flush(&self, src: &dyn Source, now: Instant) -> Option<Instant> {
        let dirty = self.dirty.replace(false);
        let mut next: Option<Instant> = None;
        let mut want = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
        for snap in Snap::ALL {
            let sched = &self.sched[snap.index()];
            if !self.wants(snap.topic()) {
                sched.pending.set(false);
                sched.evaluated.set(None);
                sched.refresh_at.set(None);
                continue;
            }
            sched.pending.set(sched.pending.get() || dirty);
            let refresh_due = sched.refresh_at.get().is_some_and(|at| now >= at);
            let mut waiting = false;
            if sched.pending.get() || refresh_due {
                let allowed = sched.evaluated.get().map_or(now, |t| t + COALESCE);
                if now >= allowed {
                    sched.pending.set(false);
                    sched.evaluated.set(Some(now));
                    // The refresh keeps its own beat: looking for news must not push it out, or
                    // a link whose round trip changes every second would never refresh.
                    if refresh_due {
                        sched.refresh_at.set(Some(now + REFRESH));
                    }
                    let mode = if refresh_due {
                        Mode::Refresh
                    } else {
                        Mode::News
                    };
                    self.evaluate(src, snap, mode);
                } else {
                    // the end of the window: the last change of a burst goes out then
                    want(allowed);
                    waiting = true;
                }
            }
            if let (Some(at), false) = (sched.refresh_at.get(), waiting) {
                want(at);
            }
        }
        next
    }

    /// Looks at one topic: builds the snapshot once and sends it to the connections it is news
    /// to.
    fn evaluate(&self, src: &dyn Source, snap: Snap, mode: Mode) {
        let snapshot = Rc::new(src.snapshot(snap));
        let topic = snap.topic();
        let mut dropped = 0u64;
        for sub in self.subs.borrow_mut().iter_mut().filter(|s| s.wants(topic)) {
            let last = &mut sub.last[snap.index()];
            let send = match (last.as_deref(), mode) {
                (None, _) => true,
                (Some(l), Mode::News) => !l.same_news(&snapshot),
                (Some(l), Mode::Refresh) => !l.same_bytes(&snapshot),
            };
            if send {
                *last = Some(snapshot.clone());
                dropped += u64::from(sub.queue.push(topic, snapshot.line.clone()));
            }
        }
        self.superseded
            .set(self.superseded.get().saturating_add(dropped));
    }

    /// A stage of a pairing, to every `pairing` subscriber except `except`, which got it another
    /// way (the connection that started the pairing, when it did not subscribe).
    pub fn publish_pairing(&self, event: &PairingEvent, except: Option<u64>) {
        if self.wants(Topic::Pairing) {
            self.publish(Topic::Pairing, &data_of(event), except);
        }
    }

    /// A toast, to every `notice` subscriber.
    pub fn publish_notice(&self, notice: &Notice) {
        if self.wants(Topic::Notice) {
            self.publish(Topic::Notice, &data_of(notice), None);
        }
    }

    /// A request that a frontend open `view`, to every `ui` subscriber.
    pub fn publish_ui(&self, view: UiView) {
        if self.wants(Topic::Ui) {
            self.publish(Topic::Ui, &data_of(&UiEvent { open: view }), None);
        }
    }

    /// A crossing over an edge of this peer, to every `edge` subscriber.
    pub fn publish_edge(&self, edge: &EdgeEvent) {
        if self.wants(Topic::Edge) {
            self.publish(Topic::Edge, &data_of(edge), None);
        }
    }

    fn publish(&self, topic: Topic, data: &str, except: Option<u64>) {
        let line = event_line(topic, data);
        let mut dropped = 0u64;
        for sub in self
            .subs
            .borrow()
            .iter()
            .filter(|s| s.wants(topic) && Some(s.conn) != except)
        {
            dropped += u64::from(sub.queue.push(topic, line.clone()));
        }
        self.superseded
            .set(self.superseded.get().saturating_add(dropped));
    }
}

/// The hub's task on the `ctl` loop: look when poked, and when a coalescing window or a refresh
/// comes due. It sleeps for good while nobody is subscribed.
pub async fn run(ctl: Rc<crate::ctl::Ctl>) {
    loop {
        let next = ctl.hub.flush(ctl.as_ref(), Instant::now());
        match next {
            Some(at) => {
                tokio::select! {
                    () = ctl.live.signal().woken() => {}
                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => {}
                }
            }
            None => ctl.live.signal().woken().await,
        }
    }
}

impl Source for crate::ctl::Ctl {
    fn snapshot(&self, snap: Snap) -> Snapshot {
        match snap {
            Snap::State => {
                let status = crate::api::status_snapshot(self);
                Snapshot::with_noise(snap, &status, &status.without_noise())
            }
            Snap::Peers => Snapshot::new(snap, &self.state.borrow().peers_view()),
            Snap::Layout => Snapshot::new(snap, &self.state.borrow().layout_view()),
            Snap::Config => Snapshot::new(snap, &crate::config_api::values(self)),
        }
    }

    fn pairing(&self) -> Option<PairingEvent> {
        crate::pairing::current(self)
    }
}

#[cfg(test)]
mod tests;
