//! Region-memory iter 5 — Cycle-detector skip on region-
//! allocated mutations (FR-8).
//!
//! Cycles inside a region are fine: the region's bulk free
//! reclaims every allocation regardless of internal refs.
//! Running the synchronous cycle detector on a region pair
//! would just burn CPU (and could falsely refuse to break a
//! benign cycle through the strong-count guard). This iter
//! adds an `is_region(p) → skip` guard in front of every
//! `cycle::check_and_break(p, …)` call in `b_set_car`,
//! `b_set_cdr`, `b_vector_set`, and `b_hashtable_set`.
//!
//! Tests verify the guard logic by replicating the same code
//! pattern at the test boundary: build a region-allocated
//! Pair, mutate it into a self-cycle via the public Pair
//! accessor (`set_cdr`), then check that an `is_region`-
//! gated `check_and_break` invocation does NOT fire the
//! detector. Contrast with an Rc-allocated Pair where the
//! same gated path DOES fire.

#![cfg(all(feature = "regions", feature = "countable-memory"))]

use std::cell::RefCell;

use cs_core::{Hashtable, HtEqKind, Pair, Value};
use cs_gc::{Gc, Region};
use cs_runtime::countable_memory_cycle::{
    cycle_broken_count, cycle_detection_count, reset_cycle_detection_count,
};

/// Replicates the guard pattern from `b_set_cdr` (cs-runtime/
/// src/builtins/mod.rs). Skips the cycle check on region-
/// allocated pairs.
fn guarded_set_cdr_cycle_check(p: &Gc<Pair>) {
    if Gc::is_region(p) {
        return;
    }
    cs_gc::cycle::check_and_break(p, |_| {
        cs_runtime::countable_memory_cycle::record_cycle_detected();
    });
}

#[test]
fn region_pair_self_cycle_skips_detector() {
    reset_cycle_detection_count();
    let detected_baseline = cycle_detection_count();
    let broken_baseline = cycle_broken_count();
    let region = Region::new();
    let p = Pair::new_in(&region, Value::Boolean(true), Value::Null);
    assert!(Gc::is_region(&p));

    // Form a self-cycle: p.cdr = p.
    p.set_cdr(Value::Pair(p.clone()));

    // Run the guarded check (mirrors b_set_cdr).
    guarded_set_cdr_cycle_check(&p);

    assert_eq!(
        cycle_detection_count(),
        detected_baseline,
        "cycle detector fired on region-allocated pair"
    );
    assert_eq!(
        cycle_broken_count(),
        broken_baseline,
        "cycle break attempted on region-allocated pair"
    );

    // Break the cycle explicitly so the Pair drops cleanly
    // (without the regions feature this would loop on
    // refcount drop; with it, the region's bulk free handles
    // reclamation regardless).
    p.set_cdr(Value::Null);
    drop(p);
    drop(region);
}

#[test]
fn rc_pair_self_cycle_fires_detector() {
    // Contrast: same guard pattern, but the pair is Rc-
    // backed. The detector fires.
    reset_cycle_detection_count();
    let detected_baseline = cycle_detection_count();
    let p = Pair::new(Value::Boolean(true), Value::Null);
    assert!(!Gc::is_region(&p));
    p.set_cdr(Value::Pair(p.clone()));
    guarded_set_cdr_cycle_check(&p);
    assert!(
        cycle_detection_count() > detected_baseline,
        "cycle detector did NOT fire on Rc-backed self-cycle"
    );
    // Clean up to avoid a leak in the test.
    p.set_cdr(Value::Null);
}

#[test]
fn region_vector_self_ref_skips_detector() {
    // Same shape but for vector-set!. The b_vector_set guard
    // is identical in spirit.
    fn guarded_vec_check(v: &Gc<RefCell<Vec<Value>>>) {
        if Gc::is_region(v) {
            return;
        }
        cs_gc::cycle::check_and_break(v, |_| {
            cs_runtime::countable_memory_cycle::record_cycle_detected();
        });
    }

    reset_cycle_detection_count();
    let baseline = cycle_detection_count();
    let region = Region::new();
    let v: Gc<RefCell<Vec<Value>>> =
        Gc::new_in(&region, RefCell::new(vec![Value::Null, Value::Null]));
    assert!(Gc::is_region(&v));
    v.borrow_mut()[0] = Value::Vector(v.clone()); // self-ref
    guarded_vec_check(&v);
    assert_eq!(
        cycle_detection_count(),
        baseline,
        "cycle detector fired on region-allocated vector"
    );
    // Clean up.
    v.borrow_mut()[0] = Value::Null;
    drop(v);
    drop(region);
}

#[test]
fn region_hashtable_self_ref_skips_detector() {
    fn guarded_ht_check(h: &Gc<Hashtable>) {
        if Gc::is_region(h) {
            return;
        }
        cs_gc::cycle::check_and_break(h, |_| {
            cs_runtime::countable_memory_cycle::record_cycle_detected();
        });
    }

    reset_cycle_detection_count();
    let baseline = cycle_detection_count();
    let region = Region::new();
    let ht = Hashtable {
        items: RefCell::new(Vec::new()),
        eq_kind: HtEqKind::Eqv,
        custom: None,
    };
    let h: Gc<Hashtable> = Gc::new_in(&region, ht);
    assert!(Gc::is_region(&h));
    h.items
        .borrow_mut()
        .push((Value::Boolean(true), Value::Hashtable(h.clone()))); // self-ref via items vec
    guarded_ht_check(&h);
    assert_eq!(
        cycle_detection_count(),
        baseline,
        "cycle detector fired on region-allocated hashtable"
    );
    // Clean up.
    h.items.borrow_mut().clear();
    drop(h);
    drop(region);
}
