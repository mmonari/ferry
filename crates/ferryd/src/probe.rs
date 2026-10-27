//! Stall probes: Pings from the capture thread to a peer that has gone quiet while it
//! matters, so the first packet after a Wi-Fi stall or cut leaves within [`PROBE_EVERY`] of the
//! link coming back instead of at quinn's next loss probe, whose timeout doubles at each one.
//!
//! A peer is probed every [`PROBE_EVERY`] while either holds:
//!
//! | Case | Why |
//! |---|---|
//! | it is **suspect** (released for silence) | its `Pong` clears suspect, so the edge stops being a wall as soon as the link carries packets. Before, only the dialer's 2 s Ping cleared it |
//! | it is the target of the running crossing and nothing came from it for [`STALL_AFTER`] | `HeldAck`s normally arrive every ~100 ms; a probe that gets through makes quinn declare the stall's packets lost and resend the unacknowledged `Held` and keys at once, inside the 650 ms source liveness |
//!
//! Each probe is a `Ping` datagram; the peer's net thread answers with a `Pong` (net_thread.rs),
//! which reaches the machine as `PeerAlive`. quinn's window floor (ferry-net
//! `EndpointConfig::min_window`) keeps the probe leaving while the stall's packets fill the
//! window. Probing stops once the peer is heard, the crossing ends, or the connection closes
//! (quinn's idle timeout, 10 s), so a stall costs at most 10 Pings a second for at most that
//! long; an idle link sends nothing extra.

use std::collections::{HashMap, HashSet};

use ferry_core::source::{SourceRole, SourceStatus};
use ferry_core::{Micros, PeerId};

/// The probe period: the held-set snapshot rhythm.
pub const PROBE_EVERY: Micros = Micros::from_millis(100);
/// A crossing's target unheard this long is stalled: two snapshot periods, so an on-time
/// `HeldAck` (one each 100 ms plus the RTT) never starts a probe.
pub const STALL_AFTER: Micros = Micros::from_millis(200);
/// Stall probes carry ids with this bit set, so a capture tells them from the dialer's Pings.
pub const PROBE_ID_BIT: u32 = 1 << 31;

/// Why a peer is being probed, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Suspect,
    Stalled,
}

/// One episode of probing a peer, from its first probe until it is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    pub why: Why,
    pub since: Micros,
    pub probes: u32,
    last: Micros,
}

#[derive(Debug, Default)]
pub struct Prober {
    suspect: Vec<PeerId>,
    /// The target of the running crossing.
    remote: Option<PeerId>,
    /// When each peer was last heard (a `HeldAck`, `Ended`, `Ping`, `Pong` or a new connection),
    /// or when its crossing began.
    heard: HashMap<PeerId, Micros>,
    probing: HashMap<PeerId, Episode>,
    /// Peers with a connection handle: a suspect peer whose connection is gone stays suspect
    /// until its next `PeerUp`, and is not probed (nor woken for) meanwhile.
    connected: HashSet<PeerId>,
    next_id: u32,
}

impl Prober {
    pub fn new() -> Self {
        Self::default()
    }

    /// The source machine published a status: take its suspect peers and its target. Returns
    /// the episodes that ended: the peer was cleared of suspect (it was heard), or the crossing
    /// whose target was stalled is over.
    pub fn status(&mut self, s: &SourceStatus, now: Micros) -> Vec<(PeerId, Episode)> {
        self.suspect.clone_from(&s.suspect);
        let remote = (s.role == SourceRole::Remote).then_some(s.peer).flatten();
        if let Some(p) = remote
            && remote != self.remote
        {
            // A new crossing: the target has not had time to answer yet.
            self.heard.insert(p, now);
        }
        self.remote = remote;
        let mut ended = Vec::new();
        let peers: Vec<PeerId> = self.probing.keys().copied().collect();
        for p in peers {
            match self.why(p, now) {
                Some(why) => {
                    if let Some(e) = self.probing.get_mut(&p) {
                        e.why = why;
                    }
                }
                None => {
                    if let Some(e) = self.probing.remove(&p) {
                        ended.push((p, e));
                    }
                }
            }
        }
        ended
    }

    /// Something arrived from `p`. Returns the episode it ends, if `p` was being probed.
    pub fn heard(&mut self, p: PeerId, now: Micros) -> Option<Episode> {
        self.heard.insert(p, now);
        // A suspect peer stays wanted until the machine clears it (its next status).
        if self.suspect.contains(&p) {
            return None;
        }
        self.probing.remove(&p)
    }

    /// `p` has a connection handle to probe on.
    pub fn connected(&mut self, p: PeerId) {
        self.connected.insert(p);
    }

    /// `p`'s connection is gone: nothing to probe it on.
    pub fn forget(&mut self, p: PeerId) {
        self.connected.remove(&p);
        self.heard.remove(&p);
        self.probing.remove(&p);
    }

    fn why(&self, p: PeerId, now: Micros) -> Option<Why> {
        if !self.connected.contains(&p) {
            return None;
        }
        if self.suspect.contains(&p) {
            return Some(Why::Suspect);
        }
        let quiet = self
            .heard
            .get(&p)
            .is_none_or(|h| now.since(*h) >= STALL_AFTER);
        (self.remote == Some(p) && quiet).then_some(Why::Stalled)
    }

    fn candidates(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.suspect
            .iter()
            .copied()
            .chain(self.remote)
            .filter(|p| self.connected.contains(p))
    }

    /// When the next probe is due, if any peer is wanted or will be.
    pub fn deadline(&self) -> Option<Micros> {
        self.candidates()
            .map(|p| match self.probing.get(&p) {
                Some(e) => e.last.plus(PROBE_EVERY),
                None if self.suspect.contains(&p) => Micros::ZERO,
                None => self
                    .heard
                    .get(&p)
                    .map_or(Micros::ZERO, |h| h.plus(STALL_AFTER)),
            })
            .min()
    }

    /// The peers to Ping now, each with its probe id. Records the probes.
    pub fn due(&mut self, now: Micros) -> Vec<(PeerId, u32, Option<Why>)> {
        let mut out = Vec::new();
        let peers: Vec<PeerId> = self.candidates().collect();
        for p in peers {
            let Some(why) = self.why(p, now) else {
                continue;
            };
            let id = PROBE_ID_BIT | (self.next_id & !PROBE_ID_BIT);
            match self.probing.get_mut(&p) {
                Some(e) if now >= e.last.plus(PROBE_EVERY) => {
                    e.last = now;
                    e.probes += 1;
                    out.push((p, id, None));
                }
                Some(_) => continue,
                None => {
                    self.probing.insert(
                        p,
                        Episode {
                            why,
                            since: now,
                            probes: 1,
                            last: now,
                        },
                    );
                    out.push((p, id, Some(why)));
                }
            }
            self.next_id = self.next_id.wrapping_add(1);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_core::source::Gate;

    const A: PeerId = PeerId([1; 32]);
    const B: PeerId = PeerId([2; 32]);

    fn ms(n: u64) -> Micros {
        Micros::from_millis(n)
    }

    fn status(role: SourceRole, peer: Option<PeerId>, suspect: &[PeerId]) -> SourceStatus {
        SourceStatus {
            role,
            peer,
            session: 1,
            last_leave: None,
            gate: Gate::Armed,
            suspect: suspect.to_vec(),
            controlled: false,
            locked_here: false,
            locked_peers: Vec::new(),
            edge: None,
        }
    }

    fn peers(v: &[(PeerId, u32, Option<Why>)]) -> Vec<PeerId> {
        v.iter().map(|(p, _, _)| *p).collect()
    }

    #[test]
    fn an_idle_link_is_never_probed() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Local, None, &[]), ms(0));
        assert_eq!(pr.deadline(), None);
        assert!(pr.due(ms(10_000)).is_empty());
    }

    #[test]
    fn a_suspect_peer_is_probed_at_once_then_every_100_ms_until_the_machine_clears_it() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Local, None, &[B]), ms(1000));
        assert_eq!(pr.deadline(), Some(Micros::ZERO));
        let first = pr.due(ms(1000));
        assert_eq!(peers(&first), vec![B]);
        assert_eq!(first[0].2, Some(Why::Suspect));
        assert!(first[0].1 & PROBE_ID_BIT != 0);
        assert_eq!(pr.deadline(), Some(ms(1100)));
        assert!(pr.due(ms(1050)).is_empty());
        assert_eq!(pr.due(ms(1100))[0].2, None, "the same episode");
        // The Pong arrives: still suspect until the machine says so, so probing goes on.
        assert_eq!(pr.heard(B, ms(1150)), None);
        assert_eq!(peers(&pr.due(ms(1200))), vec![B]);
        // The machine cleared it (PeerAlive): probing stops, and the episode is reported.
        let ended = pr.status(&status(SourceRole::Local, None, &[]), ms(1210));
        assert_eq!(ended.len(), 1);
        assert_eq!(
            (ended[0].0, ended[0].1.since, ended[0].1.probes),
            (B, ms(1000), 3)
        );
        assert_eq!(pr.deadline(), None);
        assert!(pr.due(ms(5000)).is_empty());
    }

    #[test]
    fn an_answering_target_is_never_probed_during_a_crossing() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Remote, Some(B), &[]), ms(0));
        // HeldAcks every 100 ms plus a few ms of RTT.
        for t in (0..2000).step_by(5) {
            if t % 100 == 5 {
                pr.heard(B, ms(t));
            }
            assert!(pr.due(ms(t)).is_empty(), "probed at {t} ms");
        }
    }

    #[test]
    fn a_quiet_target_is_probed_200_ms_after_it_was_last_heard_and_stops_when_heard() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Remote, Some(B), &[]), ms(0));
        pr.heard(B, ms(105));
        assert_eq!(pr.deadline(), Some(ms(305)));
        assert!(pr.due(ms(304)).is_empty());
        let d = pr.due(ms(305));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].2, Some(Why::Stalled));
        assert_eq!(pr.deadline(), Some(ms(405)));
        assert_eq!(peers(&pr.due(ms(405))), vec![B]);
        let ep = pr.heard(B, ms(450)).expect("the episode ends");
        assert_eq!((ep.why, ep.since, ep.probes), (Why::Stalled, ms(305), 2));
        assert_eq!(pr.deadline(), Some(ms(650)));
    }

    #[test]
    fn a_new_crossing_gives_the_target_its_full_grace() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        // Last heard long ago, while idle.
        pr.heard(B, ms(0));
        pr.status(&status(SourceRole::Remote, Some(B), &[]), ms(10_000));
        assert!(pr.due(ms(10_100)).is_empty());
        assert_eq!(pr.deadline(), Some(ms(10_200)));
    }

    #[test]
    fn the_crossing_ending_stops_a_stall_probe_and_a_silence_release_turns_it_into_suspect() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Remote, Some(B), &[]), ms(0));
        assert_eq!(peers(&pr.due(ms(200))), vec![B]);
        // Home by hand: no target, nothing to probe.
        assert_eq!(
            pr.status(&status(SourceRole::Local, None, &[]), ms(250))
                .len(),
            1
        );
        assert_eq!(pr.deadline(), None);
        // Another crossing that ends by silence: suspect, probed at once.
        pr.status(&status(SourceRole::Remote, Some(B), &[]), ms(1000));
        assert_eq!(peers(&pr.due(ms(1200))), vec![B]);
        assert!(
            pr.status(&status(SourceRole::Local, None, &[B]), ms(1650))
                .is_empty()
        );
        assert_eq!(
            pr.deadline(),
            Some(ms(1300)),
            "the episode goes on, now as suspect"
        );
        assert_eq!(peers(&pr.due(ms(1650))), vec![B]);
        let ended = pr.status(&status(SourceRole::Local, None, &[]), ms(1700));
        assert_eq!((ended[0].1.why, ended[0].1.since), (Why::Suspect, ms(1200)));
    }

    #[test]
    fn a_suspect_peer_without_a_connection_is_neither_probed_nor_woken_for() {
        let mut pr = Prober::new();
        pr.status(&status(SourceRole::Local, None, &[B]), ms(0));
        assert_eq!(pr.deadline(), None);
        assert!(pr.due(ms(0)).is_empty());
        pr.connected(B);
        assert_eq!(pr.deadline(), Some(Micros::ZERO));
        assert_eq!(peers(&pr.due(ms(10))), vec![B]);
        pr.forget(B);
        assert_eq!(pr.deadline(), None);
    }

    #[test]
    fn a_closed_connection_is_forgotten_and_peers_are_probed_independently() {
        let mut pr = Prober::new();
        pr.connected(A);
        pr.connected(B);
        pr.status(&status(SourceRole::Local, None, &[A, B]), ms(0));
        assert_eq!(pr.due(ms(0)).len(), 2);
        pr.forget(A);
        pr.status(&status(SourceRole::Local, None, &[B]), ms(10));
        assert_eq!(peers(&pr.due(ms(100))), vec![B]);
        assert_eq!(pr.deadline(), Some(ms(200)));
    }
}
