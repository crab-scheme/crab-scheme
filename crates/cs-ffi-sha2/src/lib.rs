//! CrabScheme FFI plugin: SHA-256 hashing.
//!
//! Demonstrates the **dual-mode** plugin pattern: the same crate
//! provides both a `crabscheme_register` C-ABI entry point (for the
//! `(load-shared-library)` dlopen path on native) and a
//! `make_sha256_proc()` factory function (for static registration
//! into a custom embedder binary — the WASM-compatible path).
//!
//! ## Both paths register `(sha256 v)`
//!
//! Post cs-ffi v2 (L1: value decoders), the dlopen path can decode
//! plugin arguments via the `decode_string` / `decode_bytevector`
//! callbacks the runtime exposes. The static-link and dlopen paths
//! now register the SAME `(sha256 v)` procedure name and accept
//! string-or-bytevector inputs.
//!
//! | Path        | Surface                 | WASM-OK? |
//! |-------------|-------------------------|----------|
//! | dlopen      | extern "C" + RuntimeFfi | ✗        |
//! | static-link | impl HostProcedure      | ✓        |
//!
//! ## Usage — native dlopen
//!
//! ```sh
//! cargo build --release -p cs-ffi-sha2
//! crabscheme -e '(load-shared-library "target/release/libcs_ffi_sha2.dylib") (display (sha256 "hello"))'
//! # prints: 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
//! ```
//!
//! ## Usage — static linking (WASM-compatible)
//!
//! See [`crates/cs-cli-sha2`](../cs-cli-sha2) for a complete
//! example. The pattern:
//!
//! ```ignore
//! let mut rt = cs_runtime::Runtime::new();
//! rt.register_host_procedure(cs_ffi_sha2::make_sha256_proc());
//! rt.eval_str("<demo>", r#"(display (sha256 "hello"))"#).unwrap();
//! // prints: 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

use sha2::{Digest, Sha256};

// ----------------------------------------------------------------------
// Static-link API — full-featured.
// ----------------------------------------------------------------------

/// Construct a [`HostProcedure`] implementing `(sha256 v)`. Pass the
/// returned `Arc` to `cs_runtime::Runtime::register_host_procedure`
/// to make `(sha256 ...)` callable from Scheme.
///
/// Pure-Rust; no `dlopen` involved. This is the WASM-compatible
/// path — `cs-cli-sha2`'s `main.rs` calls this function.
pub fn make_sha256_proc() -> Arc<dyn HostProcedure> {
    UntypedProc::new("sha256", sha256_impl)
}

fn sha256_impl(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(FfiError::ArityError {
            name: "sha256".into(),
            expected: "1".into(),
            got: args.len(),
        });
    }

    let mut hasher = Sha256::new();
    match &args[0] {
        Value::String(s) => {
            hasher.update(s.borrow().as_bytes());
        }
        Value::ByteVector(b) => {
            hasher.update(b.borrow().as_slice());
        }
        other => {
            return Err(FfiError::TypeMismatch {
                expected: "string or bytevector".into(),
                got: other.type_name().to_string(),
            });
        }
    }
    let digest = hasher.finalize();
    let hex_str = hex::encode(digest);
    Ok(Value::string(hex_str))
}

// ----------------------------------------------------------------------
// Dlopen API — C-ABI entry point used by `(load-shared-library)`.
// Demonstrates registration via the cs-ffi C-ABI without needing
// per-type argument decoders (which the iter-6 ABI doesn't yet
// expose).
// ----------------------------------------------------------------------
//
// Not compiled for WASM (no `dlopen` there); the `cfg(not(target_
// family = "wasm"))` gate keeps the cdylib output trim for the
// static-link-only WASM build path.

#[cfg(not(target_family = "wasm"))]
mod dlopen {
    use std::os::raw::c_char;

    use cs_ffi::abi::{
        EvalOutput, EvalStatus, HostProcDecl, RegHandle, RuntimeFfi, ValueKind, ValueRef,
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

    /// dlopen entry point. The runtime invokes this immediately
    /// after `dlopen`'ing this cdylib.
    ///
    /// # Safety
    ///
    /// `rt` must point to a valid `RuntimeFfi` whose `api_version`
    /// is at offset 0. The runtime guarantees this for the call's
    /// duration.
    #[no_mangle]
    pub extern "C" fn crabscheme_register(rt: *mut RuntimeFfi) -> i32 {
        if rt.is_null() {
            return RegisterStatus::NullFunctionPointer as i32;
        }
        // SAFETY: rt is non-null per the early return.
        let host_version = unsafe { (*rt).api_version };
        if host_version != CRABSCHEME_FFI_API_VERSION {
            return RegisterStatus::VersionMismatch as i32;
        }

        // Register (sha256 v) — 1 arg, accepts string-or-bytevector,
        // returns hex. Same Scheme surface as the static-link path
        // post cs-ffi v2 (L1: decoder callbacks).
        let name = b"sha256";
        let decl = HostProcDecl {
            name_ptr: name.as_ptr() as *const c_char,
            name_len: name.len(),
            call: sha256_call_cabi,
            arity: 1,
        };
        // SAFETY: rt is valid for the call; decl is valid for the
        // call (the runtime copies whatever it needs to retain).
        let _: RegHandle = unsafe { ((*rt).register_proc)(rt, &decl as *const _) };

        RegisterStatus::Ok as i32
    }

    /// C-ABI thunk for `(sha256 v)`. Decodes the arg via the v2
    /// decoder callbacks (string OR bytevector), computes SHA-256,
    /// returns the digest as a Scheme string.
    extern "C" fn sha256_call_cabi(
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

        // Use `value_kind` to discriminate before decoding. Slightly
        // verbose vs trying both decoders, but communicates intent
        // and avoids relying on decoder-returns-0 as type-check.
        let kind = unsafe { ((*rt).value_kind)(rt, arg0) };

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();

        match kind {
            ValueKind::String => {
                let mut s_ptr: *const c_char = std::ptr::null();
                let mut s_len: usize = 0;
                // SAFETY: rt non-null; decode_string writes
                // (ptr, len) view valid for the call duration.
                let ok = unsafe { ((*rt).decode_string)(rt, arg0, &mut s_ptr, &mut s_len) };
                if ok == 0 || s_ptr.is_null() {
                    write_err(out);
                    return;
                }
                let bytes = unsafe { std::slice::from_raw_parts(s_ptr as *const u8, s_len) };
                hasher.update(bytes);
            }
            ValueKind::ByteVector => {
                let mut b_ptr: *const u8 = std::ptr::null();
                let mut b_len: usize = 0;
                let ok = unsafe { ((*rt).decode_bytevector)(rt, arg0, &mut b_ptr, &mut b_len) };
                if ok == 0 || b_ptr.is_null() {
                    write_err(out);
                    return;
                }
                let bytes = unsafe { std::slice::from_raw_parts(b_ptr, b_len) };
                hasher.update(bytes);
            }
            _ => {
                // Type mismatch: not a string or bytevector.
                write_err(out);
                return;
            }
        }

        let digest = hasher.finalize();
        let hex_str = hex::encode(digest);
        let hex_bytes = hex_str.as_bytes();
        // SAFETY: rt non-null; alloc_string takes ptr + len.
        let result = unsafe {
            ((*rt).alloc_string)(rt, hex_bytes.as_ptr() as *const c_char, hex_bytes.len())
        };
        unsafe {
            *out = EvalOutput {
                status: EvalStatus::Ok,
                value: result,
                error: ValueRef { handle: 0 },
            };
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use dlopen::{crabscheme_register, RegisterStatus};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_empty_string_is_known_value() {
        let proc = make_sha256_proc();
        let empty = Value::string(String::new());
        let result = proc.call(&[empty]).unwrap();
        match result {
            Value::String(s) => assert_eq!(
                s.borrow().as_str(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            ),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn sha256_of_hello_matches_known() {
        let proc = make_sha256_proc();
        let s = Value::string("hello".to_string());
        let result = proc.call(&[s]).unwrap();
        match result {
            Value::String(s) => assert_eq!(
                s.borrow().as_str(),
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            ),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn sha256_arity_mismatch_returns_arity_error() {
        let proc = make_sha256_proc();
        let err = proc.call(&[]).unwrap_err();
        match err {
            FfiError::ArityError { name, .. } => assert_eq!(name, "sha256"),
            other => panic!("expected ArityError, got {:?}", other),
        }
    }
}
