//! Causal Map (OR-Map): keys map to embedded CRDTs.
//!
//! Per-key OR-Set semantics for the key set; per-key embedded CRDT
//! merge for values. No cross-key atomicity.

use crate::Crdt;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CausalMap<K, V>
where
    K: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash,
    V: Crdt,
{
    entries: HashMap<K, V>,
}

impl<K, V> Default for CausalMap<K, V>
where
    K: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash,
    V: Crdt,
{
    fn default() -> Self {
        CausalMap {
            entries: HashMap::new(),
        }
    }
}

impl<K, V> CausalMap<K, V>
where
    K: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash,
    V: Crdt,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, k: K, v: V) {
        self.entries.insert(k, v);
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        self.entries.get(k)
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        // Tombstone semantics deferred to M05 iter D — for now a
        // plain remove. The scaffold contract is: any reader of this
        // module knows the GC behavior is incomplete.
        self.entries.remove(k)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K, V> Crdt for CausalMap<K, V>
where
    K: Clone + std::fmt::Debug + Send + Sync + Eq + std::hash::Hash,
    V: Crdt,
{
    type Value = HashMap<K, V::Value>;

    fn merge(&mut self, other: &Self) {
        for (k, other_v) in &other.entries {
            match self.entries.get_mut(k) {
                Some(self_v) => self_v.merge(other_v),
                None => {
                    self.entries.insert(k.clone(), other_v.clone());
                }
            }
        }
    }

    fn value(&self) -> HashMap<K, V::Value> {
        self.entries
            .iter()
            .map(|(k, v)| (k.clone(), v.value()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::NodeId;
    use crate::counter::PNCounter;

    #[test]
    fn map_of_counters_merges_per_key() {
        let mut a: CausalMap<&'static str, PNCounter> = CausalMap::new();
        let mut b: CausalMap<&'static str, PNCounter> = CausalMap::new();
        let node_a: NodeId = "node-a".into();
        let node_b: NodeId = "node-b".into();

        let mut likes_a = PNCounter::new();
        likes_a.inc(&node_a, 3);
        a.insert("post-1", likes_a);

        let mut likes_b = PNCounter::new();
        likes_b.inc(&node_b, 5);
        b.insert("post-1", likes_b);

        a.merge(&b);
        // Same key; per-key counter merges = 8.
        assert_eq!(a.get(&"post-1").unwrap().value(), 8);
    }
}
