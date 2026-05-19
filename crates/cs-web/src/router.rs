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

struct Route {
    method: Method,
    path: PathKind,
    service: ArcService,
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

    /// Register a route. Returns the router for chaining.
    pub fn route(mut self, method: Method, pattern: &str, service: ArcService) -> Self {
        self.routes.push(Route {
            method,
            path: PathKind::parse(pattern),
            service,
        });
        self
    }

    /// Sugar: register a GET route.
    pub fn get(self, pattern: &str, service: ArcService) -> Self {
        self.route(Method::GET, pattern, service)
    }

    /// Sugar: register a POST route.
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

    /// Dispatch logic — exposed for tests.
    fn pick(&self, method: &Method, path: &str) -> Pick<'_> {
        let mut path_seen = false;
        for r in &self.routes {
            if r.path.matches(path) {
                path_seen = true;
                if &r.method == method {
                    return Pick::Hit(&r.service);
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
    Hit(&'a ArcService),
    MethodNotAllowed,
    NoMatch,
}

impl Service for Router {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        match self.pick(&method, &path) {
            Pick::Hit(svc) => svc.call(req),
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
}
