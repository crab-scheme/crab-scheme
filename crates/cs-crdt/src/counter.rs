//! Counter CRDTs.
//!
//! - **G-Counter** — grow-only. Per-replica vector; value = sum.
//! - **PN-Counter** — pair of G-Counters (positive + negative).
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § Type catalog.
//! Scaffold — merge / inc / value to implement in M05 iter B.

use crate::clock::NodeId;
use crate::Crdt;
use std::collections::HashMap;

/// Grow-only counter. Increment-only; merge takes per-replica max.
#[derive(Debug, Clone, Default)]
pub struct GCounter {
    per_replica: HashMap<NodeId, u64>,
}

impl GCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Local increment by `n` at replica `node`.
    pub fn inc(&mut self, node: &NodeId, n: u64) {
        let cur = self.per_replica.get(node).copied().unwrap_or(0);
        self.per_replica.insert(node.clone(), cur + n);
    }
}

impl Crdt for GCounter {
    type Value = u64;

    fn merge(&mut self, other: &Self) {
        for (node, count) in &other.per_replica {
            let cur = self.per_replica.get(node).copied().unwrap_or(0);
            if *count > cur {
                self.per_replica.insert(node.clone(), *count);
            }
        }
    }

    fn value(&self) -> u64 {
        self.per_replica.values().sum()
    }
}

/// Counter supporting both increment and decrement.
#[derive(Debug, Clone, Default)]
pub struct PNCounter {
    p: GCounter,
    n: GCounter,
}

impl PNCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc(&mut self, node: &NodeId, delta: i64) {
        if delta >= 0 {
            self.p.inc(node, delta as u64);
        } else {
            self.n.inc(node, (-delta) as u64);
        }
    }
}

impl Crdt for PNCounter {
    type Value = i64;

    fn merge(&mut self, other: &Self) {
        self.p.merge(&other.p);
        self.n.merge(&other.n);
    }

    fn value(&self) -> i64 {
        self.p.value() as i64 - self.n.value() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_counter_sums_per_replica() {
        let mut c = GCounter::new();
        let a: NodeId = "a".into();
        let b: NodeId = "b".into();
        c.inc(&a, 3);
        c.inc(&b, 5);
        assert_eq!(c.value(), 8);
    }

    #[test]
    fn g_counter_merge_takes_max() {
        let a_node: NodeId = "a".into();
        let mut x = GCounter::new();
        let mut y = GCounter::new();
        x.inc(&a_node, 3);
        y.inc(&a_node, 7);
        x.merge(&y);
        assert_eq!(x.value(), 7);
    }

    #[test]
    fn pn_counter_handles_dec() {
        let mut c = PNCounter::new();
        let a: NodeId = "a".into();
        c.inc(&a, 5);
        c.inc(&a, -2);
        assert_eq!(c.value(), 3);
    }

    #[test]
    fn pn_counter_merge_is_idempotent() {
        let a: NodeId = "a".into();
        let mut x = PNCounter::new();
        let mut y = PNCounter::new();
        x.inc(&a, 10);
        y.inc(&a, -3);
        x.merge(&y);
        let before = x.value();
        x.merge(&y);
        assert_eq!(x.value(), before);
    }
}
