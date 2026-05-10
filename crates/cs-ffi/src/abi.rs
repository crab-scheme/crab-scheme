//! Versioned C-ABI surface for dynamic-linking plugins.
//!
//! Per ADR 0008 D-2 we use a C-ABI rather than the unstable Rust ABI
//! for the dynamic-linking path. This lets users build shared
//! libraries with any toolchain (rustc, gcc-derived languages, hand-
//! rolled assembly) and gives us a tractable version-skew story:
//! a single versioned struct of function pointers.
//!
//! ## Versioning
//!
//! [`CRABSCHEME_FFI_API_VERSION`] is the wire-protocol version. Bump
//! on any breaking change to the layout, function signatures, or
//! semantics. The runtime side checks the loaded library's version
//! at registration time and refuses mismatches.
//!
//! Compatibility rules:
//! - Adding a function pointer at the end of [`RuntimeFfi`] is a
//!   breaking change (offsets shift) — bump the version.
//! - Changing an existing function's signature is a breaking change.
//! - Adding a new opaque type (`*Ref`) for a new feature is fine if
//!   no existing field's layout changes.
//!
//! ## Iter 5 status
//!
//! This module ships the *types and constants*. Iter 6 wires
//! `cs-runtime` to produce a populated `RuntimeFfi`, exposes
//! `(load-shared-library)`, and adds an end-to-end test against a
//! `cs-ffi-example` dylib.

use std::os::raw::c_char;

/// Wire-protocol version of [`RuntimeFfi`].
///
/// Bump on any breaking change. Plugins that want forward
/// compatibility check this on entry to their `crabscheme_register`
/// hook and refuse to register if their compiled-in version does
/// not match what the host runtime exports.
pub const CRABSCHEME_FFI_API_VERSION: u32 = 1;

/// Opaque handle to a Scheme value held by the runtime.
///
/// `ValueRef` is `#[repr(C)]` so its layout is stable across the FFI
/// boundary. Internally it is a u64 keyed into a per-runtime slab of
/// pinned values (see [`crate::Pinned`]); the slab is the only
/// liveness mechanism, so plugins MUST drop their `ValueRef`
/// handles via [`RuntimeFfi::release_value`] when done.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueRef {
    /// Slab key; the handle is meaningless outside the runtime that
    /// minted it.
    pub handle: u64,
}

/// Opaque handle returned by [`RuntimeFfi::register_proc`].
///
/// Mainly useful for `(unregister-proc)` (iter 7+) and diagnostics.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegHandle {
    pub id: u64,
}

/// Discriminant for evaluation results.
///
/// On `Err` the caller inspects the `error` field of [`EvalOutput`]
/// for a Scheme value carrying the condition.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalStatus {
    Ok = 0,
    ParseError = 1,
    EvalError = 2,
    Panic = 3,
}

/// Output of [`RuntimeFfi::eval_str`].
///
/// On `Ok` the `value` field is a `ValueRef` to the returned Scheme
/// value (caller releases). On any error the `error` field is a
/// `ValueRef` to a condition object (caller releases) and `value`
/// is the null `ValueRef { handle: 0 }`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EvalOutput {
    pub status: EvalStatus,
    pub value: ValueRef,
    pub error: ValueRef,
}

/// Declaration of a host procedure to register.
///
/// Plugins build this struct and pass it to
/// [`RuntimeFfi::register_proc`]. The runtime copies the name into
/// its own storage and stores the function pointer; the plugin must
/// keep the function alive for the runtime's lifetime (typically by
/// being part of a still-loaded dylib).
#[repr(C)]
pub struct HostProcDecl {
    /// UTF-8 name. Not required to be NUL-terminated; `name_len` is
    /// authoritative.
    pub name_ptr: *const c_char,
    pub name_len: usize,
    /// `extern "C"` function: receives the runtime, an `args` array,
    /// and `arity`. Returns an `EvalOutput` describing the result.
    pub call: HostProcCall,
    /// Optional fixed arity; `u32::MAX` means "variadic, the proc
    /// checks `argc` itself".
    pub arity: u32,
}

/// Type of a host-procedure callback. Receives an opaque runtime
/// pointer (cast back to `*mut RuntimeFfi` for callbacks into the
/// runtime), an args array of length `argc`, and writes its result
/// to `*out`.
pub type HostProcCall =
    extern "C" fn(rt: *mut RuntimeFfi, args: *const ValueRef, argc: usize, out: *mut EvalOutput);

/// Versioned C-ABI table exported by the runtime to dylib plugins.
///
/// Plugins receive a `*mut RuntimeFfi` from their entry point and
/// dispatch through these function pointers. The first field MUST
/// be `api_version`; plugins read it before any other field to
/// detect version skew.
#[repr(C)]
pub struct RuntimeFfi {
    /// Always [`CRABSCHEME_FFI_API_VERSION`] when produced by this
    /// crate. Plugins compare against their compile-time constant.
    pub api_version: u32,

    /// Reserved for layout alignment; always 0.
    pub _reserved: u32,

    /// Register a host procedure declared by the plugin.
    pub register_proc: extern "C" fn(rt: *mut RuntimeFfi, decl: *const HostProcDecl) -> RegHandle,

    /// Evaluate a string of Scheme source. `name_ptr/len` identifies
    /// the source span (used in diagnostics); `src_ptr/len` is the
    /// program text. UTF-8.
    pub eval_str: extern "C" fn(
        rt: *mut RuntimeFfi,
        name_ptr: *const c_char,
        name_len: usize,
        src_ptr: *const c_char,
        src_len: usize,
        out: *mut EvalOutput,
    ),

    /// Allocate a pair `(car . cdr)` and return a handle.
    pub alloc_pair: extern "C" fn(rt: *mut RuntimeFfi, car: ValueRef, cdr: ValueRef) -> ValueRef,

    /// Allocate a fixnum.
    pub alloc_fixnum: extern "C" fn(rt: *mut RuntimeFfi, n: i64) -> ValueRef,

    /// Allocate a string from a UTF-8 buffer.
    pub alloc_string:
        extern "C" fn(rt: *mut RuntimeFfi, ptr: *const c_char, len: usize) -> ValueRef,

    /// Release the runtime's slab slot for a `ValueRef`. After
    /// release the handle is invalid; double-release is a runtime
    /// error.
    pub release_value: extern "C" fn(rt: *mut RuntimeFfi, v: ValueRef),

    /// Raise a Scheme condition. Diverging — this function never
    /// returns to the caller; the runtime unwinds back to the
    /// nearest exception handler.
    pub raise: extern "C" fn(rt: *mut RuntimeFfi, condition: ValueRef) -> !,
}

// --- Stub implementations (iter 5 placeholder) ---------------------
//
// Iter 5 ships the layout. Iter 6 will replace these stubs with real
// runtime-backed implementations. Calling any stub aborts the process
// with a clear panic so accidental use during the iter-5/iter-6 gap
// is loud rather than silent.

extern "C" fn stub_register_proc(_rt: *mut RuntimeFfi, _decl: *const HostProcDecl) -> RegHandle {
    panic!("RuntimeFfi::register_proc stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_eval_str(
    _rt: *mut RuntimeFfi,
    _name_ptr: *const c_char,
    _name_len: usize,
    _src_ptr: *const c_char,
    _src_len: usize,
    _out: *mut EvalOutput,
) {
    panic!("RuntimeFfi::eval_str stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_alloc_pair(_rt: *mut RuntimeFfi, _car: ValueRef, _cdr: ValueRef) -> ValueRef {
    panic!("RuntimeFfi::alloc_pair stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_alloc_fixnum(_rt: *mut RuntimeFfi, _n: i64) -> ValueRef {
    panic!("RuntimeFfi::alloc_fixnum stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_alloc_string(
    _rt: *mut RuntimeFfi,
    _ptr: *const c_char,
    _len: usize,
) -> ValueRef {
    panic!("RuntimeFfi::alloc_string stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_release_value(_rt: *mut RuntimeFfi, _v: ValueRef) {
    panic!("RuntimeFfi::release_value stub called before iter 6 wired the runtime side")
}

extern "C" fn stub_raise(_rt: *mut RuntimeFfi, _condition: ValueRef) -> ! {
    panic!("RuntimeFfi::raise stub called before iter 6 wired the runtime side")
}

impl RuntimeFfi {
    /// Construct a `RuntimeFfi` whose function pointers are
    /// abort-on-call stubs.
    ///
    /// Useful for iter-5 layout / version tests, and for embedders
    /// that want to assert the table is present before iter 6 wires
    /// the real runtime side.
    pub fn stub() -> Self {
        Self {
            api_version: CRABSCHEME_FFI_API_VERSION,
            _reserved: 0,
            register_proc: stub_register_proc,
            eval_str: stub_eval_str,
            alloc_pair: stub_alloc_pair,
            alloc_fixnum: stub_alloc_fixnum,
            alloc_string: stub_alloc_string,
            release_value: stub_release_value,
            raise: stub_raise,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn api_version_is_one() {
        assert_eq!(CRABSCHEME_FFI_API_VERSION, 1);
    }

    #[test]
    fn value_ref_is_8_bytes() {
        // u64 handle, no padding.
        assert_eq!(size_of::<ValueRef>(), 8);
    }

    #[test]
    fn reg_handle_is_8_bytes() {
        assert_eq!(size_of::<RegHandle>(), 8);
    }

    #[test]
    fn eval_status_repr_u32() {
        assert_eq!(size_of::<EvalStatus>(), 4);
    }

    #[test]
    fn stub_layout_starts_with_version() {
        // The first field MUST be api_version so plugins can detect
        // version skew before reading anything else.
        let s = RuntimeFfi::stub();
        let base = &s as *const RuntimeFfi as usize;
        let ver_addr = &s.api_version as *const u32 as usize;
        assert_eq!(
            base, ver_addr,
            "api_version is not at offset 0: layout-breaking change in RuntimeFfi"
        );
    }

    #[test]
    fn stub_has_correct_version() {
        let s = RuntimeFfi::stub();
        assert_eq!(s.api_version, CRABSCHEME_FFI_API_VERSION);
        assert_eq!(s._reserved, 0);
    }

    #[test]
    fn null_value_ref_is_handle_zero() {
        // Convention: handle=0 is "null". The runtime never mints
        // handle 0 (next_pin_id starts at 0 only in fresh runtimes;
        // the runtime side reserves 0 for null in iter 6).
        let null = ValueRef { handle: 0 };
        assert_eq!(null.handle, 0);
    }
}
