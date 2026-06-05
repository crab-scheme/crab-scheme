//! RocksDB-backed [`RaftLogStore`] (`rocksdb-log` feature).
//!
//! Persists the Raft log, hard-state, and latest snapshot so an acknowledged
//! write survives `kill -9` + restart (crab-cache FR-5). All mutating writes
//! are **synced** (`WriteOptions::set_sync(true)`) before returning, which is
//! what makes the core's persist-before-ack discipline durable.
//!
//! ## Layout (column families)
//! | CF | Key | Value |
//! |----|-----|-------|
//! | `rlog`  | `index: u64` big-endian | encoded [`Entry`] (the wire codec) |
//! | `rhard` | `"hardstate"` | `term:u64 ‖ vote_tag:u8 ‖ vote:u64 ‖ commit:u64` |
//! | `rsnap` | `"snapshot"` | `idx:u64 ‖ term:u64 ‖ len:u64 ‖ data` |
//!
//! A small in-memory cache (`base`, `last`, `has_snapshot`) mirrors the
//! persisted tail/snapshot bounds so the hot accessors (`last_index`,
//! `base_index`, `entry_term`) don't scan RocksDB; it is rebuilt on `open`.
//!
//! The DB is opened with `DB::open` (default CF) and the three named CFs are
//! created via `create_cf`, mirroring `cs-store`'s workaround for a RocksDB
//! double-free when `open_cf` hands Rust-owned CF handles back at close.

use std::path::Path;

use rocksdb::{Options, WriteBatch, WriteOptions, DB};

use crate::codec::{decode_entry, encode_entry};
use crate::raft::{Entry, Index, Term};
use crate::store::{HardState, RaftLogStore, SnapshotMeta};
use crate::ReplicaId;

const CF_LOG: &str = "rlog";
const CF_HARD: &str = "rhard";
const CF_SNAP: &str = "rsnap";
const HARD_KEY: &[u8] = b"hardstate";
const SNAP_KEY: &[u8] = b"snapshot";

/// A RocksDB-backed durable Raft log/hard-state/snapshot store.
pub struct RocksLogStore {
    db: DB,
    // ---- cached bounds (rebuilt on open; kept in sync on every mutation) ----
    /// Snapshot base index/term (`(0, 0)` if no snapshot).
    base_index: Index,
    base_term: Term,
    has_snapshot: bool,
    /// Last live entry index/term (`base_*` if the live log is empty).
    last_index: Index,
    last_term: Term,
}

impl std::fmt::Debug for RocksLogStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RocksLogStore")
            .field("path", &self.db.path())
            .field("base_index", &self.base_index)
            .field("last_index", &self.last_index)
            .field("has_snapshot", &self.has_snapshot)
            .finish()
    }
}

fn synced() -> WriteOptions {
    let mut o = WriteOptions::default();
    o.set_sync(true);
    o
}

fn be(idx: Index) -> [u8; 8] {
    idx.to_be_bytes()
}

impl RocksLogStore {
    /// Open (creating if missing) a durable store at `path`, recovering any
    /// previously-persisted log / hard-state / snapshot.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Open all the named CFs up front. On reopen RocksDB refuses a plain
        // `DB::open` once non-default CFs exist on disk ("Column families not
        // opened"), so we must list them. We open the union of our required CFs
        // and any already on disk (the latter is empty on first creation, when
        // `list_cf` errors because there is no DB yet).
        let existing = DB::list_cf(&opts, &path).unwrap_or_default();
        let mut cfs: Vec<String> = vec![
            "default".to_string(),
            CF_LOG.to_string(),
            CF_HARD.to_string(),
            CF_SNAP.to_string(),
        ];
        for cf in existing {
            if !cfs.contains(&cf) {
                cfs.push(cf);
            }
        }
        // rocksdb 0.21 wraps RocksDB 8.1.1 (not the 10.4.x that bit cs-store
        // with an open_cf double-free), so opening with CF handles is safe here.
        let db = DB::open_cf(&opts, &path, &cfs)?;
        let mut store = RocksLogStore {
            db,
            base_index: 0,
            base_term: 0,
            has_snapshot: false,
            last_index: 0,
            last_term: 0,
        };
        store.recover_bounds();
        Ok(store)
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(name)
            .expect("column family created at open")
    }

    /// Rebuild the cached snapshot base + log tail from what's on disk.
    fn recover_bounds(&mut self) {
        // Snapshot bounds first (base_* default to it when the log is empty).
        if let Some(meta) = self.load_snapshot() {
            self.base_index = meta.last_included_index;
            self.base_term = meta.last_included_term;
            self.has_snapshot = true;
        }
        self.last_index = self.base_index;
        self.last_term = self.base_term;
        // Last live entry: the highest key in CF_LOG.
        let mut it = self.db.raw_iterator_cf(self.cf(CF_LOG));
        it.seek_to_last();
        if it.valid() {
            if let (Some(_), Some(v)) = (it.key(), it.value()) {
                if let Ok(e) = decode_entry(v) {
                    self.last_index = e.index;
                    self.last_term = e.term;
                }
            }
        }
    }

    /// Re-scan the log tail after a truncation (the cheapest correct way to
    /// restore `last_index`/`last_term` once the highest key may be gone).
    fn refresh_tail(&mut self) {
        self.last_index = self.base_index;
        self.last_term = self.base_term;
        let mut it = self.db.raw_iterator_cf(self.cf(CF_LOG));
        it.seek_to_last();
        if it.valid() {
            if let Some(v) = it.value() {
                if let Ok(e) = decode_entry(v) {
                    self.last_index = e.index;
                    self.last_term = e.term;
                }
            }
        }
    }
}

impl RaftLogStore for RocksLogStore {
    fn append(&mut self, entries: &[Entry]) {
        if entries.is_empty() {
            return;
        }
        let mut batch = WriteBatch::default();
        let cf = self.cf(CF_LOG);
        for e in entries {
            batch.put_cf(cf, be(e.index), encode_entry(e));
        }
        self.db
            .write_opt(batch, &synced())
            .expect("rocks append (synced)");
        if let Some(last) = entries.last() {
            self.last_index = last.index;
            self.last_term = last.term;
        }
    }

    fn entries(&self, lo: Index, hi: Index) -> Vec<Entry> {
        if lo > hi {
            return Vec::new();
        }
        let lo = lo.max(self.base_index + 1);
        let mut out = Vec::new();
        let mut it = self.db.raw_iterator_cf(self.cf(CF_LOG));
        it.seek(be(lo));
        while it.valid() {
            let key = match it.key() {
                Some(k) if k.len() == 8 => Index::from_be_bytes(k.try_into().unwrap()),
                _ => break,
            };
            if key > hi {
                break;
            }
            if let Some(v) = it.value() {
                if let Ok(e) = decode_entry(v) {
                    out.push(e);
                }
            }
            it.next();
        }
        out
    }

    fn entry_term(&self, idx: Index) -> Option<Term> {
        self.entry(idx).map(|e| e.term)
    }

    fn entry(&self, idx: Index) -> Option<Entry> {
        if idx <= self.base_index || idx > self.last_index {
            return None;
        }
        self.db
            .get_cf(self.cf(CF_LOG), be(idx))
            .ok()
            .flatten()
            .and_then(|v| decode_entry(&v).ok())
    }

    fn last_index(&self) -> Index {
        self.last_index
    }

    fn last_term(&self) -> Term {
        self.last_term
    }

    fn truncate_suffix(&mut self, from_idx: Index) {
        let from = from_idx.max(self.base_index + 1);
        if from > self.last_index {
            return;
        }
        let mut batch = WriteBatch::default();
        let cf = self.cf(CF_LOG);
        for idx in from..=self.last_index {
            batch.delete_cf(cf, be(idx));
        }
        self.db
            .write_opt(batch, &synced())
            .expect("rocks truncate (synced)");
        self.refresh_tail();
    }

    fn save_hard_state(&mut self, hs: HardState) {
        let mut buf = Vec::with_capacity(25);
        buf.extend_from_slice(&hs.term.to_be_bytes());
        match hs.voted_for {
            Some(v) => {
                buf.push(1);
                buf.extend_from_slice(&v.0.to_be_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u64.to_be_bytes());
            }
        }
        buf.extend_from_slice(&hs.commit_index.to_be_bytes());
        self.db
            .put_cf_opt(self.cf(CF_HARD), HARD_KEY, &buf, &synced())
            .expect("rocks save_hard_state (synced)");
    }

    fn load_hard_state(&self) -> HardState {
        let raw = match self.db.get_cf(self.cf(CF_HARD), HARD_KEY).ok().flatten() {
            Some(b) if b.len() == 25 => b,
            _ => return HardState::default(),
        };
        let term = u64::from_be_bytes(raw[0..8].try_into().unwrap());
        let voted_for = if raw[8] == 1 {
            Some(ReplicaId(u64::from_be_bytes(
                raw[9..17].try_into().unwrap(),
            )))
        } else {
            None
        };
        let commit_index = u64::from_be_bytes(raw[17..25].try_into().unwrap());
        HardState {
            term,
            voted_for,
            commit_index,
        }
    }

    fn save_snapshot(&mut self, meta: SnapshotMeta) {
        // Persist the snapshot record (synced).
        let mut buf = Vec::with_capacity(24 + meta.data.len());
        buf.extend_from_slice(&meta.last_included_index.to_be_bytes());
        buf.extend_from_slice(&meta.last_included_term.to_be_bytes());
        buf.extend_from_slice(&(meta.data.len() as u64).to_be_bytes());
        buf.extend_from_slice(&meta.data);
        self.db
            .put_cf_opt(self.cf(CF_SNAP), SNAP_KEY, &buf, &synced())
            .expect("rocks save_snapshot (synced)");

        // Trim every live entry at or below the new base.
        let cf = self.cf(CF_LOG);
        let mut batch = WriteBatch::default();
        let old_base = self.base_index;
        for idx in (old_base + 1)..=meta.last_included_index {
            batch.delete_cf(cf, be(idx));
        }
        self.db
            .write_opt(batch, &synced())
            .expect("rocks snapshot trim (synced)");

        self.base_index = meta.last_included_index;
        self.base_term = meta.last_included_term;
        self.has_snapshot = true;
        self.refresh_tail();
    }

    fn load_snapshot(&self) -> Option<SnapshotMeta> {
        let raw = self.db.get_cf(self.cf(CF_SNAP), SNAP_KEY).ok().flatten()?;
        if raw.len() < 24 {
            return None;
        }
        let last_included_index = u64::from_be_bytes(raw[0..8].try_into().unwrap());
        let last_included_term = u64::from_be_bytes(raw[8..16].try_into().unwrap());
        let len = u64::from_be_bytes(raw[16..24].try_into().unwrap()) as usize;
        let data = raw.get(24..24 + len).unwrap_or(&[]).to_vec();
        Some(SnapshotMeta {
            last_included_index,
            last_included_term,
            data,
        })
    }

    fn base_index(&self) -> Index {
        self.base_index
    }

    fn base_term(&self) -> Term {
        self.base_term
    }

    fn snapshot_data(&self) -> Vec<u8> {
        self.load_snapshot().map(|m| m.data).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::EntryPayload;
    use tempfile::TempDir;

    fn entry(term: Term, index: Index, n: u8) -> Entry {
        Entry {
            term,
            index,
            payload: EntryPayload::Command(vec![n]),
        }
    }

    #[test]
    fn append_entries_term_and_truncate_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut s = RocksLogStore::open(dir.path()).unwrap();
        s.append(&[entry(1, 1, 10), entry(1, 2, 20), entry(2, 3, 30)]);
        assert_eq!(s.last_index(), 3);
        assert_eq!(s.last_term(), 2);
        assert_eq!(s.entry_term(2), Some(1));
        assert_eq!(s.entry(3).unwrap().payload, EntryPayload::Command(vec![30]));
        assert_eq!(s.entries(1, 3).len(), 3);
        assert_eq!(s.entries(2, 2), vec![entry(1, 2, 20)]);

        // Truncate from index 2 → only entry 1 remains; tail recomputed.
        s.truncate_suffix(2);
        assert_eq!(s.last_index(), 1);
        assert_eq!(s.last_term(), 1);
        assert_eq!(s.entry(2), None);
        assert_eq!(s.entries(1, 9), vec![entry(1, 1, 10)]);
    }

    #[test]
    fn hard_state_persists_across_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let mut s = RocksLogStore::open(dir.path()).unwrap();
            s.save_hard_state(HardState {
                term: 7,
                voted_for: Some(ReplicaId(2)),
                commit_index: 5,
            });
            // A None vote must also round-trip distinctly.
            assert_eq!(s.load_hard_state().voted_for, Some(ReplicaId(2)));
        }
        // Reopen at the same path: hard-state intact.
        let s = RocksLogStore::open(dir.path()).unwrap();
        let hs = s.load_hard_state();
        assert_eq!(hs.term, 7);
        assert_eq!(hs.voted_for, Some(ReplicaId(2)));
        assert_eq!(hs.commit_index, 5);
    }

    #[test]
    fn crash_recovery_reopens_log_hard_state_and_snapshot() {
        // (a) crash-recovery, store level: write entries + hard-state +
        // snapshot, drop the store, reopen at the same path, assert intact.
        let dir = TempDir::new().unwrap();
        {
            let mut s = RocksLogStore::open(dir.path()).unwrap();
            s.append(&[entry(1, 1, 1), entry(1, 2, 2), entry(1, 3, 3)]);
            s.save_hard_state(HardState {
                term: 3,
                voted_for: Some(ReplicaId(1)),
                commit_index: 3,
            });
            // Compact entries 1..=2 into a snapshot; entry 3 stays live.
            s.save_snapshot(SnapshotMeta {
                last_included_index: 2,
                last_included_term: 1,
                data: vec![0xAB, 0xCD],
            });
            assert_eq!(s.base_index(), 2);
            assert_eq!(s.last_index(), 3);
            // dropped here → simulates process exit
        }

        let s = RocksLogStore::open(dir.path()).unwrap();
        // Snapshot + base recovered.
        assert_eq!(s.base_index(), 2);
        assert_eq!(s.base_term(), 1);
        assert_eq!(s.snapshot_data(), vec![0xAB, 0xCD]);
        let snap = s.load_snapshot().unwrap();
        assert_eq!(snap.last_included_index, 2);
        assert_eq!(snap.data, vec![0xAB, 0xCD]);
        // Live tail recovered (only entry 3 survived compaction).
        assert_eq!(s.last_index(), 3);
        assert_eq!(s.entry(3).unwrap().payload, EntryPayload::Command(vec![3]));
        assert_eq!(s.entry(1), None, "compacted entry is gone");
        assert_eq!(s.entry(2), None, "compacted entry is gone");
        // Hard-state recovered.
        let hs = s.load_hard_state();
        assert_eq!((hs.term, hs.commit_index), (3, 3));
        assert_eq!(hs.voted_for, Some(ReplicaId(1)));
    }

    #[test]
    fn fresh_store_has_no_snapshot() {
        let dir = TempDir::new().unwrap();
        let s = RocksLogStore::open(dir.path()).unwrap();
        assert_eq!(s.load_snapshot(), None);
        assert_eq!(s.load_hard_state(), HardState::default());
        assert_eq!(s.last_index(), 0);
        assert_eq!(s.base_index(), 0);
    }
}
