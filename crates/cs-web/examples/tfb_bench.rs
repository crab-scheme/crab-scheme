//! TFB-style benchmarks for cs-web.
//!
//! Modeled on the TechEmpower Framework Benchmarks Round 23
//! plaintext + JSON tests (https://www.techempower.com/benchmarks/#section=data-r23):
//!
//! - **Plaintext** — return `"Hello, World!"` with `Content-Type:
//!   text/plain`. TFB runs this with 16 threads × 256 connections,
//!   8-deep HTTP/1.1 pipelining, 15 s window. Top frameworks
//!   peak around 5–7 M RPS on dedicated hardware; we run on
//!   the dev host's CPU/loopback so absolute numbers will be
//!   lower but the relative shape (plain vs layered vs actor)
//!   tells the same story.
//!
//! - **JSON** — return `{"message":"Hello, World!"}` with
//!   `Content-Type: application/json`. TFB tunes this for
//!   serialization throughput; cs-web returns a pre-encoded
//!   `Bytes` for the same effect.
//!
//! Five route flavors:
//!
//!   /plain        Rust static route, no layers
//!   /plain-l2     Rust static behind request-id + timeout layers
//!                 (Trace omitted — its stderr writes dominate
//!                 wall-clock on loopback)
//!   /plain-al     Rust static behind a passthrough ActorLayer
//!                 (cs-actor mailbox hop on every request)
//!   /actor-plain  cs-actor handler responds with "Hello, World!"
//!   /json         Static JSON
//!
//! The harness runs each scenario for a fixed window with a
//! configurable connection count. Output is one row per
//! scenario: RPS, mean latency, p50, p99.
//!
//! Usage:
//!
//!   cargo run --release --example tfb_bench -- [duration_s] [connections]
//!
//! Defaults: 5 s window, 64 connections.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cs_web::handler::service_fn;
use cs_web::{ok, response, ArcService, Router, ServerConfig, StatusCode};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1 as client_h1;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

/// One bench scenario.
struct Scenario {
    name: &'static str,
    path: &'static str,
    service: ArcService,
}

fn build_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();

    // 1. plain: simple Rust static
    out.push(Scenario {
        name: "plain",
        path: "/plain",
        service: Router::new()
            .get(
                "/plain",
                service_fn(|_| async { plaintext("Hello, World!") }),
            )
            .into_service(),
    });

    // 2. plain-l2: same handler behind request-id + timeout
    //    (Trace writes to stderr — its cost on loopback dwarfs
    //    the layer's own ~ns of work, so it would tell us about
    //    println! throughput, not Layer overhead).
    let inner = Router::new()
        .get(
            "/plain-l2",
            service_fn(|_| async { plaintext("Hello, World!") }),
        )
        .into_service();
    let stack = cs_web::Stack::new()
        .push(cs_web::RequestId::new())
        .push(cs_web::Timeout::new(Duration::from_secs(5)));
    let service = stack.wrap(inner);
    out.push(Scenario {
        name: "plain-l2",
        path: "/plain-l2",
        service,
    });

    // 3. plain-al: same handler behind a passthrough ActorLayer.
    //    Measures the cs-actor mailbox round-trip cost per
    //    request (envelope → actor → web-continue → inner).
    let inner = Router::new()
        .get(
            "/plain-al",
            service_fn(|_| async { plaintext("Hello, World!") }),
        )
        .into_service();
    let actor_system = cs_actor::ActorSystem::new();
    let layer_actor = cs_actor::ActorSystem::spawn_async(&actor_system, |mut a| async move {
        while let Some(msg) = a.receive_async().await {
            let cs_actor::Message::User(payload) = msg else {
                break;
            };
            let Ok(envelope) = payload.downcast::<cs_web::actor::WebMessage>() else {
                continue;
            };
            // Always continue (passthrough layer).
            envelope.signal_continue();
        }
    });
    let layer = cs_web::actor::actor_layer(layer_actor, Duration::from_secs(5));
    let service = cs_web::Stack::new().push(layer).wrap(inner);
    out.push(Scenario {
        name: "plain-al",
        path: "/plain-al",
        service,
    });
    // Keep the actor system alive for the bench duration.
    Box::leak(Box::new(actor_system));

    // 4. actor-plain: cs-actor handler responds directly via
    //    spawn_handler_actor — measures the same mailbox cost
    //    as plain-al but on the handler side instead of layer.
    let actor_system2 = cs_actor::ActorSystem::new();
    let handler_actor = cs_web::actor::spawn_handler_actor(&actor_system2, |_r| async move {
        plaintext("Hello, World!")
    });
    let handler_svc =
        cs_web::actor::ActorHandler::new(handler_actor, Duration::from_secs(5)).into_service();
    let service = Router::new()
        .get("/actor-plain", handler_svc)
        .into_service();
    out.push(Scenario {
        name: "actor-plain",
        path: "/actor-plain",
        service,
    });
    Box::leak(Box::new(actor_system2));

    // 5. json: static JSON
    out.push(Scenario {
        name: "json",
        path: "/json",
        service: Router::new()
            .get(
                "/json",
                service_fn(|_| async {
                    let mut r =
                        http::Response::new(Bytes::from_static(br#"{"message":"Hello, World!"}"#));
                    r.headers_mut().insert(
                        "content-type",
                        http::HeaderValue::from_static("application/json"),
                    );
                    r
                }),
            )
            .into_service(),
    });

    out
}

fn plaintext(body: &'static str) -> cs_web::Response {
    let mut r = response(StatusCode::OK, body);
    r.headers_mut()
        .insert("content-type", http::HeaderValue::from_static("text/plain"));
    r
}

async fn spawn_server(service: ArcService) -> SocketAddr {
    let cfg = ServerConfig {
        addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        request_timeout: None,
    };
    let (listener, addr) = cs_web::bind(&cfg).await.expect("bind");
    tokio::spawn(async move {
        let _ = cs_web::serve::<futures_util::future::Pending<()>>(listener, service, None).await;
    });
    addr
}

/// Drive one connection in a tight loop. Returns
/// (request_count, latencies_micros) for the window.
async fn drive_connection(
    addr: SocketAddr,
    path: &'static str,
    deadline: Instant,
) -> (u64, Vec<u64>) {
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
    // Pre-size the latencies vec so we don't reallocate during
    // measurement (~1M slots at 8 B = 8 MB, fine).
    let mut lats: Vec<u64> = Vec::with_capacity(1_000_000);
    while Instant::now() < deadline {
        let start = Instant::now();
        let req = http::Request::builder()
            .uri(path)
            .header("host", "localhost")
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

async fn run_scenario(
    scen: &Scenario,
    duration: Duration,
    connections: usize,
) -> (u64, f64, u64, u64) {
    let addr = spawn_server(Arc::clone(&scen.service)).await;
    // 50ms settle.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let deadline = Instant::now() + duration;
    let total = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::with_capacity(connections);
    for _ in 0..connections {
        let total = Arc::clone(&total);
        let path = scen.path;
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
    let mean = if all_lats.is_empty() {
        0
    } else {
        (all_lats.iter().sum::<u64>() / all_lats.len() as u64) as u64
    };
    all_lats.sort_unstable();
    let p99 = if all_lats.is_empty() {
        0
    } else {
        all_lats[(all_lats.len() * 99) / 100]
    };
    (count, rps, mean, p99)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let duration_s: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(5);
    let connections: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(64);
    let duration = Duration::from_secs(duration_s);

    println!(
        "cs-web tfb-style bench  |  {} s window  |  {} connections  |  hyper http1 keep-alive (no pipelining)",
        duration_s, connections
    );
    println!(
        "{:<14}  {:>10}  {:>10}  {:>10}  {:>10}",
        "scenario", "requests", "RPS", "mean us", "p99 us"
    );
    println!("{}", "-".repeat(64));

    for scen in build_scenarios() {
        let (count, rps, mean, p99) = run_scenario(&scen, duration, connections).await;
        println!(
            "{:<14}  {:>10}  {:>10.0}  {:>10}  {:>10}",
            scen.name, count, rps, mean, p99
        );
    }
}
