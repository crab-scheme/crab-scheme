//! `(crab http server)` — synchronous HTTP server via `tiny_http`.
//!
//! Iter 11 of the stdlib-modules spec.
//!
//! Server and request handles are opaque fixnum slots in a process-
//! global slab (same pattern as `cs-stdlib-net`). Each
//! `(http-server-accept ...)` returns a fresh request handle the
//! caller must hand to `(http-respond ...)` exactly once — that
//! consumes the request and writes the response.
//!
//! The accept loop blocks the calling thread. For concurrent
//! request handling, accept in a loop and `(spawn …)` a BEAM actor
//! per request that owns the handle through to `(http-respond …)`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `http-server-bind`   | host port            | server-handle |
//! | `http-server-accept` | server-handle [timeout-ms] | request-handle or #f |
//! | `http-server-close`  | server-handle        | unspec |
//! | `http-request-method`  | request-handle | string |
//! | `http-request-url`     | request-handle | string |
//! | `http-request-headers` | request-handle | alist |
//! | `http-request-body`    | request-handle | bytevector |
//! | `http-respond`         | request-handle status headers body | unspec — consumes the request |

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use tiny_http::{Header, Response, Server};

// Slab entries.
enum Slot {
    Server(Arc<Server>),
    Request(tiny_http::Request),
}

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Slot>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
        })
    })
}

fn lock() -> Result<std::sync::MutexGuard<'static, Registry>, FfiError> {
    registry()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("http-server: registry poisoned: {}", e)))
}

pub(crate) fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("http-server-bind", http_server_bind),
        UntypedProc::new("http-server-accept", http_server_accept),
        UntypedProc::new("http-server-close", http_server_close),
        UntypedProc::new("http-request-method", http_request_method),
        UntypedProc::new("http-request-url", http_request_url),
        UntypedProc::new("http-request-headers", http_request_headers),
        UntypedProc::new("http-request-body", http_request_body),
        UntypedProc::new("http-respond", http_respond),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(n)) => Ok(n.to_f64() as i64),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_bv(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

fn pair(k: &str, v: Value) -> Value {
    Value::Pair(Pair::new(string_value(k.to_string()), v))
}

/// Decode an alist of (name . value) header pairs to a Vec of
/// `tiny_http::Header`. Used by `http-respond`.
fn parse_headers(val: &Value) -> Result<Vec<Header>, FfiError> {
    let mut out = Vec::new();
    let mut cur = val.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                match p.car() {
                    Value::Pair(kv) => match (kv.car(), kv.cdr()) {
                        (Value::String(k), Value::String(v)) => {
                            let header =
                                Header::from_bytes(k.borrow().as_bytes(), v.borrow().as_bytes())
                                    .map_err(|_| {
                                        FfiError::HostFailure(format!(
                                            "http-respond: invalid header: {}: {}",
                                            k.borrow(),
                                            v.borrow()
                                        ))
                                    })?;
                            out.push(header);
                        }
                        _ => {
                            return Err(FfiError::HostFailure(
                                "http-respond: header entries must be (string . string)".into(),
                            ));
                        }
                    },
                    _ => {
                        return Err(FfiError::HostFailure(
                            "http-respond: headers list must contain pairs".into(),
                        ));
                    }
                }
                cur = p.cdr();
            }
            _ => {
                return Err(FfiError::HostFailure(
                    "http-respond: headers must be a proper list".into(),
                ));
            }
        }
    }
}

// ----- server lifecycle -----

fn http_server_bind(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("http-server-bind", args, 0)?;
    let port = expect_fixnum("http-server-bind", args, 1)?;
    let addr = format!("{}:{}", host, port);
    let server = Server::http(&addr)
        .map_err(|e| FfiError::HostFailure(format!("http-server-bind: {}: {}", addr, e)))?;
    let mut r = lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, Slot::Server(Arc::new(server)));
    Ok(Value::fixnum(id))
}

fn http_server_close(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("http-server-close", args, 0)?;
    let mut r = lock()?;
    match r.slots.remove(&id) {
        Some(Slot::Server(_)) => Ok(Value::Unspecified),
        Some(other) => {
            // Put it back if the caller misused us.
            r.slots.insert(id, other);
            Err(FfiError::HostFailure(format!(
                "http-server-close: handle {} is not a server",
                id
            )))
        }
        None => Err(FfiError::HostFailure(format!(
            "http-server-close: bad handle {}",
            id
        ))),
    }
}

fn http_server_accept(args: &[Value]) -> Result<Value, FfiError> {
    let server_id = expect_fixnum("http-server-accept", args, 0)?;
    let timeout = match args.get(1) {
        None => None,
        Some(Value::Number(n)) => Some(Duration::from_millis(n.to_f64() as u64)),
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "fixnum or no arg",
                got: other.type_name().to_string(),
            })
        }
    };

    // Clone the Arc<Server> out before blocking so we don't hold
    // the registry mutex across the accept.
    let server: Arc<Server> = {
        let r = lock()?;
        match r.slots.get(&server_id) {
            Some(Slot::Server(s)) => Arc::clone(s),
            Some(_) => {
                return Err(FfiError::HostFailure(format!(
                    "http-server-accept: handle {} is not a server",
                    server_id
                )));
            }
            None => {
                return Err(FfiError::HostFailure(format!(
                    "http-server-accept: bad handle {}",
                    server_id
                )));
            }
        }
    };

    let req = match timeout {
        None => server
            .recv()
            .map_err(|e| FfiError::HostFailure(format!("http-server-accept: {}", e)))?,
        Some(d) => match server
            .recv_timeout(d)
            .map_err(|e| FfiError::HostFailure(format!("http-server-accept: {}", e)))?
        {
            Some(req) => req,
            None => return Ok(Value::Boolean(false)),
        },
    };

    let mut r = lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, Slot::Request(req));
    Ok(Value::fixnum(id))
}

// ----- request accessors -----

fn with_request<F, R>(name: &str, args: &[Value], idx: usize, f: F) -> Result<R, FfiError>
where
    F: FnOnce(&tiny_http::Request) -> R,
{
    let id = expect_fixnum(name, args, idx)?;
    let r = lock()?;
    match r.slots.get(&id) {
        Some(Slot::Request(req)) => Ok(f(req)),
        Some(_) => Err(FfiError::HostFailure(format!(
            "{}: handle {} is not a request",
            name, id
        ))),
        None => Err(FfiError::HostFailure(format!(
            "{}: bad handle {}",
            name, id
        ))),
    }
}

fn http_request_method(args: &[Value]) -> Result<Value, FfiError> {
    let s = with_request("http-request-method", args, 0, |req| {
        req.method().to_string()
    })?;
    Ok(string_value(s))
}

fn http_request_url(args: &[Value]) -> Result<Value, FfiError> {
    let s = with_request("http-request-url", args, 0, |req| req.url().to_string())?;
    Ok(string_value(s))
}

fn http_request_headers(args: &[Value]) -> Result<Value, FfiError> {
    let entries = with_request("http-request-headers", args, 0, |req| {
        req.headers()
            .iter()
            .map(|h| {
                Value::Pair(Pair::new(
                    string_value(h.field.as_str().to_string()),
                    string_value(h.value.as_str().to_string()),
                ))
            })
            .collect::<Vec<_>>()
    })?;
    Ok(Value::list(entries))
}

fn http_request_body(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("http-request-body", args, 0)?;
    // Body read needs &mut Request — pop the slot, read, reinsert.
    let mut r = lock()?;
    let mut slot = r
        .slots
        .remove(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("http-request-body: bad handle {}", id)))?;
    let bytes = match &mut slot {
        Slot::Request(req) => {
            let mut buf = Vec::new();
            req.as_reader()
                .read_to_end(&mut buf)
                .map_err(|e| FfiError::HostFailure(format!("http-request-body: {}", e)))?;
            buf
        }
        Slot::Server(_) => {
            r.slots.insert(id, slot);
            return Err(FfiError::HostFailure(format!(
                "http-request-body: handle {} is not a request",
                id
            )));
        }
    };
    r.slots.insert(id, slot);
    Ok(bv_value(bytes))
}

// ----- respond (consumes request) -----

fn http_respond(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("http-respond", args, 0)?;
    let status = expect_fixnum("http-respond", args, 1)?;
    let headers_val = args
        .get(2)
        .cloned()
        .ok_or(arity("http-respond", "4", args.len()))?;
    let body = expect_bv("http-respond", args, 3)?;
    let headers = parse_headers(&headers_val)?;

    let mut r = lock()?;
    let slot = r
        .slots
        .remove(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("http-respond: bad handle {}", id)))?;
    drop(r);
    let req = match slot {
        Slot::Request(req) => req,
        Slot::Server(_) => {
            return Err(FfiError::HostFailure(format!(
                "http-respond: handle {} is not a request",
                id
            )));
        }
    };

    let body_len = body.len() as u64;
    let mut resp = Response::new(
        tiny_http::StatusCode(status as u16),
        headers,
        std::io::Cursor::new(body),
        Some(body_len as usize),
        None,
    );
    // tiny_http::Response wants `Read + Send + 'static`; Cursor
    // satisfies that. Set Content-Length explicitly via the
    // `data_length` field above; tiny_http handles the rest.
    let _ = body_len;
    let _ = resp.headers();
    req.respond(resp)
        .map_err(|e| FfiError::HostFailure(format!("http-respond: write: {}", e)))?;
    Ok(Value::Unspecified)
}
