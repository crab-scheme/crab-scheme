//! Native HTTP client back-end (#9 iter-3) — `ureq` impl. Doesn't
//! build on any `wasm32-*` target; cfg'd into the build by `client.rs`
//! only when `target_os != "wasi"`. Exposes the same `run_request`
//! signature + response-alist shape as `client_wasi.rs`.

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;

use super::{bv_value, pair, string_value};

pub(super) fn run_request(
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
            )));
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

    // Slurp body to bytes (cap at 32 MB so a malicious server can't OOM
    // us silently). A future iter wires a streaming port wrapper.
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
