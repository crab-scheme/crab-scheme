//! WASI HTTP client back-end (#9 iter-3) — `wasi-http-client` impl,
//! binding the `wasi:http/0.2.0` outgoing-handler world. Compiled only
//! on `target_os = "wasi"` (the crate's `wasi:` imports are
//! WASI-component-only). Exposes the same `run_request` signature +
//! response-alist shape as `client_native.rs`. Requires Wasmtime 28+
//! at runtime (see ADR 0033).

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use wasi_http_client::{Client, Method};

use super::{bv_value, pair, string_value};

pub(super) fn run_request(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    headers: &[(String, String)],
) -> Result<Value, FfiError> {
    // `Method` is re-exported from `wasi::http::types::Method`. Cover the
    // common verbs explicitly; fall through to `Other` so an arbitrary
    // method (e.g. WebDAV `PROPFIND`) still flows through.
    let m = match method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "HEAD" => Method::Head,
        "PATCH" => Method::Patch,
        "OPTIONS" => Method::Options,
        "TRACE" => Method::Trace,
        "CONNECT" => Method::Connect,
        other => Method::Other(other.to_string()),
    };
    let mut req = Client::new().request(m, url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = body {
        if !b.is_empty() {
            req = req.body(b);
        }
    }
    let resp = req
        .send()
        .map_err(|e| FfiError::HostFailure(format!("http {} {}: {}", method, url, e)))?;
    Ok(response_to_value(resp))
}

fn response_to_value(resp: wasi_http_client::Response) -> Value {
    let status = resp.status() as i64;
    // `headers()` returns `&HashMap<String, String>`. Materialize as an
    // alist preserving every header (order is HashMap-undefined, which
    // is acceptable — the native back-end's ordering is ureq-internal
    // too, and callers should treat the headers alist as a map).
    let headers_alist: Vec<Value> = resp
        .headers()
        .iter()
        .map(|(k, v)| Value::Pair(Pair::new(string_value(k.clone()), string_value(v.clone()))))
        .collect();
    // `body()` consumes the response and returns Result<Vec<u8>>. On
    // body-read error treat as empty; status + headers stay accurate.
    let body = resp.body().unwrap_or_default();
    Value::list(vec![
        pair("status", Value::fixnum(status)),
        pair("headers", Value::list(headers_alist)),
        pair("body", bv_value(body)),
    ])
}
