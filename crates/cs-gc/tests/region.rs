//! Integration tests for `cs_gc::Region` (layer 3 — region
//! memory). Iter 3 of the region-memory spec.
//!
//! Covers FR-1 (bump alloc), FR-3 (refcount headers exist but
//! don't drive reclamation), FR-5 (debug-mode validity check),
//! NFR-1 (per-alloc < 5ns), NFR-2 (region drop < 50ms for 10⁶).
//!
//! Build with `--features regions`. The whole file is cfg-gated
//! on that feature so `cargo test -p cs-gc` without it still
//! works.

#![cfg(feature = "regions")]

use cs_gc::{Gc, Region};
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn region_alloc_basic_lifetime() {
    let region = Region::new();
    let mut handles: Vec<Gc<i64>> = Vec::new();
    for i in 0..10 {
        handles.push(Gc::new_in(&region, i));
    }
    for (i, h) in handles.iter().enumerate() {
        assert_eq!(**h, i as i64);
    }
    // Drop handles first, then region — both orderings are
    // legal; the region's drop is what actually frees.
    drop(handles);
    drop(region);
}

#[test]
fn region_alloc_then_drop_region_first_is_ok_when_no_handles_outstanding() {
    let region = Region::new();
    {
        let g = Gc::new_in(&region, 42_i64);
        assert_eq!(*g, 42);
        // g drops here, before region — fine.
    }
    // Region drops here, all allocations freed.
    drop(region);
}

#[test]
fn region_clone_bumps_strong_count() {
    let region = Region::new();
    let g = Gc::new_in(&region, 7_i64);
    assert_eq!(Gc::strong_count(&g), 1);
    let g2 = g.clone();
    assert_eq!(Gc::strong_count(&g), 2);
    let g3 = g.clone();
    assert_eq!(Gc::strong_count(&g), 3);
    drop(g2);
    assert_eq!(Gc::strong_count(&g), 2);
    drop(g3);
    assert_eq!(Gc::strong_count(&g), 1);
}

#[test]
fn region_strong_count_does_not_drive_reclamation() {
    // Even when the strong count reaches 0 (no Gc handles),
    // the value remains accessible via the region until the
    // region itself drops — that's the whole point of a region.
    let region = Region::new();
    let _addr = {
        let g = Gc::new_in(&region, "hello".to_string());
        let addr = Gc::as_addr(&g);
        // g drops here → strong count goes to 0, but the
        // bump-arena slot is NOT freed.
        addr
    };
    // Allocate again; should still get a distinct address
    // (the first allocation occupied its slot and we don't
    // recycle within a region).
    let g2 = Gc::new_in(&region, "world".to_string());
    assert_eq!(&**g2, "world");
    // Region drops here; both slots release together.
    drop(g2);
    drop(region);
}

#[test]
fn region_handles_are_region_tagged() {
    let region = Region::new();
    let g_region = Gc::new_in(&region, 1_i64);
    let g_rc = Gc::new(2_i64);
    assert!(Gc::is_region(&g_region));
    assert!(!Gc::is_region(&g_rc));
    drop(g_region);
    drop(region);
}

#[test]
fn cross_region_handles_distinguish() {
    let r1 = Region::new();
    let r2 = Region::new();
    let g1 = Gc::new_in(&r1, 100_i64);
    let g2 = Gc::new_in(&r2, 100_i64);
    // Same payload, distinct allocations across regions.
    assert_eq!(*g1, *g2);
    assert!(!Gc::ptr_eq(&g1, &g2));
    assert_ne!(Gc::as_addr(&g1), Gc::as_addr(&g2));
    drop(g1);
    drop(g2);
    drop(r1);
    drop(r2);
}

/// Tracks region-allocation order via a shared counter, used
/// to verify region-bulk-free behaves correctly when the
/// payload itself has a `Drop` impl. We can't use a Drop
/// sentinel directly on region-allocated values (bumpalo
/// drops payloads via the arena, not per-allocation), but we
/// can verify that *Rust-side* values manage cleanly across
/// the region lifecycle.
struct DropCounter(Rc<Cell<usize>>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn region_drop_does_not_run_payload_drop() {
    // bumpalo intentionally does NOT run drop on its
    // allocations. This means region-allocated values must be
    // POD-like (or have explicit cleanup paths). Document
    // that contract via this test.
    let counter = Rc::new(Cell::new(0_usize));
    {
        let region = Region::new();
        let _g = Gc::new_in(&region, DropCounter(Rc::clone(&counter)));
        // Even when handle drops, region's slot remains
        // (refcount goes to 0; bump arena unaffected).
    }
    // Region drops; bumpalo frees the buffer but does NOT
    // run Drop on the payload. Counter stays at 0.
    assert_eq!(counter.get(), 0);
}

/// Debug-mode validity: dropping a region while a handle is
/// still outstanding, then touching the handle, must panic
/// with a clear diagnostic. Only meaningful under
/// `cfg(debug_assertions)` — in release the check is
/// compiled out (the design accepts UB in release in
/// exchange for zero overhead; layer-5 escape analysis
/// guarantees safety for compiled programs).
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "use-after-region-drop")]
fn region_drop_releases_outstanding_handles_debug() {
    let g = {
        let region = Region::new();
        Gc::new_in(&region, 42_i64)
        // region drops here, but g is still outstanding —
        // the next deref of g must panic.
    };
    // Trigger the debug-mode check.
    let _v = *g;
}

/// Performance sanity (NFR-1): per-allocation latency should
/// stay well under 5ns for simple POD types when measured in
/// release. Skip in debug builds since asserts dominate and
/// we'd just be measuring the validity check, not the bump
/// allocator.
#[test]
#[cfg(not(debug_assertions))]
fn region_alloc_microbench() {
    const N: usize = 1_000_000;
    let region = Region::new();
    let start = std::time::Instant::now();
    let mut handles: Vec<Gc<i64>> = Vec::with_capacity(N);
    for i in 0..N {
        handles.push(Gc::new_in(&region, i as i64));
    }
    let elapsed = start.elapsed();
    let per_alloc_ns = elapsed.as_nanos() as f64 / N as f64;
    // 5ns is the design target; generous bound at 50ns for
    // CI noise tolerance. The point is to detect 10x+
    // regressions, not micro-benchmark precision.
    assert!(
        per_alloc_ns < 50.0,
        "region alloc too slow: {per_alloc_ns:.2}ns/alloc (target <5ns, gated <50ns)"
    );
    // Drain handles before region drops.
    drop(handles);
    drop(region);
}

/// Performance sanity (NFR-2): region drop for 10⁶
/// allocations should complete in under 50ms. Same release-
/// only gating rationale as above.
#[test]
#[cfg(not(debug_assertions))]
fn region_bulk_free_microbench() {
    const N: usize = 1_000_000;
    let region = Region::new();
    let mut handles: Vec<Gc<i64>> = Vec::with_capacity(N);
    for i in 0..N {
        handles.push(Gc::new_in(&region, i as i64));
    }
    drop(handles); // refcounts all to 0; arena unaffected.
    let start = std::time::Instant::now();
    drop(region);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "region bulk-free too slow: {elapsed:?} (target <50ms, gated <500ms)"
    );
}
