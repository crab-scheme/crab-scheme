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
//! ## Status (M02)
//!
//! - [`Channel`] + [`Transport`] trait + [`TransportConfig`] — the layer's
//!   shape.
//! - [`sim`] — deterministic in-memory transport (no syscalls / runtime),
//!   the test substrate (M02.B).
//! - [`framing`] — length-prefixed channel framing so a byte stream carries
//!   all six channels (M02.C).
//! - [`tcp`] — real tokio TCP transport bridging the sync trait over async
//!   I/O (M02.B), with **mTLS** via `connect_tls` / `accept_tls` ([`tls`],
//!   M02.D): all `Channel` traffic is encrypted + both peers mutually
//!   authenticated. The cs-distrib handshake protocol runs over it.
//! - [`tls`] — rustls mTLS config builders (M02.D).
//! - [`quic`] — placeholder (default-off; future).

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::fmt;
use thiserror::Error;

pub mod framing;
#[cfg(feature = "tcp")]
pub mod tls;

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

    /// Whether the connection has been closed (locally or by the peer).
    /// The router uses this to fire DOWN for monitored remote Pids on a
    /// dropped connection without consuming inbound frames.
    fn is_closed(&self) -> bool;

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

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
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
    //! TCP transport. Production cross-process default.
    //!
    //! Bridges the synchronous [`Transport`] trait over an async tokio
    //! `TcpStream`: a writer task drains an outbound queue, frames each
    //! message ([`crate::framing`]) and writes it; a reader task reassembles
    //! frames and pushes them onto per-channel inbound queues that
    //! `try_recv` drains. So `send` / `try_recv` stay non-blocking and
    //! runtime-agnostic for callers (the router), while the socket I/O runs
    //! on the tokio runtime the cluster already uses.
    //!
    //! mTLS (rustls) wraps the `TcpStream` before [`TcpTransport::from_stream`]
    //! — the [`crate::framing`] + cs-distrib handshake then run unchanged
    //! over the encrypted stream. That cert wiring is the remaining iter-D
    //! socket task; the protocol it carries is implemented + tested in
    //! cs-distrib::handshake.

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::mpsc;

    use super::framing::{encode_frame, FrameDecoder};
    use super::{Channel, Transport, TransportConfig, TransportError};

    type Inbound = Arc<[Mutex<VecDeque<Vec<u8>>>; Channel::ALL.len()]>;

    /// A per-peer TCP transport. Construct with [`Self::connect`] (client)
    /// or [`Self::from_stream`] (an accepted stream); both spawn the I/O
    /// pump on the current tokio runtime.
    pub struct TcpTransport {
        peer_label: String,
        outbound: mpsc::UnboundedSender<(Channel, Vec<u8>)>,
        inbound: Inbound,
        closed: Arc<AtomicBool>,
        /// Abort handles for the reader + writer tasks, fired on `Drop` so
        /// the socket closes deterministically.
        tasks: [tokio::task::AbortHandle; 2],
    }

    impl std::fmt::Debug for TcpTransport {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TcpTransport")
                .field("peer", &self.peer_label)
                .field("closed", &self.is_closed())
                .finish()
        }
    }

    impl TcpTransport {
        /// Open a client connection to `addr` (a `host:port`), labelling the
        /// peer `peer_label` (`name@host`).
        pub async fn connect(
            addr: &str,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> std::io::Result<Self> {
            let stream = TcpStream::connect(addr).await?;
            let _ = stream.set_nodelay(true);
            Ok(Self::from_stream(stream, peer_label, cfg))
        }

        /// Wrap an already-connected stream and spawn the reader + writer
        /// tasks. Generic over the stream type so a plain `TcpStream` (from
        /// `TcpListener::accept`) or a `tokio_rustls::TlsStream` (mTLS — see
        /// [`super::tls`]) both work; the framing + handshake run unchanged
        /// over either.
        pub fn from_stream<S>(
            stream: S,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> Self
        where
            S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        {
            let peer_label = peer_label.into();
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<(Channel, Vec<u8>)>();
            let inbound: Inbound = Arc::new(std::array::from_fn(|_| Mutex::new(VecDeque::new())));
            let closed = Arc::new(AtomicBool::new(false));
            let (mut rd, mut wr) = tokio::io::split(stream);

            // Writer: drain outbound, frame, write.
            let closed_w = closed.clone();
            let writer = tokio::spawn(async move {
                while let Some((ch, payload)) = out_rx.recv().await {
                    let mut frame = Vec::with_capacity(payload.len() + 8);
                    encode_frame(ch, &payload, &mut frame);
                    if wr.write_all(&frame).await.is_err() {
                        break;
                    }
                }
                closed_w.store(true, Ordering::Release);
            });

            // Reader: read, reassemble frames, fan out per channel.
            let inbound_r = inbound.clone();
            let closed_r = closed.clone();
            let max_frame = cfg.max_frame_bytes;
            let reader = tokio::spawn(async move {
                let mut decoder = FrameDecoder::new(max_frame);
                let mut buf = vec![0u8; 64 * 1024];
                'read: loop {
                    match rd.read(&mut buf).await {
                        Ok(0) | Err(_) => break, // EOF or socket error
                        Ok(n) => {
                            decoder.push(&buf[..n]);
                            loop {
                                match decoder.next_frame() {
                                    Ok(Some((ch, payload))) => inbound_r[ch as usize]
                                        .lock()
                                        .expect("tcp inbound poisoned")
                                        .push_back(payload),
                                    Ok(None) => break,
                                    Err(_) => break 'read, // malformed framing → drop
                                }
                            }
                        }
                    }
                }
                closed_r.store(true, Ordering::Release);
            });

            TcpTransport {
                peer_label,
                outbound: out_tx,
                inbound,
                closed,
                tasks: [writer.abort_handle(), reader.abort_handle()],
            }
        }

        /// Connect to `addr` and perform a client-side **mTLS** handshake
        /// (presenting our identity, verifying the server against
        /// `client_config`'s roots), then wrap the encrypted stream.
        /// `server_name` must match the server certificate's SAN.
        /// Build `client_config` with [`crate::tls::client_config`].
        pub async fn connect_tls(
            addr: &str,
            server_name: &str,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
            client_config: std::sync::Arc<rustls::ClientConfig>,
        ) -> Result<Self, TransportError> {
            let tcp = TcpStream::connect(addr).await?;
            let _ = tcp.set_nodelay(true);
            let domain = rustls::pki_types::ServerName::try_from(server_name.to_string())
                .map_err(|e| TransportError::Tls(format!("invalid server name: {e}")))?;
            let tls = tokio_rustls::TlsConnector::from(client_config)
                .connect(domain, tcp)
                .await
                .map_err(|e| TransportError::Tls(format!("client handshake: {e}")))?;
            Ok(Self::from_stream(tls, peer_label, cfg))
        }

        /// Accept an mTLS handshake on an already-accepted `tcp` stream
        /// (server side — requires + verifies the client certificate per
        /// `server_config`), then wrap the encrypted stream. Build
        /// `server_config` with [`crate::tls::server_config`].
        pub async fn accept_tls(
            tcp: TcpStream,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
            server_config: std::sync::Arc<rustls::ServerConfig>,
        ) -> Result<Self, TransportError> {
            let _ = tcp.set_nodelay(true);
            let tls = tokio_rustls::TlsAcceptor::from(server_config)
                .accept(tcp)
                .await
                .map_err(|e| TransportError::Tls(format!("server handshake: {e}")))?;
            Ok(Self::from_stream(tls, peer_label, cfg))
        }
    }

    impl Drop for TcpTransport {
        fn drop(&mut self) {
            // Mark closed + abort the I/O tasks so both stream halves drop
            // and the socket actually closes (tokio::io::split keeps the
            // stream alive until both halves are gone — the peer must see
            // EOF when this transport is dropped).
            self.closed.store(true, Ordering::Release);
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    impl Transport for TcpTransport {
        fn send(&self, channel: Channel, payload: &[u8]) -> Result<(), TransportError> {
            if self.is_closed() {
                return Err(TransportError::PeerClosed);
            }
            self.outbound
                .send((channel, payload.to_vec()))
                .map_err(|_| TransportError::PeerClosed)
        }

        fn try_recv(&self, channel: Channel) -> Result<Option<Vec<u8>>, TransportError> {
            if let Some(frame) = self.inbound[channel as usize]
                .lock()
                .expect("tcp inbound poisoned")
                .pop_front()
            {
                return Ok(Some(frame));
            }
            if self.is_closed() {
                Err(TransportError::PeerClosed)
            } else {
                Ok(None)
            }
        }

        fn peer_label(&self) -> &str {
            &self.peer_label
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }

        fn close(&self) -> Result<(), TransportError> {
            self.closed.store(true, Ordering::Release);
            Ok(())
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

#[cfg(all(test, feature = "tcp"))]
mod tcp_transport_tests {
    use super::tcp::TcpTransport;
    use super::{Channel, Transport, TransportConfig};
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// `try_recv` is non-blocking but TCP delivery is driven by the reader
    /// task — poll with a short backoff until the frame arrives.
    async fn recv(t: &TcpTransport, ch: Channel) -> Vec<u8> {
        for _ in 0..200 {
            if let Some(f) = t.try_recv(ch).unwrap() {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("frame not received within timeout");
    }

    async fn connected_pair() -> (TcpTransport, TcpTransport) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            TcpTransport::from_stream(stream, "client@host", &TransportConfig::default())
        });
        let client = TcpTransport::connect(
            &addr.to_string(),
            "server@host",
            &TransportConfig::default(),
        )
        .await
        .unwrap();
        let server = accept.await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn loopback_round_trip_bidirectional_and_ordered() {
        let (client, server) = connected_pair().await;
        // client → server.
        client.send(Channel::Messages, b"ping").unwrap();
        assert_eq!(recv(&server, Channel::Messages).await, b"ping");
        // server → client.
        server.send(Channel::Control, b"pong").unwrap();
        assert_eq!(recv(&client, Channel::Control).await, b"pong");
        // FIFO within a channel, and channel isolation (Bulk vs Messages).
        for i in 0u8..8 {
            client.send(Channel::Bulk, &[i]).unwrap();
        }
        client.send(Channel::Messages, b"after-bulk").unwrap();
        for i in 0u8..8 {
            assert_eq!(recv(&server, Channel::Bulk).await, vec![i]);
        }
        assert_eq!(recv(&server, Channel::Messages).await, b"after-bulk");
        assert_eq!(server.peer_label(), "client@host");
        assert_eq!(client.peer_label(), "server@host");
    }

    #[tokio::test]
    async fn dropping_peer_is_observed_as_closed() {
        let (client, server) = connected_pair().await;
        // Drop the client → its writer task ends, write half closes, the
        // server's reader hits EOF and marks the connection closed.
        drop(client);
        for _ in 0..200 {
            if server.is_closed() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(server.is_closed(), "server must observe the client drop");
        assert!(matches!(
            server.send(Channel::Messages, b"x"),
            Err(super::TransportError::PeerClosed)
        ));
    }
}

#[cfg(all(test, feature = "tcp"))]
mod mtls_tests {
    use super::tcp::TcpTransport;
    use super::{tls, Channel, Transport, TransportConfig};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::RootCertStore;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// One self-signed identity for `localhost`, used (in the tests) as both
    /// the node identity *and* the trusted root on both ends.
    fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert = CertificateDer::from(ck.cert.der().to_vec());
        let key = PrivateKeyDer::try_from(ck.key_pair.serialize_der()).unwrap();
        (cert, key)
    }

    fn roots_with(cert: &CertificateDer<'static>) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        roots
    }

    async fn recv(t: &TcpTransport, ch: Channel) -> Vec<u8> {
        for _ in 0..200 {
            if let Some(f) = t.try_recv(ch).unwrap() {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("frame not received");
    }

    #[tokio::test]
    async fn mtls_loopback_round_trip_encrypted() {
        tls::install_crypto_provider();
        let (cert, key) = identity();
        let roots = roots_with(&cert);
        let server_cfg = Arc::new(
            tls::server_config(roots.clone(), vec![cert.clone()], key.clone_key()).unwrap(),
        );
        let client_cfg = Arc::new(tls::client_config(roots, vec![cert.clone()], key).unwrap());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TcpTransport::accept_tls(tcp, "client@host", &TransportConfig::default(), server_cfg)
                .await
                .unwrap()
        });
        let client = TcpTransport::connect_tls(
            &addr.to_string(),
            "localhost",
            "server@host",
            &TransportConfig::default(),
            client_cfg,
        )
        .await
        .unwrap();
        let server = accept.await.unwrap();

        // Encrypted application traffic round-trips both ways.
        client.send(Channel::Messages, b"secret-ping").unwrap();
        assert_eq!(recv(&server, Channel::Messages).await, b"secret-ping");
        server.send(Channel::Control, b"secret-pong").unwrap();
        assert_eq!(recv(&client, Channel::Control).await, b"secret-pong");
    }

    #[tokio::test]
    async fn mtls_rejects_client_without_certificate() {
        tls::install_crypto_provider();
        let (cert, key) = identity();
        let roots = roots_with(&cert);
        let server_cfg =
            Arc::new(tls::server_config(roots.clone(), vec![cert.clone()], key).unwrap());
        // Client that presents NO certificate — mutual auth must reject it.
        let client_cfg = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TcpTransport::accept_tls(tcp, "client@host", &TransportConfig::default(), server_cfg)
                .await
        });
        let client_res = TcpTransport::connect_tls(
            &addr.to_string(),
            "localhost",
            "server@host",
            &TransportConfig::default(),
            client_cfg,
        )
        .await;

        // The server is the authoritative side: mutual auth must reject the
        // certless client. In TLS 1.3 the client may consider its own
        // handshake done before the server validates the client cert, so
        // `client_res` can be Ok — but the session is refused server-side
        // and is unusable.
        let _ = client_res;
        assert!(
            accept.await.unwrap().is_err(),
            "server must reject the certless client"
        );
    }
}
