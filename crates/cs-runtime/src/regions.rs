//! Per-thread region-scope stack for lifetime-aware
//! allocation dispatch (layer 5, escape-analysis spec iter 5).
//!
//! When the typer's effect inferencer classifies an
//! allocation as `Lifetime::Region(tag)`, the runtime needs a
//! concrete `cs_gc::Region` to allocate into. This module
//! maintains a per-thread stack of regions; each
//! lifetime-aware expression enters the stack on
//! introduction (via [`RegionScope::enter`]) and the
//! corresponding RAII guard pops on drop.
//!
//! The walker tier consults [`current_region`] in its
//! allocation dispatch helpers; the VM does the same in
//! `vm_alloc_pair_region_gc` etc. (iter 6).
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
    /// Held as `Rc<Region>` rather than borrows so the
    /// stack discipline can outlive an individual function
    /// call frame — e.g., the walker can stash a region
    /// across a tail call and the bytecode dispatcher can
    /// hold one across a yield. Reference counting keeps
    /// the region alive until the last `RegionScope` (and
    /// any region-allocated handles) drops.
    static REGION_STACK: RefCell<Vec<Rc<Region>>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard binding a region to the current thread's
/// region stack. Drops pop the entry.
pub struct RegionScope {
    _private: (),
}

impl RegionScope {
    /// Push `region` onto the per-thread stack and return a
    /// drop-guard that pops on scope exit.
    pub fn enter(region: Rc<Region>) -> Self {
        REGION_STACK.with(|s| s.borrow_mut().push(region));
        RegionScope { _private: () }
    }
}

impl Drop for RegionScope {
    fn drop(&mut self) {
        REGION_STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// The innermost in-scope region, or `None` if none is in
/// scope. Cheap (atomic clone of an `Rc`); safe to call from
/// allocation hot paths.
pub fn current_region() -> Option<Rc<Region>> {
    REGION_STACK.with(|s| s.borrow().last().cloned())
}

/// Test/debug helper: current depth of the region stack.
#[cfg(any(test, debug_assertions))]
pub fn region_stack_depth() -> usize {
    REGION_STACK.with(|s| s.borrow().len())
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
    REGION_STACK.with(|s| match s.borrow().last() {
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
}
