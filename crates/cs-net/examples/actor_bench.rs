//! Distributed-transport micro-benchmark for the cs-net cluster substrate.
//!
//! Drives every transport (`Sim` / `TCP` / `TCP+mTLS` / `QUIC`) through the
//! *same* kernels via the [`cs_net::Transport`] trait, so the numbers are
//! apples-to-apples. The kernels are the ones the actor-systems literature
//! uses to characterise a message transport:
//!
//! 1. **Ping-pong RTT** — Savina's latency micro-benchmark: one message in
//!    flight, A→B→A, repeated. Measures per-message round-trip overhead
//!    (framing + syscalls + crypto), the number that dominates request/reply
//!    actor traffic. (Savina, AGERE'14.)
//! 2. **One-way throughput** — pipeline N messages A→B as fast as the
//!    transport accepts them; time until all N are delivered. Reported in
//!    msg/s and MiB/s at 64 B / 1 KiB / 64 KiB.
//! 3. **Control latency under bulk load** — the QUIC differentiator. While a
//!    background flood saturates the `Bulk` channel, measure `Control`
//!    ping-pong RTT. The ratio to the idle RTT is the *head-of-line-blocking
//!    factor*: ~1.0 means a stalled bulk transfer does NOT delay control
//!    traffic (true per-channel isolation); >>1.0 means it does.
//!
//! These mirror what PARTISAN (USENIX ATC'19) and the KTH "low-latency
//! transport protocols in actor systems" study probe: QUIC's per-stream
//! multiplexing is supposed to win on (3) and on large/lossy transfers,
//! while paying a per-packet crypto/event-loop tax that can make it *lose*
//! to plain TCP on tiny-message ping-pong over loopback.
//!
//! Run (release is essential — debug numbers are meaningless):
//! ```text
//! cargo run --release -p cs-net --example actor_bench                  # Sim + TCP
//! cargo run --release -p cs-net --example actor_bench --features quic  # + QUIC
//! ```
//! Loopback only — absolute numbers reflect *software* overhead, not a real
//! network. The cross-transport *ratios* are the signal.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_net::{Channel, Transport, TransportConfig, TransportError};

/// Boxed transport handle — every kernel is generic over this.
type Tx = Arc<dyn Transport>;

// Workload sizes. Kept modest so the whole suite runs in a few seconds.
const PP_N: usize = 2_000; // ping-pong round trips
const TP_N_SMALL: usize = 50_000; // throughput msgs at 64 B / 1 KiB
const TP_N_LARGE: usize = 4_000; // throughput msgs at 64 KiB
const HOL_CTRL_N: usize = 300; // control round trips during bulk flood
const HOL_BULK_BYTES: usize = 256 * 1024; // size of each bulk message
const HOL_INFLIGHT: usize = 8; // bulk messages kept outstanding (bounded)

// ---------------------------------------------------------------------------
// Transport-agnostic benchmark kernels
// ---------------------------------------------------------------------------

/// Spin (yielding to the runtime so the I/O tasks make progress) until a
/// frame arrives on `ch`, or the peer closes.
async fn await_recv(t: &Tx, ch: Channel) -> Option<Vec<u8>> {
    loop {
        match t.try_recv(ch) {
            Ok(Some(m)) => return Some(m),
            Ok(None) => tokio::task::yield_now().await,
            Err(_) => return None, // closed + drained
        }
    }
}

/// Savina ping-pong: `n` sequential round trips, one message in flight.
async fn ping_pong(a: &Tx, b: &Tx, ch: Channel, payload: &[u8], n: usize) -> Duration {
    // One warm-up round so connection setup / first-stream costs don't skew it.
    a.send(ch, payload).unwrap();
    let m = await_recv(b, ch).await.expect("peer closed");
    b.send(ch, &m).unwrap();
    await_recv(a, ch).await.expect("peer closed");

    let start = Instant::now();
    for _ in 0..n {
        a.send(ch, payload).unwrap();
        let m = await_recv(b, ch).await.expect("peer closed");
        b.send(ch, &m).unwrap();
        await_recv(a, ch).await.expect("peer closed");
    }
    start.elapsed()
}

/// One-way throughput: pipeline `n` sends A→B, time until B has received all.
async fn throughput(a: &Tx, b: &Tx, ch: Channel, payload: &[u8], n: usize) -> Duration {
    let b2 = b.clone();
    let recv = tokio::spawn(async move {
        let mut got = 0usize;
        while got < n {
            match b2.try_recv(ch) {
                Ok(Some(_)) => got += 1,
                Ok(None) => tokio::task::yield_now().await,
                Err(_) => break, // peer closed early
            }
        }
    });

    let start = Instant::now();
    'send: for _ in 0..n {
        loop {
            match a.send(ch, payload) {
                Ok(()) => break,
                // Sim enforces a per-channel watermark; let the drainer catch up.
                Err(TransportError::Backpressure { .. }) => tokio::task::yield_now().await,
                Err(_) => break 'send,
            }
        }
    }
    let _ = recv.await;
    start.elapsed()
}

/// `Control` ping-pong RTT while a bounded flood keeps the `Bulk` channel
/// busy. `HOL_INFLIGHT` bulk messages are kept outstanding (bounded so memory
/// stays flat and the comparison is fair across transports).
async fn ctrl_rtt_under_bulk(a: &Tx, b: &Tx, bulk: &[u8], ctrl: &[u8], n: usize) -> Duration {
    let stop = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new(AtomicUsize::new(0));

    let a_bulk = a.clone();
    let bulk_vec = bulk.to_vec();
    let stop_s = stop.clone();
    let inflight_s = inflight.clone();
    let blaster = tokio::spawn(async move {
        while !stop_s.load(Ordering::Relaxed) {
            if inflight_s.load(Ordering::Relaxed) < HOL_INFLIGHT
                && a_bulk.send(Channel::Bulk, &bulk_vec).is_ok()
            {
                inflight_s.fetch_add(1, Ordering::Relaxed);
            }
            tokio::task::yield_now().await;
        }
    });

    let b_bulk = b.clone();
    let stop_d = stop.clone();
    let inflight_d = inflight.clone();
    let drainer = tokio::spawn(async move {
        while !stop_d.load(Ordering::Relaxed) {
            match b_bulk.try_recv(Channel::Bulk) {
                Ok(Some(_)) => {
                    inflight_d.fetch_sub(1, Ordering::Relaxed);
                }
                _ => tokio::task::yield_now().await,
            }
        }
    });

    // Let the flood ramp up before measuring.
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }

    let start = Instant::now();
    for _ in 0..n {
        a.send(Channel::Control, ctrl).unwrap();
        let m = await_recv(b, Channel::Control).await.expect("peer closed");
        b.send(Channel::Control, &m).unwrap();
        await_recv(a, Channel::Control).await.expect("peer closed");
    }
    let elapsed = start.elapsed();

    stop.store(true, Ordering::Relaxed);
    let _ = blaster.await;
    let _ = drainer.await;
    elapsed
}

// ---------------------------------------------------------------------------
// Per-transport connected-pair builders
// ---------------------------------------------------------------------------

#[cfg(feature = "sim")]
fn sim_pair() -> (Tx, Tx) {
    let (a, b) = cs_net::sim::SimPair::new("a@local", "b@local").into_endpoints();
    (Arc::new(a), Arc::new(b))
}

#[cfg(any(feature = "tcp", feature = "quic"))]
fn identity() -> (
    rustls::pki_types::CertificateDer<'static>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    (
        rustls::pki_types::CertificateDer::from(ck.cert.der().to_vec()),
        rustls::pki_types::PrivateKeyDer::try_from(ck.key_pair.serialize_der()).unwrap(),
    )
}

#[cfg(feature = "tcp")]
async fn tcp_pair() -> (Tx, Tx) {
    use cs_net::tcp::TcpTransport;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        TcpTransport::from_stream(s, "a@local", &TransportConfig::default())
    });
    let client = TcpTransport::connect(&addr.to_string(), "b@local", &TransportConfig::default())
        .await
        .unwrap();
    let server = accept.await.unwrap();
    (Arc::new(client), Arc::new(server))
}

#[cfg(feature = "tcp")]
async fn tcp_tls_pair() -> (Tx, Tx) {
    use cs_net::tcp::TcpTransport;
    use tokio::net::TcpListener;

    cs_net::tls::install_crypto_provider();
    let (cert, key) = identity();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let scfg = Arc::new(
        cs_net::tls::server_config(roots.clone(), vec![cert.clone()], key.clone_key()).unwrap(),
    );
    let ccfg = Arc::new(cs_net::tls::client_config(roots, vec![cert.clone()], key).unwrap());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        TcpTransport::accept_tls(tcp, "a@local", &TransportConfig::default(), scfg)
            .await
            .unwrap()
    });
    let client = TcpTransport::connect_tls(
        &addr.to_string(),
        "localhost",
        "b@local",
        &TransportConfig::default(),
        ccfg,
    )
    .await
    .unwrap();
    let server = accept.await.unwrap();
    (Arc::new(client), Arc::new(server))
}

/// Endpoints are returned so the caller keeps them alive — dropping a quinn
/// `Endpoint` stops its driver and closes the connection.
#[cfg(feature = "quic")]
async fn quic_pair() -> (Tx, Tx, quinn::Endpoint, quinn::Endpoint) {
    use cs_net::quic::QuicTransport;

    cs_net::tls::install_crypto_provider();
    let (cert, key) = identity();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let scfg = cs_net::tls::quic_server_config(roots.clone(), vec![cert.clone()], key.clone_key())
        .unwrap();
    let ccfg = cs_net::tls::quic_client_config(roots, vec![cert.clone()], key).unwrap();

    let server_ep = quinn::Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = server_ep.local_addr().unwrap();
    let accept_ep = server_ep.clone();
    let accept = tokio::spawn(async move {
        let conn = accept_ep.accept().await.unwrap().await.unwrap();
        QuicTransport::from_connection(conn, "a@local", &TransportConfig::default())
    });
    let client_ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let client = QuicTransport::connect(
        &client_ep,
        ccfg,
        addr,
        "localhost",
        "b@local",
        &TransportConfig::default(),
    )
    .await
    .unwrap();
    let server = accept.await.unwrap();
    (Arc::new(client), Arc::new(server), client_ep, server_ep)
}

/// A QUIC pair whose datagrams traverse a loss-injecting relay
/// ([`cs_net::quic::lossy_relay`]). `single_stream` picks the no-isolation
/// baseline (all channels share one ordered stream) vs the per-channel
/// design. Loss at the datagram level is what makes QUIC's per-stream
/// recovery matter — the head-of-line case a single stream can't avoid.
#[cfg(feature = "quic")]
async fn quic_pair_lossy(
    drop_prob: f64,
    single_stream: bool,
) -> (Tx, Tx, quinn::Endpoint, quinn::Endpoint) {
    use cs_net::quic::QuicTransport;

    cs_net::tls::install_crypto_provider();
    let (cert, key) = identity();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let scfg = cs_net::tls::quic_server_config(roots.clone(), vec![cert.clone()], key.clone_key())
        .unwrap();
    let ccfg = cs_net::tls::quic_client_config(roots, vec![cert.clone()], key).unwrap();

    let server_ep = quinn::Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let server_addr = server_ep.local_addr().unwrap();
    // Client connects to the relay; the relay forwards to the server, dropping
    // `drop_prob` of datagrams each way. TLS stays end-to-end.
    let relay_addr = cs_net::quic::lossy_relay(server_addr, drop_prob, 0xC0FFEE)
        .await
        .unwrap();

    let accept_ep = server_ep.clone();
    let accept = tokio::spawn(async move {
        let conn = accept_ep.accept().await.unwrap().await.unwrap();
        let cfg = TransportConfig::default();
        if single_stream {
            QuicTransport::from_connection_single_stream(conn, "a@local", &cfg)
        } else {
            QuicTransport::from_connection(conn, "a@local", &cfg)
        }
    });

    let client_ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let conn = client_ep
        .connect_with(ccfg, relay_addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let cfg = TransportConfig::default();
    let client = if single_stream {
        QuicTransport::from_connection_single_stream(conn, "b@local", &cfg)
    } else {
        QuicTransport::from_connection(conn, "b@local", &cfg)
    };
    let server = accept.await.unwrap();
    (Arc::new(client), Arc::new(server), client_ep, server_ep)
}

// ---------------------------------------------------------------------------
// Suite runner + reporting
// ---------------------------------------------------------------------------

async fn thrpt(a: &Tx, b: &Tx, payload: &[u8], n: usize) -> (u64, f64) {
    let d = throughput(a, b, Channel::Messages, payload, n).await;
    let s = d.as_secs_f64().max(f64::MIN_POSITIVE);
    let msg_per_s = (n as f64 / s) as u64;
    let mib_per_s = (n as f64 * payload.len() as f64) / s / (1024.0 * 1024.0);
    (msg_per_s, mib_per_s)
}

async fn run_suite(label: &str, a: &Tx, b: &Tx) {
    let p64 = vec![0u8; 64];
    let p1k = vec![0u8; 1024];
    let p64k = vec![0u8; 64 * 1024];

    let pp = ping_pong(a, b, Channel::Messages, &p64, PP_N).await;
    let pp_us = pp.as_secs_f64() * 1e6 / PP_N as f64;

    let (m64, r64) = thrpt(a, b, &p64, TP_N_SMALL).await;
    let (m1k, r1k) = thrpt(a, b, &p1k, TP_N_SMALL).await;
    let (m64k, r64k) = thrpt(a, b, &p64k, TP_N_LARGE).await;

    let idle = ping_pong(a, b, Channel::Control, &p64, HOL_CTRL_N).await;
    let idle_us = idle.as_secs_f64() * 1e6 / HOL_CTRL_N as f64;
    let bulk = vec![0u8; HOL_BULK_BYTES];
    let under = ctrl_rtt_under_bulk(a, b, &bulk, &p64, HOL_CTRL_N).await;
    let under_us = under.as_secs_f64() * 1e6 / HOL_CTRL_N as f64;
    let hol_factor = under_us / idle_us.max(f64::MIN_POSITIVE);

    println!("\n== {label} ==");
    println!("  ping-pong RTT (64 B)       : {pp_us:>10.2} µs/op");
    println!("  throughput  64 B           : {m64:>10} msg/s   ({r64:>8.1} MiB/s)");
    println!("  throughput   1 KiB         : {m1k:>10} msg/s   ({r1k:>8.1} MiB/s)");
    println!("  throughput  64 KiB         : {m64k:>10} msg/s   ({r64k:>8.1} MiB/s)");
    println!("  control RTT idle           : {idle_us:>10.2} µs/op");
    println!(
        "  control RTT under bulk load: {under_us:>10.2} µs/op   (HoL factor {hol_factor:.2}x)"
    );
}

/// Focused head-of-line probe (for the lossy comparison): control ping-pong
/// RTT idle vs under a bulk flood, and the HoL factor, over `n` round trips.
#[cfg(feature = "quic")]
async fn run_hol(label: &str, a: &Tx, b: &Tx, n: usize) {
    let p64 = vec![0u8; 64];
    let idle = ping_pong(a, b, Channel::Control, &p64, n).await;
    let idle_us = idle.as_secs_f64() * 1e6 / n as f64;
    let bulk = vec![0u8; HOL_BULK_BYTES];
    let under = ctrl_rtt_under_bulk(a, b, &bulk, &p64, n).await;
    let under_us = under.as_secs_f64() * 1e6 / n as f64;
    let hol = under_us / idle_us.max(f64::MIN_POSITIVE);
    println!("  {label:<34}: ctrl idle {idle_us:>8.1} µs, under-bulk {under_us:>9.1} µs  (HoL {hol:.2}x)");
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("cs-net distributed-transport benchmark (loopback; Savina-style kernels)");
    println!(
        "params: ping-pong N={PP_N}, throughput N={TP_N_SMALL}/{TP_N_LARGE}, \
         bulk-flood {HOL_BULK_BYTES}B x{HOL_INFLIGHT} in-flight, control N={HOL_CTRL_N}"
    );

    #[cfg(feature = "sim")]
    {
        let (a, b) = sim_pair();
        run_suite("Sim (in-memory, per-channel queues)", &a, &b).await;
    }

    #[cfg(feature = "tcp")]
    {
        let (a, b) = tcp_pair().await;
        run_suite("TCP (plaintext loopback)", &a, &b).await;
    }

    #[cfg(feature = "tcp")]
    {
        let (a, b) = tcp_tls_pair().await;
        run_suite("TCP + mTLS (rustls)", &a, &b).await;
    }

    #[cfg(feature = "quic")]
    {
        let (a, b, _client_ep, _server_ep) = quic_pair().await;
        run_suite("QUIC (quinn, mTLS, per-channel streams)", &a, &b).await;
    }

    // The point of per-channel streams is loss isolation: a dropped Bulk
    // packet must not stall Control. That only shows on a LOSSY link (clean
    // loopback is CPU-bound, not loss-bound), so inject 5% datagram loss and
    // compare the per-channel design against a single shared stream.
    #[cfg(feature = "quic")]
    {
        println!(
            "\n== QUIC over a LOSSY link (5% datagram drop): per-stream isolation vs baseline =="
        );
        let n = 80;
        {
            let (a, b, _c, _s) = quic_pair_lossy(0.05, false).await;
            run_hol("per-channel streams (the fix)", &a, &b, n).await;
        }
        {
            let (a, b, _c, _s) = quic_pair_lossy(0.05, true).await;
            run_hol("single shared stream (baseline)", &a, &b, n).await;
        }
    }

    #[cfg(not(feature = "quic"))]
    println!("\n(QUIC not built — re-run with `--features quic` to include it.)");
}
