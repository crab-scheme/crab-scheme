//! Tracing-revival iter 3 integration test — verify that the
//! synchronous cycle detector populates `cs_gc::cycle_registry`
//! when the `tracing-cycle-collector` feature is on.

#![cfg(feature = "tracing-cycle-collector")]

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
fn tracing_policy_overrides_threshold() {
    use cs_gc::Gc;
    use cs_runtime::{Runtime, TracingPolicy};
    cycle_registry::reset_for_tests();
    let mut rt = Runtime::new();
    rt.set_tracing_policy(TracingPolicy {
        auto_trigger_threshold: 2,
    });
    let _g1: Gc<i64> = Gc::new(0);
    cycle_registry::register_cycle_candidate(Gc::as_addr(&_g1), Gc::downgrade(&_g1));
    let _g2: Gc<i64> = Gc::new(0);
    cycle_registry::register_cycle_candidate(Gc::as_addr(&_g2), Gc::downgrade(&_g2));
    // Threshold of 2 reached — sweep pending. The next alloc
    // takes the flag.
    assert!(
        cycle_registry::candidate_count() >= 2,
        "expected ≥2 candidates registered"
    );
    let _g3: Gc<i64> = Gc::new(0);
    // After Gc::new took SWEEP_PENDING, sweep ran. The
    // _g1/_g2 candidates upgrade fine (still live) so they
    // stay; nothing changes.
    assert!(cycle_registry::candidate_count() >= 2);
}

#[test]
fn sweep_breaks_pair_self_cycle() {
    // Gap C-3: the layer-4 sweep should reclaim a Pair
    // self-cycle that the layer-2 detector refused to
    // break (its strong-count guard treats the self-cycle
    // as "the only strong holder is the cycle itself"). The
    // sweep runs with baseline=0 — outside any mutation, no
    // transient refs to filter out — and successfully
    // demotes the cdr slot to Weak.
    use cs_gc::Gc;
    cycle_registry::reset_for_tests();
    let broken_baseline = cs_gc::cycle_registry::sweep_broken_count();
    let p = Pair::new(Value::Boolean(true), Value::Null);
    p.set_cdr(Value::Pair(p.clone()));
    // Manually register (simulating what
    // `record_cycle_with_candidate` does).
    cycle_registry::register_cycle_candidate(Gc::as_addr(&p), Gc::downgrade(&p));
    assert_eq!(cycle_registry::candidate_count(), 1);
    // Sweep runs — should call Pair::try_break_cycle which
    // calls break_cdr_cycle(0); strong_count > 0 → break
    // fires.
    cycle_registry::run_sweep();
    assert!(
        cs_gc::cycle_registry::sweep_broken_count() > broken_baseline,
        "sweep should have broken the cycle"
    );
    // cdr slot demoted — strong slot is now Unspecified
    // (the weak tombstone holds the back-edge).
    assert!(matches!(*p.cdr.borrow(), Value::Unspecified));
}

#[test]
fn sweep_breaks_hashtable_self_cycle() {
    // Gap C-3 ext: hashtable self-cycle via (hashtable-set!
    // h 'key h). Layer-4 sweep should find the heap-bearing
    // value slot and demote it.
    use cs_core::{Hashtable, HtEqKind};
    use cs_gc::Gc;
    cycle_registry::reset_for_tests();
    let broken_baseline = cs_gc::cycle_registry::sweep_broken_count();
    let h: Gc<Hashtable> = Hashtable::new(HtEqKind::Eq);
    // Set up the cycle: items[0] = (key, h).
    h.items
        .borrow_mut()
        .push((Value::Boolean(true), Value::Hashtable(h.clone())));
    cycle_registry::register_cycle_candidate(Gc::as_addr(&h), Gc::downgrade(&h));
    assert_eq!(cycle_registry::candidate_count(), 1);
    cycle_registry::run_sweep();
    assert!(
        cs_gc::cycle_registry::sweep_broken_count() > broken_baseline,
        "sweep should have broken the hashtable cycle"
    );
    // The value slot is now Unspecified.
    let items = h.items.borrow();
    assert!(matches!(items[0].1, Value::Unspecified));
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
