//! R6RS++ Phase 2D — &contract / &type / &module condition
//! subtypes extending the R6RS &error root.
//!
//! Each new type ships with: constructor (make-…), predicate
//! (…?), and per-field accessors. Subtype relationships flow
//! through cond_has_subtype, so a guard for &error catches a
//! &contract / &type / &module condition the same way it
//! catches a &i/o-error.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- &contract ----

#[test]
fn contract_violation_basic_roundtrip() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((c (make-contract-violation '(math vec) '(graphics ray)
                                                '(-> vec? real?) 42)))
               (list (contract-violation? c)
                     (contract-violation-source c)
                     (contract-violation-target c)
                     (contract-violation-contract c)
                     (contract-violation-value c)))",
        )
        .unwrap();
    assert_eq!(
        disp(&rt, &v),
        "(#t (math vec) (graphics ray) (-> vec? real?) 42)"
    );
}

#[test]
fn contract_violation_is_error_subtype() {
    // &contract extends &error so existing R6RS guard-based
    // error handlers catch it.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((c (make-contract-violation '() '() 'any 0)))
               (error? c))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn contract_violation_via_raise_caught_as_error() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((contract-violation? c)
                        (contract-violation-value c)))
               (raise (make-contract-violation 'src 'tgt 'ctr 'bad-value)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "bad-value");
}

#[test]
fn contract_violation_predicate_false_for_non_contract() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(contract-violation? (make-violation))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn contract_violation_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt
        .eval_str("<t>", "(make-contract-violation 'a 'b 'c)")
        .is_err());
    assert!(rt.eval_str("<t>", "(contract-violation?)").is_err());
}

// ---- &type ----

#[test]
fn type_error_basic_roundtrip() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((c (make-type-error \"number\" 'not-a-number)))
               (list (type-error? c)
                     (type-error-expected c)
                     (type-error-actual c)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t number not-a-number)");
}

#[test]
fn type_error_is_error_subtype() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(error? (make-type-error 'expected 'actual))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn type_error_via_guard() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((type-error? c)
                        (list (type-error-expected c)
                              (type-error-actual c))))
               (raise (make-type-error 'integer \"hello\")))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(integer hello)");
}

// ---- &module ----

#[test]
fn module_error_basic_roundtrip() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((c (make-module-error '(http server))))
               (list (module-error? c)
                     (module-error-library c)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t (http server))");
}

#[test]
fn module_error_is_error_subtype() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(error? (make-module-error '(pkg foo)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- distinctness ----

#[test]
fn each_new_type_distinct_from_others() {
    let mut rt = Runtime::new();
    // Run all 9 predicate calls in one eval to dodge the
    // borrow checker (eval_str needs &mut; disp needs &).
    let v = rt
        .eval_str(
            "<t>",
            "(let ((c (make-contract-violation 's 't 'ctr 'v))
                   (te (make-type-error 'a 'b))
                   (m (make-module-error '(x))))
               (list (contract-violation? c)
                     (contract-violation? te)
                     (contract-violation? m)
                     (type-error? c)
                     (type-error? te)
                     (type-error? m)
                     (module-error? c)
                     (module-error? te)
                     (module-error? m)))",
        )
        .unwrap();
    // Expected: contract-v? matches only c, type-error? only te,
    // module-error? only m. So pattern: #t #f #f / #f #t #f / #f #f #t.
    assert_eq!(disp(&rt, &v), "(#t #f #f #f #t #f #f #f #t)");
}

#[test]
fn condition_predicate_recognizes_all_three() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(list (condition? (make-contract-violation 'a 'b 'c 'd))
                   (condition? (make-type-error 'x 'y))
                   (condition? (make-module-error '(foo))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t)");
}

// ---- raise propagation through higher-order builtins (issue #34) ----
//
// Higher-order builtins (map, for-each, filter, fold, ...) previously
// swallowed `raise` conditions raised inside their callbacks by
// converting `EvalError` to a plain `String` via `e.message()`. The
// original condition was lost, so an enclosing `guard` or
// `with-exception-handler` saw a generic reconstructed error rather
// than the user's condition value. The fix routes `EvalErrorKind::Raised`
// and `EvalErrorKind::Escape` through the `pending_raise` /
// `pending_escape` side-channels via the `propagate_eval_err` helper.

#[test]
fn raise_inside_map_propagates_through_guard() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c (#t 'caught))
               (map (lambda (x) (raise 'oops)) '(1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

#[test]
fn raise_inside_map_preserves_condition_value() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c (#t c))
               (map (lambda (x) (raise 42)) '(1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn raise_inside_for_each_propagates_through_guard() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c (#t 'caught))
               (for-each (lambda (x) (raise 'oops)) '(1 2 3)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

#[test]
fn raise_inside_for_each_preserves_condition_value() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c (#t c))
               (for-each (lambda (x) (raise x)) '(99)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "99");
}

#[test]
fn with_exception_handler_catches_raise_inside_map() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"(call-with-current-continuation
                 (lambda (k)
                   (with-exception-handler
                     (lambda (c) (k (list 'caught c)))
                     (lambda ()
                       (map (lambda (x) (raise 'boom)) '(1))))))"#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(caught boom)");
}
