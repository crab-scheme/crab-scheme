//! Region-scope stack for lifetime-aware allocation dispatch
//! (layer 5, escape-analysis spec iter 5).
//!
//! When the typer's effect inferencer classifies an
//! allocation as `Lifetime::Region(tag)`, the runtime needs a
//! concrete `cs_gc::Region` to allocate into. This module
//! maintains a LIFO stack of regions; each lifetime-aware
//! expression enters the stack on introduction (via
//! [`RegionScope::enter`]) and the corresponding RAII guard
//! pops on drop.
//!
//! # Dual-stack: TLS vs. tokio task-local
//!
//! There are *two* parallel stacks (parallel-runtime spec
//! C3.1):
//!
//! - **TLS stack** ([`REGION_STACK_TLS`]) — per-thread; used
//!   by single-threaded contexts (REPL, `crabscheme run`,
//!   tests). This is the only stack when `feature = "actor"`
//!   is off.
//! - **Task-local stack** ([`REGION_STACK_TASK`]) — per
//!   tokio task; used by actor bodies. An actor task can
//!   migrate between tokio worker threads at `(receive)` /
//!   reduction-budget yield boundaries; the task-local stack
//!   travels with the task, so a `(with-region …)` opened on
//!   worker A is still in scope when the actor resumes on
//!   worker B.
//!
//! `RegionScope::enter` and [`current_region`] both check the
//! task-local stack first (returning its top even when empty
//! — see the note in [`current_region`] about not falling
//! through to TLS), then fall back to TLS. The
//! [`region_lookup_diagnostic`] helper exposes which stack
//! was queried so callers (e.g., `alloc_dispatch::cons_in`)
//! can format context-aware errors.
//!
//! # Known limitation: full task-local scope wiring
//!
//! `primop_spawn` (cs-runtime/src/builtins/beam.rs) **does
//! not yet wrap actor bodies in
//! `REGION_STACK_TASK.scope(…)`**. The infrastructure is in
//! place and unit-tested synthetically, but live wiring is
//! blocked on `cs_gc::Region: !Send` — a task-local holding
//! `RefCell<Vec<Rc<Region>>>` makes the spawned Future
//! `!Send`, which the multi_thread tokio runtime rejects.
//! Until `Region` becomes Send (open follow-up), an actor's
//! `(with-region …)` rides the TLS path; it works correctly
//! as long as the body doesn't yield between push and pop
//! and migrate to a worker with empty TLS. The diagnostic
//! string in `alloc_dispatch::no_region_err` points users at
//! this case explicitly when triggered from actor context.
//!
//! # Consumers
//!
//! The walker tier consults [`current_region`] in its
//! allocation dispatch helpers; the VM does the same in
//! `vm_alloc_pair_region_gc` etc. (iter 6); AOT-emitted
//! code via [`region_resolver_for_cs_vm`] (gap B-3).
//!
//! Gated on `feature = "regions"` (forwarded from cs-gc).

#![cfg(feature = "regions")]

use std::cell::RefCell;
use std::rc::Rc;

use cs_gc::Region;

thread_local! {
    /// LIFO stack of regions in scope on this thread. The
    /// innermost (most recently entered) region is at the
    /// top.
    ///
    /// Used by non-actor contexts (single-threaded REPL,
    /// `crabscheme run` outside an actor body). Actor bodies
    /// use [`REGION_STACK_TASK`] instead — see C3.1 in the
    /// parallel-runtime spec for the dual-stack rationale.
    ///
    /// Held as `Rc<Region>` rather than borrows so the
    /// stack discipline can outlive an individual function
    /// call frame — e.g., the walker can stash a region
    /// across a tail call and the bytecode dispatcher can
    /// hold one across a yield. Reference counting keeps
    /// the region alive until the last `RegionScope` (and
    /// any region-allocated handles) drops.
    static REGION_STACK_TLS: RefCell<Vec<Rc<Region>>> = const { RefCell::new(Vec::new()) };
}

#[cfg(feature = "actor")]
tokio::task_local! {
    /// Per-tokio-task region stack (parallel-runtime spec
    /// C3.1). Actor bodies install this via
    /// `REGION_STACK_TASK.scope(...)` at task startup; for the
    /// task's lifetime, [`current_region`] and `RegionScope`
    /// route to this stack instead of the per-thread TLS.
    ///
    /// **Why a separate stack:** an actor task that opens
    /// `(with-region ...)` and then yields at a `(recv)` /
    /// reduction boundary may resume on a different tokio
    /// worker thread. The TLS stack on the new worker doesn't
    /// contain the actor's region, so `cons-in-region` errors
    /// (or worse, allocates into the wrong region). The
    /// task-local travels with the task across worker hops.
    pub(crate) static REGION_STACK_TASK: RefCell<Vec<Rc<Region>>>;
}

/// Test/debug helper enumeration of which stack was used at
/// `RegionScope::enter` time. Drop pops from the same stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackKind {
    Tls,
    #[cfg(feature = "actor")]
    Task,
}

/// RAII guard binding a region to the current thread's (or
/// task's) region stack. Drops pop from the same stack it was
/// pushed to.
pub struct RegionScope {
    kind: StackKind,
}

impl RegionScope {
    /// Push `region` onto the region stack and return a
    /// drop-guard that pops on scope exit. Chooses the
    /// task-local stack if the caller is inside a tokio task
    /// that has scoped `REGION_STACK_TASK`; otherwise falls
    /// back to the per-thread TLS stack.
    pub fn enter(region: Rc<Region>) -> Self {
        #[cfg(feature = "actor")]
        {
            // try_with returns Err if no scope set the task-local;
            // that's the "not inside an actor body" case, fall
            // through to TLS.
            let pushed_task = REGION_STACK_TASK
                .try_with(|s| s.borrow_mut().push(region.clone()))
                .is_ok();
            if pushed_task {
                return RegionScope {
                    kind: StackKind::Task,
                };
            }
        }
        REGION_STACK_TLS.with(|s| s.borrow_mut().push(region));
        RegionScope {
            kind: StackKind::Tls,
        }
    }
}

impl Drop for RegionScope {
    fn drop(&mut self) {
        match self.kind {
            StackKind::Tls => {
                REGION_STACK_TLS.with(|s| {
                    s.borrow_mut().pop();
                });
            }
            #[cfg(feature = "actor")]
            StackKind::Task => {
                // try_with is Err only if the task-local scope
                // ended before the RegionScope dropped, which
                // would be a usage bug; ignore silently here
                // since panicking from Drop is hazardous.
                let _ = REGION_STACK_TASK.try_with(|s| {
                    s.borrow_mut().pop();
                });
            }
        }
    }
}

/// The innermost in-scope region, or `None` if none is in
/// scope. Cheap (atomic clone of an `Rc`); safe to call from
/// allocation hot paths.
///
/// Checks the task-local stack first (actor context), falls
/// back to the per-thread TLS (non-actor context).
pub fn current_region() -> Option<Rc<Region>> {
    #[cfg(feature = "actor")]
    {
        if let Ok(rc) = REGION_STACK_TASK.try_with(|s| s.borrow().last().cloned()) {
            // If we're inside the task-local scope, return its
            // top — even if it's None. An empty task-local
            // stack should NOT fall through to TLS, because
            // that would let an actor see a stale region from
            // a non-actor caller on the same worker thread.
            return rc;
        }
    }
    REGION_STACK_TLS.with(|s| s.borrow().last().cloned())
}

/// Which stack(s) currently hold any region, for error messages.
/// Returned by [`region_lookup_diagnostic`] when the caller hit
/// an empty-stack situation and needs to format a context-aware
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionLookupDiagnostic {
    /// Not inside an actor task; the TLS stack is empty.
    /// Most likely the user forgot `(with-region …)`.
    NoTlsScope,
    /// Inside an actor task and the task-local stack is empty.
    /// Most likely the actor's body didn't open
    /// `(with-region …)`, or it did but yielded across the
    /// boundary on a build that doesn't yet have Send-Region
    /// (see parallel-runtime C3.2's partial scope).
    NoTaskScope,
}

/// Reports which stack the caller was queried against, so
/// `no_region_err`-style error messages can point at the
/// likely cause. Cheap (one `try_with` probe).
pub fn region_lookup_diagnostic() -> RegionLookupDiagnostic {
    #[cfg(feature = "actor")]
    {
        // try_with returns Ok inside an actor task even if the
        // task-local stack is empty. That tells us we're in
        // actor context and the user's `(with-region …)`
        // either didn't run or was lost to migration.
        if REGION_STACK_TASK.try_with(|_| ()).is_ok() {
            return RegionLookupDiagnostic::NoTaskScope;
        }
    }
    RegionLookupDiagnostic::NoTlsScope
}

/// Current depth of the region stack — the task-local scope depth (if inside a
/// task scope) plus the TLS depth. Only one path is non-zero at a time, but the
/// sum is correct either way. Used by the green driver's region-park guard
/// (`pump_coroutine`, which must run in release — a region scope spanning a
/// suspend on a shared worker is unsound) and by tests.
pub fn region_stack_depth() -> usize {
    let mut total = REGION_STACK_TLS.with(|s| s.borrow().len());
    #[cfg(feature = "actor")]
    {
        if let Ok(n) = REGION_STACK_TASK.try_with(|s| s.borrow().len()) {
            total += n;
        }
    }
    total
}

/// Gap B-3 cs-aot region resolver. Returns a raw pointer to
/// the innermost in-scope `Region`, or null if none. Used by
/// cs-vm's `vm_alloc_pair_region_gc` (and future AOT-emitted
/// code) via the `register_region_resolver` function-pointer
/// hook — avoids a cs-vm ↔ cs-runtime dep cycle.
///
/// # Safety
///
/// The returned pointer is valid only while the
/// corresponding `RegionScope` is alive on this thread. The
/// caller must use it before any `RegionScope::Drop` runs.
/// In practice, JIT/AOT emitted code calls this immediately
/// before `Pair::new_in` and discards the ptr — within a
/// single function call, region drop can't happen.
pub extern "C" fn region_resolver_for_cs_vm() -> *const () {
    // Same dual-stack lookup as current_region: task-local
    // first (if inside an actor task), then TLS.
    #[cfg(feature = "actor")]
    {
        if let Ok(ptr) = REGION_STACK_TASK.try_with(|s| match s.borrow().last() {
            Some(rc) => std::rc::Rc::as_ptr(rc) as *const (),
            None => std::ptr::null(),
        }) {
            return ptr;
        }
    }
    REGION_STACK_TLS.with(|s| match s.borrow().last() {
        Some(rc) => std::rc::Rc::as_ptr(rc) as *const (),
        None => std::ptr::null(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stack_returns_none() {
        // Stack starts empty per thread.
        assert!(current_region().is_none());
        assert_eq!(region_stack_depth(), 0);
    }

    #[test]
    fn enter_pushes_and_drop_pops() {
        assert_eq!(region_stack_depth(), 0);
        let r = Rc::new(Region::new());
        {
            let _guard = RegionScope::enter(Rc::clone(&r));
            assert_eq!(region_stack_depth(), 1);
            let cur = current_region().expect("region in scope");
            assert!(Rc::ptr_eq(&cur, &r));
        }
        assert_eq!(region_stack_depth(), 0);
        assert!(current_region().is_none());
    }

    #[test]
    fn nested_scopes_lifo() {
        let r1 = Rc::new(Region::new());
        let r2 = Rc::new(Region::new());
        let _outer = RegionScope::enter(Rc::clone(&r1));
        assert!(Rc::ptr_eq(&current_region().unwrap(), &r1));
        {
            let _inner = RegionScope::enter(Rc::clone(&r2));
            assert!(Rc::ptr_eq(&current_region().unwrap(), &r2));
        }
        // Inner popped — outer remains.
        assert!(Rc::ptr_eq(&current_region().unwrap(), &r1));
    }

    #[test]
    fn current_region_clone_keeps_region_alive_after_pop() {
        let r = Rc::new(Region::new());
        let id_before = r.id();
        let stashed = {
            let _guard = RegionScope::enter(Rc::clone(&r));
            current_region().unwrap()
        };
        // Scope popped, but `stashed` and `r` keep the region
        // alive — its id still matches the original.
        assert_eq!(stashed.id(), id_before);
        assert_eq!(r.id(), id_before);
    }

    // ---- parallel-runtime spec C3.1 — dual-stack tests ----

    /// Inside an actor-style task-local scope, `enter`/`current`
    /// route through the task-local stack, not TLS. The TLS
    /// stack stays empty.
    #[cfg(feature = "actor")]
    #[test]
    fn enter_inside_task_scope_uses_task_local() {
        // Build a single-thread runtime so we can drive the
        // task-local scope synchronously from this test.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let stack = std::cell::RefCell::new(Vec::new());
            REGION_STACK_TASK
                .scope(stack, async {
                    let r = Rc::new(Region::new());
                    assert!(current_region().is_none(), "stack starts empty in task");
                    let _g = RegionScope::enter(Rc::clone(&r));
                    assert!(
                        Rc::ptr_eq(&current_region().unwrap(), &r),
                        "current_region reads from task-local stack"
                    );
                    // TLS untouched on this thread.
                    let tls_depth = REGION_STACK_TLS.with(|s| s.borrow().len());
                    assert_eq!(tls_depth, 0, "TLS not touched when task-local in use");
                })
                .await;
            // After the scope ends, the task-local is gone but
            // TLS is still empty.
            assert!(current_region().is_none());
        });
    }

    /// Outside any task-local scope, `enter`/`current` route to
    /// TLS — same as the pre-C3.1 single-stack behavior. Covers
    /// the REPL / `crabscheme run` case.
    #[cfg(feature = "actor")]
    #[test]
    fn enter_outside_task_scope_uses_tls() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        assert!(Rc::ptr_eq(&current_region().unwrap(), &r));
        // The push landed in TLS, not the task-local (no scope).
        let tls_depth = REGION_STACK_TLS.with(|s| s.borrow().len());
        assert_eq!(tls_depth, 1);
    }

    /// Drop after task-local scope exit doesn't panic — the
    /// `try_with` in Drop swallows the missing-scope case. This
    /// would only happen on a usage bug (RegionScope outliving
    /// its scope()), but verifies the Drop is safe regardless.
    #[cfg(feature = "actor")]
    #[test]
    fn drop_after_task_scope_exit_is_safe() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Build a RegionScope inside the task, leak it out, then
        // observe Drop runs after scope() ends without panic.
        let scope_holder: std::cell::RefCell<Option<RegionScope>> = std::cell::RefCell::new(None);
        rt.block_on(async {
            let stack = std::cell::RefCell::new(Vec::new());
            REGION_STACK_TASK
                .scope(stack, async {
                    let r = Rc::new(Region::new());
                    *scope_holder.borrow_mut() = Some(RegionScope::enter(r));
                })
                .await;
        });
        // Drop happens here, outside the task scope. Should
        // not panic.
        drop(scope_holder.into_inner());
    }
}
