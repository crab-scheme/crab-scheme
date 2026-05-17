//! `cs-table` — in-memory tables, ETS-shaped.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`:
//! - v1 supports `set` (hash-backed) and `ordered_set` (btree-backed).
//! - All tables are public (no protected / private ACLs in v1).
//! - Read returns a deep clone of the stored Value (BEAM semantics —
//!   prevents accidental cross-actor mutation).
//!
//! ## Status
//!
//! Phase **B4** scaffold only.

#![allow(dead_code)]

use thiserror::Error;

/// Which kind of table.
///
/// `Set`: hash-keyed, O(1) lookup/insert/delete.
/// `OrderedSet`: btree-keyed, O(log n) ops + ordered iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableType {
    Set,
    OrderedSet,
}

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
