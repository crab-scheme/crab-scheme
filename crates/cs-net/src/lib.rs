//! `cs-net` — cluster transport substrate.
//!
//! Provides the per-peer transport connection (TCP+TLS / QUIC / WebSocket
//! / Unix / in-memory Sim) with logical-channel multiplexing so that the
//! six upper-layer traffic classes — `control`, `consensus`, `messages`,
//! `workflow`, `bulk`, `observability` — never starve each other.
//!
//! Spec: `docs/research/sdk_spec/distributed.md` § M02; task list at
//! `docs/research/sdk_spec/tasks/M02-cluster-substrate.md`.
//!
//! ## Status
//!
//! **Scaffold only.** Public types + trait shapes exist; concrete
//! implementations are deferred to follow-up iters as part of M02:
//!
//! - iter B: `Transport::Sim` (deterministic, no syscalls — for tests).
//! - iter B: `Transport::Tcp` (no TLS yet — bootstrap path).
//! - iter C: logical-channel framing + per-channel watermarks.
//! - iter D: rustls mTLS handshake.
//! - iter E: `RemoteActorRef` integration with cs-distrib.
//!
//! This scaffold exists so the workspace builds with the new crate
//! boundary locked in, mirroring the cs-actor / cs-table /
//! cs-supervisor scaffolds that preceded the BEAM v1 milestones.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::fmt;
use thiserror::Error;

/// A logical traffic class multiplexed over one transport connection.
///
/// Per-channel backpressure means a stalled `bulk` transfer cannot
/// choke `control` or `consensus` traffic. Priorities are advisory —
/// concrete transports implement them via separate streams (QUIC) or
/// priority queues (TCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Channel {
    /// Highest priority. Membership gossip, lease renewals, heartbeats.
    Control = 0,
    /// High priority. Raft RPCs from cs-consensus.
    Consensus = 1,
    /// Normal priority. Application actor sends (cs-distrib).
    Messages = 2,
    /// Normal priority. Workflow history fan-out (cs-workflow).
    Workflow = 3,
    /// Low (background). Code transfer, snapshot transfer.
    Bulk = 4,
    /// Low (background). Distributed traces, metrics.
    Observability = 5,
}

impl Channel {
    pub const ALL: [Channel; 6] = [
        Channel::Control,
        Channel::Consensus,
        Channel::Messages,
        Channel::Workflow,
        Channel::Bulk,
        Channel::Observability,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Channel::Control => "control",
            Channel::Consensus => "consensus",
            Channel::Messages => "messages",
            Channel::Workflow => "workflow",
            Channel::Bulk => "bulk",
            Channel::Observability => "observability",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Transport-layer errors. The crate-wide error type.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport closed by peer")]
    PeerClosed,
    #[error("backpressure: channel `{channel}` queue full at {depth} messages")]
    Backpressure { channel: Channel, depth: usize },
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("not implemented (cs-net scaffold; see docs/research/sdk_spec/tasks/M02-cluster-substrate.md)")]
    NotImplemented,
}

/// Per-peer transport handle. Implementations multiplex the six
/// `Channel`s over their underlying byte stream.
pub trait Transport: Send + Sync + std::fmt::Debug {
    /// Send a framed message on the given logical channel. Returns
    /// `Backpressure` if the per-channel queue is at its high
    /// watermark.
    fn send(&self, channel: Channel, payload: &[u8]) -> Result<(), TransportError>;

    /// Peer-side identity hint (`name@host`). Used for logging and
    /// for the membership-layer mapping from NodeId to transport.
    fn peer_label(&self) -> &str;

    /// Initiate a graceful close. Subsequent `send`s fail with
    /// `PeerClosed`.
    fn close(&self) -> Result<(), TransportError>;
}

/// Constructor knobs for opening a transport. Shared by all concrete
/// transports.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub control_high_watermark: usize,
    pub messages_high_watermark: usize,
    pub bulk_high_watermark: usize,
    pub max_frame_bytes: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            control_high_watermark: 1024,
            messages_high_watermark: 16 * 1024,
            bulk_high_watermark: 64,
            max_frame_bytes: 16 * 1024 * 1024,
        }
    }
}

#[cfg(feature = "sim")]
pub mod sim {
    //! In-memory simulation transport. Deterministic, no syscalls.
    //!
    //! Two peers share an `mpsc` pair per channel; sends are queued
    //! and delivered in order within a channel. Used by every cs-net
    //! consumer's unit-test suite and by the future cs-sim
    //! deterministic simulator.
    //!
    //! Not yet implemented — placeholder so the feature compiles.

    // M02 iter B will pull `Channel`, `Transport`, `TransportError`
    // from `super`. Scaffold needs none of them yet.

    /// A pair of sim-transport endpoints connected to each other.
    /// Implementation deferred to M02 iter B.
    #[derive(Debug)]
    pub struct SimPair {
        pub a_label: String,
        pub b_label: String,
    }

    impl SimPair {
        /// Construct a connected pair. The two endpoints implement
        /// `Transport` and route to each other.
        pub fn new(a_label: impl Into<String>, b_label: impl Into<String>) -> Self {
            SimPair {
                a_label: a_label.into(),
                b_label: b_label.into(),
            }
        }
    }
}

#[cfg(feature = "tcp")]
pub mod tcp {
    //! TCP + (optional) rustls TLS transport. Production default.
    //!
    //! mTLS is added in M02 iter D via the cs-distrib handshake.
    //!
    //! Not yet implemented — placeholder so the feature compiles.

    /// A stub TCP transport handle. Implementation deferred to M02 iter B.
    #[derive(Debug)]
    pub struct TcpTransport {
        pub peer: String,
    }

    impl TcpTransport {
        pub fn new(peer: impl Into<String>) -> Self {
            TcpTransport { peer: peer.into() }
        }
    }
}

#[cfg(feature = "quic")]
pub mod quic {
    //! QUIC transport via `quinn`. Multi-stream, no head-of-line
    //! blocking, mTLS via TLS 1.3 baked in. Default-off until the
    //! transport-selection plumbing is wired into cluster bootstrap.
    //!
    //! Not yet implemented — placeholder so the feature compiles.

    #[derive(Debug)]
    pub struct QuicTransport {
        pub peer: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_are_stable() {
        // Renaming a channel name breaks the wire framing —
        // discourage accidental edits.
        assert_eq!(Channel::Control.name(), "control");
        assert_eq!(Channel::Consensus.name(), "consensus");
        assert_eq!(Channel::Messages.name(), "messages");
        assert_eq!(Channel::Workflow.name(), "workflow");
        assert_eq!(Channel::Bulk.name(), "bulk");
        assert_eq!(Channel::Observability.name(), "observability");
    }

    #[test]
    fn channel_all_covers_every_variant() {
        // If a Channel variant is added without updating ALL, the
        // multiplexer's iteration is incomplete.
        assert_eq!(Channel::ALL.len(), 6);
    }

    #[test]
    fn default_config_watermarks_ordered() {
        // Control's watermark is intentionally smallest (heartbeats
        // are tiny); bulk's watermark is intentionally smallest
        // *message count* but each bulk message can be the max frame
        // size, so it's still memory-bounded.
        let c = TransportConfig::default();
        assert!(c.control_high_watermark > 0);
        assert!(c.messages_high_watermark >= c.control_high_watermark);
        assert!(c.max_frame_bytes >= 1024);
    }

    #[cfg(feature = "sim")]
    #[test]
    fn sim_pair_labels_round_trip() {
        let pair = sim::SimPair::new("node-a", "node-b");
        assert_eq!(pair.a_label, "node-a");
        assert_eq!(pair.b_label, "node-b");
    }
}
