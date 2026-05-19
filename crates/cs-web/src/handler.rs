//! Handler ergonomics — the friendly surface on top of [`Service`].
//!
//! A [`Service`] is `&self`-callable and returns a boxed future.
//! That's perfect for the runtime, awkward for users. This module
//! defines [`Handler`] (an async-fn-shaped trait that compiles to
//! a Service) and conversion helpers so users can write:
//!
//! ```ignore
//! async fn hello(_req: Request) -> Response { ok("hi") }
//! let svc: ArcService = handler_fn(hello).into_service();
//! ```

use std::future::Future;
use std::sync::Arc;

use futures_util::future::{BoxFuture, FutureExt};

use crate::{ArcService, Request, Response, Service};

/// A boxed handler future. Async closures returning `Response`
/// auto-convert into this via [`handler_fn`].
pub type HandlerFuture = BoxFuture<'static, Response>;

/// Ergonomic async-fn handler trait. Implement this for custom
/// types, or use [`handler_fn`] to wrap a free async function.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request) -> HandlerFuture;

    /// Convert into a refcounted Service for use with [`Router`]
    /// or layers.
    fn into_service(self) -> ArcService
    where
        Self: Sized,
    {
        Arc::new(HandlerService { inner: self })
    }
}

struct HandlerService<H> {
    inner: H,
}

impl<H: Handler> Service for HandlerService<H> {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        self.inner.handle(req)
    }
}

/// Wrap a free async function (or async closure) as a [`Handler`].
///
/// The function must take a [`Request`] and return a future of
/// [`Response`]. It is cloned on each call (cheap — Fn closures
/// hold their captures by reference).
pub fn handler_fn<F, Fut>(f: F) -> HandlerFn<F>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    HandlerFn(f)
}

/// Newtype wrapping a function-style handler. Returned by
/// [`handler_fn`]; rarely constructed directly.
pub struct HandlerFn<F>(F);

impl<F, Fut> Handler for HandlerFn<F>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn handle(&self, req: Request) -> HandlerFuture {
        (self.0)(req).boxed()
    }
}

/// Direct `Service` impl over a closure — useful when an
/// allocation per request is the bottleneck and the caller wants
/// to skip the `Handler` indirection.
pub fn service_fn<F, Fut>(f: F) -> ArcService
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Arc::new(FnService(f))
}

struct FnService<F>(F);

impl<F, Fut> Service for FnService<F>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        (self.0)(req).boxed()
    }
}
