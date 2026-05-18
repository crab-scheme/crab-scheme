//! R6RS++ Phase 2E — parameters audit + parameter? predicate.
//!
//! The spec's §7 calls for `make-parameter` + `parameterize`
//! over `dynamic-wind`. The audit confirms what works today
//! and pins behavior in tests:
//!
//! Working:
//! * `(make-parameter init)` — creates a parameter procedure
//! * `(parameter? p)` — Phase 2E addition; recognizes parameters
//! * `(p)` reads the current value
//! * `(parameterize ((p v) ...) body ...)` desugars to a
//!   dynamic-wind that swaps the value during body, restores
//!   on exit (including non-local exit via raise/escape).
//! * `(make-parameter init converter)` — accepts the converter
//!   arg but DOESN'T apply it (documented gap; see note below).
//!
//! Known gap (deferred):
//! * `(make-parameter init converter)`: the converter procedure
//!   is meant to filter values at write time. Applying a Scheme
//!   procedure inside cs-core's Parameter::call would require
//!   threading the eval context through the Procedure trait or
//!   moving Parameter into cs-runtime so it can use
//!   apply_procedure. Either way, a tier-crossing change.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- parameter? predicate ----

#[test]
fn parameter_p_recognizes_make_parameter_result() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(parameter? (make-parameter 42))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn parameter_p_false_for_plain_procedure() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(parameter? (lambda (x) x))").unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn parameter_p_false_for_non_procedure() {
    let mut rt = Runtime::new();
    for src in &[
        "(parameter? 42)",
        "(parameter? 'foo)",
        "(parameter? \"str\")",
        "(parameter? '())",
        "(parameter? '(1 2 3))",
        "(parameter? #t)",
    ] {
        let v = rt.eval_str("<t>", src).unwrap();
        assert_eq!(disp(&rt, &v), "#f", "for: {}", src);
    }
}

#[test]
fn parameter_p_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(parameter?)").is_err());
    assert!(rt
        .eval_str("<t>", "(parameter? (make-parameter 0) (make-parameter 0))")
        .is_err());
}

// ---- make-parameter / read / write ----

#[test]
fn make_parameter_initial_value_readable() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'initial)))
               (p))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "initial");
}

#[test]
fn make_parameter_arity_rejects_zero_or_three() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(make-parameter)").is_err());
    assert!(rt
        .eval_str("<t>", "(make-parameter 1 (lambda (x) x) 'extra)",)
        .is_err());
}

#[test]
fn make_parameter_converter_arg_type_checked() {
    // 2-arg form must be a procedure (even if we ignore it today).
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(make-parameter 0 'not-a-proc)")
        .expect_err("non-procedure converter should error");
    let s = format!("{}", err);
    assert!(
        s.contains("procedure") || s.contains("make-parameter"),
        "got: {}",
        s
    );
}

// ---- parameterize ----

#[test]
fn parameterize_overrides_during_body() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'outer)))
               (parameterize ((p 'inner))
                 (p)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "inner");
}

#[test]
fn parameterize_restores_on_normal_exit() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'outer)))
               (parameterize ((p 'inner))
                 (p))
               (p))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "outer");
}

#[test]
fn parameterize_restores_on_exception() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'outer)))
               (guard (c (#t (p)))
                 (parameterize ((p 'inner))
                   (error 'test \"boom\"))))",
        )
        .unwrap();
    // The handler runs AFTER parameterize's dynamic-wind
    // restore -- value is back to 'outer.
    assert_eq!(disp(&rt, &v), "outer");
}

#[test]
fn parameterize_multiple_bindings_independent() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p1 (make-parameter 'p1-out))
                   (p2 (make-parameter 'p2-out)))
               (parameterize ((p1 'p1-in) (p2 'p2-in))
                 (list (p1) (p2))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(p1-in p2-in)");
}

#[test]
fn parameterize_nested_inside_out() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'a)))
               (parameterize ((p 'b))
                 (parameterize ((p 'c))
                   (p))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "c");
}

#[test]
fn parameterize_nested_restores_outer_layer() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 'a)))
               (parameterize ((p 'b))
                 (parameterize ((p 'c))
                   (p))
                 (p)))",
        )
        .unwrap();
    // Inner restored 'b after nested parameterize exits.
    assert_eq!(disp(&rt, &v), "b");
}

#[test]
fn parameterize_empty_bindings_just_runs_body() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(parameterize () 'body-result)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "body-result");
}

// ---- parameter as procedure (passes procedure? test) ----

#[test]
fn parameter_is_a_procedure() {
    // R6RS parameters are procedures; both predicates apply.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((p (make-parameter 0)))
               (and (procedure? p) (parameter? p)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}
