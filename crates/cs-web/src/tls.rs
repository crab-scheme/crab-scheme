//! TLS support — [`TlsConfig`] + [`serve_tls`] for ALPN-negotiated
//! HTTP/1.1 and HTTP/2 over a single TCP listener. Behind the
//! `tls` feature.
//!
//! On every accepted connection we run the TLS handshake, look at
//! the negotiated ALPN protocol, and pick the matching hyper
//! builder:
//!
//! - `b"h2"` → `hyper::server::conn::http2::Builder` (with
//!   `TokioExecutor` so request futures spawn onto the current
//!   runtime).
//! - `b"http/1.1"` or no ALPN → `hyper::server::conn::http1::Builder`.
//!
//! The same [`Service`] runs underneath both protocols — h1 and
//! h2 share the request/response shapes.
//!
//! ## ALPN advertising
//!
//! [`TlsConfig::for_alpn`] builds a rustls server config with the
//! requested protocols pre-set. The first match between the
//! client's `ClientHello` ALPN list and our advertised list wins
//! per RFC 7301 §3.2. To support h3 alongside, set
//! `alpn_protocols` to `["h3", "h2", "http/1.1"]` and pass the
//! same config into [`crate::h3::serve_h3`].

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig as RustlsServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::{dispatch, ArcService, WebError};

/// TLS configuration. Holds a pre-built `Arc<rustls::ServerConfig>`
/// so the same handshake state can be shared with [`crate::h3`].
#[derive(Clone)]
pub struct TlsConfig {
    pub(crate) inner: Arc<RustlsServerConfig>,
    /// Advertised ALPN protocols, stored separately so callers
    /// (notably the h3 path) can inspect them without dropping
    /// down into rustls internals.
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl TlsConfig {
    /// Build from already-parsed certs + key. ALPN protocols are
    /// advertised in the supplied order; clients pick the first
    /// they support.
    pub fn for_alpn(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        alpn_protocols: Vec<Vec<u8>>,
    ) -> Result<Self, WebError> {
        // Make sure rustls has a crypto provider installed.
        // Idempotent — second call no-ops.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut cfg = RustlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| WebError::Tls(format!("server config: {e}")))?;
        cfg.alpn_protocols = alpn_protocols.clone();
        Ok(Self {
            inner: Arc::new(cfg),
            alpn_protocols,
        })
    }

    /// Convenience: load cert chain + private key from PEM files.
    /// Both files are read synchronously — call once at startup.
    pub fn from_pem_files(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
        alpn_protocols: Vec<Vec<u8>>,
    ) -> Result<Self, WebError> {
        let cert_bytes = std::fs::read(cert_path.as_ref())
            .map_err(|e| WebError::Tls(format!("read cert: {e}")))?;
        let key_bytes = std::fs::read(key_path.as_ref())
            .map_err(|e| WebError::Tls(format!("read key: {e}")))?;
        let certs = rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| WebError::Tls(format!("parse cert pem: {e}")))?;
        if certs.is_empty() {
            return Err(WebError::Tls("no certs in PEM".into()));
        }
        let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
            .map_err(|e| WebError::Tls(format!("parse key pem: {e}")))?
            .ok_or_else(|| WebError::Tls("no private key in PEM".into()))?;
        Self::for_alpn(certs, key, alpn_protocols)
    }

    /// Standard ALPN list for an HTTP/1.1 + HTTP/2 server.
    pub fn alpn_h1_h2() -> Vec<Vec<u8>> {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    }

    /// ALPN list including h3 — pair this with both
    /// [`serve_tls`] and [`crate::h3::serve_h3`] on the same
    /// (host, port) so clients can pick the best transport.
    pub fn alpn_h1_h2_h3() -> Vec<Vec<u8>> {
        vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()]
    }
}

/// Run an ALPN-negotiated h1 + h2 accept loop on `listener`.
///
/// Each accepted TCP connection runs the TLS handshake before
/// hyper sees it, so a slow handshake doesn't block other
/// connections (each handshake is its own tokio task).
///
/// `shutdown` is an optional future that, when it resolves,
/// breaks the accept loop. Returns the number of TCP connections
/// accepted (TLS handshake may have failed for some — those count
/// here but never reach the Service).
pub async fn serve_tls<F>(
    listener: TcpListener,
    tls: TlsConfig,
    service: ArcService,
    shutdown: Option<F>,
) -> Result<u64, WebError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let acceptor = TlsAcceptor::from(tls.inner.clone());
    let mut count: u64 = 0;
    let mut shutdown = shutdown.map(Box::pin);
    loop {
        let next = match shutdown.as_mut() {
            Some(s) => tokio::select! {
                _ = s => break,
                a = listener.accept() => a,
            },
            None => listener.accept().await,
        };
        let (stream, _peer) = match next {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cs-web tls: accept error: {e}");
                continue;
            }
        };
        count = count.wrapping_add(1);

        let svc = Arc::clone(&service);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cs-web tls: handshake error: {e}");
                    return;
                }
            };
            // Pick a protocol based on what ALPN negotiated.
            let alpn = tls_stream.get_ref().1.alpn_protocol().map(|p| p.to_vec());
            let io = TokioIo::new(tls_stream);

            let handler = service_fn(move |req: http::Request<hyper::body::Incoming>| {
                let svc = Arc::clone(&svc);
                async move {
                    let resp = dispatch(svc, req).await;
                    Ok::<_, std::convert::Infallible>(resp.map(Full::<Bytes>::new))
                }
            });

            let result = match alpn.as_deref() {
                Some(b"h2") => http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, handler)
                    .await
                    .map_err(|e| format!("h2: {e}")),
                _ => http1::Builder::new()
                    .serve_connection(io, handler)
                    .await
                    .map_err(|e| format!("h1: {e}")),
            };
            if let Err(err) = result {
                eprintln!("cs-web tls: connection error: {err}");
            }
        });
    }
    Ok(count)
}

/// Bind a fresh TCP listener and call [`serve_tls`]. The simple
/// path for single-protocol deployments.
pub async fn run_tls(
    addr: SocketAddr,
    tls: TlsConfig,
    service: ArcService,
) -> Result<(), WebError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| WebError::Bind { addr, source: e })?;
    serve_tls::<futures_util::future::Pending<()>>(listener, tls, service, None).await?;
    Ok(())
}
