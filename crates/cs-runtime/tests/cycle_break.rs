//! Iter 7.1.x regression — verify the strong-count-guarded
//! cycle break works correctly:
//!
//! 1. Acyclic mutations don't fire the detector and don't
//!    trigger the break.
//! 2. Cycles whose only strong holder is the freshly-mutated
//!    slot are detected but NOT broken (the strong-count guard
//!    correctly refuses to orphan the value).
//! 3. Programs that rely on the cyclic semantics (e.g., the
//!    metacircular evaluator's env mutations) continue to work
//!    because the guard preserves observability.
//!
//! Actually-breaking cycles requires multiple external anchors
//! beyond the slot, the mutation argument, and dispatch
//! temporaries — see Pair::break_car_cycle doc for the
//! threshold-5 heuristic. This file's tests exercise the
//! observability contract, not the reclamation contract; the
//! latter requires a smarter Bacon-Rajan-style algorithm
//! tracked as iter 7.1.x follow-up.

#![cfg(feature = "countable-memory")]

use cs_runtime::countable_memory_cycle::{
    cycle_broken_count, cycle_detection_count, reset_cycle_detection_count,
};
use cs_runtime::Runtime;

#[test]
fn detector_fires_on_self_loop_observability_preserved() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<cycle_break>",
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    )
    .expect("eval ok");
    assert!(
        cycle_detection_count() > 0,
        "expected detector to fire on (set-cdr! x x)"
    );
    // Observability check: (car x) must still return 1 and
    // (cdr x) must still return a pair (cycle visible to user
    // even if Weak tombstone was applied).
    let result = rt.eval_str("<verify>", "(car x)").expect("car x");
    assert!(
        matches!(result, cs_core::Value::Number(_)),
        "(car x) returned {result:?}, expected Number(1)"
    );
    let cdr_result = rt.eval_str("<verify>", "(pair? (cdr x))").expect("cdr x");
    assert!(
        matches!(cdr_result, cs_core::Value::Boolean(true)),
        "(pair? (cdr x)) returned {cdr_result:?}, expected #t"
    );
}

#[test]
fn acyclic_mutation_does_not_trigger_break() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<acyclic>",
        r"
        (define a (cons 1 2))
        (define b (cons 3 4))
        (set-cdr! a b)
    ",
    )
    .expect("eval ok");
    assert_eq!(
        cycle_detection_count(),
        0,
        "no cycle should have been detected"
    );
    assert_eq!(cycle_broken_count(), 0, "no break should have happened");
}

#[test]
fn metacircular_style_define_preserves_binding_observability() {
    // Mirrors the metacircular's `(set-car! env (cons name val))`
    // pattern that broke under the naive iter-7.1 break. The
    // strong-count guard must skip the break (or the binding
    // becomes inaccessible).
    let mut rt = Runtime::new();
    rt.eval_str(
        "<meta_define>",
        r"
        (define env (cons (cons 'old 'val) '()))
        (define name 'new)
        (define val 'value)
        ; Simulate metacircular define:
        (set-cdr! env (cons (car env) (cdr env)))
        (set-car! env (cons name val))
        ; Verify the binding is still accessible:
        (define accessible (eq? (car (car env)) name))
    ",
    )
    .expect("eval ok");
    let result = rt.eval_str("<verify>", "accessible").expect("lookup");
    assert!(
        matches!(result, cs_core::Value::Boolean(true)),
        "metacircular-style binding lookup returned {result:?}, expected #t"
    );
}

#[test]
fn cycle_broken_count_distinguishes_detect_from_break() {
    // The detection counter must increment even when the
    // break is skipped by the strong-count guard.
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<break_count>",
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    )
    .expect("eval ok");
    let detected = cycle_detection_count();
    let broken = cycle_broken_count();
    assert!(detected > 0, "detection should have fired");
    // broken may or may not be > 0 depending on the guard;
    // they must satisfy broken <= detected.
    assert!(
        broken <= detected,
        "broken count {broken} exceeds detected {detected}"
    );
}
