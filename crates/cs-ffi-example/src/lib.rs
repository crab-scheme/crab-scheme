//! Example CrabScheme FFI plugin.
//!
//! This crate is built as a cdylib so the runtime's
//! `(load-shared-library)` can dlopen it; the rlib output exists
//! solely so unit tests in this workspace can call the entry
//! point as a regular Rust function and exercise the C-ABI wire
//! protocol without dlopen.
//!
//! Plugin authors writing real plugins should depend on `cs-ffi`
//! and follow this crate's pattern.

use std::ffi::CString;
use std::os::raw::c_char;

use cs_ffi::abi::{
    EvalOutput, EvalStatus, HostProcDecl, RegHandle, RuntimeFfi, ValueRef,
    CRABSCHEME_FFI_API_VERSION,
};

/// Status code returned by [`crabscheme_register`].
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterStatus {
    /// Plugin registered all its procedures successfully.
    Ok = 0,
    /// The host's `api_version` did not match the plugin's
    /// compile-time constant. The plugin registered nothing.
    VersionMismatch = 1,
    /// The host's table contained a null function pointer the
    /// plugin needed (defensive — should never happen in practice).
    NullFunctionPointer = 2,
}

/// Plugin entry point. The runtime calls this immediately after
/// dlopening the library; the plugin reads the `api_version`,
/// confirms compatibility, and registers its procedures.
///
/// `rt` is a pointer to the runtime's `RuntimeFfi` table. The
/// pointer is valid for the duration of this call.
///
/// # Safety
///
/// `rt` must point to a valid `RuntimeFfi` instance whose
/// `api_version` field is at offset 0.
#[no_mangle]
pub extern "C" fn crabscheme_register(rt: *mut RuntimeFfi) -> i32 {
    if rt.is_null() {
        return RegisterStatus::NullFunctionPointer as i32;
    }

    // Check the wire-protocol version before reading any other
    // field. This is the only access pattern that is safe across
    // version-mismatched layouts: api_version is at offset 0 by the
    // protocol invariant tested in cs-ffi::abi tests.
    //
    // SAFETY: `rt` is non-null per the early return above; the
    // caller (the runtime) guarantees it points to a valid
    // RuntimeFfi for this call's duration.
    let host_version = unsafe { (*rt).api_version };
    if host_version != CRABSCHEME_FFI_API_VERSION {
        return RegisterStatus::VersionMismatch as i32;
    }

    // Register a marker procedure: (example-magic) -> 42. The
    // marker always returns the same value so iter 6b/6c tests can
    // verify the wire protocol without value-inspection callbacks
    // (those land in a follow-up iter when needed).
    let name = b"example-magic";
    let decl = HostProcDecl {
        name_ptr: name.as_ptr() as *const c_char,
        name_len: name.len(),
        call: example_magic_call,
        arity: 0,
    };
    // SAFETY: rt is valid for the call duration; decl is valid for
    // the call (the runtime is required to copy out anything it
    // needs to retain).
    let _: RegHandle = unsafe { ((*rt).register_proc)(rt, &decl as *const _) };

    RegisterStatus::Ok as i32
}

/// Magic constant returned by (example-magic). Tests in cs-runtime
/// match against this exact value.
pub const EXAMPLE_MAGIC_VALUE: i64 = 42;

/// Implementation of (example-magic) -> 42. Always returns the same
/// fixnum regardless of args.
extern "C" fn example_magic_call(
    rt: *mut RuntimeFfi,
    _args: *const ValueRef,
    argc: usize,
    out: *mut EvalOutput,
) {
    if rt.is_null() || out.is_null() || argc != 0 {
        if !out.is_null() {
            // SAFETY: out is non-null per the check above.
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
    // SAFETY: rt is non-null per the early return above.
    let result = unsafe { ((*rt).alloc_fixnum)(rt, EXAMPLE_MAGIC_VALUE) };
    // SAFETY: out is non-null per the early return above.
    unsafe {
        *out = EvalOutput {
            status: EvalStatus::Ok,
            value: result,
            error: ValueRef { handle: 0 },
        };
    }
}

/// Helper for tests / debugging: construct a CString of the build
/// version stamp. Not part of the FFI surface.
#[doc(hidden)]
pub fn build_version_stamp() -> CString {
    CString::new(format!("cs-ffi-example api={}", CRABSCHEME_FFI_API_VERSION)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_with_stub_runtime_returns_version_mismatch_when_changed() {
        let mut rt = RuntimeFfi::stub();
        // Version is correct as constructed; flip it to confirm
        // the mismatch path returns the right status.
        rt.api_version = 999;
        let status = crabscheme_register(&mut rt as *mut _);
        assert_eq!(status, RegisterStatus::VersionMismatch as i32);
    }

    #[test]
    fn register_with_null_returns_null_status() {
        let status = crabscheme_register(std::ptr::null_mut());
        assert_eq!(status, RegisterStatus::NullFunctionPointer as i32);
    }

    #[test]
    fn build_version_stamp_includes_api_version() {
        let s = build_version_stamp();
        let s = s.into_string().unwrap();
        // Bumped to api=2 in cs-ffi L1 (decoder callbacks added to
        // RuntimeFfi). The constant lives in cs-ffi::abi; this test
        // tracks whatever CRABSCHEME_FFI_API_VERSION is.
        let expected = format!("api={}", CRABSCHEME_FFI_API_VERSION);
        assert!(s.contains(&expected), "{s}");
    }
}
