//! End-to-end test for the cdylib module loader.
//!
//! Builds `cs-web-mod-example` via `cargo build`, then dlopens
//! the resulting library, drains its routes into a Router, and
//! hits each route through the real hyper server. Proves the
//! full cycle: build → dlopen → register → serve.

#![cfg(feature = "modules")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use bytes::Bytes;
use cs_web::{ArcService, Module, RouteSink, Router, ServerConfig, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Find the example cdylib in the workspace target directory.
/// Builds it first if missing.
fn build_and_locate_fixture() -> PathBuf {
    // Build the fixture. Idempotent if up-to-date.
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "cs-web-mod-example", "--quiet"])
        .status()
        .expect("invoke cargo");
    assert!(status.success(), "build cs-web-mod-example");

    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    };
    // CARGO_TARGET_DIR is honored; default is workspace target/.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            manifest
                .parent() // crates/
                .and_then(|p| p.parent()) // workspace root
                .expect("locate workspace root")
                .join("target")
        });
    let name = if cfg!(windows) {
        format!("cs_web_mod_example.{ext}")
    } else {
        format!("libcs_web_mod_example.{ext}")
    };
    target_dir.join("debug").join(name)
}

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
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .unwrap();
    let resp = sender.send_request(req).await.expect("send");
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.expect("collect").to_bytes();
    (parts.status, body)
}

#[tokio::test]
async fn cdylib_plugin_registers_and_serves() {
    let path = build_and_locate_fixture();
    assert!(
        path.exists(),
        "fixture not found at {} — did cargo build succeed?",
        path.display()
    );

    // Load. The `Module` holds the Library alive for the
    // remainder of this test.
    let module = unsafe { Module::load(&path) }.expect("load plugin");

    let mut sink = RouteSink::new();
    module.register_into(&mut sink);
    assert_eq!(
        sink.len(),
        2,
        "plugin registered an unexpected number of routes"
    );

    // Module is held by binding it to the future, keeping the
    // dylib mapped for the lifetime of the served routes.
    let _module_guard = module;
    let svc = Router::new().add_sink(sink).into_service();
    let addr = spawn_test_server(svc).await;

    let (status, body) = timeout(Duration::from_secs(2), http_get(addr, "/plugin/hello"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"hello from cs-web-mod-example");

    let (status, body) = timeout(
        Duration::from_secs(2),
        http_post(addr, "/plugin/upper", "abcdef"),
    )
    .await
    .expect("not stuck");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"ABCDEF");
}

#[tokio::test]
async fn missing_module_yields_error() {
    let res = unsafe { Module::load("/definitely/not/here.dylib") };
    assert!(res.is_err(), "load should fail for missing file");
}

#[tokio::test]
async fn dropping_module_still_lets_us_observe_dispatch_via_held_handle() {
    // Regression guard: a module dropped before its routes are
    // called would leave dangling fn pointers. We exercise the
    // ordering by holding the Module for the lifetime of the
    // service.
    let path = build_and_locate_fixture();
    let module = unsafe { Module::load(&path) }.expect("load plugin");
    let mut sink = RouteSink::new();
    module.register_into(&mut sink);

    // Keep `module` alive while we use the service.
    let svc = Router::new().add_sink(sink).into_service();
    let addr = spawn_test_server(svc).await;
    let (status, _) = timeout(Duration::from_secs(2), http_get(addr, "/plugin/hello"))
        .await
        .expect("not stuck");
    assert_eq!(status, StatusCode::OK);

    // Module drops at end of scope — by which point the service
    // is also being torn down with the test task.
    drop(module);
}
