//! Cleartext HTTP/2 (h2c) gRPC server transport. Behind the
//! `grpc` feature.
//!
//! This is the framing layer the etcd v3 services sit on. It owns
//! exactly three concerns and nothing above them:
//!
//! 1. **h2c transport** — accept TCP, serve each connection with
//!    `hyper::server::conn::http2::Builder` in *prior-knowledge*
//!    mode (no TLS, no ALPN — the client knows it's h2 up front).
//!    TLS/mTLS is a separate concern (`.21`); etcd permits insecure
//!    transport, so an h2c listener is a complete unary substrate.
//! 2. **gRPC framing** — every gRPC message on the wire is
//!    `compressed-flag(1) ‖ length(4, big-endian) ‖ message`. We
//!    de-frame the request body to recover the protobuf bytes and
//!    re-frame the handler's response the same way.
//! 3. **Trailers** — gRPC carries its status *out of band* in
//!    HTTP/2 trailers (`grpc-status`, optionally `grpc-message`),
//!    NOT in the HTTP status line (which is always 200 once headers
//!    are sent). cs-web's HTTP path never exposed trailers; this is
//!    the capability that unlocks gRPC. We thread them through a
//!    bespoke [`GrpcBody`] whose final `poll_frame` yields a
//!    `Frame::trailers`.
//!
//! ## What stays out of this module
//!
//! gRPC *semantics* (which method maps to which etcd RPC, protobuf
//! encode/decode, leader redirects) live in Scheme. This module
//! hands the handler a [`GrpcRequest`] (`:path` + de-framed message
//! bytes) and ships back whatever [`GrpcReply`] the handler
//! produces. The handler is an async closure so the cs-runtime
//! side can bounce the request onto a Scheme actor's mailbox and
//! await the reply (the runtime is `!Send`, so the actor bridge is
//! how Scheme runs under the multi-thread tokio server — exactly
//! like [`crate::actor::ActorHandler`] does for HTTP).
//!
//! ## Streaming (.23) — how this extends
//!
//! Unary is `1 request frame → 1 response frame → trailers`. A
//! server-streaming or bidi RPC is the same transport with a
//! different body:
//!
//! - The request side already collects *all* DATA frames; a
//!   client-streaming handler would instead be hyper's `Incoming`
//!   body fed frame-by-frame to the handler (de-framing across
//!   DATA-frame boundaries, since one gRPC message can span
//!   several HTTP/2 DATA frames or several messages can share one).
//! - The response side would swap [`GrpcBody`] (single buffered
//!   DATA frame) for a body backed by an mpsc channel: the
//!   streaming actor calls a `grpc-stream-send!` primop that pushes
//!   a framed message onto the channel, and a final
//!   `grpc-stream-close!` pushes the trailers. `poll_frame` drains
//!   the channel. The HEADERS + trailers handling here is already
//!   correct for that case — only the DATA-frame source changes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
// Re-exported (`pub`) so downstream crates (cs-runtime's grpc
// bridge) can name the `GrpcHandler::call` return type without an
// explicit `futures-util` dependency.
pub use futures_util::future::BoxFuture;
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;

use crate::WebError;

/// A de-framed unary gRPC request handed to the handler.
///
/// `path` is the HTTP/2 `:path`, e.g. `/etcdserverpb.KV/Range`.
/// gRPC encodes the (service, method) pair as `/Package.Service/Method`.
/// `message` is the request protobuf bytes with the 5-byte gRPC
/// length prefix already stripped.
#[derive(Debug, Clone)]
pub struct GrpcRequest {
    pub path: String,
    pub message: Bytes,
}

/// A handler's reply: a gRPC status code plus the response message
/// bytes (which are framed onto the wire). Status `0` is OK.
///
/// On a non-zero status the `message` is conventionally empty and
/// `error` carries a human-readable `grpc-message`. A handler that
/// wants to return data *and* a non-zero status may do both.
#[derive(Debug, Clone)]
pub struct GrpcReply {
    pub status: u32,
    pub message: Bytes,
    /// Optional `grpc-message` trailer (percent-encoded by us per
    /// the gRPC spec). Sent only when present; typically set on
    /// error.
    pub error: Option<String>,
}

impl GrpcReply {
    /// OK reply carrying `message`.
    pub fn ok(message: impl Into<Bytes>) -> Self {
        Self {
            status: 0,
            message: message.into(),
            error: None,
        }
    }

    /// Error reply: a non-zero status and an optional message. No
    /// response payload.
    pub fn error(status: u32, message: impl Into<String>) -> Self {
        Self {
            status,
            message: Bytes::new(),
            error: Some(message.into()),
        }
    }
}

/// The handler the transport drives. `&self` so one handler serves
/// every connection without locks (the cs-runtime side holds an
/// `ActorRef` and sends a mailbox message per call).
pub trait GrpcHandler: Send + Sync + 'static {
    fn call(&self, req: GrpcRequest) -> BoxFuture<'static, GrpcReply>;
}

/// Boxed, refcounted handler handle.
pub type ArcGrpcHandler = Arc<dyn GrpcHandler>;

// Blanket impl so a plain async closure can be a handler — handy
// for the echo smoke and for Rust-side tests.
impl<F> GrpcHandler for F
where
    F: Fn(GrpcRequest) -> BoxFuture<'static, GrpcReply> + Send + Sync + 'static,
{
    fn call(&self, req: GrpcRequest) -> BoxFuture<'static, GrpcReply> {
        (self)(req)
    }
}

// ---------------------------------------------------------------
// gRPC length-prefix framing.
// ---------------------------------------------------------------

/// Prepend the 5-byte gRPC frame header to `message`:
/// `00 ‖ u32be(len) ‖ message`. The leading byte is the
/// compressed-data flag — always `0` (we don't apply per-message
/// compression).
pub fn frame_message(message: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + message.len());
    buf.put_u8(0); // compressed-flag: 0 (identity)
    buf.put_u32(message.len() as u32); // big-endian length
    buf.put_slice(message);
    buf.freeze()
}

/// Strip the 5-byte gRPC frame header from a *single-message*
/// request body and return the message bytes. Returns an error if
/// the body is too short, declares a compressed payload (we don't
/// negotiate compression), or the declared length doesn't match
/// what's present.
///
/// Unary requests carry exactly one frame; a body shorter than the
/// declared length is a truncated/streaming frame this unary path
/// rejects (streaming is `.23`).
pub fn deframe_message(body: &[u8]) -> Result<Bytes, String> {
    if body.is_empty() {
        // No frame at all — treat as an empty message. Some clients
        // send a header-only body for no-arg RPCs.
        return Ok(Bytes::new());
    }
    if body.len() < 5 {
        return Err(format!(
            "grpc: request frame truncated: {} bytes, need at least 5",
            body.len()
        ));
    }
    let compressed = body[0];
    if compressed != 0 {
        return Err("grpc: compressed request frames are not supported".into());
    }
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let rest = &body[5..];
    if rest.len() < len {
        return Err(format!(
            "grpc: request frame declares {len} bytes but body has {}",
            rest.len()
        ));
    }
    // Exactly one frame for unary. Trailing bytes would be a second
    // frame (client streaming) — out of scope here.
    Ok(Bytes::copy_from_slice(&rest[..len]))
}

// ---------------------------------------------------------------
// Response body that carries one DATA frame then gRPC trailers.
// ---------------------------------------------------------------

/// A minimal [`http_body::Body`] that yields the framed response
/// message as a single DATA frame, then a TRAILERS frame carrying
/// `grpc-status` (and optional `grpc-message`).
///
/// hyper sends trailers on HTTP/2 automatically when the body's
/// final `poll_frame` returns `Frame::trailers(..)` — this is the
/// piece the plain HTTP path never used.
pub struct GrpcBody {
    /// `Some` until the DATA frame has been emitted.
    data: Option<Bytes>,
    /// `Some` until the TRAILERS frame has been emitted.
    trailers: Option<HeaderMap>,
}

impl GrpcBody {
    fn new(reply: &GrpcReply) -> Self {
        let mut trailers = HeaderMap::new();
        // grpc-status is the canonical out-of-band status code.
        trailers.insert(
            HeaderName::from_static("grpc-status"),
            HeaderValue::from_str(&reply.status.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("2")), // 2 = UNKNOWN
        );
        if let Some(msg) = &reply.error {
            if let Ok(v) = HeaderValue::from_str(&pct_encode_grpc_message(msg)) {
                trailers.insert(HeaderName::from_static("grpc-message"), v);
            }
        }
        Self {
            data: Some(frame_message(&reply.message)),
            trailers: Some(trailers),
        }
    }
}

impl Body for GrpcBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        // DATA frame first.
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        // Then the trailers frame — this is what makes hyper emit
        // the gRPC status on the wire.
        if let Some(trailers) = this.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none() && self.trailers.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        // Only the DATA frame contributes to the byte length;
        // trailers aren't counted by `SizeHint`.
        let len = self.data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
        SizeHint::with_exact(len)
    }
}

/// Percent-encode a `grpc-message` per the gRPC spec: any byte
/// outside `0x20..=0x7E` (and `%` itself) becomes `%XX`. Keeps the
/// trailer header-value-safe (no CR/LF/control bytes).
fn pct_encode_grpc_message(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if (0x20..=0x7E).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

// ---------------------------------------------------------------
// Per-request handling + accept loop.
// ---------------------------------------------------------------

/// Build the framed HTTP/2 response for one unary call. Always a
/// `200` HTTP status with `content-type: application/grpc`; the
/// gRPC status rides in the trailers via [`GrpcBody`].
fn build_response(reply: &GrpcReply) -> Response<GrpcBody> {
    let mut resp = Response::new(GrpcBody::new(reply));
    *resp.status_mut() = http::StatusCode::OK;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    resp
}

/// Handle a single HTTP/2 request as a unary gRPC call: read the
/// `:path`, collect + de-frame the request body, dispatch to the
/// handler, and frame the reply (DATA + trailers). De-framing
/// errors map to gRPC `INTERNAL` (13) with a `grpc-message`, so the
/// client always gets a well-formed trailers response rather than a
/// torn stream.
async fn handle_unary(
    handler: ArcGrpcHandler,
    req: http::Request<Incoming>,
) -> Result<Response<GrpcBody>, Infallible> {
    let path = req.uri().path().to_string();
    let collected = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            return Ok(build_response(&GrpcReply::error(
                13, // INTERNAL
                format!("read request body: {e}"),
            )));
        }
    };
    let message = match deframe_message(&collected) {
        Ok(m) => m,
        Err(e) => {
            return Ok(build_response(&GrpcReply::error(13, e)));
        }
    };
    let reply = handler.call(GrpcRequest { path, message }).await;
    Ok(build_response(&reply))
}

/// Run an h2c (prior-knowledge HTTP/2) accept loop on `listener`,
/// dispatching every request to `handler` as a unary gRPC call.
///
/// Each accepted TCP connection is served on its own tokio task by
/// `http2::Builder` — no ALPN, no TLS; the client must open with
/// HTTP/2 prior knowledge (this is what gRPC clients do for an
/// `insecure` / h2c target).
///
/// `shutdown` is an optional future that, when it resolves, breaks
/// the accept loop. Returns the number of connections accepted.
pub async fn serve_grpc<F>(
    listener: TcpListener,
    handler: ArcGrpcHandler,
    shutdown: Option<F>,
) -> Result<u64, WebError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let mut count: u64 = 0;
    let mut shutdown = shutdown.map(Box::pin);
    loop {
        let accept = listener.accept();
        let next = match shutdown.as_mut() {
            Some(s) => tokio::select! {
                _ = s => break,
                a = accept => a,
            },
            None => accept.await,
        };
        let (stream, _peer) = match next {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cs-web grpc: accept error: {e}");
                continue;
            }
        };
        count = count.wrapping_add(1);

        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req: http::Request<Incoming>| {
                let handler = Arc::clone(&handler);
                async move { handle_unary(handler, req).await }
            });
            // Prior-knowledge h2c: serve HTTP/2 directly on the
            // cleartext socket. `TokioExecutor` lets request futures
            // spawn onto the current runtime.
            if let Err(err) = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, svc)
                .await
            {
                eprintln!("cs-web grpc: connection error: {err}");
            }
        });
    }
    Ok(count)
}

/// Bind a TCP listener for the h2c gRPC server and return it with
/// the resolved local address (useful when the caller passed port
/// `0`).
pub async fn bind_grpc(addr: SocketAddr) -> Result<(TcpListener, SocketAddr), WebError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| WebError::Bind { addr, source: e })?;
    let local = listener.local_addr()?;
    Ok((listener, local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let msg = b"\x08\x96\x01"; // arbitrary protobuf-ish bytes
        let framed = frame_message(msg);
        assert_eq!(framed[0], 0); // compressed flag
        assert_eq!(&framed[1..5], &[0, 0, 0, 3]); // u32be length = 3
        assert_eq!(&framed[5..], msg);
        let back = deframe_message(&framed).unwrap();
        assert_eq!(&back[..], msg);
    }

    #[test]
    fn deframe_empty_body_is_empty_message() {
        assert!(deframe_message(&[]).unwrap().is_empty());
    }

    #[test]
    fn deframe_rejects_compressed() {
        // compressed-flag = 1
        let framed = [1u8, 0, 0, 0, 1, 0xAA];
        assert!(deframe_message(&framed).is_err());
    }

    #[test]
    fn deframe_rejects_truncated_length() {
        // declares 4 bytes but only 1 present
        let framed = [0u8, 0, 0, 0, 4, 0xAA];
        assert!(deframe_message(&framed).is_err());
    }

    #[test]
    fn pct_encode_handles_control_and_unicode() {
        assert_eq!(pct_encode_grpc_message("ok"), "ok");
        assert_eq!(pct_encode_grpc_message("a\nb"), "a%0Ab");
        assert_eq!(pct_encode_grpc_message("100%"), "100%25");
    }

    #[tokio::test]
    async fn grpc_body_emits_data_then_trailers() {
        let reply = GrpcReply::ok(Bytes::from_static(b"hi"));
        let mut body = GrpcBody::new(&reply);
        // First frame: DATA = framed("hi").
        let f1 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert!(f1.is_data());
        assert_eq!(&f1.into_data().unwrap()[..], &frame_message(b"hi")[..]);
        // Second frame: TRAILERS with grpc-status: 0.
        let f2 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert!(f2.is_trailers());
        let t = f2.into_trailers().unwrap();
        assert_eq!(t.get("grpc-status").unwrap(), "0");
        // Stream ends.
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .is_none()
        );
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn grpc_body_error_carries_message_trailer() {
        // Embed a newline to prove control bytes get percent-encoded
        // (a raw \n in a header value would be illegal). Printable
        // ASCII incl. space is left as-is, matching grpc-go.
        let reply = GrpcReply::error(5, "not found\n"); // 5 = NOT_FOUND
        let mut body = GrpcBody::new(&reply);
        // DATA frame (empty message → just the 5-byte header).
        let f1 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert!(f1.is_data());
        let f2 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let t = f2.into_trailers().unwrap();
        assert_eq!(t.get("grpc-status").unwrap(), "5");
        assert_eq!(t.get("grpc-message").unwrap(), "not found%0A");
    }
}
