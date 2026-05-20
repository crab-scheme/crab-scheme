//! Hybrid Logical Clock (HLC) + Dotted Version Vectors (DVV).
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § Causality.
//!
//! HLC = `<physical_ms | logical_u16>`. CockroachDB / Yugabyte /
//! Cassandra Accord all converge here. DVV = version vector + a single
//! dot per event; size bounded by replication factor (Almeida 2012).

use std::collections::HashMap;

/// Cluster-stable identifier for a CRDT replica. The `String` is the
/// canonical node label (see cs-distrib::NodeId::label).
pub type NodeId = String;

/// 64-bit Hybrid Logical Clock: 48 high bits of physical milliseconds
/// since the Unix epoch + 16 low bits of logical counter to break ties
/// within the same millisecond.
///
/// Monotonic by construction: `Hlc::now(prev)` returns a value
/// strictly greater than `prev`, clamping the logical counter to bump
/// when the physical wall-clock failed to advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hlc(u64);

impl Hlc {
    pub const fn from_raw(raw: u64) -> Self {
        Hlc(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn physical_ms(self) -> u64 {
        self.0 >> 16
    }

    pub const fn logical(self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    /// Construct an HLC at the given physical millisecond with logical
    /// counter zero. Real `Hlc::now()` impl lands in M05 iter A — it
    /// must consult wall-clock + a monotonic guard.
    pub const fn at(physical_ms: u64) -> Self {
        Hlc(physical_ms << 16)
    }

    /// Returns `self < other` (strict).
    pub const fn before(self, other: Self) -> bool {
        self.0 < other.0
    }
}

/// Dotted Version Vector. Per-replica counter map + a single "dot"
/// `(node, seq)` capturing the specific event this DVV describes.
///
/// Used as causal context on `OR-Set` / `OR-Map` / `MV-Register`. Size
/// bounded by replication factor, not by client count — the Riak 2.0
/// improvement over classical vector clocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dvv {
    counters: HashMap<NodeId, u64>,
    dot: Option<(NodeId, u64)>,
}

impl Dvv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tick the local counter for `node`; sets the dot to the new value.
    /// Returns the new sequence number for that node.
    pub fn tick(&mut self, node: &NodeId) -> u64 {
        let next = self.counters.get(node).copied().unwrap_or(0) + 1;
        self.counters.insert(node.clone(), next);
        self.dot = Some((node.clone(), next));
        next
    }

    /// Merge another DVV's counters, taking the per-node max. The dot
    /// is cleared (this DVV no longer describes a single event).
    pub fn merge(&mut self, other: &Dvv) {
        for (node, seq) in &other.counters {
            let cur = self.counters.get(node).copied().unwrap_or(0);
            if *seq > cur {
                self.counters.insert(node.clone(), *seq);
            }
        }
        self.dot = None;
    }

    /// Did this DVV already observe the event identified by `(node, seq)`?
    pub fn dominates_dot(&self, node: &NodeId, seq: u64) -> bool {
        self.counters.get(node).copied().unwrap_or(0) >= seq
    }

    pub fn dot(&self) -> Option<&(NodeId, u64)> {
        self.dot.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_encoding_is_lossless() {
        let h = Hlc::at(1_700_000_000_000);
        assert_eq!(h.physical_ms(), 1_700_000_000_000);
        assert_eq!(h.logical(), 0);
        // Different physical ms ⇒ different HLC.
        let h2 = Hlc::at(1_700_000_000_001);
        assert!(h.before(h2));
    }

    #[test]
    fn dvv_tick_records_dot_and_counter() {
        let mut d = Dvv::new();
        let n: NodeId = "node-a".into();
        let seq = d.tick(&n);
        assert_eq!(seq, 1);
        assert_eq!(d.dot(), Some(&(n.clone(), 1)));
        d.tick(&n);
        assert_eq!(d.dot(), Some(&(n, 2)));
    }

    #[test]
    fn dvv_merge_takes_per_node_max() {
        let mut a = Dvv::new();
        let mut b = Dvv::new();
        let na: NodeId = "node-a".into();
        let nb: NodeId = "node-b".into();
        a.tick(&na);
        a.tick(&na);
        b.tick(&nb);
        a.merge(&b);
        assert!(a.dominates_dot(&na, 2));
        assert!(a.dominates_dot(&nb, 1));
        // Merge clears the dot.
        assert!(a.dot().is_none());
    }

    #[test]
    fn dvv_dominates_is_ge() {
        let mut a = Dvv::new();
        let na: NodeId = "node-a".into();
        a.tick(&na);
        a.tick(&na);
        assert!(a.dominates_dot(&na, 1));
        assert!(a.dominates_dot(&na, 2));
        assert!(!a.dominates_dot(&na, 3));
    }
}
