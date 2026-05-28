//! WASI HTTP server stub (#9 iter-3). Compiled only on
//! `target_os = "wasi"`. Registers the same proc names as the native
//! server (`http-server-bind`, `http-server-accept`, …) so a Scheme
//! program importing them resolves cleanly, but every call raises
//! `HostFailure` with a pointer to the wasi:http/incoming-handler shape
//! that iter-5 will provide. This matches the iter-19 stdlib-modules
//! pattern for wasi-incompatible procs (see ADR 0033).
//!
//! The `tiny_http` accept-loop shape doesn't translate to
//! `wasi:sockets 0.2` (no socket creation), so a real wasi server
//! goes via the handler-callback model in iter-5, not these procs.

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

fn raise(name: &'static str) -> impl Fn(&[Value]) -> Result<Value, FfiError> {
    move |_args: &[Value]| -> Result<Value, FfiError> {
        Err(FfiError::HostFailure(format!(
            "{}: not supported on wasm32-wasi — use the wasi:http \
             incoming-handler shape (iter-5)",
            name
        )))
    }
}

pub(crate) fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
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
