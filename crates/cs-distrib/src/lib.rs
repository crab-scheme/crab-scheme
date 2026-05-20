//! `cs-distrib` — CrabScheme distributed runtime.
//!
//! Adds remote actors, gossip-based membership, phi-accrual failure
//! detection, and the SBR partition policy on top of `cs-actor`'s
//! single-node BEAM-style runtime.
//!
//! Spec: `docs/research/sdk_spec/distributed.md` (M02 transport, M04
//! membership). Task lists: `tasks/M02-cluster-substrate.md`,
//! `tasks/M04-membership.md`.
//!
//! ## Status
//!
//! **Scaffold only.** Public types + module shapes exist so future
//! milestone PRs can land focused implementation work without
//! re-litigating crate boundaries.
//!
//! - M02 iter A: `NodeId` + extended `Pid` encoding (here).
//! - M02 iter B-D: transport + handshake + multiplexing (cs-net + this).
//! - M02 iter E-F: `RemoteActorRef` impl + `(spawn-remote …)`.
//! - M04 iter A: `MemberState` machine (`membership` module here).
//! - M04 iter B: phi-accrual failure detector (`phi` module here).
//! - M04 iter C: SWIM-style gossip (`gossip` module here).
//! - M04 iter D: SBR strategies + `cluster-events` channel.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use thiserror::Error;

pub mod gossip;
pub mod membership;
pub mod phi;

pub use membership::{MemberState, PartitionPolicy};
pub use phi::PhiAccrualFailureDetector;

/// Cluster-wide identity for a CrabScheme node.
///
/// Triple of `(name, host, epoch)`. The epoch is a monotonic counter
/// bumped on every node restart — it distinguishes a freshly-restarted
/// node from its previous incarnation so stale Pids carrying the old
/// epoch are recognized and rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub name: String,
    pub host: String,
    pub epoch: u64,
}

impl NodeId {
    pub fn new(name: impl Into<String>, host: impl Into<String>, epoch: u64) -> Self {
        NodeId {
            name: name.into(),
            host: host.into(),
            epoch,
        }
    }

    /// Human-friendly form: `"name@host"` (epoch is internal).
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.host)
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}#{}", self.name, self.host, self.epoch)
    }
}

/// Errors surfaced by the distributed runtime.
#[derive(Debug, Error)]
pub enum DistribError {
    #[error("no transport for peer {0}")]
    NoTransport(String),
    #[error("peer epoch mismatch (expected #{expected}, got #{got}); peer was restarted")]
    EpochMismatch { expected: u64, got: u64 },
    #[error("peer unreachable: {0}")]
    Unreachable(String),
    #[error("transport: {0}")]
    Transport(#[from] cs_net::TransportError),
    #[error("not implemented (cs-distrib scaffold; see docs/research/sdk_spec/tasks/M02-cluster-substrate.md)")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_label_strips_epoch() {
        let nid = NodeId::new("worker", "10.0.0.4:7001", 42);
        assert_eq!(nid.label(), "worker@10.0.0.4:7001");
    }

    #[test]
    fn node_id_display_includes_epoch() {
        let nid = NodeId::new("worker", "10.0.0.4:7001", 42);
        assert_eq!(format!("{}", nid), "worker@10.0.0.4:7001#42");
    }

    #[test]
    fn node_id_eq_includes_epoch() {
        let a = NodeId::new("worker", "host", 1);
        let b = NodeId::new("worker", "host", 2);
        // Different epochs — same node identity but different
        // incarnations. The runtime treats them as different so
        // stale Pids are caught.
        assert_ne!(a, b);
    }
}
