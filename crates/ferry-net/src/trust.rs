//! The set of pinned peer fingerprints, shared with the TLS verifiers.

use crate::Fingerprint;
use std::collections::HashSet;
use std::sync::{Arc, PoisonError, RwLock};

/// The trusted peers: a shared, mutable set of pinned fingerprints.
///
/// Clones share one set. The verifiers read it on every handshake, so a pin added with
/// [`TrustSet::insert`] (ferryd's `ferry trust`) applies to the next handshake, and a removed
/// pin refuses the next one. Removing a pin does not close a live connection; the caller does
/// that.
///
/// The store is memory only: ferryd persists it (`trusted/`).
#[derive(Clone, Default)]
pub struct TrustSet {
    inner: Arc<RwLock<HashSet<Fingerprint>>>,
}

impl TrustSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins `fp`. Returns `false` if it was already pinned.
    pub fn insert(&self, fp: Fingerprint) -> bool {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(fp)
    }

    /// Unpins `fp`. Returns `false` if it was not pinned.
    pub fn remove(&self, fp: &Fingerprint) -> bool {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(fp)
    }

    pub fn contains(&self, fp: &Fingerprint) -> bool {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(fp)
    }

    /// Replaces every pin with `fps`, atomically for the verifiers.
    pub fn replace(&self, fps: impl IntoIterator<Item = Fingerprint>) {
        let new: HashSet<_> = fps.into_iter().collect();
        *self.inner.write().unwrap_or_else(PoisonError::into_inner) = new;
    }

    /// A sorted copy of the pinned fingerprints.
    pub fn snapshot(&self) -> Vec<Fingerprint> {
        let mut v: Vec<_> = self
            .inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect();
        v.sort();
        v
    }
}

impl std::fmt::Debug for TrustSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.snapshot()).finish()
    }
}

impl FromIterator<Fingerprint> for TrustSet {
    fn from_iter<I: IntoIterator<Item = Fingerprint>>(iter: I) -> Self {
        Self {
            inner: Arc::new(RwLock::new(iter.into_iter().collect())),
        }
    }
}
