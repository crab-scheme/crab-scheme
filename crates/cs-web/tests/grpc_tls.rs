//! End-to-end gRPC-over-mTLS tests (cw-u4a.21): a real
//! [`serve_grpc_tls`] server with a require-and-verify client-cert
//! verifier, driven by a real hyper HTTP/2-over-TLS client.
//!
//! Strategy: build a throwaway PKI in-process with rcgen — a CA, a
//! server leaf (SAN `IP:127.0.0.1` + `DNS:localhost`), and a client
//! leaf (CN/SAN `etcd-client`), all chaining to the CA. Then:
//!
//! 1. a client that presents its cert → the unary call succeeds and the
//!    handler sees the verified peer identity (`etcd-client`), proving
//!    both mTLS transport AND `GrpcRequest::peer_identity` extraction;
//! 2. a client that presents NO cert → rejected (the handshake / first
//!    request errors), proving require-and-verify.
//!
//! The end-to-end Scheme + `etcdctl` proof lives in crab-watchstore
//! (`test/etcd-mtls-grpc.sh`); this is the hermetic transport proof.

#![cfg(feature = "grpc-tls")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::{BufMut, Bytes, BytesMut};
use cs_web::grpc::{
    bind_grpc, frame_message, grpc_server_tls_config, serve_grpc_tls, ArcGrpcHandler, GrpcHandler,
    GrpcRequest, GrpcResponseSink,
};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

// ---------------------------------------------------------------
// In-process PKI: CA + server leaf + client leaf, all CA-signed.
// ---------------------------------------------------------------

struct Pki {
    /// Temp dir holding `ca.pem` / `server.crt` / `server.key` (the
    /// server config is built from PEM files, exactly like the Scheme
    /// `grpc-serve-tls` builtin).
    dir: PathBuf,
    /// The CA cert (client trusts it to verify the server).
    ca_der: CertificateDer<'static>,
    /// The client cert chain + key (presented for mTLS).
    client_chain: Vec<CertificateDer<'static>>,
    client_key: PrivateKeyDer<'static>,
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn gen_pki() -> Pki {
    // --- CA ---
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "crab-watchstore test CA");
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // --- server leaf: SAN IP 127.0.0.1 + DNS localhost, ServerAuth ---
    let mut sp = CertificateParams::new(Vec::new()).unwrap();
    sp.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
    ];
    sp.distinguished_name
        .push(DnType::CommonName, "crab-watchstore");
    sp.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = sp.signed_by(&server_key, &ca_cert, &ca_key).unwrap();

    // --- client leaf: CN + SAN DNS "etcd-client", ClientAuth ---
    let mut cp = CertificateParams::new(Vec::new()).unwrap();
    cp.subject_alt_names = vec![SanType::DnsName("etcd-client".try_into().unwrap())];
    cp.distinguished_name
        .push(DnType::CommonName, "etcd-client");
    cp.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_key = KeyPair::generate().unwrap();
    let client_cert = cp.signed_by(&client_key, &ca_cert, &ca_key).unwrap();

    // Write the server-side PEMs (grpc_server_tls_config reads files).
    let dir = std::env::temp_dir().join(format!(
        "cs-web-grpc-tls-{}-{}",
        std::process::id(),
        nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ca.pem"), ca_cert.pem()).unwrap();
    std::fs::write(dir.join("server.crt"), server_cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), server_key.serialize_pem()).unwrap();

    Pki {
        dir,
        ca_der: ca_cert.der().clone(),
        client_chain: vec![client_cert.der().clone()],
        client_key: PrivateKeyDer::try_from(client_key.serialize_der()).unwrap(),
    }
}

// ---------------------------------------------------------------
// A handler that replies with the verified peer identity. Proves
// both unary-over-mTLS AND identity extraction in one round trip.
// ---------------------------------------------------------------

struct IdHandler;

impl GrpcHandler for IdHandler {
    fn begin(&self, _call_id: u64, req: GrpcRequest, sink: GrpcResponseSink) {
        let id = req
            .peer_identity
            .as_deref()
            .unwrap_or("<no-client-cert>")
            .to_string();
        sink.send_message(Bytes::from(id));
        sink.close(0, None);
    }
    fn client_message(&self, _call_id: u64, _message: Bytes) {}
    fn client_end(&self, _call_id: u64) {}
}

async fn spawn_server(pki: &Pki, require_client_cert: bool) -> SocketAddr {
    let cfg = grpc_server_tls_config(
        pki.dir.join("server.crt"),
        pki.dir.join("server.key"),
        pki.dir.join("ca.pem"),
        require_client_cert,
    )
    .expect("build grpc tls config");
    let (listener, addr) = bind_grpc(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let handler: ArcGrpcHandler = Arc::new(IdHandler);
    tokio::spawn(async move {
        let _ = serve_grpc_tls::<std::future::Pending<()>>(listener, cfg, handler, None).await;
    });
    addr
}

// ---------------------------------------------------------------
// Client helpers.
// ---------------------------------------------------------------

fn grpc_request<B>(path: &str, body: B) -> http::Request<B> {
    http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(body)
        .unwrap()
}

/// Collect every response message + the final `grpc-status`.
async fn read_all(mut resp: Incoming) -> (Vec<Bytes>, Option<String>) {
    use hyper::body::Body as _;
    use std::pin::Pin;
    let mut buf = BytesMut::new();
    let mut msgs = Vec::new();
    let mut status = None;
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut resp).poll_frame(cx)).await;
        let Some(frame) = frame else { break };
        let frame = frame.expect("response frame");
        if let Some(data) = frame.data_ref() {
            buf.put_slice(data);
            while buf.len() >= 5 {
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if buf.len() < 5 + len {
                    break;
                }
                let _ = buf.split_to(5);
                msgs.push(buf.split_to(len).freeze());
            }
        } else if let Some(trailers) = frame.trailers_ref() {
            if let Some(s) = trailers.get("grpc-status") {
                status = Some(s.to_str().unwrap().to_string());
            }
        }
    }
    (msgs, status)
}

/// Build a rustls client config that trusts the CA and (optionally)
/// presents the client cert.
fn client_config(pki: &Pki, present_cert: bool) -> ClientConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = RootCertStore::empty();
    roots.add(pki.ca_der.clone()).unwrap();
    let mut cfg = if present_cert {
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(pki.client_chain.clone(), pki.client_key.clone_key())
            .unwrap()
    } else {
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    cfg
}

async fn connect_h2(
    addr: SocketAddr,
    cfg: ClientConfig,
) -> Result<http2::SendRequest<Full<Bytes>>, String> {
    let connector = TlsConnector::from(Arc::new(cfg));
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("tcp: {e}"))?;
    let server_name = ServerName::try_from("localhost").unwrap();
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("handshake: {e}"))?;
    let io = TokioIo::new(tls);
    let (sender, conn) = http2::handshake(TokioExecutor::new(), io)
        .await
        .map_err(|e| format!("h2: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(sender)
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[tokio::test]
async fn mtls_unary_succeeds_and_exposes_peer_identity() {
    let pki = gen_pki();
    let addr = spawn_server(&pki, true).await;
    let mut sender = connect_h2(addr, client_config(&pki, true))
        .await
        .expect("mTLS handshake with client cert should succeed");
    let body = Full::new(frame_message(b"ping"));
    let resp = timeout(
        Duration::from_secs(5),
        sender.send_request(grpc_request("/etcdserverpb.KV/Range", body)),
    )
    .await
    .expect("not stuck")
    .expect("send");
    assert_eq!(resp.status(), 200);
    let (msgs, status) = read_all(resp.into_body()).await;
    assert_eq!(status.as_deref(), Some("0"));
    assert_eq!(msgs.len(), 1);
    // The handler echoed the verified peer identity — the client cert's
    // SAN/CN, NOT "<no-client-cert>".
    assert_eq!(
        String::from_utf8_lossy(&msgs[0]),
        "etcd-client",
        "server should see the verified client identity"
    );
}

#[tokio::test]
async fn no_client_cert_is_rejected() {
    let pki = gen_pki();
    let addr = spawn_server(&pki, true).await;
    // A client that trusts the server CA but presents NO client cert.
    // require-and-verify must reject it: either the handshake fails, or
    // the first request fails when the server aborts with a TLS alert.
    let outcome = timeout(Duration::from_secs(5), async {
        let mut sender = connect_h2(addr, client_config(&pki, false)).await?;
        let body = Full::new(frame_message(b"ping"));
        let resp = sender
            .send_request(grpc_request("/etcdserverpb.KV/Range", body))
            .await
            .map_err(|e| format!("send: {e}"))?;
        Ok::<u16, String>(resp.status().as_u16())
    })
    .await;
    match outcome {
        // Timed out waiting (server hung up mid-handshake) — a rejection.
        Err(_elapsed) => {}
        // Errored at handshake / h2 / send — the expected rejection.
        Ok(Err(_e)) => {}
        // Got an HTTP response — require-and-verify FAILED to reject.
        Ok(Ok(status)) => panic!("no-client-cert connection was NOT rejected (HTTP {status})"),
    }
}

#[tokio::test]
async fn plain_tls_no_client_auth_has_no_identity() {
    // require_client_cert = false: encrypted, but no cert requested, so
    // the handler sees no peer identity.
    let pki = gen_pki();
    let addr = spawn_server(&pki, false).await;
    let mut sender = connect_h2(addr, client_config(&pki, false))
        .await
        .expect("plain server-TLS handshake should succeed");
    let body = Full::new(frame_message(b"ping"));
    let resp = timeout(
        Duration::from_secs(5),
        sender.send_request(grpc_request("/etcdserverpb.KV/Range", body)),
    )
    .await
    .expect("not stuck")
    .expect("send");
    let (msgs, status) = read_all(resp.into_body()).await;
    assert_eq!(status.as_deref(), Some("0"));
    assert_eq!(msgs.len(), 1);
    assert_eq!(String::from_utf8_lossy(&msgs[0]), "<no-client-cert>");
}
