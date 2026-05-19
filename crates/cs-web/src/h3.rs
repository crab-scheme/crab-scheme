//! HTTP/3 over QUIC — [`serve_h3`].
//!
//! Builds a [`quinn::Endpoint`] from a [`TlsConfig`] (which MUST
//! advertise `h3` in its ALPN list — QUIC negotiates protocol at
//! the TLS layer), accepts QUIC connections, drives an h3 server
//! connection per QUIC connection, and shuttles each h3 request
//! through the same [`Service`](crate::Service) trait the
//! HTTP/1.1 + HTTP/2 paths use.
//!
//! ## Composition with h1/h2
//!
//! Run [`crate::serve_tls`] on a TCP listener and [`serve_h3`] on
//! a UDP listener bound to the same `(host, port)`. They share
//! the same [`TlsConfig`]. A typical Alt-Svc-aware client first
//! reaches the server over h1/h2, sees an `alt-svc: h3=":443"`
//! header, and upgrades to h3 on the next request. cs-web does
//! not emit the Alt-Svc header automatically — set it in the
//! handler if your deployment wants browser upgrades.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::{Buf, Bytes, BytesMut};
use quinn::Endpoint;

use crate::{ArcService, TlsConfig, WebError};

/// Run the QUIC accept loop on `addr`. Each accepted QUIC
/// connection spawns an h3 connection task that processes
/// requests serially within that connection (per-stream
/// concurrency is implicit — h3 streams are independent at the
/// QUIC layer).
///
/// `shutdown` is an optional future that, when it resolves,
/// closes the endpoint and breaks out. Returns the count of
/// QUIC connections accepted.
pub async fn serve_h3<F>(
    addr: SocketAddr,
    tls: TlsConfig,
    service: ArcService,
    shutdown: Option<F>,
) -> Result<u64, WebError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if !tls.alpn_protocols.iter().any(|p| p == b"h3") {
        return Err(WebError::Http3(
            "TlsConfig.alpn_protocols must include `h3` for serve_h3".into(),
        ));
    }

    // Wrap the shared rustls::ServerConfig as quinn's crypto.
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls.inner.as_ref().clone())
        .map_err(|e| WebError::Http3(format!("quic crypto: {e}")))?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));

    let endpoint = Endpoint::server(server_config, addr)
        .map_err(|e| WebError::Http3(format!("endpoint bind {addr}: {e}")))?;
    let endpoint_for_shutdown = endpoint.clone();

    let mut shutdown = shutdown.map(Box::pin);
    let mut count: u64 = 0;
    loop {
        let next = match shutdown.as_mut() {
            Some(s) => tokio::select! {
                _ = s => {
                    endpoint_for_shutdown.close(0u32.into(), b"shutdown");
                    break;
                }
                a = endpoint.accept() => a,
            },
            None => endpoint.accept().await,
        };
        let Some(incoming) = next else { break };

        count = count.wrapping_add(1);
        let svc = Arc::clone(&service);
        tokio::spawn(async move {
            let quic_conn = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cs-web h3: quic handshake: {e}");
                    return;
                }
            };
            let h3_conn = h3::server::Connection::new(h3_quinn::Connection::new(quic_conn)).await;
            let mut h3_conn = match h3_conn {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cs-web h3: h3 setup: {e}");
                    return;
                }
            };
            loop {
                match h3_conn.accept().await {
                    Ok(Some(resolver)) => {
                        let svc = Arc::clone(&svc);
                        tokio::spawn(async move {
                            if let Err(e) = handle_request(svc, resolver).await {
                                eprintln!("cs-web h3: request error: {e}");
                            }
                        });
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("cs-web h3: accept error: {e}");
                        break;
                    }
                }
            }
        });
    }
    Ok(count)
}

/// One h3 request. Resolves the headers, drains the body, calls
/// the service, ships the response over the bidirectional
/// stream, finishes.
async fn handle_request<C>(
    svc: ArcService,
    resolver: h3::server::RequestResolver<C, Bytes>,
) -> Result<(), String>
where
    C: h3::quic::Connection<Bytes>,
    <C as h3::quic::OpenStreams<Bytes>>::BidiStream: h3::quic::SendStream<Bytes>,
{
    let (req, mut stream) = resolver
        .resolve_request()
        .await
        .map_err(|e| format!("resolve: {e}"))?;
    let (parts, _) = req.into_parts();

    // Drain the body. h3 streams body data through `recv_data`
    // until None.
    let mut body = BytesMut::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|e| format!("recv body: {e}"))?
    {
        // `chunk: impl Buf` — copy bytes out one segment at a time.
        while chunk.has_remaining() {
            let seg = chunk.chunk();
            body.extend_from_slice(seg);
            let n = seg.len();
            chunk.advance(n);
        }
    }
    let cs_req: crate::Request = http::Request::from_parts(parts, body.freeze());

    // Service does the work.
    let resp = svc.call(cs_req).await;
    let (resp_parts, resp_body) = resp.into_parts();
    let headers_only = http::Response::from_parts(resp_parts, ());

    stream
        .send_response(headers_only)
        .await
        .map_err(|e| format!("send_response: {e}"))?;
    if !resp_body.is_empty() {
        stream
            .send_data(resp_body)
            .await
            .map_err(|e| format!("send_data: {e}"))?;
    }
    stream.finish().await.map_err(|e| format!("finish: {e}"))?;
    Ok(())
}

/// One-shot helper: bind + serve until the endpoint is closed.
pub async fn run_h3(addr: SocketAddr, tls: TlsConfig, service: ArcService) -> Result<(), WebError> {
    serve_h3::<futures_util::future::Pending<()>>(addr, tls, service, None).await?;
    Ok(())
}
