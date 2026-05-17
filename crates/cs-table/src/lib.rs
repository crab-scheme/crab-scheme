//! `cs-table` — in-memory tables, ETS-shaped.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`:
//! - v1 supports `set` (hash-backed) and `ordered_set`
//!   (btree-backed).
//! - All tables are public (no protected / private ACLs in v1).
//! - Read returns an `Arc` of the stored Payload — readers and
//!   writers don't race on Payload mutation, since Payload is
//!   itself reference-counted and treated as immutable from
//!   Scheme's view (BEAM-style copy-on-send is enforced at the
//!   primop boundary that wraps these accessors).
//!
//! ## Rust / Scheme split
//!
//! Rust (this crate) owns: concurrent storage, atomic CRUD,
//! ordered iteration, lifecycle (create / drop / clear). Five
//! primops cross to Scheme: `make-table`, `table-insert!`,
//! `table-lookup`, `table-delete!`, `table-fold`.
//!
//! Scheme owns: transactional patterns ("single-writer-actor"
//! convention, optimistic-CAS retries, multi-table coordination),
//! schema sketches, key serialization beyond the default
//! string/integer encoding.
//!
//! ## Quick start
//!
//! ```
//! use std::sync::Arc;
//! use cs_table::{TableRegistry, TableType, Key};
//!
//! let reg = TableRegistry::new();
//! reg.create("users", TableType::Set).unwrap();
//!
//! reg.insert("users", Key::String("alice".into()), Arc::new(42i64)).unwrap();
//! let v = reg.lookup("users", &Key::String("alice".into())).unwrap();
//! assert_eq!(v.and_then(|p| p.downcast_ref::<i64>().copied()), Some(42));
//! ```

use std::sync::{Arc, RwLock};

use dashmap::DashMap;
use thiserror::Error;

// ---------- Types ----------

/// Which kind of table.
///
/// `Set`: hash-keyed, O(1) lookup/insert/delete.
/// `OrderedSet`: btree-keyed, O(log n) ops + ordered iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableType {
    Set,
    OrderedSet,
}

/// Canonical key type for a Scheme value used as a table key.
///
/// `Set` tables only need `Hash + Eq`; `OrderedSet` also needs
/// `Ord`. The Key enum encodes the small set of Scheme values
/// that have sensible Hash/Ord — extending this is a Phase
/// follow-up when we wire real Value transport (post-B3).
///
/// B4 supports integers, strings, and (raw) byte vectors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Fixnum(i64),
    String(String),
    Bytes(Vec<u8>),
}

/// Type-erased Send+Sync value held in a table cell.
///
/// Mirrors `cs_actor::Payload`. When the Scheme primop layer
/// wraps the table API, it deep-clones the incoming Value into
/// a Send+Sync wrapper before insert, and deep-clones the
/// retrieved Arc back into a fresh Value on lookup. That's BEAM's
/// copy-semantics applied to table reads — same trade as
/// cross-actor sends.
pub type Payload = Arc<dyn std::any::Any + Send + Sync>;

// ---------- Errors ----------

#[derive(Debug, Error)]
pub enum TableError {
    #[error("table {name:?} already exists")]
    AlreadyExists { name: String },
    #[error("table {name:?} not found")]
    NotFound { name: String },
    #[error("wrong table type: {name:?} is {actual:?}, expected {expected:?}")]
    WrongType {
        name: String,
        actual: TableType,
        expected: TableType,
    },
}

// ---------- Storage ----------

enum TableStorage {
    Set(DashMap<Key, Payload>),
    OrderedSet(RwLock<std::collections::BTreeMap<Key, Payload>>),
}

impl TableStorage {
    fn table_type(&self) -> TableType {
        match self {
            TableStorage::Set(_) => TableType::Set,
            TableStorage::OrderedSet(_) => TableType::OrderedSet,
        }
    }

    fn insert(&self, key: Key, value: Payload) {
        match self {
            TableStorage::Set(m) => {
                m.insert(key, value);
            }
            TableStorage::OrderedSet(rw) => {
                rw.write().expect("ordered_set poisoned").insert(key, value);
            }
        }
    }

    fn lookup(&self, key: &Key) -> Option<Payload> {
        match self {
            TableStorage::Set(m) => m.get(key).map(|e| e.value().clone()),
            TableStorage::OrderedSet(rw) => {
                rw.read().expect("ordered_set poisoned").get(key).cloned()
            }
        }
    }

    fn delete(&self, key: &Key) -> bool {
        match self {
            TableStorage::Set(m) => m.remove(key).is_some(),
            TableStorage::OrderedSet(rw) => rw
                .write()
                .expect("ordered_set poisoned")
                .remove(key)
                .is_some(),
        }
    }

    fn len(&self) -> usize {
        match self {
            TableStorage::Set(m) => m.len(),
            TableStorage::OrderedSet(rw) => rw.read().expect("ordered_set poisoned").len(),
        }
    }

    /// Apply `f` to every (key, value) pair, accumulating into `acc`.
    /// Set tables iterate in arbitrary order; OrderedSet tables
    /// iterate in ascending key order.
    fn fold<A>(&self, mut acc: A, mut f: impl FnMut(A, &Key, &Payload) -> A) -> A {
        match self {
            TableStorage::Set(m) => {
                for entry in m.iter() {
                    acc = f(acc, entry.key(), entry.value());
                }
                acc
            }
            TableStorage::OrderedSet(rw) => {
                let guard = rw.read().expect("ordered_set poisoned");
                for (k, v) in guard.iter() {
                    acc = f(acc, k, v);
                }
                acc
            }
        }
    }

    fn clear(&self) {
        match self {
            TableStorage::Set(m) => m.clear(),
            TableStorage::OrderedSet(rw) => rw.write().expect("ordered_set poisoned").clear(),
        }
    }
}

// ---------- Registry ----------

/// Process-wide registry of named tables. Tables live for the
/// lifetime of the registry; `drop_table` removes them explicitly.
///
/// Cloneable cheaply (Arc-shared internal map).
#[derive(Clone, Default)]
pub struct TableRegistry {
    tables: Arc<DashMap<String, Arc<TableStorage>>>,
}

impl TableRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a table. Returns `AlreadyExists` if a table of the
    /// same name is already registered (any type).
    pub fn create(&self, name: &str, ty: TableType) -> Result<(), TableError> {
        let storage = match ty {
            TableType::Set => TableStorage::Set(DashMap::new()),
            TableType::OrderedSet => {
                TableStorage::OrderedSet(RwLock::new(std::collections::BTreeMap::new()))
            }
        };
        match self.tables.entry(name.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(TableError::AlreadyExists {
                name: name.to_string(),
            }),
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(Arc::new(storage));
                Ok(())
            }
        }
    }

    /// Drop a table and discard all its entries. No-op if the
    /// table doesn't exist (idempotent shutdown).
    pub fn drop_table(&self, name: &str) {
        self.tables.remove(name);
    }

    /// List all registered table names (snapshot).
    pub fn names(&self) -> Vec<String> {
        self.tables.iter().map(|e| e.key().clone()).collect()
    }

    /// Look up the type of a table.
    pub fn table_type(&self, name: &str) -> Result<TableType, TableError> {
        let t = self.lookup_storage(name)?;
        Ok(t.table_type())
    }

    /// Insert a key-value pair into a table. Overwrites any prior
    /// value for the same key (set / ordered_set semantics —
    /// duplicate handling is bag / duplicate_bag, not v1).
    pub fn insert(&self, name: &str, key: Key, value: Payload) -> Result<(), TableError> {
        let t = self.lookup_storage(name)?;
        t.insert(key, value);
        Ok(())
    }

    /// Look up a key; returns `Ok(None)` for missing keys.
    pub fn lookup(&self, name: &str, key: &Key) -> Result<Option<Payload>, TableError> {
        let t = self.lookup_storage(name)?;
        Ok(t.lookup(key))
    }

    /// Delete a key. Returns whether the key was present.
    pub fn delete(&self, name: &str, key: &Key) -> Result<bool, TableError> {
        let t = self.lookup_storage(name)?;
        Ok(t.delete(key))
    }

    /// Number of entries currently in the table.
    pub fn size(&self, name: &str) -> Result<usize, TableError> {
        let t = self.lookup_storage(name)?;
        Ok(t.len())
    }

    /// Fold over the table's (key, value) pairs. Set tables
    /// iterate in arbitrary order; OrderedSet in ascending key
    /// order.
    pub fn fold<A>(
        &self,
        name: &str,
        acc: A,
        f: impl FnMut(A, &Key, &Payload) -> A,
    ) -> Result<A, TableError> {
        let t = self.lookup_storage(name)?;
        Ok(t.fold(acc, f))
    }

    /// Drop all entries from a table without removing the table.
    pub fn clear(&self, name: &str) -> Result<(), TableError> {
        let t = self.lookup_storage(name)?;
        t.clear();
        Ok(())
    }

    fn lookup_storage(&self, name: &str) -> Result<Arc<TableStorage>, TableError> {
        self.tables
            .get(name)
            .map(|e| e.value().clone())
            .ok_or_else(|| TableError::NotFound {
                name: name.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(n: i64) -> Payload {
        Arc::new(n)
    }

    fn as_i64(p: Option<Payload>) -> Option<i64> {
        p.and_then(|p| p.downcast_ref::<i64>().copied())
    }

    #[test]
    fn set_crud_roundtrip() {
        let r = TableRegistry::new();
        r.create("t", TableType::Set).unwrap();
        r.insert("t", Key::String("a".into()), val(1)).unwrap();
        r.insert("t", Key::String("b".into()), val(2)).unwrap();
        assert_eq!(r.size("t").unwrap(), 2);
        assert_eq!(
            as_i64(r.lookup("t", &Key::String("a".into())).unwrap()),
            Some(1)
        );
        assert_eq!(
            as_i64(r.lookup("t", &Key::String("b".into())).unwrap()),
            Some(2)
        );
        assert_eq!(
            as_i64(r.lookup("t", &Key::String("z".into())).unwrap()),
            None
        );
        // Overwrite
        r.insert("t", Key::String("a".into()), val(11)).unwrap();
        assert_eq!(
            as_i64(r.lookup("t", &Key::String("a".into())).unwrap()),
            Some(11)
        );
        // Delete
        assert!(r.delete("t", &Key::String("a".into())).unwrap());
        assert!(!r.delete("t", &Key::String("a".into())).unwrap());
        assert_eq!(r.size("t").unwrap(), 1);
    }

    #[test]
    fn ordered_set_iterates_in_key_order() {
        let r = TableRegistry::new();
        r.create("t", TableType::OrderedSet).unwrap();
        // Insert in random order; expect iteration to sort.
        for &n in &[5i64, 2, 8, 1, 7, 3] {
            r.insert("t", Key::Fixnum(n), val(n * 10)).unwrap();
        }
        let collected: Vec<i64> = r
            .fold("t", Vec::new(), |mut acc, k, _v| {
                if let Key::Fixnum(n) = k {
                    acc.push(*n);
                }
                acc
            })
            .unwrap();
        assert_eq!(collected, vec![1, 2, 3, 5, 7, 8]);
    }

    #[test]
    fn create_twice_errors() {
        let r = TableRegistry::new();
        r.create("t", TableType::Set).unwrap();
        let err = r.create("t", TableType::Set).unwrap_err();
        assert!(matches!(err, TableError::AlreadyExists { .. }));
    }

    #[test]
    fn missing_table_errors() {
        let r = TableRegistry::new();
        let err = r.lookup("nonexistent", &Key::Fixnum(0)).unwrap_err();
        assert!(matches!(err, TableError::NotFound { .. }));
    }

    #[test]
    fn drop_then_recreate() {
        let r = TableRegistry::new();
        r.create("t", TableType::Set).unwrap();
        r.insert("t", Key::Fixnum(1), val(1)).unwrap();
        r.drop_table("t");
        // After drop, creating again with a different type works.
        r.create("t", TableType::OrderedSet).unwrap();
        assert_eq!(r.table_type("t").unwrap(), TableType::OrderedSet);
    }

    #[test]
    fn concurrent_set_inserts_dont_lose_writes() {
        // Concurrent insert correctness — DashMap should serialize
        // per-bucket so two threads inserting different keys both
        // commit, two threads inserting the same key both commit
        // (last writer wins, but we don't care which).
        use std::thread;
        let r = TableRegistry::new();
        r.create("t", TableType::Set).unwrap();
        let nthreads = 8;
        let per_thread = 1000;
        let mut handles = Vec::new();
        for t in 0..nthreads {
            let r2 = r.clone();
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    let k = Key::Fixnum((t * per_thread + i) as i64);
                    r2.insert("t", k, val(t as i64)).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(r.size("t").unwrap(), nthreads * per_thread);
    }

    #[test]
    fn names_lists_registered_tables() {
        let r = TableRegistry::new();
        r.create("a", TableType::Set).unwrap();
        r.create("b", TableType::OrderedSet).unwrap();
        let mut n = r.names();
        n.sort();
        assert_eq!(n, vec!["a", "b"]);
    }

    #[test]
    fn clear_empties_without_dropping() {
        let r = TableRegistry::new();
        r.create("t", TableType::Set).unwrap();
        r.insert("t", Key::Fixnum(1), val(10)).unwrap();
        r.insert("t", Key::Fixnum(2), val(20)).unwrap();
        assert_eq!(r.size("t").unwrap(), 2);
        r.clear("t").unwrap();
        assert_eq!(r.size("t").unwrap(), 0);
        // Table itself still exists.
        assert!(r.lookup("t", &Key::Fixnum(1)).is_ok());
    }
}
