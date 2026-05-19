//! cs-web — Tower-style async web framework for CrabScheme.
//!
//! Composition primitives:
//! - [`Service`] — async `(Request) -> Response`. Cheaply shareable
//!   via `Arc<dyn Service>`.
//! - [`Layer`] — wraps a `Service` to produce a new `Service`
//!   (middleware).
//! - [`Stack`] — ordered list of layers applied outside-in.
//! - [`Router`] — `Service` impl that dispatches by method+path.
//!
//! Concurrency posture: each accepted connection runs on its own
//! tokio task. Request handling is `&self`, so a single
//! `Arc<Service>` serves N connections without locks. Middleware
//! adds wrappers; the router holds an `Arc<dyn Service>` per
//! route.
//!
//! Integration crates (feature-gated):
//! - `actor` — back a route with a cs-actor PID; request → message,
//!   reply → response.
//! - `table` — access-log layer (writes hits to a cs-table) and
//!   session-store helpers.
//! - `modules` — load a `Handler` from a cdylib at runtime.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

pub mod handler;
pub mod layers;
pub mod router;

#[cfg(feature = "modules")]
pub mod module;

// Re-export http types so users don't need an explicit `http` dep.
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version};

pub use handler::{handler_fn, Handler, HandlerFn};
pub use layers::{CatchPanic, Layer, RequestId, Stack, Timeout, Trace};
pub use router::{RouteSink, Router};

#[cfg(feature = "modules")]
pub use module::{Module, ENTRY_POINT};

/// Owned request body — fully buffered. Streaming bodies are a
/// follow-up; the dominant CrabScheme use case is small JSON.
pub type Body = Full<Bytes>;

/// HTTP request with a fully-buffered body.
pub type Request = http::Request<Bytes>;

/// HTTP response with a fully-buffered body.
pub type Response = http::Response<Bytes>;

/// Errors that can escape the server itself. Handler errors are
/// caught and converted to 500 by [`CatchPanic`]; this enum
/// covers infrastructure failures (bind, accept, IO).
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("hyper: {0}")]
    Hyper(#[from] hyper::Error),

    #[cfg(feature = "modules")]
    #[error("module: {0}")]
    Module(String),
}

/// Async `(Request) -> Response` — the core abstraction. `&self`
/// (not `&mut self`) because services are shared across many
/// concurrent connections; mutability lives behind locks the
/// implementor owns.
pub trait Service: Send + Sync + 'static {
    fn call(&self, req: Request) -> BoxFuture<'static, Response>;
}

/// A boxed, refcounted service handle — the canonical way layers
/// and routers reference downstream services.
pub type ArcService = Arc<dyn Service>;

/// Helper: build a [`Response`] with status + body.
pub fn response(status: StatusCode, body: impl Into<Bytes>) -> Response {
    let mut resp = http::Response::new(body.into());
    *resp.status_mut() = status;
    resp
}

/// Convenience 200 OK with text body.
pub fn ok(body: impl Into<Bytes>) -> Response {
    response(StatusCode::OK, body)
}

/// 404 Not Found with a short text body — used by the default
/// router fallback.
pub fn not_found() -> Response {
    response(StatusCode::NOT_FOUND, "Not Found")
}

/// Configuration for [`serve`]. Most users want defaults.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: SocketAddr,
    /// Maximum time a single request can run before the handler is
    /// dropped and the connection sees a 504. `None` = no limit.
    pub request_timeout: Option<Duration>,
}

impl ServerConfig {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            request_timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// Bind a TCP listener and return both it and the resolved local
/// address (useful when callers passed `0` for the port).
pub async fn bind(cfg: &ServerConfig) -> Result<(TcpListener, SocketAddr), WebError> {
    let listener = TcpListener::bind(cfg.addr)
        .await
        .map_err(|e| WebError::Bind {
            addr: cfg.addr,
            source: e,
        })?;
    let local = listener.local_addr()?;
    Ok((listener, local))
}

/// Run the accept loop until the listener is dropped or errors out.
///
/// Each accepted connection is spawned as its own tokio task. The
/// service is cloned (Arc bump) per request, so a single
/// `Arc<dyn Service>` fans out to many concurrent calls without
/// contention.
///
/// `shutdown` is an optional future that, when it resolves, breaks
/// the accept loop. Returns the number of connections that were
/// accepted before shutdown.
pub async fn serve<F>(
    listener: TcpListener,
    service: ArcService,
    shutdown: Option<F>,
) -> Result<u64, WebError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let mut count: u64 = 0;
    let shutdown = shutdown.map(Box::pin);

    let mut shutdown = shutdown;
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
                // Connection-level accept errors (EMFILE, etc.) are
                // logged and skipped — taking down the whole server
                // for a transient accept failure is the wrong call.
                eprintln!("cs-web: accept error: {e}");
                continue;
            }
        };
        count = count.wrapping_add(1);

        let svc = Arc::clone(&service);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let handler = service_fn(move |req: http::Request<hyper::body::Incoming>| {
                let svc = Arc::clone(&svc);
                async move {
                    let resp = dispatch(svc, req).await;
                    Ok::<_, Infallible>(resp.map(Full::new))
                }
            });
            if let Err(err) = http1::Builder::new().serve_connection(io, handler).await {
                eprintln!("cs-web: connection error: {err}");
            }
        });
    }
    Ok(count)
}

/// Glue: read the incoming body to bytes, hand it to the service,
/// translate panics into 500 (defensive — handlers should already
/// be wrapped in CatchPanic, but we double up so a panic on the
/// boundary cannot crash the connection).
async fn dispatch(svc: ArcService, req: http::Request<hyper::body::Incoming>) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            return response(StatusCode::BAD_REQUEST, format!("read body: {e}"));
        }
    };
    let req = http::Request::from_parts(parts, bytes);
    svc.call(req).await
}

/// One-shot helper: bind, serve, and wait for shutdown.
pub async fn run(cfg: ServerConfig, service: ArcService) -> Result<(), WebError> {
    let (listener, _) = bind(&cfg).await?;
    serve::<futures_util::future::Pending<()>>(listener, service, None).await?;
    Ok(())
}
