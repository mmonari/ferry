//! Which layout edits are this peer's own, and whether one was lost (M3 design 4.3).
//!
//! `layout.set` and `layout.place` remember the revision they made. A peer that keeps a layout
//! another peer pushed, or already holds it, pushes it back when that peer made the revision (the
//! echo, `ctl`); an equal revision from a peer therefore says "I hold your edit". When a higher
//! revision then replaces this peer's own edit, the edit lost if its author never held ours: the
//! author made its change on top of ours otherwise, and nothing was lost.
//!
//! Pure bookkeeping. `ctl` feeds it and calls `on_layout_replaced` with the answer.

use ferry_core::PeerId;
use ferry_core::geom::Revision;

/// The latest edit this peer made, and the peers known to hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnEdit {
    rev: Revision,
    acked: Vec<PeerId>,
}

/// What this peer remembers of its own layout edit. In memory only: an edit made before a restart
/// is not tracked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditLog {
    own: Option<OwnEdit>,
}

impl EditLog {
    /// This peer made `rev` with `layout.set` or `layout.place`. Nobody holds it yet.
    pub fn made(&mut self, rev: Revision) {
        self.own = Some(OwnEdit {
            rev,
            acked: Vec::new(),
        });
    }

    /// `from` has the layout at revision `rev`: it kept ours, or pushed it back. Only the peers
    /// that are pinned here can say so, so the list stays as short as the peers are few.
    pub fn held_by(&mut self, rev: Revision, from: PeerId) {
        if let Some(own) = &mut self.own
            && own.rev == rev
            && !own.acked.contains(&from)
        {
            own.acked.push(from);
        }
    }

    /// A pushed layout `new` replaced the one at `old`. Answers the peer that won when `old` was
    /// this peer's own edit and the winner's change was not made on top of it; `None` otherwise.
    /// Either way the layout has moved on, so the record is closed.
    pub fn replaced(&mut self, old: Revision, new: Revision) -> Option<PeerId> {
        let own = self.own.take()?;
        (own.rev == old && !own.acked.contains(&new.peer)).then_some(new.peer)
    }

    /// The revision of the edit being tracked.
    pub fn tracked(&self) -> Option<Revision> {
        self.own.as_ref().map(|o| o.rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> PeerId {
        PeerId([n; 32])
    }

    fn rev(counter: u64, n: u8) -> Revision {
        Revision {
            counter,
            peer: id(n),
        }
    }

    #[test]
    fn an_edit_that_a_peer_replaces_without_having_held_it_is_lost() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        // lap (2) pushes revision 3 by lap: same counter, higher peer; lap never held ours
        assert_eq!(log.replaced(rev(3, 1), rev(3, 2)), Some(id(2)));
        assert_eq!(log.tracked(), None, "closed");
    }

    #[test]
    fn a_change_made_on_top_of_our_edit_is_not_a_loss() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        // lap kept our edit and pushed it back, then edited
        log.held_by(rev(3, 1), id(2));
        assert_eq!(log.replaced(rev(3, 1), rev(4, 2)), None);
    }

    #[test]
    fn only_the_author_of_the_winning_revision_counts_as_having_held_it() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        log.held_by(rev(3, 1), id(2));
        // tower (3) held nothing; lap's acknowledgement is not its
        assert_eq!(log.replaced(rev(3, 1), rev(5, 3)), Some(id(3)));
    }

    #[test]
    fn a_revision_that_is_not_ours_replaced_is_not_our_loss() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        // the current layout is not the one we made (a peer's higher edit came in between)
        assert_eq!(log.replaced(rev(4, 2), rev(5, 3)), None);
        assert_eq!(log.tracked(), None);
        // nothing made, nothing lost
        assert_eq!(EditLog::default().replaced(rev(1, 1), rev(2, 2)), None);
    }

    #[test]
    fn an_acknowledgement_of_another_revision_is_ignored_and_repeats_are_kept_once() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        log.held_by(rev(2, 1), id(2));
        log.held_by(rev(3, 9), id(2));
        assert_eq!(log.replaced(rev(3, 1), rev(4, 2)), Some(id(2)));
        log.made(rev(7, 1));
        log.held_by(rev(7, 1), id(2));
        log.held_by(rev(7, 1), id(2));
        assert_eq!(log.own.as_ref().map(|o| o.acked.len()), Some(1));
    }

    #[test]
    fn a_newer_edit_starts_a_new_record() {
        let mut log = EditLog::default();
        log.made(rev(3, 1));
        log.held_by(rev(3, 1), id(2));
        log.made(rev(4, 1));
        assert_eq!(log.tracked(), Some(rev(4, 1)));
        // lap held revision 3, not 4
        assert_eq!(log.replaced(rev(4, 1), rev(4, 2)), Some(id(2)));
    }
}
