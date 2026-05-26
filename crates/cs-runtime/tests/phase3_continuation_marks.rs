//! R6RS++ Phase 3A — continuation marks.
//!
//! `with-continuation-mark` extends the dynamic mark chain for
//! the duration of body evaluation. `current-continuation-marks`
//! reads back the chain (whole, or filtered by key).
//!
//! Naive impl: mark chain held in a parameter; not tail-safe.
//! See lib/cmarks/cmarks.scm for caveats. Tail-safe semantics is
//! a future iteration.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_cmarks() -> Runtime {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/cmarks/cmarks.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<cmarks>", &src).expect("load cmarks.scm");
    rt
}

// ---- basic with-continuation-mark / current-continuation-marks ----

#[test]
fn empty_chain_outside_any_mark() {
    let mut rt = load_cmarks();
    let v = rt.eval_str("<t>", "(current-continuation-marks)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn single_mark_visible_in_body() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'k 1
               (current-continuation-marks 'k))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1)");
}

#[test]
fn mark_chain_unwinds_after_body() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'k 1
               (current-continuation-marks 'k))
             (current-continuation-marks 'k)",
        )
        .unwrap();
    // Outer read sees empty chain again — parameterize unwinds.
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn nested_same_key_marks_in_tail_position_replace() {
    // Tail-safe semantics (issue #36): each inner `with-continuation-mark`
    // is in tail position of the enclosing one, so all three target the
    // SAME continuation frame and the same key — the innermost replaces
    // the others (Racket / R7RS tail-mark semantics). The naive
    // parameter-based impl accumulated `(3 2 1)`; the correct answer is
    // just the innermost value.
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'k 1
               (with-continuation-mark 'k 2
                 (with-continuation-mark 'k 3
                   (current-continuation-marks 'k))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3)");
}

#[test]
fn nested_distinct_key_marks_collect_innermost_first() {
    // Distinct keys don't replace each other — the full alist collects
    // all of them, innermost (most recently installed) first.
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'a 1
               (with-continuation-mark 'b 2
                 (with-continuation-mark 'c 3
                   (current-continuation-marks))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((c . 3) (b . 2) (a . 1))");
}

#[test]
fn different_keys_independent() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'a 1
               (with-continuation-mark 'b 2
                 (list (current-continuation-marks 'a)
                       (current-continuation-marks 'b)
                       (current-continuation-marks 'missing))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1) (2) ())");
}

#[test]
fn unfiltered_returns_full_alist() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(with-continuation-mark 'a 1
               (with-continuation-mark 'b 2
                 (current-continuation-marks)))",
        )
        .unwrap();
    // Innermost-first alist.
    assert_eq!(disp(&rt, &v), "((b . 2) (a . 1))");
}

// ---- propagation through calls ----

#[test]
fn marks_propagate_into_called_procedure() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(define (peek) (current-continuation-marks 'request-id))
             (with-continuation-mark 'request-id \"abc-123\"
               (peek))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(abc-123)");
}

#[test]
fn marks_propagate_through_nested_calls() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            "(define (inner) (current-continuation-marks 'depth))
             (define (outer) (inner))
             (with-continuation-mark 'depth 'top
               (outer))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(top)");
}

// ---- body returns its value normally ----

#[test]
fn with_continuation_mark_returns_body_value() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str("<t>", "(with-continuation-mark 'k 1 (+ 1 2 3))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn with_continuation_mark_multibody_evaluates_in_order() {
    let mut rt = load_cmarks();
    // parameterize's body is an expression context, so
    // with-continuation-mark inherits the same restriction.
    // Sequencing multiple body expressions returns the last.
    let v = rt
        .eval_str(
            "<t>",
            "(define counter 0)
             (with-continuation-mark 'k 1
               (set! counter (+ counter 1))
               (set! counter (+ counter 10))
               counter)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "11");
}

// ---- profile-like use ----

#[test]
fn marks_usable_as_dynamic_context() {
    let mut rt = load_cmarks();
    let v = rt
        .eval_str(
            "<t>",
            // Simulate: a worker pulls the current request ID
            // out of the mark chain rather than threading it as
            // an explicit argument.
            "(define (current-request-id)
               (let ((vs (current-continuation-marks 'request-id)))
                 (if (null? vs) 'none (car vs))))
             (list
               (current-request-id)
               (with-continuation-mark 'request-id 'A (current-request-id))
               (with-continuation-mark 'request-id 'B
                 (with-continuation-mark 'request-id 'C (current-request-id))))",
        )
        .unwrap();
    // Outside-mark: 'none. Inside A: 'A. Inside B then C: innermost 'C.
    assert_eq!(disp(&rt, &v), "(none A C)");
}

// ---- error cases ----

#[test]
fn current_continuation_marks_too_many_args() {
    let mut rt = load_cmarks();
    let err = rt
        .eval_str("<t>", "(current-continuation-marks 'a 'b)")
        .expect_err("> 1 arg should error");
    let s = format!("{}", err);
    assert!(s.contains("expected 0 or 1"), "got: {}", s);
}
