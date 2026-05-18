//! Gap B-3 integration — verify the cs-vm
//! `vm_alloc_pair_region_gc` helper successfully dispatches
//! to the per-thread region stack via the resolver
//! registered from `Runtime::new`.

#![cfg(all(feature = "regions", feature = "countable-memory"))]

use std::rc::Rc;

use cs_core::Value;
use cs_gc::{Gc, Region};
use cs_runtime::regions::RegionScope;

#[test]
fn region_resolver_returns_null_when_no_scope() {
    // Force a fresh Runtime so the resolver gets registered.
    let _rt = cs_runtime::Runtime::new();
    let ptr = cs_runtime::regions::region_resolver_for_cs_vm();
    assert!(ptr.is_null(), "no RegionScope entered; expected null");
}

#[test]
fn region_resolver_returns_current_region_under_scope() {
    let _rt = cs_runtime::Runtime::new();
    let r = Rc::new(Region::new());
    let want_ptr = Rc::as_ptr(&r) as *const ();
    let _guard = RegionScope::enter(Rc::clone(&r));
    let got_ptr = cs_runtime::regions::region_resolver_for_cs_vm();
    assert_eq!(got_ptr, want_ptr);
}

#[test]
fn vm_alloc_pair_region_gc_returns_nonzero_handle() {
    // Verify the helper returns a non-null handle in both
    // the region-in-scope and no-region cases. (Decoding
    // the raw nanbox requires cs-vm-private helpers; the
    // decode path is exercised in cs-vm's own gc_helper_tests
    // module. This test just confirms the wiring + non-null
    // contract from cs-runtime's perspective.)
    use cs_vm::vm::{vm_alloc_pair_region_gc, JIT_RT_FIXNUM};
    let _rt = cs_runtime::Runtime::new();

    // 1. No RegionScope — fallback path.
    let raw = unsafe { vm_alloc_pair_region_gc(1, JIT_RT_FIXNUM, 2, JIT_RT_FIXNUM) };
    assert_ne!(raw, 0, "fallback path returned null handle");
    // Reclaim the raw handle so the test doesn't leak.
    unsafe {
        let _: Gc<cs_core::Pair> = Gc::from_raw_jit(extract_pair_ptr(raw));
    }

    // 2. Inside a RegionScope — region path.
    let r = Rc::new(Region::new());
    let _guard = RegionScope::enter(Rc::clone(&r));
    let raw2 = unsafe { vm_alloc_pair_region_gc(3, JIT_RT_FIXNUM, 4, JIT_RT_FIXNUM) };
    assert_ne!(raw2, 0, "region path returned null handle");
    unsafe {
        let _: Gc<cs_core::Pair> = Gc::from_raw_jit(extract_pair_ptr(raw2));
    }
    let _ = Value::Null; // touch Value to use the import
}

/// Strip the NaN-box tag bits to get the raw Pair pointer.
/// Mirrors cs-vm's internal NB_PAYLOAD_MASK + cast.
fn extract_pair_ptr(nb_i64: i64) -> *const () {
    const NB_PAYLOAD_MASK: u64 = (1u64 << 47) - 1;
    let bits = nb_i64 as u64;
    (bits & NB_PAYLOAD_MASK) as *const ()
}
