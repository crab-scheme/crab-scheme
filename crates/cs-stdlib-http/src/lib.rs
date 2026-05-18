//! CrabScheme stdlib module: `(crab http client)`.
//!
//! Synchronous HTTP client via `ureq` (TLS via rustls). Iter 10 of
//! the `stdlib-modules` spec. Supersedes the example `cs-ffi-http`
//! crate.
//!
//! All requests block the calling thread until the response is
//! fully received. For concurrency, drive these from BEAM actors.
//! Streaming-body variants land when `Value::Opaque` enables a
//! port wrapper over a `ureq::Response`.
//!
//! Headers are passed as a Scheme alist `(("Name" . "value") …)`
//! on both request and response sides. Bodies are bytevectors
//! (empty bytevector means "no body").
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `http-get`     | url [headers]          | response-alist |
//! | `http-post`    | url body [headers]     | response-alist |
//! | `http-put`     | url body [headers]     | response-alist |
//! | `http-delete`  | url [headers]          | response-alist |
//! | `http-request` | method url body headers| response-alist |
//!
//! Response alist shape:
//!
//! ```scheme
//! (("status"  . 200)                   ; fixnum
//!  ("headers" . (("Content-Type" . "application/json") …))
//!  ("body"    . #vu8(…)))              ; bytevector
//! ```

use std::sync::Arc;

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("http-get", http_get),
        UntypedProc::new("http-post", http_post),
        UntypedProc::new("http-put", http_put),
        UntypedProc::new("http-delete", http_delete),
        UntypedProc::new("http-request", http_request),
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

/// Decode an optional headers arg (Scheme alist of (name . value)
/// pairs) into Vec<(String, String)>. Missing arg returns empty.
fn opt_headers(args: &[Value], idx: usize) -> Result<Vec<(String, String)>, FfiError> {
    let Some(val) = args.get(idx) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut cur = val.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                match p.car() {
                    Value::Pair(kv) => match (kv.car(), kv.cdr()) {
                        (Value::String(k), Value::String(v)) => {
                            out.push((k.borrow().clone(), v.borrow().clone()));
                        }
                        _ => {
                            return Err(FfiError::HostFailure(
                                "http: headers entries must be (string . string) pairs".into(),
                            ));
                        }
                    },
                    _ => {
                        return Err(FfiError::HostFailure(
                            "http: headers list must contain pairs".into(),
                        ));
                    }
                }
                cur = p.cdr();
            }
            _ => {
                return Err(FfiError::HostFailure(
                    "http: headers must be a proper list of pairs".into(),
                ));
            }
        }
    }
}

fn run_request(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    headers: &[(String, String)],
) -> Result<Value, FfiError> {
    let mut req = ureq::request(method, url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp_result = match body {
        Some(b) if !b.is_empty() => req.send_bytes(b),
        _ => req.call(),
    };
    let resp = match resp_result {
        Ok(r) => r,
        // ureq distinguishes "got a response, status != 2xx" via
        // `Error::Status`, where the response body is still
        // available. Surface both shapes as the same response-alist.
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => {
            return Err(FfiError::HostFailure(format!(
                "http {} {}: {}",
                method, url, e
            )))
        }
    };
    Ok(response_to_value(resp))
}

fn response_to_value(resp: ureq::Response) -> Value {
    let status = resp.status() as i64;
    let header_names: Vec<String> = resp.headers_names();
    let headers_alist: Vec<Value> = header_names
        .iter()
        .filter_map(|name| {
            resp.header(name)
                .map(|val| Value::Pair(Pair::new(string_value(name.clone()), string_value(val))))
        })
        .collect();

    // Slurp body to bytes (cap at 32 MB so a malicious server can't
    // OOM us silently). Future iter wires a streaming port wrapper.
    use std::io::Read;
    let mut buf = Vec::new();
    let _ = resp
        .into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut buf);

    Value::list(vec![
        pair("status", Value::fixnum(status)),
        pair("headers", Value::list(headers_alist)),
        pair("body", bv_value(buf)),
    ])
}

// ----- procedures -----

fn http_get(args: &[Value]) -> Result<Value, FfiError> {
    let url = expect_string("http-get", args, 0)?;
    let headers = opt_headers(args, 1)?;
    run_request("GET", &url, None, &headers)
}

fn http_post(args: &[Value]) -> Result<Value, FfiError> {
    let url = expect_string("http-post", args, 0)?;
    let body = expect_bv("http-post", args, 1)?;
    let headers = opt_headers(args, 2)?;
    run_request("POST", &url, Some(&body), &headers)
}

fn http_put(args: &[Value]) -> Result<Value, FfiError> {
    let url = expect_string("http-put", args, 0)?;
    let body = expect_bv("http-put", args, 1)?;
    let headers = opt_headers(args, 2)?;
    run_request("PUT", &url, Some(&body), &headers)
}

fn http_delete(args: &[Value]) -> Result<Value, FfiError> {
    let url = expect_string("http-delete", args, 0)?;
    let headers = opt_headers(args, 1)?;
    run_request("DELETE", &url, None, &headers)
}

fn http_request(args: &[Value]) -> Result<Value, FfiError> {
    let method = expect_string("http-request", args, 0)?;
    let url = expect_string("http-request", args, 1)?;
    let body = expect_bv("http-request", args, 2)?;
    let headers = opt_headers(args, 3)?;
    run_request(&method, &url, Some(&body), &headers)
}
