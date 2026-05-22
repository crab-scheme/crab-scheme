//! `cs-consensus` — replication / orchestration engines for the cluster.
//!
//! Two homegrown consensus engines built on the M02 substrate (cs-net's
//! `Channel::Consensus` + cs-distrib routing + cs-actor):
//!
//! - [`raft`] — leader-based, linearizable, CP-under-partition. Leader
//!   election, log replication, commit/apply, linearizable reads (ReadIndex),
//!   log compaction (snapshots), and joint-consensus membership change.
//! - `epaxos` — leaderless, dependency-graph execution (next milestone).
//!
//! ## Design: deterministic core + thin I/O shim
//!
//! Each engine's protocol logic is a **pure, synchronous state machine** —
//! no clocks, no sockets, no tasks. A node consumes inputs (a logical timer
//! tick, a received message, a client proposal) and returns outputs
//! (messages to send, plus internal state changes observable via accessors).
//! This is the only sane way to get consensus right: the whole protocol is
//! exercised by the deterministic [`sim`] cluster harness — message
//! delivery, drops, and partitions — with zero wall-clock flakiness. The
//! networking/actor driver (cs-net adapter) is a thin pump around the core.
//!
//! Spec: `docs/research/sdk_spec/tasks/M06-consensus.md`,
//! `docs/research/sdk_spec/consistency.md`.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod codec;
pub mod driver;
pub mod epaxos;
pub mod raft;
pub mod sim;

pub use driver::{spawn_raft_actor, RaftCommand, RaftDriver};

/// Stable identity of one replica within a consensus group.
///
/// A small `Copy` integer (not cs-distrib's `NodeId`) so the deterministic
/// cores stay cheap and ordering-friendly; the cs-net adapter maps each
/// `ReplicaId` to a cluster `NodeId` + transport.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplicaId(pub u64);

impl std::fmt::Debug for ReplicaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "R{}", self.0)
    }
}

impl std::fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "R{}", self.0)
    }
}

/// A replicated state machine: applies committed commands in log order.
///
/// Commands and results are opaque bytes (the Scheme layer encodes
/// `(state, op) → (state, result)` on top). Implementations **must** be
/// deterministic — same command sequence ⇒ same state on every replica.
/// `snapshot`/`restore` back log compaction: a snapshot captures the state
/// after applying every command up to some index.
pub trait StateMachine {
    /// Apply one committed command, returning an opaque result for the caller.
    fn apply(&mut self, command: &[u8]) -> Vec<u8>;

    /// Answer a read-only query against the current applied state, without
    /// mutating it. Backs linearizable ReadIndex reads.
    fn query(&self, query: &[u8]) -> Vec<u8>;

    /// Serialize the full applied state (for a log-compaction snapshot).
    fn snapshot(&self) -> Vec<u8>;

    /// Replace the state with a snapshot produced by [`Self::snapshot`].
    fn restore(&mut self, snapshot: &[u8]);
}
