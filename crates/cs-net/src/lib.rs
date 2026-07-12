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
//! - [`tls`] — rustls mTLS config builders (M02.D), shared by TCP + QUIC.
//! - [`quic`] — quinn QUIC transport (default-off feature): TLS 1.3 mandatory
//!   (always encrypted + mutually authenticated) and **one QUIC stream per
//!   channel**, so channels never head-of-line-block each other.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::fmt;
use thiserror::Error;

pub mod framing;
/// Dev-mode QUIC listen/connect helpers (shared self-signed mTLS identity).
/// Wraps the quinn `Endpoint` so consumers (cs-runtime's `distrib` builtins)
/// drive QUIC without naming quinn types.
#[cfg(all(feature = "quic", feature = "dev-certs"))]
pub mod quic_dev;
#[cfg(any(feature = "tcp", feature = "quic"))]
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

    /// Wire-scheduler priority; higher is flushed first. Maps the channel's
    /// traffic class onto a QUIC stream priority (`quinn::SendStream::set_priority`)
    /// and the TCP transport's drain order, so latency-sensitive control
    /// traffic is sent ahead of background bulk transfers. The ordering
    /// matches the `Channel::ALL` / enum order (Control highest).
    pub const fn priority(self) -> i32 {
        match self {
            Channel::Control => 10,
            Channel::Consensus => 8,
            Channel::Messages | Channel::Workflow => 0,
            Channel::Bulk | Channel::Observability => -10,
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
/// Callback fired by a transport whenever an inbound frame is queued (on any
/// channel). Lets a consumer (cs-distrib's Router) block on "any inbound
/// frame arrived" instead of sleep-polling `try_recv` — polling granularity
/// otherwise becomes the mesh hop latency (crab-watchstore cw-xq9).
pub type InboundWaker = std::sync::Arc<dyn Fn() + Send + Sync>;

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

    /// Register a callback fired whenever an inbound frame is queued (any
    /// channel). At most one waker per transport; a later call replaces the
    /// earlier one. Default: no-op (the transport doesn't support wakeups
    /// and consumers must poll).
    fn set_inbound_waker(&self, _waker: InboundWaker) {}
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
    struct SimQueues {
        channels: [SimChannelQueue; Channel::ALL.len()],
        /// Waker of the endpoint that READS these queues (set via its
        /// `set_inbound_waker`); fired by the peer's `send`.
        waker: Mutex<Option<super::InboundWaker>>,
    }

    impl std::fmt::Debug for SimQueues {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SimQueues")
                .field("channels", &self.channels)
                .finish_non_exhaustive()
        }
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
                waker: Mutex::new(None),
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
            drop(deque);
            if let Some(w) = self
                .outbound
                .waker
                .lock()
                .expect("sim waker poisoned")
                .as_ref()
            {
                w();
            }
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

        fn set_inbound_waker(&self, waker: super::InboundWaker) {
            *self.inbound.waker.lock().expect("sim waker poisoned") = Some(waker);
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
    use tokio::sync::Notify;

    use super::framing::{encode_frame, FrameDecoder};
    use super::{Channel, Transport, TransportConfig, TransportError};

    type Inbound = Arc<[Mutex<VecDeque<Vec<u8>>>; Channel::ALL.len()]>;
    /// Per-channel outbound queues, drained by the writer in channel-priority
    /// order so a flood of `Bulk` frames cannot delay a `Control` frame.
    type Outbound = Arc<[Mutex<VecDeque<Vec<u8>>>; Channel::ALL.len()]>;

    /// A per-peer TCP transport. Construct with [`Self::connect`] (client)
    /// or [`Self::from_stream`] (an accepted stream); both spawn the I/O
    /// pump on the current tokio runtime.
    pub struct TcpTransport {
        peer_label: String,
        outbound: Outbound,
        /// Wakes the writer task when a frame is enqueued (or on close).
        outbound_wake: Arc<Notify>,
        inbound: Inbound,
        /// Consumer waker fired by the reader task per queued inbound frame.
        inbound_waker: Arc<Mutex<Option<super::InboundWaker>>>,
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
            let outbound: Outbound = Arc::new(std::array::from_fn(|_| Mutex::new(VecDeque::new())));
            let outbound_wake = Arc::new(Notify::new());
            let inbound: Inbound = Arc::new(std::array::from_fn(|_| Mutex::new(VecDeque::new())));
            let closed = Arc::new(AtomicBool::new(false));
            let (mut rd, mut wr) = tokio::io::split(stream);

            // Writer: when woken, drain every queued frame in channel-priority
            // order (`Channel::ALL` is ordered Control..Observability), framing
            // and writing one at a time. Picking the highest-priority non-empty
            // channel each step lets a Control frame jump ahead of frames
            // already queued on Bulk instead of waiting behind them in a FIFO.
            // (One in-flight `write_all` is the irreducible single-stream HoL.)
            let out_w = outbound.clone();
            let wake_w = outbound_wake.clone();
            let closed_w = closed.clone();
            let writer = tokio::spawn(async move {
                let mut frame = Vec::new();
                loop {
                    loop {
                        let next = {
                            let mut picked = None;
                            for ch in Channel::ALL {
                                if let Some(f) = out_w[ch as usize]
                                    .lock()
                                    .expect("tcp outbound poisoned")
                                    .pop_front()
                                {
                                    picked = Some((ch, f));
                                    break;
                                }
                            }
                            picked
                        };
                        match next {
                            Some((ch, payload)) => {
                                frame.clear();
                                encode_frame(ch, &payload, &mut frame);
                                if wr.write_all(&frame).await.is_err() {
                                    closed_w.store(true, Ordering::Release);
                                    return;
                                }
                            }
                            None => break,
                        }
                    }
                    // Queue drained — flush so buffered bytes reach the peer.
                    // A plain TcpStream is unbuffered, but a tokio_rustls
                    // TlsStream buffers records; without a flush a small write
                    // (e.g. a ping-pong reply) can sit unsent until the next
                    // one, stalling request/reply traffic. Flushing only when
                    // the queue empties keeps batched throughput intact.
                    if wr.flush().await.is_err() {
                        closed_w.store(true, Ordering::Release);
                        return;
                    }
                    if closed_w.load(Ordering::Acquire) {
                        return;
                    }
                    wake_w.notified().await;
                }
            });

            // WAN-RTT simulation (cw-c1l/cw-wan): CW_NET_DELAY_MS holds each
            // INBOUND frame for that many ms before it becomes visible to
            // `try_recv`, modelling one-way link propagation (both peers delay
            // their receive side, so a request/reply round-trip pays ~2×). The
            // delay is added via `sleep_until(ready_at)` computed at enqueue, so
            // it shifts delivery in time WITHOUT throttling bandwidth and keeps
            // per-channel order (single FIFO drain, monotonic deadlines). 0 (the
            // default) takes the original zero-overhead direct-push path.
            let delay_ms: u64 = std::env::var("CW_NET_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let inbound_waker: Arc<Mutex<Option<super::InboundWaker>>> = Arc::new(Mutex::new(None));
            let wake_inbound = |slot: &Arc<Mutex<Option<super::InboundWaker>>>| {
                if let Some(w) = slot.lock().expect("tcp waker poisoned").as_ref() {
                    w();
                }
            };
            let delay_tx = if delay_ms > 0 {
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<(usize, Vec<u8>, tokio::time::Instant)>(
                    );
                let inbound_d = inbound.clone();
                let waker_d = inbound_waker.clone();
                tokio::spawn(async move {
                    while let Some((chi, payload, ready)) = rx.recv().await {
                        tokio::time::sleep_until(ready).await;
                        inbound_d[chi]
                            .lock()
                            .expect("tcp inbound poisoned")
                            .push_back(payload);
                        wake_inbound(&waker_d);
                    }
                });
                Some(tx)
            } else {
                None
            };

            // Reader: read, reassemble frames, fan out per channel.
            let inbound_r = inbound.clone();
            let waker_r = inbound_waker.clone();
            let closed_r = closed.clone();
            let max_frame = cfg.max_frame_bytes;
            let delay = std::time::Duration::from_millis(delay_ms);
            let reader = tokio::spawn(async move {
                let mut decoder = FrameDecoder::new(max_frame);
                let mut buf = vec![0u8; 64 * 1024];
                'read: loop {
                    match rd.read(&mut buf).await {
                        Ok(0) | Err(_) => break, // EOF or socket error
                        Ok(n) => {
                            decoder.push(&buf[..n]);
                            let mut queued = false;
                            loop {
                                match decoder.next_frame() {
                                    Ok(Some((ch, payload))) => match &delay_tx {
                                        Some(tx) => {
                                            let ready = tokio::time::Instant::now() + delay;
                                            // drain task gone => peer torn down; stop.
                                            if tx.send((ch as usize, payload, ready)).is_err() {
                                                break 'read;
                                            }
                                        }
                                        None => {
                                            inbound_r[ch as usize]
                                                .lock()
                                                .expect("tcp inbound poisoned")
                                                .push_back(payload);
                                            queued = true;
                                        }
                                    },
                                    Ok(None) => break,
                                    Err(_) => break 'read, // malformed framing → drop
                                }
                            }
                            // one wake per read() batch, not per frame
                            if queued {
                                wake_inbound(&waker_r);
                            }
                        }
                    }
                }
                closed_r.store(true, Ordering::Release);
            });

            TcpTransport {
                peer_label,
                outbound,
                outbound_wake,
                inbound,
                inbound_waker,
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
            self.outbound[channel as usize]
                .lock()
                .expect("tcp outbound poisoned")
                .push_back(payload.to_vec());
            self.outbound_wake.notify_one();
            Ok(())
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
            // Wake the writer so it flushes any queued frames, then exits.
            self.outbound_wake.notify_one();
            Ok(())
        }

        fn set_inbound_waker(&self, waker: super::InboundWaker) {
            *self.inbound_waker.lock().expect("tcp waker poisoned") = Some(waker);
        }
    }
}

#[cfg(feature = "quic")]
pub mod quic {
    //! QUIC transport via `quinn` (SDK M02, QUIC variant).
    //!
    //! mTLS (TLS 1.3) is mandatory in QUIC, so this transport is always
    //! encrypted + mutually authenticated. Crucially, each logical
    //! [`Channel`] gets its **own QUIC stream**, so a stalled `Bulk`
    //! transfer can't head-of-line-block `Control` — the multiplexing the
    //! single-stream TCP transport's framing can't provide.
    //!
    //! Like the TCP transport it bridges the sync [`Transport`] trait over
    //! async: `send` queues to a per-channel writer task; in the default
    //! per-channel mode each task owns its own `SendStream` (prioritized),
    //! while [`QuicTransport::from_connection_single_stream`] shares one
    //! stream across all channels (the no-isolation baseline). Frames are
    //! self-describing — `[channel][len][payload]` — so an acceptor task can
    //! demux any stream onto the per-channel inbound queues `try_recv` drains.
    //!
    //! [`lossy_relay`] injects datagram loss for tests/benchmarks, so the
    //! per-stream design's head-of-line-blocking advantage under loss can be
    //! measured (see `examples/actor_bench.rs`).

    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use quinn::{Connection, Endpoint};
    use tokio::sync::mpsc;
    use tokio::sync::Mutex as AsyncMutex;

    use super::{Channel, Transport, TransportConfig, TransportError};

    type Inbound = Arc<[Mutex<VecDeque<Vec<u8>>>; Channel::ALL.len()]>;

    /// A per-peer QUIC transport.
    pub struct QuicTransport {
        peer_label: String,
        /// One writer mpsc per channel; each feeds a dedicated writer task that
        /// owns that channel's QUIC stream, so a `Bulk` write blocked on flow
        /// control can't stall `Control` behind it in a shared queue.
        outbound: [mpsc::UnboundedSender<Vec<u8>>; Channel::ALL.len()],
        inbound: Inbound,
        /// Consumer waker fired by the stream readers per queued inbound frame.
        inbound_waker: Arc<Mutex<Option<super::InboundWaker>>>,
        closed: Arc<AtomicBool>,
        conn: Connection,
        tasks: Vec<tokio::task::AbortHandle>,
    }

    impl std::fmt::Debug for QuicTransport {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("QuicTransport")
                .field("peer", &self.peer_label)
                .field("closed", &self.is_closed())
                .finish()
        }
    }

    impl QuicTransport {
        /// Open a client QUIC connection to `addr` (verifying the server +
        /// presenting our identity per the endpoint's client config), then
        /// wrap it. `server_name` must match the server cert SAN.
        pub async fn connect(
            endpoint: &Endpoint,
            client_config: quinn::ClientConfig,
            addr: SocketAddr,
            server_name: &str,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> Result<Self, TransportError> {
            let conn = endpoint
                .connect_with(client_config, addr, server_name)
                .map_err(|e| TransportError::Tls(format!("quic connect: {e}")))?
                .await
                .map_err(|e| TransportError::Tls(format!("quic handshake: {e}")))?;
            Ok(Self::from_connection(conn, peer_label, cfg))
        }

        /// Wrap an established QUIC connection (client or server side) with
        /// **per-channel** stream multiplexing: one QUIC stream per
        /// [`Channel`], independent writer tasks, per-stream priority. A
        /// `Bulk` transfer — including its packet-loss retransmissions —
        /// therefore can't head-of-line-block `Control`.
        pub fn from_connection(
            conn: Connection,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> Self {
            Self::new_io(conn, peer_label.into(), cfg, false)
        }

        /// Like [`from_connection`](Self::from_connection) but multiplexes
        /// **all** channels onto a single shared QUIC stream — the
        /// no-isolation baseline (equivalent to the single-ordered-stream TCP
        /// transport): one lost packet stalls every channel queued behind it.
        /// Used to demonstrate the per-stream design's advantage under loss;
        /// not the production path.
        pub fn from_connection_single_stream(
            conn: Connection,
            peer_label: impl Into<String>,
            cfg: &TransportConfig,
        ) -> Self {
            Self::new_io(conn, peer_label.into(), cfg, true)
        }

        fn new_io(
            conn: Connection,
            peer_label: String,
            cfg: &TransportConfig,
            single_stream: bool,
        ) -> Self {
            let inbound: Inbound = Arc::new(std::array::from_fn(|_| Mutex::new(VecDeque::new())));
            let inbound_waker: Arc<Mutex<Option<super::InboundWaker>>> = Arc::new(Mutex::new(None));
            let closed = Arc::new(AtomicBool::new(false));
            let max_frame = cfg.max_frame_bytes;
            let mut tasks: Vec<tokio::task::AbortHandle> =
                Vec::with_capacity(Channel::ALL.len() + 1);

            // In single-stream mode every channel task shares one lazily-opened
            // stream behind an async mutex, so writes serialize onto it (the
            // HoL-prone baseline). In per-channel mode each task owns its own
            // prioritized stream. Either way frames are self-describing —
            // `[channel][len][payload]` — so the reader below handles both.
            let shared: Option<Arc<AsyncMutex<Option<quinn::SendStream>>>> =
                single_stream.then(|| Arc::new(AsyncMutex::new(None)));

            let mut senders: Vec<mpsc::UnboundedSender<Vec<u8>>> =
                Vec::with_capacity(Channel::ALL.len());
            for ch in Channel::ALL {
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                senders.push(tx);
                let conn_w = conn.clone();
                let closed_w = closed.clone();
                let shared_w = shared.clone();
                let w = tokio::spawn(async move {
                    match shared_w {
                        // Single shared stream: lock, lazily open, write frame.
                        Some(shared) => {
                            while let Some(payload) = rx.recv().await {
                                let mut guard = shared.lock().await;
                                if guard.is_none() {
                                    match conn_w.open_uni().await {
                                        Ok(s) => *guard = Some(s),
                                        Err(_) => {
                                            closed_w.store(true, Ordering::Release);
                                            return;
                                        }
                                    }
                                }
                                let s = guard.as_mut().expect("shared stream open");
                                if write_frame(s, ch, &payload).await.is_err() {
                                    closed_w.store(true, Ordering::Release);
                                    return;
                                }
                            }
                        }
                        // Dedicated per-channel stream (lazily opened, prioritized).
                        None => {
                            let mut stream: Option<quinn::SendStream> = None;
                            while let Some(payload) = rx.recv().await {
                                if stream.is_none() {
                                    match conn_w.open_uni().await {
                                        Ok(s) => {
                                            let _ = s.set_priority(ch.priority());
                                            stream = Some(s);
                                        }
                                        Err(_) => {
                                            closed_w.store(true, Ordering::Release);
                                            return;
                                        }
                                    }
                                }
                                let s = stream.as_mut().expect("stream open");
                                if write_frame(s, ch, &payload).await.is_err() {
                                    closed_w.store(true, Ordering::Release);
                                    return;
                                }
                            }
                        }
                    }
                });
                tasks.push(w.abort_handle());
            }
            let outbound: [mpsc::UnboundedSender<Vec<u8>>; Channel::ALL.len()] =
                senders.try_into().expect("one sender per channel");

            // Acceptor: each incoming uni stream gets a reader pulling
            // self-describing `[channel][len][payload]` frames and demuxing
            // them (one stream may carry several channels in single-stream mode).
            let conn_r = conn.clone();
            let inbound_r = inbound.clone();
            let waker_r = inbound_waker.clone();
            let closed_r = closed.clone();
            let acceptor = tokio::spawn(async move {
                // One reader task per incoming uni stream; loop ends when the
                // connection closes (`accept_uni` errors).
                while let Ok(mut recv) = conn_r.accept_uni().await {
                    let inb = inbound_r.clone();
                    let waker_s = waker_r.clone();
                    tokio::spawn(async move {
                        loop {
                            let mut hdr = [0u8; 5];
                            if recv.read_exact(&mut hdr).await.is_err() {
                                return; // stream finished / reset
                            }
                            let Some(ch) = Channel::ALL.into_iter().find(|c| *c as u8 == hdr[0])
                            else {
                                return; // unknown channel tag
                            };
                            let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
                            if len > max_frame {
                                return;
                            }
                            let mut payload = vec![0u8; len];
                            if recv.read_exact(&mut payload).await.is_err() {
                                return;
                            }
                            inb[ch as usize]
                                .lock()
                                .expect("quic inbound poisoned")
                                .push_back(payload);
                            if let Some(w) = waker_s.lock().expect("quic waker poisoned").as_ref() {
                                w();
                            }
                        }
                    });
                }
                closed_r.store(true, Ordering::Release);
            });

            tasks.push(acceptor.abort_handle());
            QuicTransport {
                peer_label,
                outbound,
                inbound,
                inbound_waker,
                closed,
                conn,
                tasks,
            }
        }
    }

    /// Write one self-describing frame `[channel:u8][len:u32 BE][payload]`.
    async fn write_frame(s: &mut quinn::SendStream, ch: Channel, payload: &[u8]) -> Result<(), ()> {
        let mut hdr = [0u8; 5];
        hdr[0] = ch as u8;
        hdr[1..5].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        if s.write_all(&hdr).await.is_err() || s.write_all(payload).await.is_err() {
            return Err(());
        }
        Ok(())
    }

    impl Drop for QuicTransport {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
            self.conn.close(0u32.into(), b"transport dropped");
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    impl Transport for QuicTransport {
        fn send(&self, channel: Channel, payload: &[u8]) -> Result<(), TransportError> {
            if self.is_closed() {
                return Err(TransportError::PeerClosed);
            }
            self.outbound[channel as usize]
                .send(payload.to_vec())
                .map_err(|_| TransportError::PeerClosed)
        }

        fn try_recv(&self, channel: Channel) -> Result<Option<Vec<u8>>, TransportError> {
            if let Some(frame) = self.inbound[channel as usize]
                .lock()
                .expect("quic inbound poisoned")
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
            self.conn.close(0u32.into(), b"closed");
            Ok(())
        }

        fn set_inbound_waker(&self, waker: super::InboundWaker) {
            *self.inbound_waker.lock().expect("quic waker poisoned") = Some(waker);
        }
    }

    /// Loss-injecting UDP relay for tests/benchmarks (not a production path).
    ///
    /// Forwards QUIC datagrams between a client and `server_addr`, dropping
    /// each datagram with probability `drop_prob` (deterministic per `seed`).
    /// Point the QUIC client at the returned relay address; TLS stays
    /// end-to-end (the relay only drops, never inspects). Dropping at the
    /// *datagram* level is what exercises QUIC's per-stream loss recovery —
    /// the regime where one stream per [`Channel`] avoids the head-of-line
    /// blocking a single ordered stream cannot.
    pub async fn lossy_relay(
        server_addr: SocketAddr,
        drop_prob: f64,
        seed: u64,
    ) -> std::io::Result<SocketAddr> {
        let sock = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await?);
        let relay_addr = sock.local_addr()?;
        tokio::spawn(async move {
            let mut state = seed | 1; // LCG state (nonzero)
            let mut client: Option<SocketAddr> = None;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let (n, from) = match sock.recv_from(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let dst = if from == server_addr {
                    match client {
                        Some(c) => c,
                        None => continue, // no client seen yet
                    }
                } else {
                    client = Some(from);
                    server_addr
                };
                // PCG-style LCG → pseudo-random in [0, 1); deterministic per seed.
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let r = (state >> 33) as f64 / (1u64 << 31) as f64;
                if r < drop_prob {
                    continue; // drop this datagram
                }
                let _ = sock.send_to(&buf[..n], dst).await;
            }
        });
        Ok(relay_addr)
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

    // ---- CA-issued per-node certs (real mutual auth of distinct identities) --

    /// A fresh CA (root cert + signing key).
    fn make_ca() -> (rcgen::Certificate, rcgen::KeyPair) {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
        let mut p = CertificateParams::new(Vec::new()).unwrap();
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().unwrap();
        let cert = p.self_signed(&key).unwrap();
        (cert, key)
    }

    /// A leaf cert (chain + key) for `name`, signed by `ca`. Usable as both a
    /// server and client cert (serverAuth + clientAuth EKUs).
    fn ca_leaf(
        ca: &(rcgen::Certificate, rcgen::KeyPair),
        name: &str,
    ) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
        let mut p =
            CertificateParams::new(vec![name.to_string(), "localhost".to_string()]).unwrap();
        p.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &ca.0, &ca.1).unwrap();
        (
            vec![cert.der().clone()],
            PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
    }

    fn ca_roots(ca: &(rcgen::Certificate, rcgen::KeyPair)) -> RootCertStore {
        let mut r = RootCertStore::empty();
        r.add(ca.0.der().clone()).unwrap();
        r
    }

    #[tokio::test]
    async fn mtls_ca_accepts_peer_signed_by_cluster_ca() {
        tls::install_crypto_provider();
        let ca = make_ca();
        let (s_chain, s_key) = ca_leaf(&ca, "server");
        let (c_chain, c_key) = ca_leaf(&ca, "client");
        let server_cfg = Arc::new(tls::server_config(ca_roots(&ca), s_chain, s_key).unwrap());
        let client_cfg = Arc::new(tls::client_config(ca_roots(&ca), c_chain, c_key).unwrap());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TcpTransport::accept_tls(tcp, "client", &TransportConfig::default(), server_cfg).await
        });
        let client = TcpTransport::connect_tls(
            &addr.to_string(),
            "localhost",
            "server",
            &TransportConfig::default(),
            client_cfg,
        )
        .await
        .expect("CA-signed client must be accepted");
        let server = accept
            .await
            .unwrap()
            .expect("server accepts CA-signed client");

        client.send(Channel::Messages, b"ca-ping").unwrap();
        assert_eq!(recv(&server, Channel::Messages).await, b"ca-ping");
    }

    #[tokio::test]
    async fn mtls_ca_rejects_peer_signed_by_foreign_ca() {
        tls::install_crypto_provider();
        let cluster = make_ca();
        let foreign = make_ca(); // a different CA, not in the cluster trust store
        let (s_chain, s_key) = ca_leaf(&cluster, "server");
        // Client trusts the cluster CA (so it accepts the server) but presents a
        // cert signed by the FOREIGN CA — the server must reject it.
        let (c_chain, c_key) = ca_leaf(&foreign, "intruder");
        let server_cfg = Arc::new(tls::server_config(ca_roots(&cluster), s_chain, s_key).unwrap());
        let client_cfg = Arc::new(tls::client_config(ca_roots(&cluster), c_chain, c_key).unwrap());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TcpTransport::accept_tls(tcp, "intruder", &TransportConfig::default(), server_cfg).await
        });
        let _ = TcpTransport::connect_tls(
            &addr.to_string(),
            "localhost",
            "server",
            &TransportConfig::default(),
            client_cfg,
        )
        .await;

        // Server is authoritative: a cert that doesn't chain to the cluster CA
        // is refused, proving the CA actually verifies the chain.
        assert!(
            accept.await.unwrap().is_err(),
            "server must reject a peer whose cert is signed by a foreign CA"
        );
    }
}

#[cfg(all(test, feature = "quic"))]
mod quic_tests {
    use super::quic::QuicTransport;
    use super::{tls, Channel, Transport, TransportConfig};
    use quinn::Endpoint;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::RootCertStore;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        (
            CertificateDer::from(ck.cert.der().to_vec()),
            PrivateKeyDer::try_from(ck.key_pair.serialize_der()).unwrap(),
        )
    }

    async fn recv(t: &QuicTransport, ch: Channel) -> Vec<u8> {
        for _ in 0..400 {
            if let Some(f) = t.try_recv(ch).unwrap() {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("frame not received");
    }

    /// Connected client+server transports. Returns the endpoints too — they
    /// must outlive the connections (dropping a quinn Endpoint stops its
    /// driver and closes its connections).
    async fn connected() -> (QuicTransport, QuicTransport, Endpoint, Endpoint) {
        tls::install_crypto_provider();
        let (cert, key) = identity();
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let scfg =
            tls::quic_server_config(roots.clone(), vec![cert.clone()], key.clone_key()).unwrap();
        let ccfg = tls::quic_client_config(roots, vec![cert.clone()], key).unwrap();

        let server_ep = Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server_ep.local_addr().unwrap();
        let accept_ep = server_ep.clone();
        let accept = tokio::spawn(async move {
            let conn = accept_ep.accept().await.unwrap().await.unwrap();
            QuicTransport::from_connection(conn, "client@host", &TransportConfig::default())
        });
        let client_ep = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let client = QuicTransport::connect(
            &client_ep,
            ccfg,
            addr,
            "localhost",
            "server@host",
            &TransportConfig::default(),
        )
        .await
        .unwrap();
        let server = accept.await.unwrap();
        (client, server, client_ep, server_ep)
    }

    #[tokio::test]
    async fn quic_loopback_round_trip_encrypted() {
        let (client, server, _cep, _sep) = connected().await;
        // QUIC is always TLS 1.3 → this traffic is encrypted + mutually authd.
        client.send(Channel::Messages, b"q-ping").unwrap();
        assert_eq!(recv(&server, Channel::Messages).await, b"q-ping");
        server.send(Channel::Control, b"q-pong").unwrap();
        assert_eq!(recv(&client, Channel::Control).await, b"q-pong");
        assert_eq!(server.peer_label(), "client@host");
    }

    #[tokio::test]
    async fn quic_channels_use_independent_streams() {
        // Each channel rides its own QUIC stream → no head-of-line blocking
        // across channels. Send on Bulk + Control + Messages; all arrive on
        // their own channel, FIFO within each.
        let (client, server, _cep, _sep) = connected().await;
        for i in 0u8..4 {
            client.send(Channel::Bulk, &[i]).unwrap();
        }
        client.send(Channel::Control, b"ctl").unwrap();
        client.send(Channel::Messages, b"msg").unwrap();
        assert_eq!(recv(&server, Channel::Control).await, b"ctl");
        assert_eq!(recv(&server, Channel::Messages).await, b"msg");
        for i in 0u8..4 {
            assert_eq!(recv(&server, Channel::Bulk).await, vec![i]);
        }
    }

    /// Connected pair whose datagrams cross a loss-injecting relay
    /// ([`super::quic::lossy_relay`]). `single_stream` selects the
    /// no-isolation baseline. Both ends use the same mode (and the same drop
    /// seed, so the comparison is fair).
    async fn connected_lossy(
        single_stream: bool,
        drop_prob: f64,
    ) -> (QuicTransport, QuicTransport, Endpoint, Endpoint) {
        tls::install_crypto_provider();
        let (cert, key) = identity();
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let scfg =
            tls::quic_server_config(roots.clone(), vec![cert.clone()], key.clone_key()).unwrap();
        let ccfg = tls::quic_client_config(roots, vec![cert.clone()], key).unwrap();
        let cfg = TransportConfig::default();

        let server_ep = Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server_ep.local_addr().unwrap();
        let relay = super::quic::lossy_relay(server_addr, drop_prob, 0x1234)
            .await
            .unwrap();

        let accept_ep = server_ep.clone();
        let accept = tokio::spawn(async move {
            let conn = accept_ep.accept().await.unwrap().await.unwrap();
            let cfg = TransportConfig::default();
            if single_stream {
                QuicTransport::from_connection_single_stream(conn, "client@host", &cfg)
            } else {
                QuicTransport::from_connection(conn, "client@host", &cfg)
            }
        });
        let client_ep = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn = client_ep
            .connect_with(ccfg, relay, "localhost")
            .unwrap()
            .await
            .unwrap();
        let client = if single_stream {
            QuicTransport::from_connection_single_stream(conn, "server@host", &cfg)
        } else {
            QuicTransport::from_connection(conn, "server@host", &cfg)
        };
        let server = accept.await.unwrap();
        (client, server, client_ep, server_ep)
    }

    /// Time `n` `Control` round trips while a bounded `Bulk` flood runs a→b.
    async fn control_under_bulk(
        a: Arc<QuicTransport>,
        b: Arc<QuicTransport>,
        n: usize,
    ) -> Duration {
        let stop = Arc::new(AtomicBool::new(false));
        let inflight = Arc::new(AtomicUsize::new(0));
        let bulk = vec![0u8; 256 * 1024];

        let a_b = a.clone();
        let stop1 = stop.clone();
        let inf1 = inflight.clone();
        let blaster = tokio::spawn(async move {
            while !stop1.load(Ordering::Relaxed) {
                if inf1.load(Ordering::Relaxed) < 8 && a_b.send(Channel::Bulk, &bulk).is_ok() {
                    inf1.fetch_add(1, Ordering::Relaxed);
                }
                tokio::task::yield_now().await;
            }
        });
        let b_b = b.clone();
        let stop2 = stop.clone();
        let inf2 = inflight.clone();
        let drainer = tokio::spawn(async move {
            while !stop2.load(Ordering::Relaxed) {
                match b_b.try_recv(Channel::Bulk) {
                    Ok(Some(_)) => {
                        inf2.fetch_sub(1, Ordering::Relaxed);
                    }
                    _ => tokio::task::yield_now().await,
                }
            }
        });

        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
        let start = Instant::now();
        for _ in 0..n {
            a.send(Channel::Control, b"ping").unwrap();
            loop {
                match b.try_recv(Channel::Control) {
                    Ok(Some(m)) => {
                        b.send(Channel::Control, &m).unwrap();
                        break;
                    }
                    Ok(None) => tokio::task::yield_now().await,
                    Err(_) => panic!("peer closed"),
                }
            }
            loop {
                match a.try_recv(Channel::Control) {
                    Ok(Some(_)) => break,
                    Ok(None) => tokio::task::yield_now().await,
                    Err(_) => panic!("peer closed"),
                }
            }
        }
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        let _ = blaster.await;
        let _ = drainer.await;
        elapsed
    }

    #[tokio::test]
    async fn quic_per_stream_isolates_control_from_bulk_under_loss() {
        // The payoff of one stream per channel: under datagram loss, a `Bulk`
        // flood's retransmissions must not stall `Control`. With per-channel
        // streams Control rides its own stream (isolated); with a single shared
        // stream one lost Bulk packet head-of-line-blocks Control behind it.
        // On a clean link this difference is invisible — loss is what reveals
        // it — so cross a 5%-loss relay and compare the two modes.
        let n = 12;
        let drop = 0.05;

        let (ca, cb, _c1, _s1) = connected_lossy(false, drop).await; // per-channel
        let multi = control_under_bulk(Arc::new(ca), Arc::new(cb), n).await;

        let (sa, sb, _c2, _s2) = connected_lossy(true, drop).await; // single stream
        let single = control_under_bulk(Arc::new(sa), Arc::new(sb), n).await;

        // The measured gap is ~100x; assert a conservative 4x so the test is
        // robust to scheduling/loss variance while still proving the isolation.
        assert!(
            single > multi * 4,
            "per-channel streams should keep Control far more responsive than a \
             single shared stream under loss; got multi={multi:?}, single={single:?}"
        );
    }
}
