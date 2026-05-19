//! cs-table integration — access log middleware + session store.
//!
//! Two pieces:
//!
//! 1. [`AccessLog`] — a [`Layer`] that records every request in
//!    a cs-table ordered set. Schema:
//!    `Key::Fixnum(seq) -> AccessLogEntry { ts_micros, method,
//!    path, status, elapsed_ms }`. Inspect with the same
//!    Scheme primops (`(table-fold ...)`) you'd use for any
//!    other table — the access log isn't a special-case storage.
//!
//! 2. [`SessionStore`] — a wrapper around a cs-table set that
//!    stores `Arc<T>` sessions keyed by string IDs. Use cases:
//!    cookie-based sessions, OAuth tokens, ephemeral
//!    server-side state. The `T: Any + Send + Sync` constraint
//!    means the consumer crate defines the session shape.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cs_table::{Key, Payload, TableRegistry, TableType};
use futures_util::future::{BoxFuture, FutureExt};
use http::Method;

use crate::{ArcService, Layer, Request, Response, Service};

// ---------------------------------------------------------------
// Access log
// ---------------------------------------------------------------

/// One row recorded by [`AccessLog`].
#[derive(Debug, Clone)]
pub struct AccessLogEntry {
    /// Wall-clock microseconds since the unix epoch. Falls back
    /// to 0 if `SystemTime::now()` is before the epoch — which
    /// only happens on misconfigured hosts.
    pub ts_micros: u128,
    pub method: Method,
    pub path: String,
    pub status: u16,
    pub elapsed_ms: u64,
}

/// Layer that records each request into a cs-table ordered set.
///
/// The table is created on construction if it doesn't already
/// exist. Multiple [`AccessLog`] instances pointed at the same
/// table interleave their writes — sequence numbers come from a
/// shared atomic so they remain strictly increasing within one
/// registry handle but interleave across registries.
pub struct AccessLog {
    registry: TableRegistry,
    table_name: String,
    seq: Arc<AtomicU64>,
}

impl AccessLog {
    /// Build an access log writing to `table_name` in `registry`.
    /// Creates the table as `OrderedSet` if not present; if it
    /// exists with a different type, returns an error.
    pub fn new(
        registry: TableRegistry,
        table_name: impl Into<String>,
    ) -> Result<Self, cs_table::TableError> {
        let table_name = table_name.into();
        match registry.create(&table_name, TableType::OrderedSet) {
            Ok(()) => {}
            Err(cs_table::TableError::AlreadyExists { .. }) => {
                // Make sure the existing table is the right shape.
                let actual = registry.table_type(&table_name)?;
                if actual != TableType::OrderedSet {
                    return Err(cs_table::TableError::WrongType {
                        name: table_name,
                        actual,
                        expected: TableType::OrderedSet,
                    });
                }
            }
            Err(e) => return Err(e),
        }
        Ok(Self {
            registry,
            table_name,
            seq: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Read-only count of recorded rows. Cheap (single table
    /// size lookup).
    pub fn size(&self) -> usize {
        self.registry.size(&self.table_name).unwrap_or(0)
    }

    /// Snapshot every row as `(seq, entry)` pairs sorted by seq.
    /// Useful for tests; production callers should fold instead.
    pub fn snapshot(&self) -> Vec<(u64, AccessLogEntry)> {
        self.registry
            .fold(&self.table_name, Vec::new(), |mut acc, k, v| {
                if let (Key::Fixnum(n), Some(entry)) = (k, v.downcast_ref::<AccessLogEntry>()) {
                    acc.push((*n as u64, entry.clone()));
                }
                acc
            })
            .unwrap_or_default()
    }
}

impl Layer for AccessLog {
    fn layer(&self, inner: ArcService) -> ArcService {
        Arc::new(AccessLogService {
            inner,
            registry: self.registry.clone(),
            table_name: self.table_name.clone(),
            seq: Arc::clone(&self.seq),
        })
    }
}

struct AccessLogService {
    inner: ArcService,
    registry: TableRegistry,
    table_name: String,
    seq: Arc<AtomicU64>,
}

impl Service for AccessLogService {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        let registry = self.registry.clone();
        let table_name = self.table_name.clone();
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        async move {
            let resp = inner.call(req).await;
            let entry = AccessLogEntry {
                ts_micros: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_micros())
                    .unwrap_or(0),
                method,
                path,
                status: resp.status().as_u16(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
            let payload: Payload = Arc::new(entry);
            // i64 cast is fine — the seq is monotonic and grows
            // ~1/req; even at 1 Mrps it takes >292,000 years to
            // overflow.
            let _ = registry.insert(&table_name, Key::Fixnum(seq as i64), payload);
            resp
        }
        .boxed()
    }
}

// ---------------------------------------------------------------
// Session store
// ---------------------------------------------------------------

/// String-keyed session store backed by a cs-table set.
///
/// Sessions are `Arc<T>` where `T: Any + Send + Sync + 'static`.
/// Consumers pick T (e.g. a struct with user_id + expires_at +
/// flash messages). The store is cheap to clone — internals are
/// behind an `Arc` on the registry.
#[derive(Clone)]
pub struct SessionStore {
    registry: TableRegistry,
    table_name: String,
}

impl SessionStore {
    /// Build a store backed by `table_name` in `registry`.
    /// Creates the table as `Set` if absent; refuses to use an
    /// existing table of a different type.
    pub fn new(
        registry: TableRegistry,
        table_name: impl Into<String>,
    ) -> Result<Self, cs_table::TableError> {
        let table_name = table_name.into();
        match registry.create(&table_name, TableType::Set) {
            Ok(()) => {}
            Err(cs_table::TableError::AlreadyExists { .. }) => {
                let actual = registry.table_type(&table_name)?;
                if actual != TableType::Set {
                    return Err(cs_table::TableError::WrongType {
                        name: table_name,
                        actual,
                        expected: TableType::Set,
                    });
                }
            }
            Err(e) => return Err(e),
        }
        Ok(Self {
            registry,
            table_name,
        })
    }

    /// Load a session. Returns `None` if no entry exists or the
    /// entry's type doesn't downcast to `T` (the latter is a
    /// programming error — sessions should be a single type per
    /// store).
    pub fn load<T: 'static + Send + Sync>(&self, id: &str) -> Option<Arc<T>> {
        let payload = self
            .registry
            .lookup(&self.table_name, &Key::String(id.to_string()))
            .ok()
            .flatten()?;
        // `Arc::downcast` requires Arc<dyn Any> not Arc<dyn Any +
        // Send + Sync> — go through a transmute-free downcast via
        // the rec type.
        let any_arc: Arc<dyn std::any::Any + Send + Sync> = payload;
        // Convert Arc<dyn Any + Send + Sync> -> Arc<dyn Any> via
        // the std blanket impl and back via downcast.
        any_arc.downcast::<T>().ok()
    }

    /// Save a session under `id`. Overwrites any existing entry.
    pub fn save<T: 'static + Send + Sync>(&self, id: &str, session: T) {
        let payload: Payload = Arc::new(session);
        let _ = self
            .registry
            .insert(&self.table_name, Key::String(id.to_string()), payload);
    }

    /// Drop a session. Returns true if it was present.
    pub fn delete(&self, id: &str) -> bool {
        self.registry
            .delete(&self.table_name, &Key::String(id.to_string()))
            .unwrap_or(false)
    }

    /// Number of stored sessions.
    pub fn size(&self) -> usize {
        self.registry.size(&self.table_name).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{handler::service_fn, ok, response, Stack};
    use bytes::Bytes;
    use http::StatusCode as Http;

    fn req(method: Method, path: &str) -> Request {
        http::Request::builder()
            .method(method)
            .uri(path)
            .body(Bytes::new())
            .unwrap()
    }

    #[tokio::test]
    async fn access_log_records_every_request() {
        let reg = TableRegistry::new();
        let log = AccessLog::new(reg.clone(), "access").expect("create log");
        let svc = Stack::new()
            .push(log)
            .wrap(service_fn(|r: Request| async move {
                if r.uri().path() == "/boom" {
                    response(Http::INTERNAL_SERVER_ERROR, "oops")
                } else {
                    ok("hi")
                }
            }));

        let _ = svc.call(req(Method::GET, "/a")).await;
        let _ = svc.call(req(Method::POST, "/b")).await;
        let _ = svc.call(req(Method::GET, "/boom")).await;

        let log = AccessLog::new(reg.clone(), "access").expect("reopen log");
        let rows = log.snapshot();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1.method, Method::GET);
        assert_eq!(rows[0].1.path, "/a");
        assert_eq!(rows[0].1.status, 200);
        assert_eq!(rows[1].1.method, Method::POST);
        assert_eq!(rows[1].1.path, "/b");
        assert_eq!(rows[2].1.path, "/boom");
        assert_eq!(rows[2].1.status, 500);
        // Monotonic seq.
        assert!(rows[0].0 < rows[1].0);
        assert!(rows[1].0 < rows[2].0);
    }

    #[tokio::test]
    async fn access_log_wrong_existing_type_errors() {
        let reg = TableRegistry::new();
        // Pre-create as Set; AccessLog wants OrderedSet.
        reg.create("ax", TableType::Set).unwrap();
        let res = AccessLog::new(reg, "ax");
        assert!(matches!(res, Err(cs_table::TableError::WrongType { .. })));
    }

    #[derive(Debug)]
    struct Session {
        user: String,
        flash: Option<String>,
    }

    #[test]
    fn session_store_round_trip() {
        let reg = TableRegistry::new();
        let store = SessionStore::new(reg.clone(), "sessions").expect("create");

        store.save(
            "abc",
            Session {
                user: "alice".to_string(),
                flash: Some("welcome".to_string()),
            },
        );
        assert_eq!(store.size(), 1);

        let loaded: Arc<Session> = store.load("abc").expect("present");
        assert_eq!(loaded.user, "alice");
        assert_eq!(loaded.flash.as_deref(), Some("welcome"));

        assert!(store.delete("abc"));
        assert_eq!(store.size(), 0);
        assert!(store.load::<Session>("abc").is_none());
    }

    #[test]
    fn session_store_missing_id_returns_none() {
        let reg = TableRegistry::new();
        let store = SessionStore::new(reg, "sessions").unwrap();
        assert!(store.load::<Session>("never-set").is_none());
    }

    // Multiple SessionStore handles for the same table share state.
    #[test]
    fn session_store_handles_share_storage() {
        let reg = TableRegistry::new();
        let a = SessionStore::new(reg.clone(), "shared").unwrap();
        let b = SessionStore::new(reg, "shared").unwrap();
        a.save(
            "k",
            Session {
                user: "u".into(),
                flash: None,
            },
        );
        let loaded: Arc<Session> = b.load("k").expect("visible via second handle");
        assert_eq!(loaded.user, "u");
    }
}
