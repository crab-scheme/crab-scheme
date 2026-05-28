//! WASI HTTP server (#9 iter-5) — `wasi:http/incoming-handler` shape.
//!
//! `wasi:sockets 0.2`'s missing socket-creation means the native
//! accept-loop model (`http-server-bind` → `http-server-accept` →
//! `http-respond`) cannot be implemented on `wasm32-wasip2`. Instead
//! wasi:http exposes a handler-callback world: the runtime calls into
//! the WASM component per request via the `handle(request, response-
//! out)` export, the component fills the response, the runtime writes
//! it back.
//!
//! This module:
//!   1. Registers `http-incoming-handler` — a Scheme proc that
//!      stashes the user's lambda in a thread-local.
//!   2. Implements `wasi::exports::http::incoming_handler::Guest::handle`
//!      and exports it at the component level via
//!      `wasi::http::proxy::export!`.
//!   3. Keeps the accept-loop procs (`http-server-bind`/`-accept`/
//!      `-respond`/...) registered but raising `HostFailure`, so a
//!      Scheme program that imports them on wasi gets a clear error
//!      rather than an unbound-identifier crash.
//!
//! ## Status
//!
//! Iter-5 ships the wasi:http integration + `http-incoming-handler`
//! registration. **Iter-5b** wires the actual Scheme-lambda invocation
//! from `handle()` (needs Runtime-accessible-from-static-context
//! plumbing — the lambda holds Rc state pinned to the thread that
//! registered it, and `handle()` runs in the same wasi-component
//! thread but without an active cs-runtime evaluation context). Until
//! then `handle()` returns a 503-style placeholder if no lambda is
//! registered and a 200 acknowledgement if one is — enough to prove
//! the component-export wiring works under `wasmtime serve`.

use std::cell::RefCell;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

// Thread-local stash. Wasm components on `wasi:http/proxy` are
// single-threaded today, so a `thread_local!` matches the runtime's
// concurrency model (and sidesteps the `Send + Sync` requirement on
// `OnceLock` that `Value` — `Rc`-heavy — cannot meet).
thread_local! {
    static REGISTERED_HANDLER: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// `(http-incoming-handler lambda)` — register a Scheme procedure to
/// be invoked by the runtime per incoming HTTP request. The wasi:http
/// `handle(request, response-out)` export looks up the stashed lambda
/// + invokes it (iter-5b will perform the actual call).
fn http_incoming_handler(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(FfiError::ArityError {
            name: "http-incoming-handler".into(),
            expected: "1".into(),
            got: args.len(),
        });
    }
    let lambda = args[0].clone();
    if !matches!(lambda, Value::Procedure(_)) {
        return Err(FfiError::TypeMismatch {
            expected: "procedure",
            got: lambda.type_name().to_string(),
        });
    }
    REGISTERED_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(lambda);
    });
    Ok(Value::Boolean(true))
}

/// Returns whether the runtime currently has a Scheme handler
/// registered. `wasi:http/incoming-handler::handle` uses this to
/// distinguish "module loaded but no handler" (503) from "ready to
/// dispatch" (200) until iter-5b wires the actual call-through.
pub(crate) fn has_registered_handler() -> bool {
    REGISTERED_HANDLER.with(|cell| cell.borrow().is_some())
}

fn raise(name: &'static str) -> impl Fn(&[Value]) -> Result<Value, FfiError> {
    move |_args: &[Value]| -> Result<Value, FfiError> {
        Err(FfiError::HostFailure(format!(
            "{}: not supported on wasm32-wasi — use (http-incoming-handler …) \
             with the wasi:http/incoming-handler world (ADR 0033)",
            name
        )))
    }
}

pub(crate) fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        // New on wasi — the handler-callback shape.
        UntypedProc::new("http-incoming-handler", http_incoming_handler),
        // Native accept-loop shape — not portable to wasi (no socket
        // creation in wasi:sockets 0.2). Raise on call so a Scheme
        // program gets a clear error at the call site rather than an
        // unbound-identifier failure at import time.
        UntypedProc::new("http-server-bind", raise("http-server-bind")),
        UntypedProc::new("http-server-accept", raise("http-server-accept")),
        UntypedProc::new("http-server-close", raise("http-server-close")),
        UntypedProc::new("http-request-method", raise("http-request-method")),
        UntypedProc::new("http-request-url", raise("http-request-url")),
        UntypedProc::new("http-request-headers", raise("http-request-headers")),
        UntypedProc::new("http-request-body", raise("http-request-body")),
        UntypedProc::new("http-respond", raise("http-respond")),
    ]
}

// ---- wasi:http/incoming-handler integration ----
//
// `wasi::http::proxy::export!(CsHttpHandler)` declares the component
// exports for the `wasi:http/proxy` world. The Guest impl below is the
// per-request entry point the runtime calls via
// `wasi:http/incoming-handler::handle`.

use wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct CsHttpHandler;

wasi::http::proxy::export!(CsHttpHandler);

impl wasi::exports::http::incoming_handler::Guest for CsHttpHandler {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // Iter-5 scope: prove the export is wired by reflecting handler
        // registration state in the response. Iter-5b replaces the
        // placeholder body with the actual Scheme-lambda invocation.
        let (status, body_bytes): (u16, &[u8]) = if has_registered_handler() {
            (
                200,
                b"crabscheme wasi:http handler registered (Scheme-lambda \
                  invocation lands in #9 iter-5b)",
            )
        } else {
            (
                503,
                b"crabscheme: no Scheme handler registered. Call \
                  (http-incoming-handler <lambda>) at startup.",
            )
        };

        let resp = OutgoingResponse::new(Fields::new());
        // `set_status_code` returns Err only on an invalid code; 200/503
        // are always valid, so ignore.
        let _ = resp.set_status_code(status);
        let body = match resp.body() {
            Ok(b) => b,
            Err(_) => return,
        };
        ResponseOutparam::set(response_out, Ok(resp));

        if let Ok(mut out) = body.write() {
            let _ = std::io::Write::write_all(&mut out, body_bytes);
            let _ = std::io::Write::flush(&mut out);
            drop(out);
        }
        let _ = OutgoingBody::finish(body, None);
    }
}
