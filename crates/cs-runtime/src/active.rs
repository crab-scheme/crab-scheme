//! Active-runtime back-pointer machinery.
//!
//! Provides the thread-local `ACTIVE_RUNTIME` slot plus `with_active`
//! / `active` accessors. Used throughout the runtime (JIT tier-up
//! hook, builtins that need `&mut Runtime` from inside an eval call,
//! the heap-pointer install for JIT-allocated Gc handles).
//!
//! Pre-M10 these lived in `crate::ffi` because the FFI subsystem was
//! the original consumer. M10 Track W iter 1 extracted them into
//! this module so they remain available when `cs-runtime` is built
//! without the `ffi` feature (e.g. for WASM targets).

use std::cell::Cell;

use crate::Runtime;

thread_local! {
    /// Active-runtime back-pointer, set by [`Runtime::with_active`]
    /// for the duration of an eval call. Read by builtins (e.g.
    /// `(load-shared-library)`, `(gc-stats)`, `(jit-stats)`) to
    /// recover `&mut Runtime` from inside the eval call chain
    /// (where the borrow has been downgraded to `&mut EvalCtx`).
    ///
    /// Single-threaded model: only one runtime active per thread.
    /// Nested `with_active` saves and restores via a stack-local
    /// `prev`, so re-entrancy is safe.
    pub(crate) static ACTIVE_RUNTIME: Cell<*mut Runtime> = const { Cell::new(std::ptr::null_mut()) };
}

impl Runtime {
    /// Run `f` with this runtime stashed in the thread-local
    /// `ACTIVE_RUNTIME` slot. The previous value is saved and
    /// restored on return so nested calls work correctly.
    ///
    /// Used by `Runtime::eval_str` / `eval_str_via_vm` to make
    /// the active runtime reachable from inside builtins that
    /// need it.
    ///
    /// Also installs this runtime's `Heap` pointer in the JIT
    /// allocation TLS slot (ADR 0012 D-2, iter BP). JIT-allocated
    /// Gc<Value> handles produced by Cranelift-emitted code go
    /// through `Heap::alloc` instead of unregistered `Gc::new`,
    /// so the tracing GC sees them. The pointer is cleared on
    /// scope exit.
    pub fn with_active<R>(&mut self, f: impl FnOnce(&mut Runtime) -> R) -> R {
        let prev = ACTIVE_RUNTIME.with(|c| c.replace(self as *mut Runtime));
        // Under default (tracing) the JIT helpers consult
        // JIT_ACTIVE_HEAP to register allocations with the
        // Runtime's tracing GC. Under countable-memory there is
        // no Heap — allocations live by refcount alone — so the
        // setup is a no-op.
        #[cfg(not(feature = "countable-memory"))]
        let prev_heap = {
            // SAFETY: `self.heap` is owned by this Runtime and `self`
            // outlives the closure call below.
            let prev_heap = cs_vm::vm::current_jit_active_heap();
            unsafe { cs_vm::vm::set_jit_active_heap(&self.heap as *const cs_gc::Heap) };
            prev_heap
        };
        let result = f(self);
        #[cfg(not(feature = "countable-memory"))]
        {
            // Restore previous heap pointer (typically null) so nested
            // with_active calls work correctly.
            unsafe { cs_vm::vm::set_jit_active_heap(prev_heap) };
        }
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
            Some(unsafe { &mut *ptr })
        }
    }
}
