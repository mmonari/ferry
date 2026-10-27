//! Which toasts ferryd shows, and when.
//!
//! Pure and sans-IO, like the machines: [`Policy::handle`] takes an [`Event`] and the time as
//! `Micros`, and answers with [`Action`]s; [`Policy::next_deadline`] is the only timer. The
//! caller owns the clock, so every rate limit is tested with a synthetic one.
//!
//! | Kind | Rule |
//! |---|---|
//! | `pair_code` | only for the acceptor (FR-PAIR-13 is about an *incoming* request: the dialer started the pairing where it sees the code); closed when the stage moves on |
//! | `paired`, `pair_failed` | always; they take the place of the code toast |
//! | `peer_lost`, `peer_back` | one per peer per [`CONNECTION_WINDOW`], both kinds together. The state the window swallowed is announced when the window ends, so a flap never leaves the last toast stale. The first connect of a peer is silent |
//! | `unpaired` | always |
//! | `layout_replaced` | always |
//! | `attention` | one per code and peer per [`ATTENTION_WINDOW`] |
//!
//! There is no event for a crossing, so no crossing can make a toast.

use std::collections::HashMap;
use std::fmt;

use ferry_core::Micros;
use ferry_core::api::PairingRole;
use ferry_net::Fingerprint;

use super::toast::Toast;

/// At most one connection toast per peer in this window.
pub const CONNECTION_WINDOW: Micros = Micros::from_millis(30_000);
/// One `attention` toast per code in this window.
pub const ATTENTION_WINDOW: Micros = Micros::from_millis(300_000);
/// The codes (per peer) the attention limiter remembers. A frontend has a handful.
const ATTENTION_KEYS_MAX: usize = 64;

/// The two windows. Production uses [`Limits::default`]; a test shortens them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub connection: Micros,
    pub attention: Micros,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connection: CONNECTION_WINDOW,
            attention: ATTENTION_WINDOW,
        }
    }
}

/// Text that must not reach a log: its `Debug` is `Secret(..)`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// What happened, as ctl's hooks tell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A pairing reached its code (`418 207`). `window_ms` is what is left of the window.
    PairCode {
        role: PairingRole,
        peer: String,
        code: Secret,
        window_ms: u64,
    },
    /// The pairing moved on without a result of its own: the local user answered, the
    /// acceptor listens for another attempt, or its window ended.
    PairMoved,
    /// The pin is written. `note` says why it carries another name than the peer sent.
    Paired {
        peer: String,
        note: Option<String>,
    },
    PairFailed {
        reason: String,
        message: String,
    },
    /// A trusted peer's link is up or down. `Connecting` is not an event.
    Link {
        peer: Fingerprint,
        name: String,
        up: bool,
    },
    /// The peer was unpaired or paired here: whatever the policy remembers of its link is void.
    Forget {
        peer: Fingerprint,
    },
    /// The other side unpaired this machine.
    Unpaired {
        peer: Fingerprint,
        name: String,
    },
    LayoutReplaced {
        winner: String,
    },
    /// A problem that needs the user: a frontend's `attention` entry that just appeared.
    Attention {
        code: String,
        message: String,
        hint: Option<String>,
        peer: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show it. A pairing kind takes the place of the pairing toast that is open.
    Show(Toast),
    /// Close the pair-code toast.
    ClosePairing,
}

/// What the policy keeps of one trusted peer's link.
#[derive(Debug)]
struct PeerLink {
    name: String,
    /// It has been up since it was paired here or since ferryd started. Until then nothing is
    /// lost: the first connect is silent, and so is a peer that is still down.
    seen_up: bool,
    /// The last state seen.
    up: bool,
    /// The state the last connection toast announced; `true` before there is one, because the
    /// silent first connect stands for it.
    announced_up: bool,
    /// The next connection toast may come at this time.
    until: Micros,
}

impl PeerLink {
    fn new(name: String) -> Self {
        Self {
            name,
            seen_up: false,
            up: false,
            announced_up: true,
            until: Micros::ZERO,
        }
    }

    /// The window swallowed a change: the last toast no longer says how the link is.
    fn pending(&self) -> bool {
        self.seen_up && self.up != self.announced_up
    }
}

/// The rules. One per daemon, owned by the notify task.
#[derive(Debug)]
pub struct Policy {
    limits: Limits,
    links: HashMap<Fingerprint, PeerLink>,
    /// When each (code, peer) last toasted.
    attention: HashMap<(String, Option<String>), Micros>,
    /// A pair-code toast is open.
    code_open: bool,
}

impl Policy {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            links: HashMap::new(),
            attention: HashMap::new(),
            code_open: false,
        }
    }

    pub fn handle(&mut self, now: Micros, event: Event) -> Vec<Action> {
        match event {
            Event::PairCode {
                role,
                peer,
                code,
                window_ms,
            } => {
                if role != PairingRole::Acceptor {
                    return Vec::new();
                }
                self.code_open = true;
                vec![Action::Show(Toast::pair_code(
                    &peer,
                    code.expose(),
                    window_ms,
                ))]
            }
            Event::PairMoved => self.close_code().into_iter().collect(),
            Event::Paired { peer, note } => {
                self.code_open = false;
                vec![Action::Show(Toast::paired(&peer, note.as_deref()))]
            }
            Event::PairFailed { reason, message } => {
                self.code_open = false;
                vec![Action::Show(Toast::pair_failed(&reason, &message))]
            }
            Event::Link { peer, name, up } => self.link(now, peer, name, up),
            Event::Forget { peer } => {
                self.links.remove(&peer);
                Vec::new()
            }
            Event::Unpaired { peer, name } => {
                self.links.remove(&peer);
                vec![Action::Show(Toast::unpaired(&name))]
            }
            Event::LayoutReplaced { winner } => {
                vec![Action::Show(Toast::layout_replaced(&winner))]
            }
            Event::Attention {
                code,
                message,
                hint,
                peer,
            } => self.attention(now, code, &message, hint.as_deref(), peer),
        }
    }

    /// The toasts whose time has come: a change the window swallowed, announced when it ends.
    pub fn tick(&mut self, now: Micros) -> Vec<Action> {
        let mut due: Vec<Fingerprint> = self
            .links
            .iter()
            .filter(|(_, l)| l.pending() && now >= l.until)
            .map(|(fp, _)| *fp)
            .collect();
        // a HashMap's order must not decide which toast comes first
        due.sort_unstable();
        due.into_iter()
            .filter_map(|fp| self.settle(now, fp))
            .map(Action::Show)
            .collect()
    }

    /// When [`tick`](Self::tick) next has something to do.
    pub fn next_deadline(&self) -> Option<Micros> {
        self.links
            .values()
            .filter(|l| l.pending())
            .map(|l| l.until)
            .min()
    }

    fn close_code(&mut self) -> Option<Action> {
        std::mem::take(&mut self.code_open).then_some(Action::ClosePairing)
    }

    fn link(&mut self, now: Micros, peer: Fingerprint, name: String, up: bool) -> Vec<Action> {
        let l = self
            .links
            .entry(peer)
            .or_insert_with(|| PeerLink::new(name.clone()));
        l.name = name;
        l.up = up;
        if !l.seen_up {
            l.seen_up = up;
            return Vec::new();
        }
        self.settle(now, peer)
            .map(Action::Show)
            .into_iter()
            .collect()
    }

    /// Announces the peer's state if the last toast does not say it and the window is over.
    fn settle(&mut self, now: Micros, peer: Fingerprint) -> Option<Toast> {
        let window = self.limits.connection;
        let l = self.links.get_mut(&peer)?;
        if !l.pending() || now < l.until {
            return None;
        }
        l.announced_up = l.up;
        l.until = now.plus(window);
        Some(if l.up {
            Toast::peer_back(&l.name)
        } else {
            Toast::peer_lost(&l.name)
        })
    }

    fn attention(
        &mut self,
        now: Micros,
        code: String,
        message: &str,
        hint: Option<&str>,
        peer: Option<String>,
    ) -> Vec<Action> {
        let window = self.limits.attention;
        if self.attention.len() >= ATTENTION_KEYS_MAX {
            self.attention.retain(|_, at| now.since(*at) < window);
            if self.attention.len() >= ATTENTION_KEYS_MAX {
                self.attention.clear();
            }
        }
        let toast = Toast::attention(message, hint, peer.as_deref());
        let key = (code, peer);
        if self
            .attention
            .get(&key)
            .is_some_and(|at| now.since(*at) < window)
        {
            return Vec::new();
        }
        self.attention.insert(key, now);
        vec![Action::Show(toast)]
    }
}

#[cfg(test)]
mod tests;
