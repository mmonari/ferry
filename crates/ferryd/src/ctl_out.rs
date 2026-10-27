//! What the `net` thread has for `ctl`, waiting for room in `ctl`'s queue (RG-CONC-02: net never
//! waits for another thread; B-03).
//!
//! net used to `.await` ctl's bounded queue for every peer up, link change, `Monitors` and
//! `Layout`. ctl's work per message is not small (a `Monitors` that changed saves `layout.toml`
//! with two fsyncs), so a trusted peer that sent `Monitors` in a loop kept net parked in that
//! `.await`: its own queue backed up (the `Held`s and `Leave` of a session, the Pings) and ctl's
//! queue stayed full for everyone else.
//!
//! Now net only ever `try_send`s, from here:
//!
//! - **Latest wins**, one slot per peer: the link state ([`LinkUpdate`], merged as ctl would
//!   merge it), the peer's monitors, and its pushed layout (the higher revision, which is what
//!   ctl keeps of two). A flood of these costs ctl one message per peer per flush.
//! - **In order** ([`CtlOut::push`]): peer up, `last_seen`, a refused inbound connection, the
//!   pairing notes. They are never superseded; past [`QUEUE_CAP`] they are dropped and counted
//!   (`ctl_out_dropped`): ctl has then not read for a long time.
//!
//! The queue goes first, then the slots, so a `PeerUp` (which carries the monitors of its
//! `Hello`) reaches ctl before any later `Monitors` of the same peer.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use ferry_core::geom::{Layout, MonitorInfo};
use ferry_net::Fingerprint;
use tokio::sync::mpsc::{self, error::TrySendError};

use crate::chan::{CtlMsg, LinkUpdate};
use crate::live::SharedLive;
use crate::ratelimit::log_limited;

/// The in-order queue's bound: far above what a healthy ctl lets pile up (a few per peer).
pub const QUEUE_CAP: usize = 256;

/// The net thread's messages for ctl. Cheap to clone: the pairing side shares it with the loop.
#[derive(Clone)]
pub struct CtlOut(Rc<RefCell<Inner>>);

struct Inner {
    tx: mpsc::Sender<CtlMsg>,
    live: SharedLive,
    queue: VecDeque<CtlMsg>,
    links: HashMap<Fingerprint, LinkUpdate>,
    monitors: HashMap<Fingerprint, Vec<MonitorInfo>>,
    layouts: HashMap<Fingerprint, Layout>,
    /// ctl has stopped (shutdown): nothing is kept any more.
    closed: bool,
}

impl CtlOut {
    pub fn new(tx: mpsc::Sender<CtlMsg>, live: SharedLive) -> Self {
        Self(Rc::new(RefCell::new(Inner {
            tx,
            live,
            queue: VecDeque::new(),
            links: HashMap::new(),
            monitors: HashMap::new(),
            layouts: HashMap::new(),
            closed: false,
        })))
    }

    /// A message that is never superseded, after everything pushed before it.
    pub fn push(&self, msg: CtlMsg) {
        let mut i = self.0.borrow_mut();
        if i.closed {
            return;
        }
        if let CtlMsg::PeerUp { peer, .. } = &msg {
            // PeerUp carries the monitors of the Hello, newer than any still waiting.
            i.monitors.remove(peer);
        }
        if i.queue.len() >= QUEUE_CAP {
            i.live.bump("ctl_out_dropped");
            log_limited!(
                warn,
                ?msg,
                "net → ctl: ctl has not read for a long time; dropped"
            );
            return;
        }
        i.queue.push_back(msg);
    }

    /// The link state of a peer: the newest wins. A newer update without `proto` keeps the
    /// waiting one's, as ctl's `apply_link` would.
    pub fn link(&self, mut up: LinkUpdate) {
        let mut i = self.0.borrow_mut();
        if let Some(old) = i.links.remove(&up.peer) {
            up.proto = up.proto.or(old.proto);
        }
        i.links.insert(up.peer, up);
    }

    /// A peer's `Monitors`: the newest wins.
    pub fn monitors(&self, peer: Fingerprint, monitors: Vec<MonitorInfo>) {
        self.0.borrow_mut().monitors.insert(peer, monitors);
    }

    /// A peer's pushed layout: the higher revision wins, as at ctl.
    pub fn layout(&self, peer: Fingerprint, layout: Layout) {
        let mut i = self.0.borrow_mut();
        if i.layouts.get(&peer).is_none_or(|old| layout.rev > old.rev) {
            i.layouts.insert(peer, layout);
        }
    }

    /// Something waits for room in ctl's queue.
    pub fn pending(&self) -> bool {
        let i = self.0.borrow();
        !i.closed
            && !(i.queue.is_empty()
                && i.links.is_empty()
                && i.monitors.is_empty()
                && i.layouts.is_empty())
    }

    /// Hands ctl as much as fits now: the queue while there is room, the slots only while more
    /// than half of ctl's queue is free, so a flood of them never fills it for the others. What
    /// does not go waits for the next flush.
    pub fn flush(&self) {
        let mut i = self.0.borrow_mut();
        while let Some(msg) = i.next() {
            let slot = matches!(
                msg,
                CtlMsg::Link(_) | CtlMsg::PeerMonitors { .. } | CtlMsg::PeerLayout { .. }
            );
            if slot && i.tx.capacity() * 2 <= i.tx.max_capacity() {
                i.put_back(msg);
                return;
            }
            match i.tx.try_send(msg) {
                Ok(()) => {}
                Err(TrySendError::Full(msg)) => {
                    i.put_back(msg);
                    return;
                }
                Err(TrySendError::Closed(_)) => {
                    i.close();
                    return;
                }
            }
        }
    }

    /// ctl is gone: drop everything and keep nothing more.
    pub fn close(&self) {
        self.0.borrow_mut().close();
    }
}

impl Inner {
    /// The queue first, then the slots.
    fn next(&mut self) -> Option<CtlMsg> {
        if let Some(m) = self.queue.pop_front() {
            return Some(m);
        }
        if let Some(&peer) = self.links.keys().next() {
            return self.links.remove(&peer).map(CtlMsg::Link);
        }
        if let Some(&peer) = self.monitors.keys().next() {
            let monitors = self.monitors.remove(&peer)?;
            return Some(CtlMsg::PeerMonitors { peer, monitors });
        }
        if let Some(&peer) = self.layouts.keys().next() {
            let layout = self.layouts.remove(&peer)?;
            return Some(CtlMsg::PeerLayout { peer, layout });
        }
        None
    }

    /// A message `next` gave out that did not fit. Nothing was added in between (one borrow).
    fn put_back(&mut self, msg: CtlMsg) {
        match msg {
            CtlMsg::Link(up) => {
                self.links.insert(up.peer, up);
            }
            CtlMsg::PeerMonitors { peer, monitors } => {
                self.monitors.insert(peer, monitors);
            }
            CtlMsg::PeerLayout { peer, layout } => {
                self.layouts.insert(peer, layout);
            }
            other => self.queue.push_front(other),
        }
    }

    fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
        self.links.clear();
        self.monitors.clear();
        self.layouts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::api::LinkState;

    fn fp(n: u8) -> Fingerprint {
        Fingerprint([n; 32])
    }

    fn link(n: u8, state: LinkState, proto: Option<&str>) -> LinkUpdate {
        LinkUpdate {
            peer: fp(n),
            state,
            remote: None,
            rtt_us: None,
            proto: proto.map(str::to_string),
            detail: None,
        }
    }

    fn mon(w: u32) -> Vec<MonitorInfo> {
        vec![MonitorInfo {
            name: "eDP-1".into(),
            description: String::new(),
            rect: ferry_core::geom::WireRect {
                x: 0,
                y: 0,
                w,
                h: 768,
            },
            scale: 1.0,
            transform: 0,
            shift: None,
        }]
    }

    fn drain(rx: &mut mpsc::Receiver<CtlMsg>) -> Vec<CtlMsg> {
        let mut v = Vec::new();
        while let Ok(m) = rx.try_recv() {
            v.push(m);
        }
        v
    }

    #[test]
    fn a_flood_of_monitors_costs_one_message_per_peer() {
        let (tx, mut rx) = mpsc::channel(4);
        let out = CtlOut::new(tx, SharedLive::default());
        for w in 1000..2000 {
            out.monitors(fp(1), mon(w));
            out.monitors(fp(2), mon(w));
        }
        let mut got = Vec::new();
        while out.pending() {
            out.flush();
            got.extend(drain(&mut rx));
        }
        assert_eq!(got.len(), 2, "{got:?}");
        for m in got {
            match m {
                CtlMsg::PeerMonitors { monitors, .. } => assert_eq!(monitors, mon(1999)),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_full_queue_keeps_everything_for_the_next_flush_in_order() {
        let (tx, mut rx) = mpsc::channel(1);
        let out = CtlOut::new(tx, SharedLive::default());
        out.monitors(fp(1), mon(800));
        out.push(CtlMsg::PeerUp {
            peer: fp(1),
            monitors: mon(1366),
        });
        out.monitors(fp(1), mon(1280));
        out.link(link(1, LinkState::Connected, Some("1.0")));
        out.push(CtlMsg::InboundFailed(ferry_net::ConnectError::Abandoned));
        let mut got = Vec::new();
        while out.pending() {
            out.flush();
            got.extend(drain(&mut rx));
        }
        // the queue in order, then the slots; the monitors that waited before PeerUp are gone
        assert!(
            matches!(&got[0], CtlMsg::PeerUp { monitors, .. } if *monitors == mon(1366)),
            "{got:?}"
        );
        assert!(matches!(got[1], CtlMsg::InboundFailed(_)), "{got:?}");
        assert!(matches!(got[2], CtlMsg::Link(_)), "{got:?}");
        assert!(
            matches!(&got[3], CtlMsg::PeerMonitors { monitors, .. } if *monitors == mon(1280)),
            "{got:?}"
        );
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn a_link_keeps_the_proto_of_the_one_it_replaces() {
        let (tx, mut rx) = mpsc::channel(4);
        let out = CtlOut::new(tx, SharedLive::default());
        out.link(link(1, LinkState::Connected, Some("1.0")));
        out.link(link(1, LinkState::Connected, None));
        out.flush();
        match drain(&mut rx).as_slice() {
            [CtlMsg::Link(up)] => assert_eq!(up.proto.as_deref(), Some("1.0")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_higher_layout_revision_waits() {
        use ferry_core::PeerId;
        let (tx, mut rx) = mpsc::channel(4);
        let out = CtlOut::new(tx, SharedLive::default());
        let at = |counter| {
            let mut l = Layout::empty(PeerId([1; 32]));
            l.rev.counter = counter;
            l
        };
        out.layout(fp(1), at(5));
        out.layout(fp(1), at(3));
        out.flush();
        match drain(&mut rx).as_slice() {
            [CtlMsg::PeerLayout { layout, .. }] => assert_eq!(layout.rev.counter, 5),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_queue_is_bounded_and_counts_what_it_drops() {
        let (tx, _rx) = mpsc::channel(1);
        let live = SharedLive::default();
        let out = CtlOut::new(tx, live.clone());
        for _ in 0..QUEUE_CAP + 10 {
            out.push(CtlMsg::InboundFailed(ferry_net::ConnectError::Abandoned));
        }
        assert_eq!(live.lock().counters.get("ctl_out_dropped"), Some(&10));
    }

    #[test]
    fn a_closed_ctl_ends_the_backlog() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let out = CtlOut::new(tx, SharedLive::default());
        out.monitors(fp(1), mon(800));
        out.flush();
        assert!(!out.pending());
        out.push(CtlMsg::InboundFailed(ferry_net::ConnectError::Abandoned));
        assert!(!out.pending());
    }
}
