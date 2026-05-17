//! Iter 7 regression — verify the synchronous cycle detector
//! fires when mutation builtins (`set-car!`, `set-cdr!`,
//! `vector-set!`, `hashtable-set!`) construct cycles.
//!
//! These tests check *detection*, not *reclamation*: the
//! storage-slot Strong/Weak refactor (iter 7.1) lands separately.
//! For now, the detector records a count via
//! `cs_runtime::countable_memory_cycle::cycle_detection_count`.
//! User-visible cycle semantics (cycles stay refcount-leaking
//! until iter 7.1) are unchanged from M5 Phase 1.

#![cfg(feature = "countable-memory")]

use cs_runtime::countable_memory_cycle::{cycle_detection_count, reset_cycle_detection_count};
use cs_runtime::Runtime;

fn run_scheme(rt: &mut Runtime, src: &str) {
    rt.eval_str("<cycle_test>", src).unwrap();
}

#[test]
fn set_cdr_self_loop_triggers_detector() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    run_scheme(
        &mut rt,
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    );
    let after = cycle_detection_count();
    assert!(
        after > before,
        "expected cycle detector to fire on (set-cdr! x x); count before={before} after={after}"
    );
}

#[test]
fn set_car_mutual_cycle_triggers_detector() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    run_scheme(
        &mut rt,
        r"
        (define a (cons 1 2))
        (define b (cons 3 4))
        (set-car! a b)
        (set-car! b a)
    ",
    );
    let after = cycle_detection_count();
    assert!(
        after > before,
        "expected detector to fire on mutual a<->b cycle; count before={before} after={after}"
    );
}

#[test]
fn vector_set_self_loop_triggers_detector() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    run_scheme(
        &mut rt,
        r"
        (define v (make-vector 3 #f))
        (vector-set! v 0 v)
    ",
    );
    let after = cycle_detection_count();
    assert!(
        after > before,
        "expected detector to fire on vector-set! self-loop; count before={before} after={after}"
    );
}

#[test]
fn set_cdr_acyclic_does_not_trigger_detector() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    run_scheme(
        &mut rt,
        r"
        (define a (cons 1 2))
        (define b (cons 3 4))
        (set-cdr! a b)
    ",
    );
    let after = cycle_detection_count();
    assert_eq!(
        before, after,
        "detector fired spuriously on acyclic set-cdr!: count before={before} after={after}"
    );
}

#[test]
fn vector_set_acyclic_does_not_trigger_detector() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    run_scheme(
        &mut rt,
        r"
        (define v (make-vector 3 #f))
        (vector-set! v 0 42)
        (vector-set! v 1 (cons 1 2))
        (vector-set! v 2 'sym)
    ",
    );
    let after = cycle_detection_count();
    assert_eq!(
        before, after,
        "detector fired spuriously on acyclic vector-set!: count before={before} after={after}"
    );
}
