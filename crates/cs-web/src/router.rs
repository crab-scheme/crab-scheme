//! Router — `Service` that dispatches by method + path.
//!
//! Path matching is currently literal-only: `"/api/users"` matches
//! exactly that path. A trailing `*` makes it a prefix match:
//! `"/static/*"` matches `"/static/foo.css"`. Parameterized
//! segments (e.g. `/users/:id`) are a follow-up — the design
//! anticipates them but the build-out is M2+.
//!
//! Method matching is exact. The same path can be registered for
//! multiple methods (e.g. GET + POST on `"/items"`); a request
//! whose method doesn't match returns 405.
//!
//! Composition:
//! - [`Router::merge`] concatenates another router's routes.
//! - [`Router::nest`] mounts a sub-router under a path prefix,
//!   stripping the prefix before sub-dispatch — so each module
//!   can register routes relative to its own root.
//! - [`Router::add_sink`] pulls routes from a [`RouteSink`] (the
//!   target a cdylib module fills in via [`crate::module`]).

use std::sync::Arc;

use futures_util::future::BoxFuture;
use http::{Method, StatusCode};

use crate::{not_found, response, ArcService, Request, Response, Service};

/// Internal: how the path is matched.
#[derive(Debug, Clone)]
enum PathKind {
    Exact(String),
    Prefix(String),
}

impl PathKind {
    fn parse(pattern: &str) -> Self {
        if let Some(prefix) = pattern.strip_suffix("/*") {
            PathKind::Prefix(prefix.to_string())
        } else if pattern == "*" {
            PathKind::Prefix(String::new())
        } else {
            PathKind::Exact(pattern.to_string())
        }
    }

    fn matches(&self, path: &str) -> bool {
        match self {
            PathKind::Exact(p) => p == path,
            PathKind::Prefix(p) => path.starts_with(p.as_str()),
        }
    }
}

/// Internal: leaf route (method + service) vs nested sub-router.
enum Target {
    /// Single handler bound to one method.
    Leaf { method: Method, service: ArcService },
    /// Nested sub-router. The prefix is stripped from the request
    /// URI before the sub-router dispatches.
    Nest { prefix: String, sub: Arc<Router> },
}

struct Route {
    path: PathKind,
    target: Target,
}

/// Bag of `(method, pattern, service)` triples. Used by module
/// loaders to hand routes back to the host; users typically don't
/// construct one directly.
#[derive(Default)]
pub struct RouteSink {
    routes: Vec<(Method, String, ArcService)>,
}

impl RouteSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, method: Method, pattern: impl Into<String>, service: ArcService) {
        self.routes.push((method, pattern.into(), service));
    }

    pub fn get(&mut self, pattern: impl Into<String>, service: ArcService) {
        self.add(Method::GET, pattern, service);
    }

    pub fn post(&mut self, pattern: impl Into<String>, service: ArcService) {
        self.add(Method::POST, pattern, service);
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (Method, String, ArcService)> + '_ {
        self.routes.drain(..)
    }
}

/// HTTP router. Routes are tested in insertion order; first match
/// wins. Fall back to a customizable 404 handler.
pub struct Router {
    routes: Vec<Route>,
    fallback: ArcService,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            fallback: Arc::new(NotFound),
        }
    }

    /// Register a leaf route.
    pub fn route(mut self, method: Method, pattern: &str, service: ArcService) -> Self {
        self.routes.push(Route {
            path: PathKind::parse(pattern),
            target: Target::Leaf { method, service },
        });
        self
    }

    /// Sugar: register a GET leaf route.
    pub fn get(self, pattern: &str, service: ArcService) -> Self {
        self.route(Method::GET, pattern, service)
    }

    /// Sugar: register a POST leaf route.
    pub fn post(self, pattern: &str, service: ArcService) -> Self {
        self.route(Method::POST, pattern, service)
    }

    /// Override the fallback (default 404).
    pub fn fallback(mut self, service: ArcService) -> Self {
        self.fallback = service;
        self
    }

    /// Wrap the router into an `ArcService`.
    pub fn into_service(self) -> ArcService {
        Arc::new(self)
    }

    /// Sugar for `stack.wrap(self.into_service())`.
    pub fn with_stack(self, stack: &crate::Stack) -> ArcService {
        stack.wrap(self.into_service())
    }

    /// Pull every route from a [`RouteSink`] into this router.
    /// Used by [`crate::module`] when loading a cdylib plugin.
    pub fn add_sink(mut self, mut sink: RouteSink) -> Self {
        for (method, pattern, service) in sink.drain() {
            self = self.route(method, &pattern, service);
        }
        self
    }

    /// Merge another router's routes into this one. The other
    /// router's fallback is discarded — composition keeps the
    /// caller's fallback.
    pub fn merge(mut self, other: Router) -> Self {
        for r in other.routes {
            self.routes.push(r);
        }
        self
    }

    /// Mount a sub-router under a path prefix. Requests under
    /// `prefix` have the prefix stripped before sub-dispatch — so
    /// a sub-router that registered `"/users"` mounted at `"/api"`
    /// answers `GET /api/users`.
    ///
    /// Trailing slashes in `prefix` are normalized away. An empty
    /// or `"/"` prefix delegates to [`merge`].
    pub fn nest(mut self, prefix: &str, sub: Router) -> Self {
        let prefix = prefix.trim_end_matches('/').to_string();
        if prefix.is_empty() {
            return self.merge(sub);
        }
        self.routes.push(Route {
            path: PathKind::Prefix(prefix.clone()),
            target: Target::Nest {
                prefix,
                sub: Arc::new(sub),
            },
        });
        self
    }

    /// Find a matching target for (method, path). Returns the
    /// service to call (with the original request) — for
    /// nested sub-routers the caller must strip the prefix first;
    /// that work is done in `call`.
    fn pick<'a>(&'a self, method: &Method, path: &str) -> Pick<'a> {
        let mut path_seen = false;
        for r in &self.routes {
            if !r.path.matches(path) {
                continue;
            }
            match &r.target {
                Target::Leaf { method: m, service } => {
                    path_seen = true;
                    if m == method {
                        return Pick::Leaf(service);
                    }
                }
                // A nested sub-router accepts any method — let it
                // decide whether to 404/405. This means a nested
                // mount masks both wrong-method and not-found from
                // the parent, which is consistent with how axum/
                // express compose sub-apps.
                Target::Nest { prefix, sub } => {
                    return Pick::Nested(sub, prefix);
                }
            }
        }
        if path_seen {
            Pick::MethodNotAllowed
        } else {
            Pick::NoMatch
        }
    }
}

enum Pick<'a> {
    Leaf(&'a ArcService),
    Nested(&'a Arc<Router>, &'a str),
    MethodNotAllowed,
    NoMatch,
}

impl Service for Router {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        match self.pick(&method, &path) {
            Pick::Leaf(svc) => svc.call(req),
            Pick::Nested(sub, prefix) => {
                // Rewrite the URI to drop the prefix before
                // forwarding to the sub-router. An empty
                // remainder means the request hit exactly the
                // mount point — sub-router gets "/".
                let suffix = &path[prefix.len()..];
                let new_path = if suffix.is_empty() { "/" } else { suffix };
                let new_uri = rebuild_uri(req.uri(), new_path);
                let sub = Arc::clone(sub);
                let mut req = req;
                *req.uri_mut() = new_uri;
                Box::pin(async move { sub.call(req).await })
            }
            Pick::MethodNotAllowed => {
                let r = response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
                Box::pin(async move { r })
            }
            Pick::NoMatch => {
                let svc = Arc::clone(&self.fallback);
                svc.call(req)
            }
        }
    }
}

/// Build a new URI from `orig` with `new_path` replacing the path
/// component (and preserving the query if any). Falls back to a
/// parse-from-string path if the typed builder rejects.
fn rebuild_uri(orig: &http::Uri, new_path: &str) -> http::Uri {
    let path_and_query = match orig.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let mut parts = orig.clone().into_parts();
    parts.path_and_query = path_and_query.parse().ok();
    http::Uri::from_parts(parts).unwrap_or_else(|_| http::Uri::from_static("/"))
}

struct NotFound;

impl Service for NotFound {
    fn call(&self, _req: Request) -> BoxFuture<'static, Response> {
        Box::pin(async move { not_found() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{handler::service_fn, ok};
    use bytes::Bytes;
    use http::Method;

    fn req(method: Method, path: &str) -> Request {
        http::Request::builder()
            .method(method)
            .uri(path)
            .body(Bytes::new())
            .unwrap()
    }

    #[tokio::test]
    async fn exact_match_dispatches() {
        let r = Router::new()
            .get("/", service_fn(|_| async { ok("root") }))
            .get("/foo", service_fn(|_| async { ok("foo") }))
            .into_service();
        let resp = r.call(req(Method::GET, "/foo")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), &Bytes::from_static(b"foo"));
    }

    #[tokio::test]
    async fn prefix_match_routes_subpath() {
        let r = Router::new()
            .get("/static/*", service_fn(|_| async { ok("asset") }))
            .into_service();
        let resp = r.call(req(Method::GET, "/static/css/site.css")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), &Bytes::from_static(b"asset"));
    }

    #[tokio::test]
    async fn no_route_returns_404() {
        let r = Router::new()
            .get("/foo", service_fn(|_| async { ok("foo") }))
            .into_service();
        let resp = r.call(req(Method::GET, "/missing")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_method_returns_405() {
        let r = Router::new()
            .get("/foo", service_fn(|_| async { ok("foo") }))
            .into_service();
        let resp = r.call(req(Method::POST, "/foo")).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn first_match_wins() {
        let r = Router::new()
            .get("/a", service_fn(|_| async { ok("first") }))
            .get("/a", service_fn(|_| async { ok("second") }))
            .into_service();
        let resp = r.call(req(Method::GET, "/a")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"first"));
    }

    #[tokio::test]
    async fn custom_fallback() {
        let r = Router::new()
            .fallback(service_fn(|_| async {
                response(StatusCode::IM_A_TEAPOT, "teapot")
            }))
            .into_service();
        let resp = r.call(req(Method::GET, "/anywhere")).await;
        assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
    }

    #[tokio::test]
    async fn merge_combines_routes() {
        let a = Router::new().get("/a", service_fn(|_| async { ok("a") }));
        let b = Router::new().get("/b", service_fn(|_| async { ok("b") }));
        let svc = a.merge(b).into_service();
        let resp = svc.call(req(Method::GET, "/a")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"a"));
        let resp = svc.call(req(Method::GET, "/b")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"b"));
    }

    #[tokio::test]
    async fn nest_strips_prefix_before_sub_dispatch() {
        let api = Router::new()
            .get("/users", service_fn(|_| async { ok("users") }))
            .get("/items", service_fn(|_| async { ok("items") }));
        let svc = Router::new().nest("/api", api).into_service();

        let resp = svc.call(req(Method::GET, "/api/users")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"users"));

        let resp = svc.call(req(Method::GET, "/api/items")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"items"));
    }

    #[tokio::test]
    async fn nested_router_inherits_its_own_fallback() {
        let api = Router::new()
            .get("/users", service_fn(|_| async { ok("users") }))
            .fallback(service_fn(|_| async {
                response(StatusCode::IM_A_TEAPOT, "api-fallback")
            }));
        let svc = Router::new().nest("/api", api).into_service();

        let resp = svc.call(req(Method::GET, "/api/missing")).await;
        assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(resp.body(), &Bytes::from_static(b"api-fallback"));
    }

    #[tokio::test]
    async fn nest_preserves_query_string() {
        let api = Router::new().get(
            "/echo",
            service_fn(|r: Request| async move { ok(r.uri().to_string()) }),
        );
        let svc = Router::new().nest("/api", api).into_service();
        let resp = svc.call(req(Method::GET, "/api/echo?x=1&y=2")).await;
        let body = String::from_utf8(resp.body().to_vec()).unwrap();
        assert!(body.contains("x=1"));
        assert!(body.contains("y=2"));
        // Path should be stripped of the prefix.
        assert!(body.starts_with("/echo"));
    }

    #[tokio::test]
    async fn nest_at_root_falls_back_to_merge() {
        let inner = Router::new().get("/x", service_fn(|_| async { ok("x") }));
        let svc = Router::new().nest("/", inner).into_service();
        let resp = svc.call(req(Method::GET, "/x")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"x"));
    }

    #[tokio::test]
    async fn route_sink_drained_into_router() {
        let mut sink = RouteSink::new();
        sink.get("/from-sink", service_fn(|_| async { ok("sunk") }));
        assert_eq!(sink.len(), 1);

        let svc = Router::new().add_sink(sink).into_service();
        let resp = svc.call(req(Method::GET, "/from-sink")).await;
        assert_eq!(resp.body(), &Bytes::from_static(b"sunk"));
    }
}
