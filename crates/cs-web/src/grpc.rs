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
//!    transport, so an h2c listener is a complete substrate.
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
//! ## Streaming (.23)
//!
//! Both unary and streaming RPCs use ONE uniform path:
//!
//! - **Request side** — the body is read *incrementally*. Each gRPC
//!   message (which may span several HTTP/2 DATA frames, or share
//!   one) is de-framed and delivered to the handler as it arrives:
//!   the FIRST message via [`GrpcHandler::begin`], every subsequent
//!   client-streamed message via [`GrpcHandler::client_message`],
//!   and the client half-close via [`GrpcHandler::client_end`]. A
//!   unary handler simply replies after `begin` and ignores the
//!   rest; a bidi handler interleaves.
//! - **Response side** — [`GrpcBody`] is backed by an mpsc channel.
//!   The handler pushes response messages through a
//!   [`GrpcResponseSink`] (`send_message`), each becoming a framed
//!   DATA frame, and ends the stream with `close` (which flushes the
//!   gRPC-status trailers). Unary is just "one message then close".
//!
//! gRPC *semantics* (method → etcd RPC, protobuf encode/decode,
//! leader redirects) live in Scheme. This module hands the handler a
//! [`GrpcRequest`] (`:path` + de-framed message bytes) plus a sink,
//! and the handler drives the response. The handler is the
//! cs-runtime actor bridge: each call/message becomes a mailbox
//! message to a Scheme actor (the runtime is `!Send`).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::WebError;

/// A de-framed gRPC request message handed to the handler.
///
/// `path` is the HTTP/2 `:path`, e.g. `/etcdserverpb.KV/Range`.
/// gRPC encodes the (service, method) pair as `/Package.Service/Method`.
/// `message` is the request protobuf bytes with the 5-byte gRPC
/// length prefix already stripped — for a streaming call this is the
/// FIRST client message.
#[derive(Debug, Clone)]
pub struct GrpcRequest {
    pub path: String,
    pub message: Bytes,
}

// ---------------------------------------------------------------
// Response sink: the handler pushes framed messages then closes.
// ---------------------------------------------------------------

/// One unit the response body emits: a message (framed onto the wire
/// as a DATA frame) or the terminal gRPC-status trailers.
enum GrpcFrame {
    Message(Bytes),
    Trailers {
        status: u32,
        message: Option<String>,
    },
}

/// The handle a handler uses to drive a call's response stream. It is
/// `Clone + Send + Sync` (the underlying mpsc sender is), so the
/// Scheme bridge can stash it in a registry and push frames from any
/// thread. Unary = one `send_message` then `close`; streaming =
/// many `send_message` then `close`.
#[derive(Clone)]
pub struct GrpcResponseSink {
    tx: mpsc::UnboundedSender<GrpcFrame>,
}

impl GrpcResponseSink {
    /// Queue one response message — framed onto the wire as a DATA
    /// frame. Returns `false` if the client already went away (the
    /// response body was dropped), so a streaming handler can stop.
    pub fn send_message(&self, message: Bytes) -> bool {
        self.tx.send(GrpcFrame::Message(message)).is_ok()
    }

    /// End the response stream: flush the `grpc-status` (+ optional
    /// `grpc-message`) trailers. Idempotent-ish — a second close just
    /// fails to send. Returns `false` if the client already went away.
    pub fn close(&self, status: u32, message: Option<String>) -> bool {
        self.tx
            .send(GrpcFrame::Trailers { status, message })
            .is_ok()
    }
}

/// The handler the transport drives. `&self` so one handler serves
/// every connection without locks (the cs-runtime side holds an
/// `ActorRef` and sends a mailbox message per event). `call_id` is a
/// transport-assigned, process-unique id correlating the three
/// callbacks of one call; the Scheme bridge uses it directly as the
/// handle Scheme sees.
pub trait GrpcHandler: Send + Sync + 'static {
    /// A new call: its `:path`, the first request message, and the
    /// sink to drive the response. Must not block.
    fn begin(&self, call_id: u64, req: GrpcRequest, sink: GrpcResponseSink);
    /// A subsequent client-streamed request message for `call_id`.
    fn client_message(&self, call_id: u64, message: Bytes);
    /// The client half-closed the request stream for `call_id`.
    fn client_end(&self, call_id: u64);
}

/// Boxed, refcounted handler handle.
pub type ArcGrpcHandler = Arc<dyn GrpcHandler>;

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

/// Strip the 5-byte gRPC frame header from a *single-message* body
/// and return the message bytes. Retained for tests and ad-hoc
/// callers; the serve path uses the incremental [`BodyReader`].
pub fn deframe_message(body: &[u8]) -> Result<Bytes, String> {
    if body.is_empty() {
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
    Ok(Bytes::copy_from_slice(&rest[..len]))
}

/// Try to split one complete gRPC message off the front of `buf`.
/// Returns `Ok(None)` if more bytes are needed, `Ok(Some(msg))` on a
/// full frame (consuming it), or `Err` on a compressed frame.
fn try_take_message(buf: &mut BytesMut) -> Result<Option<Bytes>, String> {
    if buf.len() < 5 {
        return Ok(None);
    }
    let flag = buf[0];
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 5 + len {
        return Ok(None);
    }
    let _ = buf.split_to(5);
    let msg = buf.split_to(len).freeze();
    if flag != 0 {
        return Err("grpc: compressed request frames are not supported".into());
    }
    Ok(Some(msg))
}

/// Incremental re-framer over an HTTP/2 request body: yields one gRPC
/// message at a time, de-framing across DATA-frame boundaries.
struct BodyReader {
    body: Incoming,
    buf: BytesMut,
    eof: bool,
}

impl BodyReader {
    fn new(body: Incoming) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            eof: false,
        }
    }

    /// Next complete gRPC message, or `None` at end of stream.
    async fn next_message(&mut self) -> Result<Option<Bytes>, String> {
        loop {
            if let Some(m) = try_take_message(&mut self.buf)? {
                return Ok(Some(m));
            }
            if self.eof {
                return Ok(None);
            }
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf.put_slice(&data);
                    }
                    // TRAILERS frames on the request side carry no
                    // message payload — ignore.
                }
                Some(Err(e)) => return Err(format!("grpc: request body error: {e}")),
                None => {
                    self.eof = true;
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    // Leftover bytes that don't form a full frame:
                    // a truncated request. Surface it once.
                    return Err("grpc: truncated request frame at end of stream".into());
                }
            }
        }
    }
}

// ---------------------------------------------------------------
// Response body: drains the sink's mpsc channel into DATA frames
// then a TRAILERS frame.
// ---------------------------------------------------------------

/// A minimal [`http_body::Body`] that yields each queued response
/// message as a framed DATA frame, then a TRAILERS frame carrying
/// `grpc-status` (and optional `grpc-message`).
///
/// hyper sends trailers on HTTP/2 automatically when the body's final
/// `poll_frame` returns `Frame::trailers(..)`. If the sink is dropped
/// without an explicit close we synthesise an UNKNOWN(2) status so the
/// client always gets a well-formed trailers response.
pub struct GrpcBody {
    rx: mpsc::UnboundedReceiver<GrpcFrame>,
    /// Set once a TRAILERS frame has been emitted — the stream then
    /// ends on the next poll.
    ended: bool,
}

impl GrpcBody {
    fn new(rx: mpsc::UnboundedReceiver<GrpcFrame>) -> Self {
        Self { rx, ended: false }
    }
}

impl Body for GrpcBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(GrpcFrame::Message(bytes))) => {
                Poll::Ready(Some(Ok(Frame::data(frame_message(&bytes)))))
            }
            Poll::Ready(Some(GrpcFrame::Trailers { status, message })) => {
                this.ended = true;
                Poll::Ready(Some(Ok(Frame::trailers(build_trailers(status, message)))))
            }
            // Sink dropped without an explicit close — synthesise a
            // status so the wire is never a torn stream.
            Poll::Ready(None) => {
                this.ended = true;
                Poll::Ready(Some(Ok(Frame::trailers(build_trailers(
                    2,
                    Some("handler closed stream without status".into()),
                )))))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended
    }

    fn size_hint(&self) -> SizeHint {
        // Length is unknown for a streamed body.
        SizeHint::default()
    }
}

/// Build the gRPC trailers HeaderMap.
fn build_trailers(status: u32, message: Option<String>) -> HeaderMap {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        HeaderName::from_static("grpc-status"),
        HeaderValue::from_str(&status.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("2")), // 2 = UNKNOWN
    );
    if let Some(msg) = message {
        if let Ok(v) = HeaderValue::from_str(&pct_encode_grpc_message(&msg)) {
            trailers.insert(HeaderName::from_static("grpc-message"), v);
        }
    }
    trailers
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

/// Process-unique call ids correlate the three handler callbacks of
/// one call (and double as the handle the Scheme bridge exposes).
fn next_call_id() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Build the framed HTTP/2 response shell for a call: always a `200`
/// HTTP status with `content-type: application/grpc`; the gRPC status
/// rides in the trailers via [`GrpcBody`].
fn build_response(rx: mpsc::UnboundedReceiver<GrpcFrame>) -> Response<GrpcBody> {
    let mut resp = Response::new(GrpcBody::new(rx));
    *resp.status_mut() = http::StatusCode::OK;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    resp
}

/// Handle a single HTTP/2 request as a (possibly streaming) gRPC call.
///
/// Reads the FIRST request message, hands it + a response sink to the
/// handler via `begin`, returns the streaming response shell, and
/// spawns a task to drain the rest of the request body into
/// `client_message` / `client_end`. Framing errors map to gRPC
/// `INTERNAL` (13) so the client gets a clean trailers response.
async fn handle_call(
    handler: ArcGrpcHandler,
    req: http::Request<Incoming>,
) -> Result<Response<GrpcBody>, Infallible> {
    let path = req.uri().path().to_string();
    let call_id = next_call_id();
    let (tx, rx) = mpsc::unbounded_channel::<GrpcFrame>();
    let sink = GrpcResponseSink { tx };

    let mut reader = BodyReader::new(req.into_body());
    let first = match reader.next_message().await {
        Ok(Some(msg)) => msg,
        // Headers-only / empty body: deliver an empty first message
        // (matches the old unary no-arg behaviour).
        Ok(None) => Bytes::new(),
        Err(e) => {
            // Malformed framing: close immediately with INTERNAL.
            let _ = sink.close(13, Some(e));
            return Ok(build_response(rx));
        }
    };

    handler.begin(
        call_id,
        GrpcRequest {
            path,
            message: first,
        },
        sink,
    );

    // Drain the remainder of the request body concurrently with the
    // response. Subsequent messages → client_message; EOF → client_end.
    let h = Arc::clone(&handler);
    tokio::spawn(async move {
        loop {
            match reader.next_message().await {
                Ok(Some(msg)) => h.client_message(call_id, msg),
                Ok(None) => {
                    h.client_end(call_id);
                    break;
                }
                Err(_) => {
                    h.client_end(call_id);
                    break;
                }
            }
        }
    });

    Ok(build_response(rx))
}

/// Run an h2c (prior-knowledge HTTP/2) accept loop on `listener`,
/// dispatching every request to `handler` as a (possibly streaming)
/// gRPC call.
///
/// Each accepted TCP connection is served on its own tokio task by
/// `http2::Builder` — no ALPN, no TLS; the client must open with
/// HTTP/2 prior knowledge (what gRPC clients do for an `insecure` /
/// h2c target).
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
                async move { handle_call(handler, req).await }
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
        let framed = [1u8, 0, 0, 0, 1, 0xAA];
        assert!(deframe_message(&framed).is_err());
    }

    #[test]
    fn deframe_rejects_truncated_length() {
        let framed = [0u8, 0, 0, 0, 4, 0xAA];
        assert!(deframe_message(&framed).is_err());
    }

    #[test]
    fn pct_encode_handles_control_and_unicode() {
        assert_eq!(pct_encode_grpc_message("ok"), "ok");
        assert_eq!(pct_encode_grpc_message("a\nb"), "a%0Ab");
        assert_eq!(pct_encode_grpc_message("100%"), "100%25");
    }

    // Re-framer: two messages packed into one buffer, plus a message
    // split across appends.
    #[test]
    fn try_take_message_splits_frames() {
        let mut buf = BytesMut::new();
        buf.put_slice(&frame_message(b"aa"));
        buf.put_slice(&frame_message(b"bbb"));
        let m1 = try_take_message(&mut buf).unwrap().unwrap();
        assert_eq!(&m1[..], b"aa");
        let m2 = try_take_message(&mut buf).unwrap().unwrap();
        assert_eq!(&m2[..], b"bbb");
        assert!(try_take_message(&mut buf).unwrap().is_none());
    }

    #[test]
    fn try_take_message_waits_for_full_payload() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        buf.put_u32(4);
        buf.put_slice(b"ab"); // only 2 of 4 payload bytes
        assert!(try_take_message(&mut buf).unwrap().is_none());
        buf.put_slice(b"cd");
        let m = try_take_message(&mut buf).unwrap().unwrap();
        assert_eq!(&m[..], b"abcd");
    }

    // GrpcBody drains messages then trailers from the sink channel.
    #[tokio::test]
    async fn grpc_body_streams_messages_then_trailers() {
        let (tx, rx) = mpsc::unbounded_channel::<GrpcFrame>();
        let sink = GrpcResponseSink { tx };
        let mut body = GrpcBody::new(rx);
        assert!(sink.send_message(Bytes::from_static(b"one")));
        assert!(sink.send_message(Bytes::from_static(b"two")));
        assert!(sink.close(0, None));

        let f1 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&f1.into_data().unwrap()[..], &frame_message(b"one")[..]);
        let f2 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&f2.into_data().unwrap()[..], &frame_message(b"two")[..]);
        let f3 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let t = f3.into_trailers().unwrap();
        assert_eq!(t.get("grpc-status").unwrap(), "0");
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .is_none()
        );
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn grpc_body_error_close_carries_message_trailer() {
        let (tx, rx) = mpsc::unbounded_channel::<GrpcFrame>();
        let sink = GrpcResponseSink { tx };
        let mut body = GrpcBody::new(rx);
        assert!(sink.close(5, Some("not found\n".into()))); // 5 = NOT_FOUND
        let f = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let t = f.into_trailers().unwrap();
        assert_eq!(t.get("grpc-status").unwrap(), "5");
        assert_eq!(t.get("grpc-message").unwrap(), "not found%0A");
    }

    // Dropping the sink without closing still yields a status trailer.
    #[tokio::test]
    async fn grpc_body_synthesises_status_on_drop() {
        let (tx, rx) = mpsc::unbounded_channel::<GrpcFrame>();
        let sink = GrpcResponseSink { tx };
        let mut body = GrpcBody::new(rx);
        drop(sink);
        let f = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let t = f.into_trailers().unwrap();
        assert_eq!(t.get("grpc-status").unwrap(), "2");
    }
}
