//! CrabScheme FFI plugin: synchronous HTTP GET via `reqwest::blocking`.
//!
//! Demonstrates a **native-only** plugin pattern: this plugin depends
//! on `reqwest`, which uses `tokio` under the hood (the `blocking`
//! feature wraps the async core in a per-call `tokio::runtime` and
//! `block_on`s the future). That works on native targets but doesn't
//! compile cleanly for `wasm32-wasip1` — tokio's full reactor needs
//! OS-level epoll/kqueue/IOCP which WASI doesn't yet expose. The
//! eventual fix is the WASI Preview 2 wasi-http component model
//! (still ecosystem-pending — see `docs/milestones/m10-trackW-exit.md`
//! for the WASM scope decision).
//!
//! Built as a cdylib; users load it via:
//!
//!   `(load-shared-library "target/release/libcs_ffi_http.dylib")`
//!
//! Then `(http-get url)` takes any string URL, blocks on the
//! request, and returns the body as a Scheme string. Powered by
//! the cs-ffi v2 decoder callbacks (`decode_string`).
//!
//! ## Scheme surface
//!
//!   `(http-get url)` → string
//!     `url` must be a string (`http:` or `https:`); blocks until
//!     the response arrives. Errors flatten to a generic EvalError.

use std::os::raw::c_char;

use cs_ffi::abi::{
    EvalOutput, EvalStatus, HostProcDecl, RegHandle, RuntimeFfi, ValueRef,
    CRABSCHEME_FFI_API_VERSION,
};

/// Status code returned by [`crabscheme_register`].
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterStatus {
    Ok = 0,
    VersionMismatch = 1,
    NullFunctionPointer = 2,
}

/// dlopen entry point. The runtime invokes this immediately after
/// `dlopen`'ing this cdylib.
///
/// # Safety
///
/// `rt` must point to a valid `RuntimeFfi` whose `api_version` is
/// at offset 0. The runtime guarantees this for the call's
/// duration.
#[no_mangle]
pub extern "C" fn crabscheme_register(rt: *mut RuntimeFfi) -> i32 {
    if rt.is_null() {
        return RegisterStatus::NullFunctionPointer as i32;
    }
    let host_version = unsafe { (*rt).api_version };
    if host_version != CRABSCHEME_FFI_API_VERSION {
        return RegisterStatus::VersionMismatch as i32;
    }

    let name = b"http-get";
    let decl = HostProcDecl {
        name_ptr: name.as_ptr() as *const c_char,
        name_len: name.len(),
        call: http_get_call,
        arity: 1,
    };
    let _: RegHandle = unsafe { ((*rt).register_proc)(rt, &decl as *const _) };

    RegisterStatus::Ok as i32
}

/// C-ABI thunk for `(http-get url)`. Decodes the URL string via
/// the cs-ffi v2 `decode_string` callback, calls
/// `reqwest::blocking::get`, returns the body via `alloc_string`.
///
/// `reqwest::blocking::get` spawns a `tokio::runtime::Runtime`
/// internally and blocks on the request. Works on native; doesn't
/// compile for `wasm32-wasip1` — see this crate's docs for the
/// `wasi-http` future-state discussion.
extern "C" fn http_get_call(
    rt: *mut RuntimeFfi,
    args: *const ValueRef,
    argc: usize,
    out: *mut EvalOutput,
) {
    let write_err = |out: *mut EvalOutput| {
        if !out.is_null() {
            unsafe {
                *out = EvalOutput {
                    status: EvalStatus::EvalError,
                    value: ValueRef { handle: 0 },
                    error: ValueRef { handle: 0 },
                };
            }
        }
    };
    if rt.is_null() || out.is_null() || argc != 1 || args.is_null() {
        write_err(out);
        return;
    }
    let arg0 = unsafe { *args };

    // Decode the URL argument via the cs-ffi v2 decode_string
    // callback. The (ptr, len) view is valid for this call's
    // duration; we copy to an owned String before invoking reqwest
    // (which spawns a tokio runtime and could in principle yield
    // control back to the runtime via internal callbacks).
    let mut s_ptr: *const c_char = std::ptr::null();
    let mut s_len: usize = 0;
    let ok = unsafe { ((*rt).decode_string)(rt, arg0, &mut s_ptr, &mut s_len) };
    if ok == 0 || s_ptr.is_null() {
        write_err(out);
        return;
    }
    let url_bytes = unsafe { std::slice::from_raw_parts(s_ptr as *const u8, s_len) };
    let url = match std::str::from_utf8(url_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            write_err(out);
            return;
        }
    };

    let body = match reqwest::blocking::get(&url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
    {
        Ok(b) => b,
        Err(_) => {
            write_err(out);
            return;
        }
    };

    let body_bytes = body.as_bytes();
    let result =
        unsafe { ((*rt).alloc_string)(rt, body_bytes.as_ptr() as *const c_char, body_bytes.len()) };
    unsafe {
        *out = EvalOutput {
            status: EvalStatus::Ok,
            value: result,
            error: ValueRef { handle: 0 },
        };
    }
}
