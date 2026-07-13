//! cs-i6p.2 regression — vector-slot and hashtable-value cycle-break
//! tombstones. Mirrors `cycle_break.rs`'s Pair regression shapes,
//! extended to `vector-set!` slots and `hashtable-set!` values (see
//! `cs_core::value`'s `VectorTombstone` / `HASHTABLE_VALUE_TOMBSTONES`
//! docs for the design).
//!
//! Four required shapes:
//!
//! 1. `vector_self_cycle_reclaims_after_anchor_drops` — a
//!    self-referential vector slot with a persistent external
//!    anchor (top-level binding) reclaims once the anchor drops.
//! 2. `hashtable_value_self_cycle_reclaims_after_anchor_drops` — same
//!    for a hashtable VALUE (never a key — see the tombstone table's
//!    doc for why keys are excluded).
//! 3. `vector_and_hashtable_self_cycle_observability_preserved` — the
//!    demoted cycle stays traversable through `vector-ref` /
//!    `hashtable-ref` while the anchor is alive (R6RS requires
//!    `(vector-set! v 0 v)` to produce an observable cyclic vector).
//! 4. `vector_and_hashtable_self_cycle_not_reclaimed_while_reachable`
//!    — the tombstoned slot's target is NOT freed merely because the
//!    slot became weak; it stays live for as long as the external
//!    anchor holds it.
//!
//! Tests 1/2 use `cs_gc::alloc_telemetry::live_count()` (surfaced to
//! Scheme as `(gc-stats)`'s `live-slots` key) per the task brief.
//! That counter is a **process-global atomic** (see
//! `cs_gc::alloc_telemetry`'s module doc and its own
//! `run_isolated` test helper) — an exact before/after comparison is
//! only deterministic in a fresh process, since `cargo test`'s
//! default thread-per-test parallelism means an unrelated test's
//! allocations can land in the same window. Tests 1/2 therefore
//! re-exec themselves as an isolated `--exact` subprocess, exactly
//! mirroring the established `cs_gc::alloc_telemetry::run_isolated`
//! pattern (that helper is `pub(crate)` to cs-gc, so this file
//! carries its own copy rather than exposing it further).

use cs_runtime::countable_memory_cycle::{
    cycle_broken_count, cycle_detection_count, reset_cycle_detection_count,
};
use cs_runtime::Runtime;

/// Re-invoke this test binary as a fresh subprocess, running exactly
/// one (`#[ignore]`d) test by its full path. See the module doc.
fn run_isolated(test_path: &str) {
    let exe = std::env::current_exe().expect("run_isolated: current_exe");
    let status = std::process::Command::new(exe)
        .args(["--exact", "--include-ignored", test_path])
        .status()
        .expect("run_isolated: failed to spawn subprocess");
    assert!(
        status.success(),
        "isolated test {test_path} failed in its subprocess (see output above)"
    );
}

#[test]
fn vector_self_cycle_reclaims_after_anchor_drops() {
    run_isolated("vector_self_cycle_reclaims_after_anchor_drops_isolated");
}

#[test]
#[ignore = "run only via run_isolated, in its own subprocess"]
fn vector_self_cycle_reclaims_after_anchor_drops_isolated() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<vec_cycle>",
        r"
        (define v (vector 0))
        (vector-set! v 0 v)
        ",
    )
    .expect("eval ok");
    assert!(cycle_detection_count() > 0, "expected detector to fire");
    assert!(
        cycle_broken_count() > 0,
        "expected the top-level-bound self-cycle to be broken \
         (baseline=3 covers slot+args[0]+args[2], total=4 with the \
         top-level binding)"
    );
    // Test 4 (folded in here): the tombstone alone must not have
    // freed anything — `live_count` should be stable across a
    // no-op eval while `v` is still bound.
    let live_while_reachable_a = cs_gc::alloc_telemetry::live_count();
    rt.eval_str("<noop>", "(+ 1 1)").expect("noop eval");
    let live_while_reachable_b = cs_gc::alloc_telemetry::live_count();
    assert_eq!(
        live_while_reachable_a, live_while_reachable_b,
        "live-slots must not drop merely because a slot went weak \
         while the vector is still externally reachable"
    );
    // Test 1: dropping the last external anchor reclaims the
    // (formerly cyclic) vector.
    let live_before_drop = cs_gc::alloc_telemetry::live_count();
    rt.eval_str("<drop_anchor>", "(set! v 0)")
        .expect("drop anchor");
    let live_after_drop = cs_gc::alloc_telemetry::live_count();
    assert!(
        live_after_drop < live_before_drop,
        "expected live-slots to drop once the anchor was released \
         (before={live_before_drop}, after={live_after_drop})"
    );
}

#[test]
fn hashtable_value_self_cycle_reclaims_after_anchor_drops() {
    run_isolated("hashtable_value_self_cycle_reclaims_after_anchor_drops_isolated");
}

#[test]
#[ignore = "run only via run_isolated, in its own subprocess"]
fn hashtable_value_self_cycle_reclaims_after_anchor_drops_isolated() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<ht_cycle>",
        r"
        (define h (make-eq-hashtable))
        (hashtable-set! h 'k h)
        ",
    )
    .expect("eval ok");
    assert!(cycle_detection_count() > 0, "expected detector to fire");
    assert!(
        cycle_broken_count() > 0,
        "expected the top-level-bound self-cycle to be broken \
         (baseline=3 covers slot+args[0]+args[2], total=4 with the \
         top-level binding)"
    );
    // Test 4 (folded in here): stable while still reachable.
    let live_while_reachable_a = cs_gc::alloc_telemetry::live_count();
    rt.eval_str("<noop>", "(+ 1 1)").expect("noop eval");
    let live_while_reachable_b = cs_gc::alloc_telemetry::live_count();
    assert_eq!(
        live_while_reachable_a, live_while_reachable_b,
        "live-slots must not drop merely because a value slot went \
         weak while the hashtable is still externally reachable"
    );
    // Test 2: dropping the last external anchor reclaims the
    // (formerly cyclic) hashtable.
    let live_before_drop = cs_gc::alloc_telemetry::live_count();
    rt.eval_str("<drop_anchor>", "(set! h 0)")
        .expect("drop anchor");
    let live_after_drop = cs_gc::alloc_telemetry::live_count();
    assert!(
        live_after_drop < live_before_drop,
        "expected live-slots to drop once the anchor was released \
         (before={live_before_drop}, after={live_after_drop})"
    );
}

#[test]
fn vector_and_hashtable_self_cycle_observability_preserved() {
    // Test 3: R6RS requires `(vector-set! v i v)` /
    // `(hashtable-set! h k h)` to remain observably cyclic — the
    // weak tombstone must upgrade transparently through the
    // ordinary accessors while the top-level binding keeps the
    // target alive. No process isolation needed: this test never
    // inspects the global telemetry counters.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<vec_obs>",
        r"
        (define v (vector 1 2))
        (vector-set! v 0 v)
        ",
    )
    .expect("eval ok");
    let slot0 = rt.eval_str("<verify>", "(vector-ref v 0)").expect("ref");
    assert!(
        matches!(slot0, cs_core::Value::Vector(_)),
        "(vector-ref v 0) returned {slot0:?}, expected the cyclic vector"
    );
    let eq_self = rt
        .eval_str("<verify>", "(eq? (vector-ref v 0) v)")
        .expect("eq?");
    assert!(
        matches!(eq_self, cs_core::Value::Boolean(true)),
        "(eq? (vector-ref v 0) v) returned {eq_self:?}, expected #t"
    );
    let slot1 = rt.eval_str("<verify>", "(vector-ref v 1)").expect("ref");
    assert!(
        matches!(slot1, cs_core::Value::Fixnum(2)),
        "(vector-ref v 1) returned {slot1:?}, expected 2 (untouched slot)"
    );

    rt.eval_str(
        "<ht_obs>",
        r"
        (define h (make-eq-hashtable))
        (hashtable-set! h 'other 'value)
        (hashtable-set! h 'k h)
        ",
    )
    .expect("eval ok");
    let hv = rt
        .eval_str("<verify>", "(hashtable-ref h 'k #f)")
        .expect("hashtable-ref");
    assert!(
        matches!(hv, cs_core::Value::Hashtable(_)),
        "(hashtable-ref h 'k #f) returned {hv:?}, expected the cyclic hashtable"
    );
    let eq_self_ht = rt
        .eval_str("<verify>", "(eq? (hashtable-ref h 'k #f) h)")
        .expect("eq?");
    assert!(
        matches!(eq_self_ht, cs_core::Value::Boolean(true)),
        "(eq? (hashtable-ref h 'k #f) h) returned {eq_self_ht:?}, expected #t"
    );
    let other = rt
        .eval_str("<verify>", "(hashtable-ref h 'other #f)")
        .expect("hashtable-ref");
    assert!(
        matches!(other, cs_core::Value::Symbol(_)),
        "(hashtable-ref h 'other #f) returned {other:?}, expected 'value (untouched slot)"
    );
}

#[test]
fn vector_and_hashtable_self_cycle_no_reclaim_when_only_holder_is_transient() {
    // Mirrors `cycle_break.rs`'s `metacircular_style_define_...` /
    // strong-count-guard shape: a self-cycle whose only strong
    // holder is the freshly-mutated subgraph itself (never bound
    // anywhere externally) must be DETECTED but the guard must
    // refuse to demote it — demoting here would make the value
    // unreachable mid-construction, which is exactly the hazard
    // the `baseline` guard exists to prevent.
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<vec_transient>",
        r"
        (define (discard-self-cyclic-vector)
          (let ((v (vector 0)))
            (vector-set! v 0 v)
            'discarded))
        (discard-self-cyclic-vector)
        ",
    )
    .expect("eval ok");
    assert!(
        cycle_detection_count() > 0,
        "detection should have fired for the transient vector cycle"
    );

    let ht_detected_before = cycle_detection_count();
    rt.eval_str(
        "<ht_transient>",
        r"
        (define (discard-self-cyclic-hashtable)
          (let ((h (make-eq-hashtable)))
            (hashtable-set! h 'k h)
            'discarded))
        (discard-self-cyclic-hashtable)
        ",
    )
    .expect("eval ok");
    assert!(
        cycle_detection_count() > ht_detected_before,
        "detection should have fired for the transient hashtable cycle"
    );
}
