//! OR-Set (Observed-Remove Set).
//!
//! Each `add(x)` attaches a unique tag; `remove(x)` removes only tags
//! the local replica has observed. Preserves "add wins concurrent with
//! remove" — the canonical eventually-consistent set CRDT.
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § Type catalog.
//! Scaffold — tombstone GC requires causal stability, deferred to
//! M05 iter D.

use crate::Crdt;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct OrSet<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> {
    /// Live elements with their unique tags.
    elements: HashSet<(T, u64)>,
    /// Tombstones — observed-but-removed tags. Causally-stable GC in
    /// M05 iter D bounds growth.
    tombstones: HashSet<(T, u64)>,
    next_tag: u64,
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> Default for OrSet<T> {
    fn default() -> Self {
        OrSet {
            elements: HashSet::new(),
            tombstones: HashSet::new(),
            next_tag: 0,
        }
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> OrSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, x: T) {
        self.elements.insert((x, self.next_tag));
        self.next_tag += 1;
    }

    /// Remove every observed tag for `x`.
    pub fn remove(&mut self, x: &T) {
        let removed: Vec<_> = self
            .elements
            .iter()
            .filter(|(e, _)| e == x)
            .cloned()
            .collect();
        for entry in removed {
            self.elements.remove(&entry);
            self.tombstones.insert(entry);
        }
    }

    pub fn contains(&self, x: &T) -> bool {
        self.elements.iter().any(|(e, _)| e == x)
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> Crdt for OrSet<T> {
    type Value = HashSet<T>;

    fn merge(&mut self, other: &Self) {
        // Union live elements, then drop any that match a tombstone.
        for e in &other.elements {
            self.elements.insert(e.clone());
        }
        for t in &other.tombstones {
            self.elements.remove(t);
            self.tombstones.insert(t.clone());
        }
        // Tag counter must dominate to avoid future collisions.
        if other.next_tag > self.next_tag {
            self.next_tag = other.next_tag;
        }
    }

    fn value(&self) -> HashSet<T> {
        self.elements.iter().map(|(e, _)| e.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_contains() {
        let mut s = OrSet::<&'static str>::new();
        s.add("alice");
        s.add("bob");
        assert!(s.contains(&"alice"));
        assert!(s.contains(&"bob"));
    }

    #[test]
    fn remove_drops_all_observed_tags() {
        let mut s = OrSet::<&'static str>::new();
        s.add("alice");
        s.add("alice");
        s.remove(&"alice");
        assert!(!s.contains(&"alice"));
    }

    #[test]
    fn merge_observes_tombstones() {
        // Replica A adds "alice"; replica B adds "alice" then removes.
        // Concurrent remove wins because B's tombstone observes A's
        // tag through the merge.
        let mut a = OrSet::<&'static str>::new();
        let mut b = OrSet::<&'static str>::new();
        a.add("alice");
        b.merge(&a);
        b.remove(&"alice");
        a.merge(&b);
        assert!(!a.contains(&"alice"));
    }

    #[test]
    fn merge_add_wins_concurrent_remove() {
        // Replica A adds "alice"; replica B concurrently removes (but
        // hasn't observed A's add). Result: alice survives.
        let mut a = OrSet::<&'static str>::new();
        let mut b = OrSet::<&'static str>::new();
        a.add("alice");
        b.remove(&"alice"); // b didn't see a's tag → no-op
        a.merge(&b);
        assert!(a.contains(&"alice"));
    }
}
