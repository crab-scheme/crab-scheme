//! Layers — composable middleware in the tower style.
//!
//! [`Layer`] wraps an `ArcService` to produce a new `ArcService`.
//! [`Stack`] holds a list of layers and applies them in
//! outermost-first order (so the first-pushed layer sees the
//! request first and the response last — same as tower).
//!
//! Built-in layers:
//! - [`Trace`] — logs `method path -> status` to stderr.
//! - [`RequestId`] — injects an `x-request-id` header on requests
//!   that don't already have one, then echoes it on the response.
//! - [`Timeout`] — cancels handlers that exceed a deadline and
//!   returns 504.
//! - [`CatchPanic`] — converts handler panics into 500 instead of
//!   crashing the connection.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt};
use http::{HeaderName, HeaderValue, StatusCode};

use crate::{response, ArcService, Request, Response, Service};

/// Wraps an `ArcService` to produce a new `ArcService`.
pub trait Layer: Send + Sync + 'static {
    fn layer(&self, inner: ArcService) -> ArcService;
}

/// Ordered middleware chain. Layers are pushed outside-first:
/// `Stack::new().push(A).push(B).wrap(svc)` calls A first, then B,
/// then svc.
#[derive(Default)]
pub struct Stack {
    layers: Vec<Arc<dyn Layer>>,
}

impl Stack {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn push<L: Layer>(mut self, layer: L) -> Self {
        self.layers.push(Arc::new(layer));
        self
    }

    /// Apply the stack to a service. Inner layers wrap first so
    /// the outer layer (first pushed) is the outermost wrapper.
    pub fn wrap(&self, service: ArcService) -> ArcService {
        let mut svc = service;
        for layer in self.layers.iter().rev() {
            svc = layer.layer(svc);
        }
        svc
    }
}

// ---------------------------------------------------------------
// Trace
// ---------------------------------------------------------------

/// Stderr access log. Format: `cs-web: METHOD path -> status (Nms)`.
#[derive(Default, Clone, Copy)]
pub struct Trace;

impl Layer for Trace {
    fn layer(&self, inner: ArcService) -> ArcService {
        Arc::new(TraceService { inner })
    }
}

struct TraceService {
    inner: ArcService,
}

impl Service for TraceService {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let start = std::time::Instant::now();
        async move {
            let resp = inner.call(req).await;
            let elapsed = start.elapsed();
            eprintln!(
                "cs-web: {method} {path} -> {} ({}ms)",
                resp.status().as_u16(),
                elapsed.as_millis()
            );
            resp
        }
        .boxed()
    }
}

// ---------------------------------------------------------------
// RequestId
// ---------------------------------------------------------------

/// Injects `x-request-id` on the request if absent, and echoes it
/// on the response. ID is a monotonic counter — cheap and
/// debuggable. Swap for UUID via a custom builder if a globally-
/// unique ID is needed across processes.
#[derive(Default)]
pub struct RequestId {
    next: AtomicU64,
}

impl RequestId {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }
}

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

impl Layer for RequestId {
    fn layer(&self, inner: ArcService) -> ArcService {
        Arc::new(RequestIdService {
            inner,
            counter: AtomicU64::new(self.next.load(Ordering::Relaxed)),
        })
    }
}

struct RequestIdService {
    inner: ArcService,
    counter: AtomicU64,
}

impl Service for RequestIdService {
    fn call(&self, mut req: Request) -> BoxFuture<'static, Response> {
        let id = if let Some(existing) = req.headers().get(&REQUEST_ID_HEADER) {
            existing.clone()
        } else {
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            let v = HeaderValue::from_str(&n.to_string()).unwrap();
            req.headers_mut().insert(&REQUEST_ID_HEADER, v.clone());
            v
        };
        let inner = Arc::clone(&self.inner);
        async move {
            let mut resp = inner.call(req).await;
            resp.headers_mut().insert(&REQUEST_ID_HEADER, id);
            resp
        }
        .boxed()
    }
}

// ---------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------

/// Drops the handler future after `dur` and returns 504. The
/// handler task is cancelled; whatever side effects it has
/// already completed remain.
#[derive(Clone, Copy)]
pub struct Timeout {
    pub dur: Duration,
}

impl Timeout {
    pub fn new(dur: Duration) -> Self {
        Self { dur }
    }
}

impl Layer for Timeout {
    fn layer(&self, inner: ArcService) -> ArcService {
        Arc::new(TimeoutService {
            inner,
            dur: self.dur,
        })
    }
}

struct TimeoutService {
    inner: ArcService,
    dur: Duration,
}

impl Service for TimeoutService {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        let dur = self.dur;
        async move {
            match tokio::time::timeout(dur, inner.call(req)).await {
                Ok(resp) => resp,
                Err(_) => response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout"),
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------
// CatchPanic
// ---------------------------------------------------------------

/// Returns 500 instead of crashing the connection task when a
/// handler panics. Uses `AssertUnwindSafe` because Response is
/// `Send` and the panic boundary terminates the handler regardless
/// of UnwindSafe.
#[derive(Default, Clone, Copy)]
pub struct CatchPanic;

impl Layer for CatchPanic {
    fn layer(&self, inner: ArcService) -> ArcService {
        Arc::new(CatchPanicService { inner })
    }
}

struct CatchPanicService {
    inner: ArcService,
}

impl Service for CatchPanicService {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        async move {
            let fut = std::panic::AssertUnwindSafe(inner.call(req));
            match fut.catch_unwind().await {
                Ok(resp) => resp,
                Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::service_fn;
    use crate::ok;
    use bytes::Bytes;

    fn req() -> Request {
        http::Request::builder()
            .uri("/")
            .body(Bytes::new())
            .unwrap()
    }

    #[tokio::test]
    async fn request_id_injects_and_echoes() {
        let svc = Stack::new()
            .push(RequestId::new())
            .wrap(service_fn(|r: Request| async move {
                // Handler can read the injected id.
                let id = r.headers().get("x-request-id").cloned();
                assert!(id.is_some(), "RequestId layer should inject before handler");
                ok("hi")
            }));
        let resp = svc.call(req()).await;
        assert!(resp.headers().get("x-request-id").is_some());
    }

    #[tokio::test]
    async fn request_id_preserves_existing() {
        let svc = Stack::new()
            .push(RequestId::new())
            .wrap(service_fn(|_| async { ok("hi") }));
        let mut r = req();
        r.headers_mut().insert(
            "x-request-id",
            HeaderValue::from_static("client-supplied-42"),
        );
        let resp = svc.call(r).await;
        assert_eq!(
            resp.headers()
                .get("x-request-id")
                .unwrap()
                .to_str()
                .unwrap(),
            "client-supplied-42"
        );
    }

    #[tokio::test]
    async fn timeout_short_circuits_slow_handler() {
        let svc = Stack::new()
            .push(Timeout::new(Duration::from_millis(20)))
            .wrap(service_fn(|_| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                ok("never")
            }));
        let start = std::time::Instant::now();
        let resp = svc.call(req()).await;
        let elapsed = start.elapsed();
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout should fire fast, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn catch_panic_returns_500() {
        let svc = Stack::new().push(CatchPanic).wrap(service_fn(|_| async {
            panic!("boom");
            #[allow(unreachable_code)]
            ok("never")
        }));
        let resp = svc.call(req()).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn stack_applies_layers_outside_in() {
        // Build a layer that flags request/response by mutating a
        // header so we can observe order.
        struct Tag(&'static str);
        impl Layer for Tag {
            fn layer(&self, inner: ArcService) -> ArcService {
                let name = self.0;
                Arc::new(TagSvc { inner, name })
            }
        }
        struct TagSvc {
            inner: ArcService,
            name: &'static str,
        }
        impl Service for TagSvc {
            fn call(&self, mut req: Request) -> BoxFuture<'static, Response> {
                let inner = Arc::clone(&self.inner);
                let name = self.name;
                req.headers_mut().append(
                    "x-trace",
                    HeaderValue::from_str(&format!("in:{name}")).unwrap(),
                );
                async move {
                    let mut resp = inner.call(req).await;
                    resp.headers_mut().append(
                        "x-trace",
                        HeaderValue::from_str(&format!("out:{name}")).unwrap(),
                    );
                    resp
                }
                .boxed()
            }
        }

        let svc =
            Stack::new()
                .push(Tag("A"))
                .push(Tag("B"))
                .wrap(service_fn(|r: Request| async move {
                    let trace: Vec<String> = r
                        .headers()
                        .get_all("x-trace")
                        .iter()
                        .map(|v| v.to_str().unwrap().to_string())
                        .collect();
                    let mut resp = ok("");
                    resp.headers_mut().insert(
                        "x-handler-saw",
                        HeaderValue::from_str(&trace.join(",")).unwrap(),
                    );
                    resp
                }));
        let resp = svc.call(req()).await;
        // Handler saw A then B on the way in.
        assert_eq!(
            resp.headers()
                .get("x-handler-saw")
                .unwrap()
                .to_str()
                .unwrap(),
            "in:A,in:B"
        );
        // Response was tagged B then A on the way out.
        let out: Vec<&str> = resp
            .headers()
            .get_all("x-trace")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(out, vec!["out:B", "out:A"]);
    }
}
