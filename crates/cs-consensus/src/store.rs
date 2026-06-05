//! Pluggable persistence for a [`RaftNode`](crate::raft::RaftNode)'s log,
//! hard-state, and latest snapshot.
//!
//! Raft's correctness rests on a small amount of state surviving a crash: the
//! log of replicated entries, the durable "hard state" (current term, the vote
//! cast this term, and the commit index), and the most recent snapshot a
//! follower can be caught up from. The deterministic core ([`crate::raft`])
//! reaches all of this through the [`RaftLogStore`] trait, so the same protocol
//! code runs over an in-memory [`MemLogStore`] (Sim / tests — today's exact
//! behavior) or a crash-durable RocksDB store (the `rocksdb-log` feature).
//!
//! ## Ordering contract (persist-before-ack)
//!
//! The core calls the mutating methods (`append`, `truncate_suffix`,
//! `save_hard_state`, `save_snapshot`) *before* it returns the outbound
//! messages that acknowledge a write — `propose` returns only after the new
//! entry is appended; `handle_append_entries` splices entries and persists
//! hard-state before building its `AppendEntriesResp`. A synchronous,
//! synced-write store (`RocksLogStore`) therefore guarantees the bytes are on
//! disk before the driver dispatches the ack. `MemLogStore` satisfies the same
//! interface without durability, for deterministic testing.

use crate::raft::{Entry, Index, Term};
use crate::ReplicaId;

/// Durable hard-state (Raft §5.1): the fields that **must** survive a crash for
/// safety. `commit_index` is included so a recovered node never un-commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HardState {
    pub term: Term,
    pub voted_for: Option<ReplicaId>,
    pub commit_index: Index,
}

/// The snapshot metadata + blob folded into a compaction (the live log holds
/// only entries after `last_included_index`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub last_included_index: Index,
    pub last_included_term: Term,
    pub data: Vec<u8>,
}

/// Persistence seam for the Raft log + hard-state + snapshot.
///
/// Indices are 1-based (matching the core); `0` means "before the first
/// entry". Entries earlier than the snapshot base are compacted away — the
/// store holds only the live suffix `(base_index, last_index]` plus the
/// snapshot blob covering everything up to and including `base_index`.
///
/// Mutating methods must be applied synchronously: a durable implementation
/// fsyncs before returning so the core's persist-before-ack discipline holds.
pub trait RaftLogStore {
    /// Append entries to the end of the live log. They must be contiguous with
    /// the current last index (the core only ever appends at the tail). A
    /// durable store fsyncs before returning.
    fn append(&mut self, entries: &[Entry]);

    /// Live entries with index in `[lo, hi]` (inclusive, 1-based), skipping any
    /// that are at or below the snapshot base.
    fn entries(&self, lo: Index, hi: Index) -> Vec<Entry>;

    /// Term of the live entry at `idx`, or `None` if `idx` is outside the live
    /// log (beyond the tail, or already compacted — the snapshot base term is
    /// the core's concern, not the store's).
    fn entry_term(&self, idx: Index) -> Option<Term>;

    /// The single live entry at `idx`, or `None` if it isn't in the live log.
    fn entry(&self, idx: Index) -> Option<Entry>;

    /// Index of the last live entry, or `base_index` if the live log is empty.
    fn last_index(&self) -> Index;

    /// Term of the last live entry, or `base_term` if the live log is empty.
    fn last_term(&self) -> Term;

    /// Drop every live entry with index `>= from_idx` (conflict resolution).
    fn truncate_suffix(&mut self, from_idx: Index);

    /// Persist the hard-state (term / vote / commit). Durable store fsyncs.
    fn save_hard_state(&mut self, hs: HardState);

    /// Load the persisted hard-state (default if none was ever saved).
    fn load_hard_state(&self) -> HardState;

    /// Persist the latest snapshot (metadata + blob) and drop every live entry
    /// at or before `meta.last_included_index`. Durable store fsyncs.
    fn save_snapshot(&mut self, meta: SnapshotMeta);

    /// Load the latest persisted snapshot, if any.
    fn load_snapshot(&self) -> Option<SnapshotMeta>;

    /// The snapshot base index (`last_included_index`), or `0` if none.
    fn base_index(&self) -> Index;

    /// The snapshot base term, or `0` if none.
    fn base_term(&self) -> Term;

    /// The current snapshot blob (the bytes shipped in `InstallSnapshot`),
    /// empty if there is no snapshot.
    fn snapshot_data(&self) -> Vec<u8>;
}

/// In-memory [`RaftLogStore`]: a `Vec<Entry>` plus snapshot/hard-state cells.
///
/// Reproduces exactly the behavior the core had before the store seam existed
/// (a bare `Vec<Entry>` with `base_index`/`base_term`/`snapshot_data` fields).
/// This is the default backing store for the Sim harness and every unit test.
#[derive(Debug, Default)]
pub struct MemLogStore {
    /// Live entries after `base_index`, in index order.
    log: Vec<Entry>,
    base_index: Index,
    base_term: Term,
    snapshot_data: Vec<u8>,
    hard_state: HardState,
    /// Whether a snapshot has ever been saved (so `load_snapshot` can return
    /// `None` for a fresh node, matching a durable store with no `rsnap` key).
    has_snapshot: bool,
}

impl MemLogStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offset of `idx` within `log`, if it lies in the live suffix.
    fn slot(&self, idx: Index) -> Option<usize> {
        if idx <= self.base_index {
            return None;
        }
        let off = (idx - self.base_index - 1) as usize;
        (off < self.log.len()).then_some(off)
    }
}

impl RaftLogStore for MemLogStore {
    fn append(&mut self, entries: &[Entry]) {
        self.log.extend_from_slice(entries);
    }

    fn entries(&self, lo: Index, hi: Index) -> Vec<Entry> {
        self.log
            .iter()
            .filter(|e| e.index >= lo && e.index <= hi && e.index > self.base_index)
            .cloned()
            .collect()
    }

    fn entry_term(&self, idx: Index) -> Option<Term> {
        self.slot(idx).map(|off| self.log[off].term)
    }

    fn entry(&self, idx: Index) -> Option<Entry> {
        self.slot(idx).map(|off| self.log[off].clone())
    }

    fn last_index(&self) -> Index {
        self.log.last().map(|e| e.index).unwrap_or(self.base_index)
    }

    fn last_term(&self) -> Term {
        self.log.last().map(|e| e.term).unwrap_or(self.base_term)
    }

    fn truncate_suffix(&mut self, from_idx: Index) {
        if from_idx <= self.base_index {
            self.log.clear();
            return;
        }
        let keep = (from_idx - self.base_index - 1) as usize;
        if keep < self.log.len() {
            self.log.truncate(keep);
        }
    }

    fn save_hard_state(&mut self, hs: HardState) {
        self.hard_state = hs;
    }

    fn load_hard_state(&self) -> HardState {
        self.hard_state
    }

    fn save_snapshot(&mut self, meta: SnapshotMeta) {
        self.log.retain(|e| e.index > meta.last_included_index);
        self.base_index = meta.last_included_index;
        self.base_term = meta.last_included_term;
        self.snapshot_data = meta.data;
        self.has_snapshot = true;
    }

    fn load_snapshot(&self) -> Option<SnapshotMeta> {
        self.has_snapshot.then(|| SnapshotMeta {
            last_included_index: self.base_index,
            last_included_term: self.base_term,
            data: self.snapshot_data.clone(),
        })
    }

    fn base_index(&self) -> Index {
        self.base_index
    }

    fn base_term(&self) -> Term {
        self.base_term
    }

    fn snapshot_data(&self) -> Vec<u8> {
        self.snapshot_data.clone()
    }
}

#[cfg(feature = "rocksdb-log")]
mod rocks;
#[cfg(feature = "rocksdb-log")]
pub use rocks::RocksLogStore;
