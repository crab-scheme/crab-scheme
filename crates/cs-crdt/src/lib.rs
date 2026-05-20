//! `cs-crdt` — Conflict-Free Replicated Data Types for CrabScheme.
//!
//! Six v1 types — G-Counter, PN-Counter, OR-Set, OR-Map (causal map),
//! LWW-Register, MV-Register — with Hybrid Logical Clock timestamps and
//! Dotted Version Vector causal contexts. State-based with delta-state
//! extension; anti-entropy via gossip + Merkle reconciliation on the
//! cs-net `messages` channel.
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § M05, task list
//! `tasks/M05-crdt.md`.
//!
//! ## Status
//!
//! **Scaffold only.** Public type shapes, trait surface, HLC/DVV
//! primitives. Concrete merge logic, delta computation, and Merkle
//! anti-entropy are filled in by M05 iters A-E.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use thiserror::Error;

pub mod clock;
pub mod counter;
pub mod map;
pub mod register;
pub mod set;
pub mod sync;

pub use clock::{Dvv, Hlc, NodeId};
pub use counter::{GCounter, PNCounter};
pub use map::CausalMap;
pub use register::{LwwRegister, MvRegister};
pub use set::OrSet;

#[derive(Debug, Error)]
pub enum CrdtError {
    #[error("merge of incompatible CRDT shapes: {0}")]
    IncompatibleShape(String),
    #[error("clock skew exceeds bound: {0}")]
    ClockSkew(String),
    #[error("not implemented (cs-crdt scaffold; see docs/research/sdk_spec/tasks/M05-crdt.md)")]
    NotImplemented,
}

/// The universal CRDT trait — state-based merge over a join semilattice.
/// Every concrete type (counters, sets, maps, registers) implements it.
pub trait Crdt: Send + Clone + std::fmt::Debug {
    /// The Value the application sees when it reads. For counters this is
    /// the integer total; for sets the materialized set; etc.
    type Value;

    /// Merge another replica's state into this one. Must be associative,
    /// commutative, idempotent — the lattice join.
    fn merge(&mut self, other: &Self);

    /// Materialize the current state as the application-facing value.
    fn value(&self) -> Self::Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_exports_compile() {
        // Smoke: ensure the public re-exports actually exist and can
        // be referenced. A future iter that renames or removes a type
        // breaks this test, surfacing it loudly in the diff.
        let _ = GCounter::default;
        let _ = PNCounter::default;
        let _ = OrSet::<u32>::default;
        let _ = CausalMap::<u32, u32>::default;
        let _ = LwwRegister::<u32>::default;
        let _ = MvRegister::<u32>::default;
    }
}
