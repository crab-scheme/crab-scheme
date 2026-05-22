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

pub mod framing;

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
    #[error("framing: {0}")]
    Framing(String),
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

    /// Non-blocking inbound poll: return the next frame received on
    /// `channel`, `Ok(None)` if none is queued, or `Err(PeerClosed)`
    /// once the connection is closed and the channel is drained. Lets a
    /// router poll any transport uniformly without an async runtime.
    fn try_recv(&self, channel: Channel) -> Result<Option<Vec<u8>>, TransportError>;

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
    //! In-memory simulation transport. Deterministic, no syscalls, no
    //! async runtime.
    //!
    //! A [`SimPair`] connects two [`SimEndpoint`]s. Each endpoint owns one
    //! inbound queue per [`Channel`] and writes into the *peer's* inbound
    //! queues; delivery is in-order within a channel (FIFO) and isolated
    //! across channels (a stalled `Bulk` transfer can't block `Control`).
    //! A shared closed-flag models a connection drop in both directions.
    //!
    //! Used by every cs-net / cs-distrib consumer's unit-test suite (and
    //! the future cs-sim deterministic simulator): construct a pair, drive
    //! `send` / `try_recv` synchronously, assert on what arrives.

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{Channel, Transport, TransportConfig, TransportError};

    /// One channel's FIFO queue plus its high-watermark for backpressure.
    #[derive(Debug)]
    struct SimChannelQueue {
        deque: Mutex<VecDeque<Vec<u8>>>,
        high_watermark: usize,
    }

    /// The six per-channel inbound queues for one direction.
    #[derive(Debug)]
    struct SimQueues {
        channels: [SimChannelQueue; Channel::ALL.len()],
    }

    impl SimQueues {
        fn new(cfg: &TransportConfig) -> Arc<Self> {
            let hwm = |c: Channel| match c {
                Channel::Control | Channel::Consensus => cfg.control_high_watermark,
                Channel::Messages | Channel::Workflow => cfg.messages_high_watermark,
                Channel::Bulk | Channel::Observability => cfg.bulk_high_watermark,
            };
            Arc::new(SimQueues {
                channels: Channel::ALL.map(|c| SimChannelQueue {
                    deque: Mutex::new(VecDeque::new()),
                    high_watermark: hwm(c),
                }),
            })
        }
    }

    /// One end of a connected [`SimPair`]. Implements [`Transport`]: `send`
    /// enqueues into the peer's inbound queue; `try_recv` drains its own.
    #[derive(Debug)]
    pub struct SimEndpoint {
        label: String,
        peer_label: String,
        /// Peer's inbound queues — where this endpoint's `send` writes.
        outbound: Arc<SimQueues>,
        /// This endpoint's inbound queues — where `try_recv` reads.
        inbound: Arc<SimQueues>,
        /// Shared connection-closed flag (drops both directions at once).
        closed: Arc<AtomicBool>,
    }

    impl SimEndpoint {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }
    }

    impl Transport for SimEndpoint {
        fn send(&self, channel: Channel, payload: &[u8]) -> Result<(), TransportError> {
            if self.is_closed() {
                return Err(TransportError::PeerClosed);
            }
            let q = &self.outbound.channels[channel as usize];
            let mut deque = q.deque.lock().expect("sim queue poisoned");
            if deque.len() >= q.high_watermark {
                return Err(TransportError::Backpressure {
                    channel,
                    depth: deque.len(),
                });
            }
            deque.push_back(payload.to_vec());
            Ok(())
        }

        fn try_recv(&self, channel: Channel) -> Result<Option<Vec<u8>>, TransportError> {
            let q = &self.inbound.channels[channel as usize];
            let mut deque = q.deque.lock().expect("sim queue poisoned");
            match deque.pop_front() {
                Some(frame) => Ok(Some(frame)),
                // Closed *and* drained → signal end-of-stream.
                None if self.is_closed() => Err(TransportError::PeerClosed),
                None => Ok(None),
            }
        }

        fn peer_label(&self) -> &str {
            &self.peer_label
        }

        fn close(&self) -> Result<(), TransportError> {
            self.closed.store(true, Ordering::Release);
            Ok(())
        }
    }

    /// A connected pair of in-memory endpoints. `into_endpoints` yields the
    /// two [`SimEndpoint`]s; whatever `a` sends, `b` receives, and vice
    /// versa.
    #[derive(Debug)]
    pub struct SimPair {
        a: SimEndpoint,
        b: SimEndpoint,
    }

    impl SimPair {
        /// Construct a connected pair with the default [`TransportConfig`].
        pub fn new(a_label: impl Into<String>, b_label: impl Into<String>) -> Self {
            Self::with_config(a_label, b_label, &TransportConfig::default())
        }

        /// Construct a connected pair with explicit watermarks.
        pub fn with_config(
            a_label: impl Into<String>,
            b_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> Self {
            let a_label = a_label.into();
            let b_label = b_label.into();
            let qa = SimQueues::new(cfg); // a's inbound
            let qb = SimQueues::new(cfg); // b's inbound
            let closed = Arc::new(AtomicBool::new(false));
            let a = SimEndpoint {
                label: a_label.clone(),
                peer_label: b_label.clone(),
                outbound: qb.clone(),
                inbound: qa.clone(),
                closed: closed.clone(),
            };
            let b = SimEndpoint {
                label: b_label,
                peer_label: a_label,
                outbound: qa,
                inbound: qb,
                closed,
            };
            SimPair { a, b }
        }

        /// This endpoint's own label.
        pub fn a_label(&self) -> &str {
            &self.a.label
        }
        pub fn b_label(&self) -> &str {
            &self.b.label
        }

        /// Consume the pair into its two endpoints `(a, b)`.
        pub fn into_endpoints(self) -> (SimEndpoint, SimEndpoint) {
            (self.a, self.b)
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
    mod sim_transport {
        use super::super::sim::SimPair;
        use super::super::{Channel, Transport, TransportConfig, TransportError};

        #[test]
        fn labels_round_trip() {
            let pair = SimPair::new("node-a", "node-b");
            assert_eq!(pair.a_label(), "node-a");
            assert_eq!(pair.b_label(), "node-b");
            let (a, b) = pair.into_endpoints();
            assert_eq!(a.peer_label(), "node-b");
            assert_eq!(b.peer_label(), "node-a");
        }

        #[test]
        fn send_is_received_by_peer() {
            let (a, b) = SimPair::new("a", "b").into_endpoints();
            a.send(Channel::Messages, b"hello").unwrap();
            assert_eq!(
                b.try_recv(Channel::Messages).unwrap(),
                Some(b"hello".to_vec())
            );
            // Drained.
            assert_eq!(b.try_recv(Channel::Messages).unwrap(), None);
        }

        #[test]
        fn delivery_is_bidirectional() {
            let (a, b) = SimPair::new("a", "b").into_endpoints();
            a.send(Channel::Messages, b"a->b").unwrap();
            b.send(Channel::Messages, b"b->a").unwrap();
            assert_eq!(
                b.try_recv(Channel::Messages).unwrap(),
                Some(b"a->b".to_vec())
            );
            assert_eq!(
                a.try_recv(Channel::Messages).unwrap(),
                Some(b"b->a".to_vec())
            );
        }

        #[test]
        fn order_is_fifo_within_a_channel() {
            let (a, b) = SimPair::new("a", "b").into_endpoints();
            for i in 0u8..5 {
                a.send(Channel::Messages, &[i]).unwrap();
            }
            for i in 0u8..5 {
                assert_eq!(b.try_recv(Channel::Messages).unwrap(), Some(vec![i]));
            }
        }

        #[test]
        fn channels_are_isolated() {
            let (a, b) = SimPair::new("a", "b").into_endpoints();
            a.send(Channel::Messages, b"msg").unwrap();
            // Nothing on Control — a stalled/empty channel doesn't leak.
            assert_eq!(b.try_recv(Channel::Control).unwrap(), None);
            assert_eq!(
                b.try_recv(Channel::Messages).unwrap(),
                Some(b"msg".to_vec())
            );
        }

        #[test]
        fn backpressure_at_high_watermark() {
            let cfg = TransportConfig {
                control_high_watermark: 2,
                ..TransportConfig::default()
            };
            let (a, _b) = SimPair::with_config("a", "b", &cfg).into_endpoints();
            a.send(Channel::Control, b"1").unwrap();
            a.send(Channel::Control, b"2").unwrap();
            // Third send hits the watermark.
            match a.send(Channel::Control, b"3") {
                Err(TransportError::Backpressure { channel, depth }) => {
                    assert_eq!(channel, Channel::Control);
                    assert_eq!(depth, 2);
                }
                other => panic!("expected Backpressure, got {other:?}"),
            }
            // A higher-watermark channel still accepts traffic — no
            // cross-channel starvation.
            a.send(Channel::Messages, b"ok").unwrap();
        }

        #[test]
        fn close_fails_sends_and_drains_then_ends() {
            let (a, b) = SimPair::new("a", "b").into_endpoints();
            a.send(Channel::Messages, b"queued").unwrap();
            a.close().unwrap();
            // Subsequent sends fail (both directions — shared flag).
            assert!(matches!(
                a.send(Channel::Messages, b"x"),
                Err(TransportError::PeerClosed)
            ));
            assert!(matches!(
                b.send(Channel::Messages, b"y"),
                Err(TransportError::PeerClosed)
            ));
            // Already-queued frames still drain, then end-of-stream.
            assert_eq!(
                b.try_recv(Channel::Messages).unwrap(),
                Some(b"queued".to_vec())
            );
            assert!(matches!(
                b.try_recv(Channel::Messages),
                Err(TransportError::PeerClosed)
            ));
        }
    }
}
