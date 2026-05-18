//! CrabScheme stdlib module: `(crab url)`.
//!
//! URL parsing + percent-encoding via the `url` and
//! `percent-encoding` Rust crates. Iter 6 of the `stdlib-modules`
//! spec.
//!
//! A parsed URL is exposed as an alist of `(component . value)`
//! pairs — `scheme`, `host`, `port`, `path`, `query`, `fragment`,
//! `username`, `password`. Missing components have `#f` as value.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `url-parse`     | string  | alist     | `(("scheme" . "https") …)`. Errors on malformed input. |
//! | `url-scheme`    | string  | string    | Convenience accessor; returns "" on no scheme. |
//! | `url-host`      | string  | string or #f | |
//! | `url-encode`    | string  | string    | Percent-encode all non-unreserved bytes. |
//! | `url-decode`    | string  | string    | Percent-decode; errors on invalid UTF-8. |

use std::sync::Arc;

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use url::Url;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("url-parse", url_parse),
        UntypedProc::new("url-scheme", url_scheme),
        UntypedProc::new("url-host", url_host),
        UntypedProc::new("url-encode", url_encode_proc),
        UntypedProc::new("url-decode", url_decode_proc),
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
            expected: "string".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn opt_string_value(o: Option<String>) -> Value {
    o.map_or(Value::Boolean(false), string_value)
}

fn pair(k: &str, v: Value) -> Value {
    Value::Pair(Pair::new(string_value(k.to_string()), v))
}

// ----- procedures -----

fn url_parse(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("url-parse", args, 0)?;
    let u = Url::parse(&s).map_err(|e| FfiError::HostFailure(format!("url-parse: {}", e)))?;
    let entries = vec![
        pair("scheme", string_value(u.scheme().to_string())),
        pair("host", opt_string_value(u.host_str().map(str::to_owned))),
        pair(
            "port",
            u.port()
                .map_or(Value::Boolean(false), |p| Value::fixnum(p as i64)),
        ),
        pair("path", string_value(u.path().to_string())),
        pair("query", opt_string_value(u.query().map(str::to_owned))),
        pair(
            "fragment",
            opt_string_value(u.fragment().map(str::to_owned)),
        ),
        pair(
            "username",
            // `Url::username()` returns "" when none; preserve "" on
            // empty so callers can distinguish a known-no-user URL
            // from one we haven't parsed yet.
            string_value(u.username().to_string()),
        ),
        pair(
            "password",
            opt_string_value(u.password().map(str::to_owned)),
        ),
    ];
    Ok(Value::list(entries))
}

fn url_scheme(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("url-scheme", args, 0)?;
    match Url::parse(&s) {
        Ok(u) => Ok(string_value(u.scheme().to_string())),
        Err(_) => Ok(string_value(String::new())),
    }
}

fn url_host(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("url-host", args, 0)?;
    match Url::parse(&s)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
    {
        Some(h) => Ok(string_value(h)),
        None => Ok(Value::Boolean(false)),
    }
}

fn url_encode_proc(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("url-encode", args, 0)?;
    Ok(string_value(
        utf8_percent_encode(&s, NON_ALPHANUMERIC).to_string(),
    ))
}

fn url_decode_proc(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("url-decode", args, 0)?;
    percent_decode_str(&s)
        .decode_utf8()
        .map(|cow| string_value(cow.into_owned()))
        .map_err(|e| FfiError::HostFailure(format!("url-decode: {}", e)))
}
