//! End-to-end HTTP/3 over QUIC test using h3-quinn on the client
//! side and `cs_web::serve_h3` on the server side. Generates a
//! self-signed cert, advertises `h3` ALPN, drives a real
//! request over QUIC.

#![cfg(feature = "http3")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use cs_web::{handler::service_fn, ok, Router, TlsConfig};
use rcgen::CertifiedKey;
use rustls::pki_types::CertificateDer;
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

fn client_endpoint(trust: CertificateDer<'static>) -> quinn::Endpoint {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(trust).expect("trust cert");
    let mut client_crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![b"h3".to_vec()];
    let quic_client_cfg = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
        .expect("quic client config");
    let client_config = quinn::ClientConfig::new(Arc::new(quic_client_cfg));
    let mut endpoint =
        quinn::Endpoint::client(SocketAddr::from(([127, 0, 0, 1], 0))).expect("client endpoint");
    endpoint.set_default_client_config(client_config);
    endpoint
}

async fn h3_get(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    path: &str,
) -> (http::StatusCode, Bytes) {
    let conn = endpoint
        .connect(addr, "localhost")
        .expect("connect")
        .await
        .expect("handshake");
    let h3_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut send_req) = h3::client::new(h3_conn).await.expect("h3 client");
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let req = http::Request::builder()
        .method("GET")
        .uri(format!("https://localhost{path}"))
        .body(())
        .unwrap();
    let mut stream = send_req.send_request(req).await.expect("send_request");
    stream.finish().await.expect("finish");
    let resp = stream.recv_response().await.expect("recv_response");
    let status = resp.status();
    let mut body = BytesMut::new();
    while let Some(mut chunk) = stream.recv_data().await.expect("recv body") {
        while chunk.has_remaining() {
            let seg = chunk.chunk();
            body.extend_from_slice(seg);
            let n = seg.len();
            chunk.advance(n);
        }
    }
    (status, body.freeze())
}

/// Pick a known-free UDP port. The probe binds + drops; the
/// returned addr can race with another process for that port,
/// but on a test runner that's negligible.
fn pick_udp_port() -> SocketAddr {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe");
    let addr = probe.local_addr().expect("addr");
    drop(probe);
    addr
}

#[tokio::test]
async fn h3_round_trip_via_serve_h3() {
    install_crypto();
    let (certs, key) = self_signed();
    let trust = certs[0].clone();
    let tls = TlsConfig::for_alpn(certs, key, vec![b"h3".to_vec()]).expect("tls");
    let svc = Router::new()
        .get("/q", service_fn(|_| async { ok("hello-over-quic") }))
        .into_service();
    let addr = pick_udp_port();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = cs_web::serve_h3(
            addr,
            tls,
            svc,
            Some(async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    let client = client_endpoint(trust);
    let (status, body) = timeout(Duration::from_secs(3), h3_get(&client, addr, "/q"))
        .await
        .expect("not stuck");
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(&body[..], b"hello-over-quic");

    client.close(0u32.into(), b"done");
    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn h3_404_for_unknown_route() {
    install_crypto();
    let (certs, key) = self_signed();
    let trust = certs[0].clone();
    let tls = TlsConfig::for_alpn(certs, key, vec![b"h3".to_vec()]).expect("tls");
    let svc = Router::new()
        .get("/known", service_fn(|_| async { ok("ok") }))
        .into_service();
    let addr = pick_udp_port();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = cs_web::serve_h3(
            addr,
            tls,
            svc,
            Some(async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    let client = client_endpoint(trust);
    let (status, _) = timeout(Duration::from_secs(3), h3_get(&client, addr, "/missing"))
        .await
        .expect("not stuck");
    assert_eq!(status, http::StatusCode::NOT_FOUND);

    client.close(0u32.into(), b"done");
    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn serve_h3_rejects_config_without_h3_alpn() {
    install_crypto();
    let (certs, key) = self_signed();
    let tls = TlsConfig::for_alpn(certs, key, TlsConfig::alpn_h1_h2()).expect("tls");
    let svc = Router::new().into_service();
    let res = cs_web::serve_h3::<futures_util::future::Pending<()>>(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        tls,
        svc,
        None,
    )
    .await;
    assert!(res.is_err(), "should reject configs without h3 ALPN");
}
