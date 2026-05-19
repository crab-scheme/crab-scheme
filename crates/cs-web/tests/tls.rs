//! End-to-end TLS tests: h1 and h2 over a TLS-terminated socket
//! with ALPN-negotiated protocol selection.
//!
//! Strategy: generate a self-signed cert in-process via rcgen,
//! advertise both ALPN tokens on the server, run two clients (one
//! offering `http/1.1`, one offering `h2`), and assert that each
//! round trip uses the negotiated protocol.

#![cfg(feature = "tls")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cs_web::{handler::service_fn, ok, ArcService, Router, TlsConfig};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::{http1 as client_h1, http2 as client_h2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rcgen::CertifiedKey;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::TcpStream;
use tokio::time::timeout;

fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn self_signed() -> (
    Vec<CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("rcgen");
    let cert_der: CertificateDer<'static> = cert.der().clone();
    let key_pem = key_pair.serialize_pem();
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .expect("parse key pem")
        .expect("key present");
    (vec![cert_der], key)
}

async fn spawn_tls_server(tls: TlsConfig, svc: ArcService) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ =
            cs_web::serve_tls::<futures_util::future::Pending<()>>(listener, tls, svc, None).await;
    });
    addr
}

async fn tls_connect(
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    alpn: &[&[u8]],
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("trust cert");
    let mut client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
    let tcp = TcpStream::connect(addr).await.expect("tcp connect");
    let server_name = ServerName::try_from("localhost").expect("server name");
    connector
        .connect(server_name, tcp)
        .await
        .expect("tls handshake")
}

#[tokio::test]
async fn h1_over_tls_round_trip() {
    install_crypto();
    let (certs, key) = self_signed();
    let trust_anchor = certs[0].clone();
    let tls = TlsConfig::for_alpn(certs, key, TlsConfig::alpn_h1_h2()).expect("tls config");

    let svc = Router::new()
        .get("/hi", service_fn(|_| async { ok("h1-greetings") }))
        .into_service();
    let addr = spawn_tls_server(tls, svc).await;

    let stream = tls_connect(addr, trust_anchor, &[b"http/1.1"]).await;
    // Confirm we negotiated h1 — TLS half is what does it.
    assert_eq!(
        stream.get_ref().1.alpn_protocol().unwrap_or_default(),
        b"http/1.1"
    );

    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_h1::handshake(io).await.expect("client h1");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = http::Request::builder()
        .uri("/hi")
        .header("host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = timeout(Duration::from_secs(2), sender.send_request(req))
        .await
        .expect("not stuck")
        .expect("send");
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.expect("body").to_bytes();
    assert_eq!(parts.status, hyper::StatusCode::OK);
    assert_eq!(&body[..], b"h1-greetings");
}

#[tokio::test]
async fn h2_over_tls_round_trip() {
    install_crypto();
    let (certs, key) = self_signed();
    let trust_anchor = certs[0].clone();
    let tls = TlsConfig::for_alpn(certs, key, TlsConfig::alpn_h1_h2()).expect("tls config");

    let svc = Router::new()
        .get("/hi", service_fn(|_| async { ok("h2-greetings") }))
        .into_service();
    let addr = spawn_tls_server(tls, svc).await;

    let stream = tls_connect(addr, trust_anchor, &[b"h2"]).await;
    assert_eq!(
        stream.get_ref().1.alpn_protocol().unwrap_or_default(),
        b"h2"
    );

    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_h2::handshake(TokioExecutor::new(), io)
        .await
        .expect("client h2");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = http::Request::builder()
        .uri("/hi")
        .header("host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = timeout(Duration::from_secs(2), sender.send_request(req))
        .await
        .expect("not stuck")
        .expect("send");
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.expect("body").to_bytes();
    assert_eq!(parts.status, hyper::StatusCode::OK);
    assert_eq!(&body[..], b"h2-greetings");
}

#[tokio::test]
async fn alpn_picks_h2_when_both_offered() {
    install_crypto();
    let (certs, key) = self_signed();
    let trust_anchor = certs[0].clone();
    let tls = TlsConfig::for_alpn(certs, key, TlsConfig::alpn_h1_h2()).expect("tls config");

    let svc = Router::new()
        .get("/x", service_fn(|_| async { ok("ok") }))
        .into_service();
    let addr = spawn_tls_server(tls, svc).await;

    // Client offers both — server's preferred order
    // [h2, http/1.1] wins, so we end up on h2.
    let stream = tls_connect(addr, trust_anchor, &[b"h2", b"http/1.1"]).await;
    assert_eq!(
        stream.get_ref().1.alpn_protocol().unwrap_or_default(),
        b"h2"
    );
}
