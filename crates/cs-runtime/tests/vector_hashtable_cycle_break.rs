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
    // Mirrors `cycle_break.rs`'s `metacircular_style_define_...`
    // shape: a self-cycle whose only strong holder is a `let`-local
    // that never escapes the enclosing call. Detection fires either
    // way; demotion ALSO fires here (`total` = slot + args[0] +
    // args[2] + the `let` binding = 4 > baseline 3), so this is NOT
    // a case where the `baseline` guard declines to demote — the
    // guard's job is purely to avoid demoting a slot whose demotion
    // would drop the value's *only* strong reference (0 vs. 1
    // external refs at baseline+1 total). Here the demoted slot's
    // target stays reachable for the rest of the call purely
    // because `vector_get`/`value_at` transparently upgrade the
    // weak tombstone back to the live value — the same
    // observability path proven by
    // `vector_and_hashtable_self_cycle_observability_preserved`
    // above. Once the `let` binding itself goes out of scope, the
    // (now-weak) reference drops and the value reclaims normally;
    // this test only checks that detection AND (here) demotion both
    // fire without panicking or corrupting the transient value
    // before it's discarded.
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
    assert!(
        cycle_broken_count() > 0,
        "total refs (slot+args[0]+args[2]+let-binding=4) exceed baseline=3, \
         so demotion should fire even though the binding is transient"
    );

    let ht_detected_before = cycle_detection_count();
    let ht_broken_before = cycle_broken_count();
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
    assert!(
        cycle_broken_count() > ht_broken_before,
        "same total-refs-over-baseline reasoning applies to the hashtable case"
    );
}

/// Multi-element vector: cycling through slot 1 of a 3-element
/// vector must not disturb slots 0/2, and the demoted slot must
/// still round-trip through `vector-ref`.
#[test]
fn vector_multi_element_self_cycle_isolates_slot() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<multi_vec>",
        r"
        (define v (vector 'a 0 'c))
        (vector-set! v 1 v)
        ",
    )
    .expect("eval ok");
    let r0 = rt
        .eval_str("<r0>", "(eq? (vector-ref v 0) 'a)")
        .expect("r0");
    let r2 = rt
        .eval_str("<r2>", "(eq? (vector-ref v 2) 'c)")
        .expect("r2");
    assert_eq!(format!("{r0}"), "#t");
    assert_eq!(format!("{r2}"), "#t");
    let r1 = rt.eval_str("<r1>", "(eq? (vector-ref v 1) v)").expect("r1");
    assert_eq!(format!("{r1}"), "#t");
}

/// Cross-type cycle: vector -> pair -> vector, closing through the
/// vector's own slot. Exercises `CycleVisit`/`BreakCycle` walking
/// through a heap-bearing intermediate type rather than a pure
/// self-reference.
#[test]
fn vector_pair_vector_cross_type_cycle_breaks_and_stays_observable() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<cross_cycle>",
        r"
        (define v (vector 0))
        (define p (cons v 'tail))
        (vector-set! v 0 p)
        ",
    )
    .expect("eval ok");
    // Still observable: (car (vector-ref v 0)) should be v itself.
    let r = rt
        .eval_str("<cross_read>", "(eq? (car (vector-ref v 0)) v)")
        .expect("cross read");
    assert_eq!(format!("{r}"), "#t");
}

/// Regression for the self-healing fix: a raw writer that bypasses
/// `vector_set` (`vector-fill!`) must not leave a stale tombstone
/// that shadows the fresh value on a later `vector-ref`.
#[test]
fn vector_fill_after_demotion_is_not_shadowed_by_stale_tombstone() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<fill_setup>",
        r"
        (define v (vector 0))
        (vector-set! v 0 v)
        ",
    )
    .expect("eval ok");
    // Sanity: the self-cycle is observable before the fill.
    let before = rt
        .eval_str("<before_fill>", "(eq? (vector-ref v 0) v)")
        .expect("before fill");
    assert_eq!(format!("{before}"), "#t");
    rt.eval_str("<do_fill>", "(vector-fill! v 42)")
        .expect("fill ok");
    let after = rt
        .eval_str("<after_fill>", "(vector-ref v 0)")
        .expect("after fill");
    assert_eq!(
        format!("{after}"),
        "42",
        "a stale tombstone must not shadow vector-fill!'s raw write"
    );
}

// A VM-tier-vs-walker-tier hashtable-builtin regression
// (`vm_tier_hashtable_builtins_honor_value_tombstones`) lives as a
// whitebox unit test in `cs-runtime/src/lib.rs`'s own `tests`
// module instead of here: a walker-tier `eval_str` call and a
// VM-tier `eval_str_via_vm` call on the same `Runtime` don't share
// a top-level environment (`top` vs. `vm_env` are separate globals
// tables), so there's no way to hand a walker-demoted hashtable to
// a VM-tier session through Scheme source alone. The unit test
// builds the demoted hashtable directly against `cs_core::Hashtable`
// (the same API `b_hashtable_set` uses) and defines it straight
// into `vm_env`, which requires access to that private field.
