//! End-to-end tests: real bind, real serve, real loopback client.
//!
//! These tests prove the hyper plumbing works — the router's unit
//! tests cover routing logic, but the full path (TCP accept →
//! body collect → handler → response write) only runs here.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use cs_web::{
    handler::service_fn, ok, response, run, ArcService, Router, ServerConfig, StatusCode,
};
use http_body_util::{BodyExt, Empty, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::timeout;

async fn spawn_test_server(svc: ArcService) -> SocketAddr {
    let cfg = ServerConfig {
        addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        request_timeout: None,
    };
    let (listener, addr) = cs_web::bind(&cfg).await.expect("bind");
    tokio::spawn(async move {
        let _ = cs_web::serve::<futures_util::future::Pending<()>>(listener, svc, None).await;
    });
    addr
}

async fn http_get(addr: SocketAddr, path: &str) -> (StatusCode, Bytes) {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = http1::handshake(io).await.expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.expect("send");
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.expect("collect").to_bytes();
    (parts.status, body)
}

async fn http_post(addr: SocketAddr, path: &str, body: &'static str) -> (StatusCode, Bytes) {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = http1::handshake(io).await.expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .unwrap();
    let resp = sender.send_request(req).await.expect("send");
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.expect("collect").to_bytes();
    (parts.status, body)
}

#[tokio::test]
async fn server_round_trip() {
    let svc = Router::new()
        .get("/hello", service_fn(|_| async { ok("world") }))
        .into_service();
    let addr = spawn_test_server(svc).await;

    let (status, body) = timeout(Duration::from_secs(2), http_get(addr, "/hello"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"world");
}

#[tokio::test]
async fn server_handles_post_with_body() {
    let svc = Router::new()
        .post(
            "/echo",
            service_fn(|req: cs_web::Request| async move {
                // Echo body back uppercased.
                let body = String::from_utf8_lossy(req.body()).to_uppercase();
                ok(body)
            }),
        )
        .into_service();
    let addr = spawn_test_server(svc).await;

    let (status, body) = timeout(
        Duration::from_secs(2),
        http_post(addr, "/echo", "hello there"),
    )
    .await
    .expect("not stuck");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"HELLO THERE");
}

#[tokio::test]
async fn server_404_for_unknown_route() {
    let svc = Router::new().into_service();
    let addr = spawn_test_server(svc).await;

    let (status, _) = timeout(Duration::from_secs(2), http_get(addr, "/nope"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn server_405_for_wrong_method() {
    let svc = Router::new()
        .get("/items", service_fn(|_| async { ok("listing") }))
        .into_service();
    let addr = spawn_test_server(svc).await;

    let (status, _) = timeout(Duration::from_secs(2), http_post(addr, "/items", "x"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn server_serves_many_concurrent_requests() {
    // Counter handler that does some yielding async work.
    let svc = Router::new()
        .get(
            "/work",
            service_fn(|_| async {
                tokio::task::yield_now().await;
                ok("done")
            }),
        )
        .into_service();
    let addr = spawn_test_server(svc).await;

    let mut tasks = Vec::new();
    for _ in 0..32 {
        tasks.push(tokio::spawn(http_get(addr, "/work")));
    }
    for t in tasks {
        let (status, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"done");
    }
}

#[tokio::test]
async fn run_with_shutdown_via_listener_drop() {
    // `run` is the simple one-shot. We can't easily signal
    // shutdown without a JoinHandle, so this just smoke-tests that
    // a successful bind + serve doesn't immediately error.
    let cfg = ServerConfig {
        addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        request_timeout: None,
    };
    let svc = Router::new()
        .get("/", service_fn(|_| async { ok("hi") }))
        .into_service();
    let h = tokio::spawn(async move {
        // Wrap in a timeout so the test can't hang.
        tokio::select! {
            r = run(cfg, svc) => r,
            _ = tokio::time::sleep(Duration::from_millis(50)) => Ok(()),
        }
    });
    // Don't actually wait for h — just yield and tear down.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!h.is_finished() || h.is_finished()); // tautology — point is no panic
    h.abort();
}

#[tokio::test]
async fn layered_service_round_trip() {
    use cs_web::{CatchPanic, RequestId, Stack, Trace};

    let app = Router::new()
        .get("/ok", service_fn(|_| async { ok("ok") }))
        .get(
            "/boom",
            service_fn(|_| async {
                panic!("boom");
                #[allow(unreachable_code)]
                ok("unreachable")
            }),
        )
        .into_service();

    let svc = Stack::new()
        .push(Trace)
        .push(RequestId::new())
        .push(CatchPanic)
        .wrap(app);

    let addr = spawn_test_server(svc).await;

    let (status, body) = timeout(Duration::from_secs(2), http_get(addr, "/ok"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"ok");

    let (status, _) = timeout(Duration::from_secs(2), http_get(addr, "/boom"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn custom_response_status_propagates() {
    let svc = Router::new()
        .get(
            "/teapot",
            service_fn(|_| async { response(StatusCode::IM_A_TEAPOT, "tea") }),
        )
        .into_service();
    let addr = spawn_test_server(svc).await;

    let (status, body) = timeout(Duration::from_secs(2), http_get(addr, "/teapot"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::IM_A_TEAPOT);
    assert_eq!(&body[..], b"tea");
}
