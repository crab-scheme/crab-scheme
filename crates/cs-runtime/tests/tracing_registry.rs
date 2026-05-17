//! Tracing-revival iter 3 integration test — verify that the
//! synchronous cycle detector populates `cs_gc::cycle_registry`
//! when the `tracing-cycle-collector` feature is on.

#![cfg(all(feature = "tracing-cycle-collector", feature = "countable-memory"))]

use cs_core::{Pair, Value};
use cs_gc::cycle_registry;
use cs_runtime::countable_memory_cycle::{
    record_cycle_with_candidate, reset_cycle_detection_count,
};

#[test]
fn self_cycle_registers_candidate() {
    cycle_registry::reset_for_tests();
    reset_cycle_detection_count();
    let p = Pair::new(Value::Boolean(true), Value::Null);
    p.set_cdr(Value::Pair(p.clone()));
    // Simulate what b_set_cdr's break callback does:
    record_cycle_with_candidate(&p);
    assert_eq!(cycle_registry::candidate_count(), 1);
    // Break the cycle for clean drop.
    p.set_cdr(Value::Null);
}

#[test]
fn region_cycle_does_not_register_candidate() {
    #[cfg(feature = "regions")]
    {
        use cs_gc::{Gc, Region};
        cycle_registry::reset_for_tests();
        let region = Region::new();
        let p = Pair::new_in(&region, Value::Boolean(true), Value::Null);
        p.set_cdr(Value::Pair(p.clone()));
        record_cycle_with_candidate(&p);
        assert_eq!(
            cycle_registry::candidate_count(),
            0,
            "region-allocated cycle must NOT register a candidate (FR-5)"
        );
        // Region drop reclaims.
        let _ = Gc::is_region(&p); // touch to use the import
        p.set_cdr(Value::Null);
        drop(p);
        drop(region);
    }
}

#[test]
fn auto_trigger_fires_sweep_on_next_alloc() {
    use cs_gc::Gc;
    cycle_registry::reset_for_tests();
    cycle_registry::set_auto_trigger_threshold(3);
    // Register 3 candidates from dropped Gc<i64> — their
    // Weaks will be dead, so the sweep will clear them.
    for _ in 0..3 {
        let g: Gc<i64> = Gc::new(0);
        cycle_registry::register_cycle_candidate(Gc::as_addr(&g), Gc::downgrade(&g));
        drop(g);
    }
    assert_eq!(cycle_registry::candidate_count(), 3, "all registered");
    // Threshold reached → SWEEP_PENDING is set; next Gc::new
    // takes the flag and runs run_sweep, which drops the
    // dead-Weak entries.
    let _next: Gc<i64> = Gc::new(99);
    assert_eq!(
        cycle_registry::candidate_count(),
        0,
        "sweep should have cleared dead entries via auto-trigger"
    );
}

#[test]
fn collect_builtin_runs_sweep() {
    use cs_gc::Gc;
    use cs_runtime::Runtime;
    cycle_registry::reset_for_tests();
    // Seed registry with a dead-Weak entry.
    let addr = {
        let g: Gc<i64> = Gc::new(0);
        let a = Gc::as_addr(&g);
        cycle_registry::register_cycle_candidate(a, Gc::downgrade(&g));
        a
    };
    let _ = addr;
    assert_eq!(cycle_registry::candidate_count(), 1);
    // Invoke (collect) through the runtime.
    let mut rt = Runtime::new();
    rt.eval_str("<collect>", "(collect)").expect("collect ok");
    assert_eq!(
        cycle_registry::candidate_count(),
        0,
        "(collect) should run the sweep"
    );
}

#[test]
fn many_cycles_populate_registry() {
    cycle_registry::reset_for_tests();
    let mut pairs = Vec::with_capacity(20);
    for _ in 0..20 {
        let p = Pair::new(Value::Boolean(false), Value::Null);
        p.set_cdr(Value::Pair(p.clone()));
        record_cycle_with_candidate(&p);
        pairs.push(p);
    }
    assert_eq!(cycle_registry::candidate_count(), 20);
    // Break cycles for clean drop.
    for p in &pairs {
        p.set_cdr(Value::Null);
    }
}
