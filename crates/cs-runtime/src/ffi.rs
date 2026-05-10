//! C-ABI runtime backend.
//!
//! Iter 5 introduced the [`cs_ffi::abi::RuntimeFfi`] type with stub
//! function pointers. This module ships the real backend: a
//! [`RuntimeFfiContext`] that owns the `RuntimeFfi` table plus a
//! back-pointer to the [`Runtime`] it serves, and the `extern "C"`
//! callback bodies that the table's function pointers reference.
//!
//! Layout invariant: `RuntimeFfiContext` is `#[repr(C)]` with `ffi`
//! at offset 0. The runtime hands out `*mut RuntimeFfi` pointers
//! whose address is also a valid `*mut RuntimeFfiContext`; the
//! callbacks cast back through that aliasing to reach the runtime
//! and the value slab.
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ RuntimeFfiContext (repr(C))             │
//! │  ┌──────────────────────────────────┐   │
//! │  │ ffi: RuntimeFfi  (offset 0)      │◄──── plugin sees this
//! │  └──────────────────────────────────┘   │  pointer
//! │ runtime: *mut Runtime                   │
//! └─────────────────────────────────────────┘
//! ```
//!
//! See `.spec-workflow/specs/ffi/{requirements,design}.md` and
//! `docs/adr/0008-ffi-design.md` D-2.

use std::cell::Cell;
use std::os::raw::c_char;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::abi::{
    EvalOutput, EvalStatus, HostProcDecl, RegHandle, RuntimeFfi, ValueRef,
    CRABSCHEME_FFI_API_VERSION,
};
use cs_ffi::{FfiError, HostProcedure};

use crate::Runtime;

thread_local! {
    /// Active-runtime back-pointer, set by [`Runtime::with_active`]
    /// for the duration of an eval call. Read by the
    /// `(load-shared-library)` builtin to recover `&mut Runtime`
    /// from inside the eval call chain (where the borrow has been
    /// downgraded to `&mut EvalCtx`).
    ///
    /// Single-threaded model: only one runtime active per thread.
    /// Nested `with_active` saves and restores via the returned
    /// guard, so re-entrancy is safe.
    static ACTIVE_RUNTIME: Cell<*mut Runtime> = const { Cell::new(std::ptr::null_mut()) };
}

impl Runtime {
    /// Run `f` with this runtime stashed in the thread-local
    /// `ACTIVE_RUNTIME` slot. The previous value is saved and
    /// restored on return so nested calls work correctly.
    ///
    /// Used by `Runtime::eval_str` / `eval_str_via_vm` to make
    /// the active runtime reachable from inside builtins that
    /// need it (`(load-shared-library)` etc.).
    pub fn with_active<R>(&mut self, f: impl FnOnce(&mut Runtime) -> R) -> R {
        let prev = ACTIVE_RUNTIME.with(|c| c.replace(self as *mut Runtime));
        let result = f(self);
        ACTIVE_RUNTIME.with(|c| c.set(prev));
        result
    }

    /// Borrow the thread's active runtime, if any. Returns `None`
    /// if no runtime is currently active.
    ///
    /// # Safety
    ///
    /// The caller MUST ensure that no other live `&mut Runtime`
    /// exists. The intended use is from inside a builtin call
    /// where the only `&mut Runtime` was just downgraded via
    /// `with_active`, so the back-pointer is the unique mutable
    /// access for the call's duration.
    pub unsafe fn active<'a>() -> Option<&'a mut Runtime> {
        let ptr = ACTIVE_RUNTIME.with(|c| c.get());
        if ptr.is_null() {
            None
        } else {
            Some(&mut *ptr)
        }
    }

    /// Open a shared library at `path`, look up its
    /// `crabscheme_register` symbol, and call it with a freshly-
    /// built C-ABI context so the plugin can register its host
    /// procedures.
    ///
    /// The library handle is retained on this runtime so the
    /// plugin's code (function bodies for registered procedures)
    /// remains mapped for the runtime's lifetime.
    ///
    /// # Errors
    ///
    /// - `FfiError::HostFailure` on dlopen failure, missing
    ///   `crabscheme_register` symbol, or non-zero return from
    ///   the plugin's register entry point.
    pub fn load_shared_library(&mut self, path: &str) -> Result<(), FfiError> {
        // SAFETY: dlopen of a user-provided path. Loading native
        // code is inherently unsafe and the caller is asserting
        // the path is trusted.
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| {
            FfiError::HostFailure(format!("load_shared_library({path}): dlopen: {e}"))
        })?;

        // SAFETY: the symbol's signature must match the C-ABI
        // contract; mismatch is the plugin author's bug.
        let register: libloading::Symbol<extern "C" fn(*mut RuntimeFfi) -> i32> = unsafe {
            lib.get(b"crabscheme_register\0").map_err(|e| {
                FfiError::HostFailure(format!(
                    "load_shared_library({path}): crabscheme_register symbol: {e}"
                ))
            })?
        };

        // Use the cached context — its *mut Runtime back-pointer
        // outlives this call, so registered host procedures'
        // captured rt_ptr stays valid for the runtime's lifetime.
        let p = self.ffi_context_ptr();
        let status = register(p);

        if status != 0 {
            return Err(FfiError::HostFailure(format!(
                "load_shared_library({path}): plugin register returned {status}"
            )));
        }

        self.loaded_libs.push(lib);
        Ok(())
    }
}

/// Boxed wrapper produced by [`Runtime::ffi_context`]. The plugin
/// sees only the `ffi` field; the runtime's callbacks cast back
/// through it to reach the [`Runtime`] back-pointer.
///
/// Keep this struct on the heap (via [`Runtime::ffi_context`]'s
/// `Box`) so its address is stable for the duration of the FFI
/// session. Moving it would invalidate the `*mut RuntimeFfi`
/// pointer the plugin holds.
#[repr(C)]
pub struct RuntimeFfiContext {
    /// MUST be at offset 0. Plugins receive `*mut RuntimeFfi`
    /// pointing here.
    ffi: RuntimeFfi,

    /// Back-pointer to the Runtime that owns this context. The
    /// pointer is valid for the context's lifetime; the embedder is
    /// responsible for not dropping the Runtime while the context
    /// is in use.
    runtime: *mut Runtime,
}

impl RuntimeFfiContext {
    /// Get a `*mut RuntimeFfi` pointing at the embedded `ffi` field.
    /// This is what plugins receive; equivalent to casting `self`
    /// because of the offset-0 layout invariant.
    pub fn as_ffi_ptr(&mut self) -> *mut RuntimeFfi {
        &mut self.ffi as *mut RuntimeFfi
    }
}

impl Runtime {
    /// Borrow this runtime's lazy [`RuntimeFfiContext`], creating
    /// it on first use. The Box is kept alive for the runtime's
    /// lifetime so any plugin-captured `*mut RuntimeFfi` stays
    /// valid.
    ///
    /// Returns a `*mut RuntimeFfi` pointing into the cached box.
    /// Equivalent to `self.ffi_ctx.as_mut().unwrap().as_ffi_ptr()`
    /// after the lazy init.
    pub fn ffi_context_ptr(&mut self) -> *mut RuntimeFfi {
        if self.ffi_ctx.is_none() {
            let runtime_ptr = self as *mut Runtime;
            let ffi = RuntimeFfi {
                api_version: CRABSCHEME_FFI_API_VERSION,
                _reserved: 0,
                register_proc: ffi_register_proc,
                eval_str: ffi_eval_str,
                alloc_pair: ffi_alloc_pair,
                alloc_fixnum: ffi_alloc_fixnum,
                alloc_string: ffi_alloc_string,
                release_value: ffi_release_value,
                raise: ffi_raise,
            };
            self.ffi_ctx = Some(Box::new(RuntimeFfiContext {
                ffi,
                runtime: runtime_ptr,
            }));
        }
        self.ffi_ctx.as_mut().unwrap().as_ffi_ptr()
    }

    /// Test-only helper: build a fresh non-cached `Box<RuntimeFfiContext>`.
    /// Used by unit tests that exercise the C-ABI directly without
    /// needing the cached singleton lifetime.
    #[doc(hidden)]
    pub fn ffi_context(&mut self) -> Box<RuntimeFfiContext> {
        let runtime_ptr = self as *mut Runtime;
        let ffi = RuntimeFfi {
            api_version: CRABSCHEME_FFI_API_VERSION,
            _reserved: 0,
            register_proc: ffi_register_proc,
            eval_str: ffi_eval_str,
            alloc_pair: ffi_alloc_pair,
            alloc_fixnum: ffi_alloc_fixnum,
            alloc_string: ffi_alloc_string,
            release_value: ffi_release_value,
            raise: ffi_raise,
        };
        Box::new(RuntimeFfiContext {
            ffi,
            runtime: runtime_ptr,
        })
    }
}

// --- Helpers ----------------------------------------------------

/// Cast a `*mut RuntimeFfi` back to `*mut RuntimeFfiContext`.
///
/// # Safety
///
/// The caller MUST guarantee that `rt` was originally produced by
/// [`Runtime::ffi_context`] (and therefore points to a
/// `RuntimeFfiContext` whose first field is `RuntimeFfi`). The
/// invariant is preserved by every public path that produces such
/// a pointer.
unsafe fn ctx_from_ffi_ptr<'a>(rt: *mut RuntimeFfi) -> &'a mut RuntimeFfiContext {
    debug_assert!(!rt.is_null(), "ctx_from_ffi_ptr called with null");
    // Safe per #[repr(C)] layout invariant: ffi is at offset 0.
    &mut *(rt as *mut RuntimeFfiContext)
}

/// Borrow the runtime behind an `*mut RuntimeFfi`. Used by every
/// callback body.
///
/// # Safety
///
/// Same as [`ctx_from_ffi_ptr`].
unsafe fn runtime_from_ffi_ptr<'a>(rt: *mut RuntimeFfi) -> &'a mut Runtime {
    let ctx = ctx_from_ffi_ptr(rt);
    debug_assert!(!ctx.runtime.is_null(), "RuntimeFfiContext::runtime is null");
    &mut *ctx.runtime
}

// --- extern "C" callbacks ---------------------------------------

extern "C" fn ffi_register_proc(rt: *mut RuntimeFfi, decl: *const HostProcDecl) -> RegHandle {
    if rt.is_null() || decl.is_null() {
        return RegHandle { id: 0 };
    }

    // SAFETY: rt produced by Runtime::ffi_context; decl produced by
    // the plugin and required to be valid for the call duration.
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    let decl_ref = unsafe { &*decl };

    // Decode the name from the (ptr, len) pair. UTF-8 required.
    let name_bytes = if decl_ref.name_ptr.is_null() || decl_ref.name_len == 0 {
        return RegHandle { id: 0 };
    } else {
        // SAFETY: plugin guarantees ptr/len describe valid UTF-8
        // for the call duration. We copy the slice into an owned
        // String below so retention is the runtime's problem.
        unsafe { std::slice::from_raw_parts(decl_ref.name_ptr as *const u8, decl_ref.name_len) }
    };
    let name = match std::str::from_utf8(name_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return RegHandle { id: 0 },
    };

    let arity = decl_ref.arity;
    let call = decl_ref.call;
    let rt_ptr = rt;
    let proc: Arc<dyn HostProcedure> = Arc::new(CAbiProc {
        name,
        arity,
        call,
        rt_ptr: SendPtr(rt_ptr),
    });
    runtime.register_host_procedure(proc);

    // For now we don't track per-registration metadata. Iter 7+ may
    // surface unregister; reserve handles 1+ for that.
    RegHandle { id: 1 }
}

extern "C" fn ffi_eval_str(
    rt: *mut RuntimeFfi,
    name_ptr: *const c_char,
    name_len: usize,
    src_ptr: *const c_char,
    src_len: usize,
    out: *mut EvalOutput,
) {
    if out.is_null() {
        return;
    }
    let mut output = EvalOutput {
        status: EvalStatus::EvalError,
        value: ValueRef { handle: 0 },
        error: ValueRef { handle: 0 },
    };
    if rt.is_null() || src_ptr.is_null() {
        // SAFETY: out is non-null per the early return above.
        unsafe {
            *out = output;
        }
        return;
    }
    // SAFETY: pointers/lengths described above; UTF-8 required by
    // the C-ABI contract.
    let name = if name_ptr.is_null() || name_len == 0 {
        "<ffi>"
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(name_ptr as *const u8, name_len) };
        std::str::from_utf8(bytes).unwrap_or("<ffi>")
    };
    let src = unsafe { std::slice::from_raw_parts(src_ptr as *const u8, src_len) };
    let src = match std::str::from_utf8(src) {
        Ok(s) => s,
        Err(_) => {
            unsafe { *out = output };
            return;
        }
    };
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    match runtime.eval_str(name, src) {
        Ok(v) => {
            output.status = EvalStatus::Ok;
            output.value = ValueRef {
                handle: runtime.pin_raw(v),
            };
        }
        Err(_) => {
            output.status = EvalStatus::EvalError;
            // Iter 7 will surface a real condition value.
        }
    }
    unsafe {
        *out = output;
    }
}

extern "C" fn ffi_alloc_pair(rt: *mut RuntimeFfi, car: ValueRef, cdr: ValueRef) -> ValueRef {
    if rt.is_null() {
        return ValueRef { handle: 0 };
    }
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    let car_v = match runtime.lookup_raw(car.handle) {
        Some(v) => v,
        None => return ValueRef { handle: 0 },
    };
    let cdr_v = match runtime.lookup_raw(cdr.handle) {
        Some(v) => v,
        None => return ValueRef { handle: 0 },
    };
    let pair = Value::Pair(cs_core::Pair::new(car_v, cdr_v));
    ValueRef {
        handle: runtime.pin_raw(pair),
    }
}

extern "C" fn ffi_alloc_fixnum(rt: *mut RuntimeFfi, n: i64) -> ValueRef {
    if rt.is_null() {
        return ValueRef { handle: 0 };
    }
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    let v = Value::Number(cs_core::Number::Fixnum(n));
    ValueRef {
        handle: runtime.pin_raw(v),
    }
}

extern "C" fn ffi_alloc_string(rt: *mut RuntimeFfi, ptr: *const c_char, len: usize) -> ValueRef {
    if rt.is_null() || (ptr.is_null() && len > 0) {
        return ValueRef { handle: 0 };
    }
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    let bytes = if len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees ptr/len describe valid UTF-8
        // for the call.
        unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }
    };
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return ValueRef { handle: 0 },
    };
    ValueRef {
        handle: runtime.pin_raw(Value::string(s)),
    }
}

extern "C" fn ffi_release_value(rt: *mut RuntimeFfi, v: ValueRef) {
    if rt.is_null() || v.handle == 0 {
        return;
    }
    let runtime = unsafe { runtime_from_ffi_ptr(rt) };
    runtime.unpin_raw(v.handle);
}

extern "C" fn ffi_raise(_rt: *mut RuntimeFfi, _condition: ValueRef) -> ! {
    // Iter 7 wires this to runtime's exception machinery. For now
    // the diverging contract is honored via panic; the catch_unwind
    // around dispatch in the host-proc layer translates panics.
    panic!("RuntimeFfi::raise not yet wired (planned for iter 7)")
}

// --- C-ABI HostProcedure adapter ---------------------------------

/// Adapter that converts a HostProcCall (extern "C") into the
/// HostProcedure trait used by `Runtime::register_host_procedure`.
///
/// The `rt_ptr` is captured at registration time; every call passes
/// it back to the plugin so the plugin can call back into the
/// runtime for value construction. Captured pointers are wrapped in
/// `SendPtr` to satisfy `Send + Sync` bounds (the runtime model is
/// single-threaded so this is fine).
struct CAbiProc {
    name: String,
    #[allow(dead_code)]
    arity: u32,
    call: cs_ffi::abi::HostProcCall,
    rt_ptr: SendPtr<RuntimeFfi>,
}

impl HostProcedure for CAbiProc {
    fn name(&self) -> &str {
        &self.name
    }

    fn call(&self, args: &[Value]) -> Result<Value, FfiError> {
        // Pin every arg into the slab and build a ValueRef array
        // for the plugin. Release them after the call.
        // SAFETY: rt_ptr was produced by Runtime::ffi_context; the
        // box is kept alive by the embedder for the FFI session.
        let runtime = unsafe { runtime_from_ffi_ptr(self.rt_ptr.0) };
        let mut handles: Vec<ValueRef> = Vec::with_capacity(args.len());
        for v in args {
            handles.push(ValueRef {
                handle: runtime.pin_raw(v.clone()),
            });
        }

        let mut out = EvalOutput {
            status: EvalStatus::EvalError,
            value: ValueRef { handle: 0 },
            error: ValueRef { handle: 0 },
        };

        // The plugin's call is a non-unsafe extern "C" fn pointer
        // we registered ourselves; calling it does not require an
        // unsafe block.
        (self.call)(self.rt_ptr.0, handles.as_ptr(), handles.len(), &mut out);

        // Release input handles.
        for h in &handles {
            runtime.unpin_raw(h.handle);
        }

        match out.status {
            EvalStatus::Ok => {
                let v = runtime.lookup_raw(out.value.handle).ok_or_else(|| {
                    FfiError::HostFailure("plugin returned invalid ValueRef".into())
                })?;
                runtime.unpin_raw(out.value.handle);
                Ok(v)
            }
            EvalStatus::ParseError | EvalStatus::EvalError | EvalStatus::Panic => {
                let msg = if out.error.handle != 0 {
                    let v = runtime.lookup_raw(out.error.handle);
                    runtime.unpin_raw(out.error.handle);
                    match v {
                        Some(Value::String(s)) => s.borrow().clone(),
                        _ => format!("plugin returned status {:?}", out.status),
                    }
                } else {
                    format!("plugin returned status {:?}", out.status)
                };
                Err(FfiError::HostFailure(msg))
            }
        }
    }
}

/// `*mut T` wrapper that asserts Send + Sync. Used only inside the
/// FFI module and only with pointers whose lifetime is managed by
/// the embedder (single-threaded runtime model).
struct SendPtr<T>(*mut T);

// SAFETY: the runtime model is single-threaded; the only consumer
// of these pointers is the same thread that created the FFI session.
unsafe impl<T> Send for SendPtr<T> {}
// SAFETY: same as above.
unsafe impl<T> Sync for SendPtr<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_context_starts_with_ffi_at_offset_zero() {
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let ctx_addr = &*ctx as *const _ as usize;
        let ffi_addr = ctx.as_ffi_ptr() as usize;
        assert_eq!(ctx_addr, ffi_addr);
    }

    #[test]
    fn alloc_fixnum_round_trip_via_callback() {
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let p = ctx.as_ffi_ptr();
        // SAFETY: p is non-null and was just minted.
        let r = unsafe { ((*p).alloc_fixnum)(p, 7) };
        assert_ne!(r.handle, 0);
        let stored = rt.lookup_raw(r.handle).unwrap();
        match stored {
            Value::Number(cs_core::Number::Fixnum(7)) => {}
            other => panic!("expected fixnum 7, got {:?}", other),
        }
        rt.unpin_raw(r.handle);
        assert_eq!(rt.pin_count(), 0);
    }

    #[test]
    fn release_value_drops_pin() {
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let p = ctx.as_ffi_ptr();
        let r = unsafe { ((*p).alloc_fixnum)(p, 99) };
        assert_eq!(rt.pin_count(), 1);
        unsafe { ((*p).release_value)(p, r) };
        assert_eq!(rt.pin_count(), 0);
    }

    #[test]
    fn alloc_pair_constructs_pair_from_handles() {
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let p = ctx.as_ffi_ptr();
        let car = unsafe { ((*p).alloc_fixnum)(p, 1) };
        let cdr = unsafe { ((*p).alloc_fixnum)(p, 2) };
        let pair = unsafe { ((*p).alloc_pair)(p, car, cdr) };
        assert_ne!(pair.handle, 0);
        let v = rt.lookup_raw(pair.handle).unwrap();
        let s = rt.format_value(&v, cs_core::WriteMode::Write);
        assert_eq!(s, "(1 . 2)");
    }

    #[test]
    fn null_handle_returns_null_from_alloc_pair() {
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let p = ctx.as_ffi_ptr();
        let null = ValueRef { handle: 0 };
        let r = unsafe { ((*p).alloc_pair)(p, null, null) };
        assert_eq!(r.handle, 0);
    }

    #[test]
    fn cs_ffi_example_register_via_direct_call() {
        // Drive cs-ffi-example's crabscheme_register through the
        // real backend, then call (example-magic) and verify it
        // returns 42 on both tiers.
        let mut rt = Runtime::new();
        let mut ctx = rt.ffi_context();
        let p = ctx.as_ffi_ptr();
        let status = cs_ffi_example::crabscheme_register(p);
        assert_eq!(status, cs_ffi_example::RegisterStatus::Ok as i32);

        let walker = rt.eval_str("<test>", "(example-magic)").unwrap();
        match walker {
            Value::Number(cs_core::Number::Fixnum(n)) => {
                assert_eq!(n, cs_ffi_example::EXAMPLE_MAGIC_VALUE);
            }
            other => panic!("walker: expected fixnum, got {:?}", other),
        }

        let vm = rt.eval_str_via_vm("<test>", "(example-magic)").unwrap();
        match vm {
            Value::Number(cs_core::Number::Fixnum(n)) => {
                assert_eq!(n, cs_ffi_example::EXAMPLE_MAGIC_VALUE);
            }
            other => panic!("vm: expected fixnum, got {:?}", other),
        }
    }
}
