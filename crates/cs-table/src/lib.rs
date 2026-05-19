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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

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

// ---------- Mailbox (cs-actor's per-actor inbox, backed by
//            an OrderedSet table) ----------

/// A FIFO mailbox built on top of cs-table's OrderedSet
/// storage. Used by cs-actor as the per-actor inbox so all
/// mailboxes live in the same table fabric as ETS-style
/// shared state — uniform inspect / persistence story.
///
/// Mechanics:
///
/// - Each mailbox owns an `OrderedSet` table named like
///   `__mailbox:<some-id>`. The Mailbox struct owns the
///   name; the registry holds the storage.
/// - Enqueue allocates a fresh monotonic sequence number
///   (`next_seq: AtomicU64`) and inserts at
///   `Key::Fixnum(seq) → payload`.
/// - Dequeue uses `pop_first` which atomically takes the
///   smallest-key entry. Ordering = insert order because
///   sequences are monotonic.
/// - Blocking dequeue waits on a per-mailbox `Condvar`
///   signaled by every `push`. Wait_timeout returns
///   `Ok(None)` on timeout.
/// - Drop removes the underlying table from the registry,
///   freeing all queued payloads.
///
/// **Why not just put a Notify inside the table directly?**
/// The notify state is mailbox-specific (a generic ETS
/// table doesn't want recv-blocking semantics). Wrapping
/// here keeps the registry's general-purpose ops free of
/// mailbox-specific bookkeeping.
pub struct Mailbox {
    table_name: String,
    registry: TableRegistry,
    next_seq: AtomicU64,
    /// Signaled by `push`. Wakers re-check `size()` after
    /// the wake and re-loop if the queue is still empty
    /// (handles spurious wakeups).
    notify: Arc<(Mutex<()>, Condvar)>,
    /// Tracks whether this mailbox is still attached to a
    /// live receiver. Once `close` runs, future `push`
    /// returns `Err(MailboxClosed)`. Multi-sender semantics
    /// match tokio's UnboundedSender: any sender can push
    /// until the receiver side closes.
    open: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Error)]
pub enum MailboxError {
    #[error("mailbox {name:?} is closed (receiver dropped)")]
    Closed { name: String },
    #[error("underlying table error: {0}")]
    Table(#[from] TableError),
}

impl Mailbox {
    /// Create a new mailbox under `name` (must be unique
    /// across the registry). The caller is responsible for
    /// uniqueness — typically the actor PID makes a good
    /// namespace, e.g. `format!("__mailbox:{node}.{local}")`.
    pub fn create(registry: TableRegistry, name: String) -> Result<Self, MailboxError> {
        registry.create(&name, TableType::OrderedSet)?;
        Ok(Self {
            table_name: name,
            registry,
            next_seq: AtomicU64::new(0),
            notify: Arc::new((Mutex::new(()), Condvar::new())),
            open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        })
    }

    /// Push a payload at the tail of the FIFO. Returns
    /// `Err(Closed)` if `close` has been called. Wakes one
    /// blocked receiver via the Condvar.
    pub fn push(&self, payload: Payload) -> Result<(), MailboxError> {
        if !self.open.load(Ordering::Acquire) {
            return Err(MailboxError::Closed {
                name: self.table_name.clone(),
            });
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.registry
            .insert(&self.table_name, Key::Fixnum(seq as i64), payload)?;
        // Wake any blocked receiver. The lock acquire makes
        // the notify visible to a parked receiver that's
        // mid-`wait_timeout`.
        let (lock, cv) = &*self.notify;
        let _g = lock.lock().expect("notify lock poisoned");
        cv.notify_one();
        Ok(())
    }

    /// Try to dequeue immediately. Returns `Ok(None)` if the
    /// queue is empty.
    pub fn try_pop(&self) -> Result<Option<Payload>, MailboxError> {
        let popped = self.registry.pop_first_ordered(&self.table_name)?;
        Ok(popped.map(|(_k, v)| v))
    }

    /// Blocking dequeue with optional timeout. `None` timeout
    /// = block forever. Returns `Ok(None)` on timeout or
    /// once the mailbox is closed AND empty.
    pub fn pop_or_wait(&self, timeout: Option<Duration>) -> Result<Option<Payload>, MailboxError> {
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            if let Some(payload) = self.try_pop()? {
                return Ok(Some(payload));
            }
            if !self.open.load(Ordering::Acquire) {
                // Closed + empty → return None (channel
                // semantics: mirror tokio mpsc's recv on
                // dropped sender).
                return Ok(None);
            }
            let (lock, cv) = &*self.notify;
            let guard = lock.lock().expect("notify lock poisoned");
            let remaining = match deadline {
                None => Duration::from_millis(250),
                Some(d) => match d.checked_duration_since(std::time::Instant::now()) {
                    Some(rem) => rem.min(Duration::from_millis(250)),
                    None => return Ok(None), // deadline passed
                },
            };
            // Re-check inside the lock to avoid the
            // missed-notify race. wait_timeout is fine as
            // a safety-net heartbeat.
            if self.registry.size(&self.table_name)? > 0 {
                drop(guard);
                continue;
            }
            if !self.open.load(Ordering::Acquire) {
                return Ok(None);
            }
            let (_g, _t) = cv
                .wait_timeout(guard, remaining)
                .expect("notify wait poisoned");
        }
    }

    /// Mark the mailbox closed. Future `push` returns
    /// `Closed`. Any parked receivers will wake (via timeout
    /// or notify_all) and return `None`. Idempotent.
    pub fn close(&self) {
        self.open.store(false, Ordering::Release);
        let (lock, cv) = &*self.notify;
        let _g = lock.lock().expect("notify lock poisoned");
        cv.notify_all();
    }

    /// Current queue depth (approximate; readers can race
    /// the next dequeue). Useful for backpressure decisions.
    pub fn len(&self) -> usize {
        self.registry.size(&self.table_name).unwrap_or(0)
    }

    /// `true` if `len() == 0`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` once `close` has been called. Senders that
    /// race with close may still see `open` true and succeed;
    /// the resulting payload sits in the table until a
    /// drainer picks it up (or the Mailbox is dropped).
    pub fn is_closed(&self) -> bool {
        !self.open.load(Ordering::Acquire)
    }

    /// The underlying table name. Lets debug tools / Scheme
    /// introspection query the queue contents directly.
    pub fn table_name(&self) -> &str {
        &self.table_name
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        // Mark closed first so any concurrent push errors
        // out cleanly instead of inserting into a
        // soon-dropped table.
        self.close();
        self.registry.drop_table(&self.table_name);
    }
}

impl TableRegistry {
    /// Atomic OrderedSet "take the smallest key" — used by
    /// `Mailbox::try_pop`. Returns `Ok(None)` on empty,
    /// `Ok(Some((key, value)))` on success. Errors only if
    /// the table doesn't exist or is the wrong type.
    pub fn pop_first_ordered(&self, name: &str) -> Result<Option<(Key, Payload)>, TableError> {
        let storage = self.lookup_storage(name)?;
        match &*storage {
            TableStorage::OrderedSet(rw) => {
                let mut guard = rw.write().expect("ordered_set poisoned");
                if let Some((k, _)) = guard.iter().next() {
                    let k = k.clone();
                    let v = guard.remove(&k).expect("just observed");
                    Ok(Some((k, v)))
                } else {
                    Ok(None)
                }
            }
            TableStorage::Set(_) => Err(TableError::WrongType {
                name: name.to_string(),
                actual: TableType::Set,
                expected: TableType::OrderedSet,
            }),
        }
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

    // ---- Mailbox (cs-actor inbox backing) ----

    #[test]
    fn pop_first_ordered_takes_smallest() {
        let r = TableRegistry::new();
        r.create("os", TableType::OrderedSet).unwrap();
        r.insert("os", Key::Fixnum(5), val(50)).unwrap();
        r.insert("os", Key::Fixnum(1), val(10)).unwrap();
        r.insert("os", Key::Fixnum(3), val(30)).unwrap();

        let (k1, v1) = r.pop_first_ordered("os").unwrap().unwrap();
        assert_eq!(k1, Key::Fixnum(1));
        assert_eq!(as_i64(Some(v1)), Some(10));

        let (k2, _) = r.pop_first_ordered("os").unwrap().unwrap();
        assert_eq!(k2, Key::Fixnum(3));

        let (k3, _) = r.pop_first_ordered("os").unwrap().unwrap();
        assert_eq!(k3, Key::Fixnum(5));

        assert!(r.pop_first_ordered("os").unwrap().is_none());
    }

    #[test]
    fn pop_first_ordered_rejects_set_table() {
        let r = TableRegistry::new();
        r.create("s", TableType::Set).unwrap();
        let err = r.pop_first_ordered("s").unwrap_err();
        assert!(matches!(err, TableError::WrongType { .. }));
    }

    #[test]
    fn mailbox_push_pop_fifo() {
        let r = TableRegistry::new();
        let mb = Mailbox::create(r, "__mb:fifo".to_string()).unwrap();
        mb.push(val(1)).unwrap();
        mb.push(val(2)).unwrap();
        mb.push(val(3)).unwrap();
        assert_eq!(mb.len(), 3);
        let a = mb.try_pop().unwrap();
        let b = mb.try_pop().unwrap();
        let c = mb.try_pop().unwrap();
        assert_eq!(as_i64(a), Some(1));
        assert_eq!(as_i64(b), Some(2));
        assert_eq!(as_i64(c), Some(3));
        assert!(mb.is_empty());
        assert!(mb.try_pop().unwrap().is_none());
    }

    #[test]
    fn mailbox_pop_or_wait_blocks_then_receives() {
        let r = TableRegistry::new();
        let mb = Arc::new(Mailbox::create(r, "__mb:wait".to_string()).unwrap());
        let mb_clone = Arc::clone(&mb);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            mb_clone.push(val(42)).unwrap();
        });
        let v = mb.pop_or_wait(Some(Duration::from_secs(2))).unwrap();
        t.join().unwrap();
        assert_eq!(as_i64(v), Some(42));
    }

    #[test]
    fn mailbox_pop_or_wait_returns_none_on_timeout() {
        let r = TableRegistry::new();
        let mb = Mailbox::create(r, "__mb:timeout".to_string()).unwrap();
        let v = mb.pop_or_wait(Some(Duration::from_millis(30))).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn mailbox_close_then_pop_returns_none() {
        let r = TableRegistry::new();
        let mb = Mailbox::create(r, "__mb:close".to_string()).unwrap();
        mb.push(val(7)).unwrap();
        mb.close();
        // Drain ok.
        let v = mb.pop_or_wait(Some(Duration::from_secs(1))).unwrap();
        assert_eq!(as_i64(v), Some(7));
        // Now empty + closed → None immediately.
        let v = mb.pop_or_wait(None).unwrap();
        assert!(v.is_none());
        // Subsequent push errors.
        let err = mb.push(val(1)).unwrap_err();
        assert!(matches!(err, MailboxError::Closed { .. }));
    }

    #[test]
    fn mailbox_drop_removes_table() {
        let r = TableRegistry::new();
        {
            let mb = Mailbox::create(r.clone(), "__mb:dropper".to_string()).unwrap();
            mb.push(val(1)).unwrap();
            assert_eq!(r.size("__mb:dropper").unwrap(), 1);
        }
        // After mb's Drop runs, the table is gone.
        assert!(matches!(
            r.size("__mb:dropper"),
            Err(TableError::NotFound { .. })
        ));
    }
}
