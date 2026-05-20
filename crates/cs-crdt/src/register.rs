//! Register CRDTs.
//!
//! - **LWW-Register** — Last-Writer-Wins by HLC timestamp. Concurrent
//!   writes are silently dropped (the spec-doc default is MV, NOT LWW;
//!   LWW is exposed but its footgun is documented).
//! - **MV-Register** — Multi-Value. Concurrent writes survive as a set,
//!   resolution is the application's call.

use crate::clock::{Dvv, Hlc, NodeId};
use crate::Crdt;
use std::collections::HashSet;

/// Last-Writer-Wins register.
///
/// **Footgun**: concurrent writes vanish silently. Use MV-Register
/// instead unless you genuinely want last-write-wins semantics.
#[derive(Debug, Clone)]
pub struct LwwRegister<T: Clone + std::fmt::Debug + Send + Sync> {
    value: Option<T>,
    ts: Hlc,
    writer: NodeId,
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Default> Default for LwwRegister<T> {
    fn default() -> Self {
        LwwRegister {
            value: None,
            ts: Hlc::default(),
            writer: String::new(),
        }
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync> LwwRegister<T> {
    pub fn new() -> Self
    where
        T: Default,
    {
        Self::default()
    }

    pub fn set(&mut self, v: T, ts: Hlc, writer: NodeId) {
        if ts > self.ts || (ts == self.ts && writer > self.writer) {
            self.value = Some(v);
            self.ts = ts;
            self.writer = writer;
        }
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync> Crdt for LwwRegister<T> {
    type Value = Option<T>;

    fn merge(&mut self, other: &Self) {
        // Same tiebreaker as set(): higher HLC, then higher writer
        // NodeId.
        if other.ts > self.ts || (other.ts == self.ts && other.writer > self.writer) {
            self.value = other.value.clone();
            self.ts = other.ts;
            self.writer = other.writer.clone();
        }
    }

    fn value(&self) -> Option<T> {
        self.value.clone()
    }
}

/// Multi-Value register. Concurrent writes survive as a set; app
/// resolves.
#[derive(Debug, Clone)]
pub struct MvRegister<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> {
    /// Each entry is (value, the DVV at the moment of write). Merge
    /// keeps the values whose DVV is not dominated by any other.
    entries: HashSet<MvEntry<T>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MvEntry<T: Clone + std::fmt::Debug + Eq + std::hash::Hash> {
    value: T,
    dot: (NodeId, u64),
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> Default for MvRegister<T> {
    fn default() -> Self {
        MvRegister {
            entries: HashSet::new(),
        }
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> MvRegister<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Local write. The caller-provided DVV must already carry the dot
    /// for this write (see `Dvv::tick`). Real merge dominance logic
    /// lands in M05 iter C; this scaffold just inserts.
    pub fn set(&mut self, v: T, dvv: &Dvv) {
        if let Some((node, seq)) = dvv.dot().cloned() {
            self.entries.insert(MvEntry {
                value: v,
                dot: (node, seq),
            });
        }
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash> Crdt for MvRegister<T> {
    type Value = HashSet<T>;

    fn merge(&mut self, other: &Self) {
        // Scaffold: union. The real merge drops entries whose dot is
        // dominated by the other replica's DVV — implementation in M05
        // iter C.
        for e in &other.entries {
            self.entries.insert(e.clone());
        }
    }

    fn value(&self) -> HashSet<T> {
        self.entries.iter().map(|e| e.value.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lww_keeps_latest_by_hlc() {
        let mut r = LwwRegister::<u32>::new();
        r.set(1, Hlc::at(100), "a".into());
        r.set(2, Hlc::at(200), "b".into());
        assert_eq!(r.value(), Some(2));
        // Older write doesn't overwrite.
        r.set(3, Hlc::at(50), "c".into());
        assert_eq!(r.value(), Some(2));
    }

    #[test]
    fn lww_tiebreaks_by_writer_node_id() {
        let mut r = LwwRegister::<u32>::new();
        r.set(1, Hlc::at(100), "a".into());
        // Same HLC; higher node-id wins.
        r.set(2, Hlc::at(100), "b".into());
        assert_eq!(r.value(), Some(2));
    }

    #[test]
    fn mv_register_union_on_merge() {
        let mut a = MvRegister::<u32>::new();
        let mut b = MvRegister::<u32>::new();
        let mut da = Dvv::new();
        let mut db = Dvv::new();
        da.tick(&"a".into());
        db.tick(&"b".into());
        a.set(10, &da);
        b.set(20, &db);
        a.merge(&b);
        let v = a.value();
        assert!(v.contains(&10));
        assert!(v.contains(&20));
    }
}
