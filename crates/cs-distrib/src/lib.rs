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
//! **M02 substrate implemented + tested** (deterministic via the cs-net Sim
//! transport):
//!
//! - A — [`NodeId`] + [`DistPid`] cluster identity + wire codec ([`pid`]).
//! - C/E — [`Router`] (local vs remote dispatch) + [`RemoteRef`]; the
//!   3-node ping/pong acceptance is a [`router`] test.
//! - D — [`Hello`] / [`evaluate_hello`] handshake protocol ([`handshake`]):
//!   accept / quarantine on version, self-identity, or stale epoch.
//! - F — DOWN-on-disconnect for monitored remote Pids ([`DownNotice`]).
//!
//! Remaining M02 tail: mTLS cert provisioning (the protocol is done; rustls
//! wraps the cs-net TCP stream); `(spawn-remote …)` closure transfer (needs
//! M12's content-addressed codebase); and the cs-actor mailbox / Scheme
//! `(send pid msg)` binding (cs-runtime integration).
//!
//! M04 modules ([`membership`] / [`phi`] / [`gossip`]) remain scaffolds for
//! that milestone.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use thiserror::Error;

pub mod gossip;
pub mod handshake;
pub mod membership;
pub mod phi;
pub mod pid;
pub mod router;

pub use handshake::{evaluate_hello, HandshakeOutcome, Hello};
pub use membership::{MemberState, PartitionPolicy};
pub use phi::PhiAccrualFailureDetector;
pub use pid::DistPid;
pub use router::{DownNotice, DownReason, RemoteRef, Router};

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
    #[error("wire decode: {0}")]
    Decode(String),
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
