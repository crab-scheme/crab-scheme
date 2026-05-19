//! TFB-style load generator (client only).
//!
//! Companion to `tfb_bench.rs` — instead of spawning the cs-web
//! server in-process, this one connects to an *external* server
//! on a given host:port and runs the same load loop. Used for
//! apples-to-apples comparisons against axum / Express / Gin / etc.
//!
//! Usage:
//!
//!   cargo run --release --example tfb_client -p cs-web -- \
//!     <host:port> <path> <duration_s> <connections>
//!
//! Output: one line with name + counts + RPS + mean/p50/p99
//! latencies in microseconds.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1 as client_h1;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

async fn drive_connection(addr: SocketAddr, path: String, deadline: Instant) -> (u64, Vec<u64>) {
    let stream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(_) => return (0, Vec::new()),
    };
    let _ = stream.set_nodelay(true);
    let io = TokioIo::new(stream);
    let (mut sender, conn) = match client_h1::handshake(io).await {
        Ok(v) => v,
        Err(_) => return (0, Vec::new()),
    };
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut count: u64 = 0;
    let mut lats: Vec<u64> = Vec::with_capacity(1_000_000);
    while Instant::now() < deadline {
        let start = Instant::now();
        let req = http::Request::builder()
            .uri(&path)
            .header("host", addr.to_string())
            .header("connection", "keep-alive")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let resp = match sender.send_request(req).await {
            Ok(r) => r,
            Err(_) => break,
        };
        let (_, body) = resp.into_parts();
        if body.collect().await.is_err() {
            break;
        }
        let lat = start.elapsed().as_micros() as u64;
        count += 1;
        lats.push(lat);
    }
    (count, lats)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: tfb_client <host:port> <path> <duration_s> <connections>");
        std::process::exit(2);
    }
    let host_port = &args[1];
    let path: String = args[2].clone();
    let duration_s: u64 = args[3].parse().expect("duration_s");
    let connections: usize = args[4].parse().expect("connections");
    let addr: SocketAddr = host_port.parse().expect("host:port");
    let duration = Duration::from_secs(duration_s);

    let deadline = Instant::now() + duration;
    let total = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::with_capacity(connections);
    for _ in 0..connections {
        let total = Arc::clone(&total);
        let path = path.clone();
        tasks.push(tokio::spawn(async move {
            let (c, lats) = drive_connection(addr, path, deadline).await;
            total.fetch_add(c, Ordering::Relaxed);
            lats
        }));
    }
    let mut all_lats: Vec<u64> = Vec::new();
    for t in tasks {
        if let Ok(lats) = t.await {
            all_lats.extend(lats);
        }
    }
    let count = total.load(Ordering::Relaxed);
    let rps = count as f64 / duration.as_secs_f64();
    all_lats.sort_unstable();
    let mean = if all_lats.is_empty() {
        0
    } else {
        all_lats.iter().sum::<u64>() / all_lats.len() as u64
    };
    let p50 = if all_lats.is_empty() {
        0
    } else {
        all_lats[all_lats.len() / 2]
    };
    let p99 = if all_lats.is_empty() {
        0
    } else {
        all_lats[(all_lats.len() * 99) / 100]
    };
    println!(
        "{:<12}  requests={:>10}  RPS={:>10.0}  mean={:>5}us  p50={:>5}us  p99={:>5}us",
        host_port, count, rps, mean, p50, p99
    );
}
