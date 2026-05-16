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
//! Then `(http-get-example-com)` returns the body of
//! `https://example.com/` as a Scheme string. The arity-0 API is
//! a current limitation of the cs-ffi C-ABI: it exposes argument
//! *encoders* (`alloc_string`, etc.) but not *decoders* into Rust
//! types, so plugins can't easily take user-provided strings yet.
//! A future cs-ffi iter could add `decode_string` to lift this
//! constraint; for now the example demonstrates the dlopen +
//! real-library integration end-to-end via a fixed URL.
//!
//! ## Scheme surface
//!
//!   `(http-get-example-com)` → string
//!     GET https://example.com/, block until the response arrives,
//!     return the body. Errors flatten to a generic EvalError.

use std::os::raw::c_char;

use cs_ffi::abi::{
    EvalOutput, EvalStatus, HostProcDecl, RegHandle, RuntimeFfi, ValueRef,
    CRABSCHEME_FFI_API_VERSION,
};

/// Fixed URL the demo fetches.
const DEMO_URL: &str = "https://example.com/";

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

    let name = b"http-get-example-com";
    let decl = HostProcDecl {
        name_ptr: name.as_ptr() as *const c_char,
        name_len: name.len(),
        call: http_get_example_com_call,
        arity: 0,
    };
    let _: RegHandle = unsafe { ((*rt).register_proc)(rt, &decl as *const _) };

    RegisterStatus::Ok as i32
}

/// C-ABI thunk for `(http-get-example-com)`. Hits the fixed
/// `DEMO_URL` via `reqwest::blocking::get` and returns the body as
/// a Scheme string via `alloc_string`.
///
/// `reqwest::blocking::get` spawns a `tokio::runtime::Runtime`
/// internally and blocks on the request. That works on native
/// targets but not on `wasm32-wasip1` — which is the whole point
/// of having this plugin be cdylib-only.
extern "C" fn http_get_example_com_call(
    rt: *mut RuntimeFfi,
    _args: *const ValueRef,
    argc: usize,
    out: *mut EvalOutput,
) {
    if rt.is_null() || out.is_null() || argc != 0 {
        if !out.is_null() {
            unsafe {
                *out = EvalOutput {
                    status: EvalStatus::EvalError,
                    value: ValueRef { handle: 0 },
                    error: ValueRef { handle: 0 },
                };
            }
        }
        return;
    }

    let body = match reqwest::blocking::get(DEMO_URL)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
    {
        Ok(b) => b,
        Err(_) => {
            unsafe {
                *out = EvalOutput {
                    status: EvalStatus::EvalError,
                    value: ValueRef { handle: 0 },
                    error: ValueRef { handle: 0 },
                };
            }
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
