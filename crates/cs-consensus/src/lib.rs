//! `cs-consensus` — Raft-backed replicated actors + leases + fencing.
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § M06-M07; task lists at
//! `tasks/M06-consensus.md` and `tasks/M07-leases-and-fencing.md`.
//!
//! ## Status
//!
//! **Scaffold only.** Public type shapes for `RaftGroup`,
//! `ReplicatedActor`, `Lease`, `FencingToken`. M06 iter A wires
//! openraft + cs-net's `consensus` channel; M07 iter A-B add the
//! lease state machine + fencing.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use thiserror::Error;

pub mod group;
pub mod lease;

pub use group::{ConsistencyLevel, RaftGroup, RaftGroupConfig, ReplicaId};
pub use lease::{FencingToken, Lease, LeaseConfig};

/// Errors surfaced by the consensus layer.
#[derive(Debug, Error)]
pub enum ConsensusError {
    /// Submitted to a Raft group while no leader is known. Caller
    /// should retry; clients sit through one election (~150-300 ms).
    #[error("no leader for group {0}")]
    NoLeader(String),
    /// Caller's lease has expired or another holder has bumped the
    /// fencing token. Equivalent to Martin Kleppmann's "I/O-failure
    /// post-stop-the-world" scenario — see security.md.
    #[error("fenced: token {got} < highest seen {expected} for resource {resource}")]
    Fenced {
        resource: String,
        expected: u64,
        got: u64,
    },
    #[error("state machine is non-deterministic: {0}")]
    NonDeterministic(String),
    #[error("not implemented (cs-consensus scaffold; see docs/research/sdk_spec/tasks/M06-consensus.md)")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_error_carries_resource_and_tokens() {
        let e = ConsensusError::Fenced {
            resource: "email-sender".into(),
            expected: 43,
            got: 42,
        };
        let msg = format!("{}", e);
        assert!(msg.contains("email-sender"));
        assert!(msg.contains("42"));
        assert!(msg.contains("43"));
    }
}
