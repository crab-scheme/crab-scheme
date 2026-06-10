//! Streaming gRPC transport (.23): real h2c server + a real hyper
//! HTTP/2 prior-knowledge client driving unary, server-streaming, and
//! bidirectional-streaming calls over the length-prefixed gRPC wire
//! format with out-of-band `grpc-status` trailers.
//!
//! This is the hermetic transport proof for the `begin` /
//! `client_message` / `client_end` + mpsc-backed [`GrpcBody`] rewrite.
//! The end-to-end Scheme + etcdctl proof lives in crab-watchstore.

#![cfg(feature = "grpc")]

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
use cs_web::grpc::{
    bind_grpc, frame_message, serve_grpc, ArcGrpcHandler, GrpcHandler, GrpcRequest,
    GrpcResponseSink,
};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

// ---------------------------------------------------------------
// A real GrpcHandler echo server exercising all three call shapes.
// ---------------------------------------------------------------

/// Bidi calls keep their sink between `client_message`s.
fn streams() -> &'static Mutex<HashMap<u64, GrpcResponseSink>> {
    static S: OnceLock<Mutex<HashMap<u64, GrpcResponseSink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

struct EchoHandler;

impl GrpcHandler for EchoHandler {
    fn begin(&self, call_id: u64, req: GrpcRequest, sink: GrpcResponseSink) {
        match req.path.as_str() {
            // UNARY: one response, one status.
            "/echo.Echo/Unary" => {
                sink.send_message(req.message);
                sink.close(0, None);
            }
            // SERVER-STREAM: one request -> three responses + status.
            "/echo.Echo/ServerStream" => {
                let base = String::from_utf8_lossy(&req.message).into_owned();
                for i in 0..3 {
                    sink.send_message(Bytes::from(format!("{base}-{i}")));
                }
                sink.close(0, None);
            }
            // BIDI: echo the first message, then each subsequent one.
            "/echo.Echo/BidiStream" => {
                sink.send_message(prefix_echo(&req.message));
                streams().lock().unwrap().insert(call_id, sink);
            }
            _ => {
                sink.close(12, Some("unimplemented".into()));
            }
        }
    }

    fn client_message(&self, call_id: u64, message: Bytes) {
        if let Some(sink) = streams().lock().unwrap().get(&call_id) {
            sink.send_message(prefix_echo(&message));
        }
    }

    fn client_end(&self, call_id: u64) {
        if let Some(sink) = streams().lock().unwrap().remove(&call_id) {
            sink.close(0, None);
        }
    }
}

fn prefix_echo(msg: &[u8]) -> Bytes {
    let mut out = BytesMut::new();
    out.put_slice(b"echo:");
    out.put_slice(msg);
    out.freeze()
}

async fn spawn_echo_server() -> SocketAddr {
    let (listener, addr) = bind_grpc(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let handler: ArcGrpcHandler = Arc::new(EchoHandler);
    tokio::spawn(async move {
        let _ = serve_grpc::<futures_util::future::Pending<()>>(listener, handler, None).await;
    });
    addr
}

// ---------------------------------------------------------------
// A minimal client-streaming request body backed by a channel, so
// the bidi test can interleave sends and receives.
// ---------------------------------------------------------------

struct ChannelBody {
    rx: mpsc::UnboundedReceiver<Bytes>,
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut().rx.poll_recv(cx) {
            Poll::Ready(Some(b)) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------
// Response reader: collect DATA frames into gRPC messages + capture
// the grpc-status trailer.
// ---------------------------------------------------------------

fn take_msg(buf: &mut BytesMut) -> Option<Bytes> {
    if buf.len() < 5 {
        return None;
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 5 + len {
        return None;
    }
    let _ = buf.split_to(5);
    Some(buf.split_to(len).freeze())
}

/// Read every response message + the final grpc-status from `resp`.
async fn read_all(mut resp: Incoming) -> (Vec<Bytes>, Option<String>) {
    let mut buf = BytesMut::new();
    let mut msgs = Vec::new();
    let mut status = None;
    while let Some(frame) = resp.frame().await {
        let frame = frame.expect("response frame");
        if let Some(data) = frame.data_ref() {
            buf.put_slice(data);
            while let Some(m) = take_msg(&mut buf) {
                msgs.push(m);
            }
        } else if let Some(trailers) = frame.trailers_ref() {
            if let Some(s) = trailers.get("grpc-status") {
                status = Some(s.to_str().unwrap().to_string());
            }
        }
    }
    (msgs, status)
}

async fn connect_unary(addr: SocketAddr) -> http2::SendRequest<Full<Bytes>> {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let io = TokioIo::new(stream);
    let (sender, conn) = http2::handshake(TokioExecutor::new(), io)
        .await
        .expect("h2c handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender
}

async fn connect_stream(addr: SocketAddr) -> http2::SendRequest<ChannelBody> {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let io = TokioIo::new(stream);
    let (sender, conn) = http2::handshake(TokioExecutor::new(), io)
        .await
        .expect("h2c handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender
}

fn grpc_request<B>(path: &str, body: B) -> http::Request<B> {
    http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(body)
        .unwrap()
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[tokio::test]
async fn unary_echo_roundtrips() {
    let addr = spawn_echo_server().await;
    let mut sender = connect_unary(addr).await;
    let body = Full::new(frame_message(b"ping"));
    let resp = sender
        .send_request(grpc_request("/echo.Echo/Unary", body))
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    let (msgs, status) = read_all(resp.into_body()).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(&msgs[0][..], b"ping");
    assert_eq!(status.as_deref(), Some("0"));
}

#[tokio::test]
async fn server_stream_yields_n_then_status() {
    let addr = spawn_echo_server().await;
    let mut sender = connect_unary(addr).await;
    let body = Full::new(frame_message(b"x"));
    let resp = sender
        .send_request(grpc_request("/echo.Echo/ServerStream", body))
        .await
        .expect("send");
    let (msgs, status) = read_all(resp.into_body()).await;
    let got: Vec<String> = msgs
        .iter()
        .map(|m| String::from_utf8_lossy(m).into_owned())
        .collect();
    assert_eq!(got, vec!["x-0", "x-1", "x-2"]);
    assert_eq!(status.as_deref(), Some("0"));
}

#[tokio::test]
async fn unknown_method_is_unimplemented() {
    let addr = spawn_echo_server().await;
    let mut sender = connect_unary(addr).await;
    let body = Full::new(frame_message(b""));
    let resp = sender
        .send_request(grpc_request("/echo.Echo/Nope", body))
        .await
        .expect("send");
    let (msgs, status) = read_all(resp.into_body()).await;
    assert!(msgs.is_empty());
    assert_eq!(status.as_deref(), Some("12")); // UNIMPLEMENTED
}

#[tokio::test]
async fn bidi_stream_echoes_each_interleaved() {
    let addr = spawn_echo_server().await;
    let mut sender = connect_stream(addr).await;
    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    // First client message rides with the request open.
    tx.send(frame_message(b"a")).unwrap();
    let resp = sender
        .send_request(grpc_request("/echo.Echo/BidiStream", ChannelBody { rx }))
        .await
        .expect("send");
    let mut body = resp.into_body();
    let mut buf = BytesMut::new();

    // Helper: pull the next response message, interleaving with sends.
    async fn next_msg(body: &mut Incoming, buf: &mut BytesMut) -> Bytes {
        loop {
            if let Some(m) = take_msg(buf) {
                return m;
            }
            let frame = body.frame().await.expect("frame").expect("ok");
            if let Some(data) = frame.data_ref() {
                buf.put_slice(data);
            }
        }
    }

    assert_eq!(&next_msg(&mut body, &mut buf).await[..], b"echo:a");
    tx.send(frame_message(b"b")).unwrap();
    assert_eq!(&next_msg(&mut body, &mut buf).await[..], b"echo:b");
    tx.send(frame_message(b"c")).unwrap();
    assert_eq!(&next_msg(&mut body, &mut buf).await[..], b"echo:c");

    // Half-close the client stream -> server closes with status 0.
    drop(tx);
    let mut status = None;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("frame");
        if let Some(trailers) = frame.trailers_ref() {
            status = trailers
                .get("grpc-status")
                .map(|s| s.to_str().unwrap().to_string());
        }
    }
    assert_eq!(status.as_deref(), Some("0"));
}
