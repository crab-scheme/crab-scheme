//! R6RS++ Phase 2A.2 — user-defined syntax classes via
//! `define-syntax-class`.
//!
//! `(define-syntax-class name predicate)` binds `name` as a
//! syntax class whose predicate is the named procedure. Later
//! `define-syntax-parser` clauses can use `pvar:name` and the
//! generated class-check calls the predicate.
//!
//! Predicate-only form here; Racket's compound
//! `(pattern ... #:when ...)` form lands in a later iter.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- basic registration + use ----

#[test]
fn user_class_with_builtin_predicate() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-class even-number even?)
        (define-syntax-parser only-even
          ((_ n:even-number) (* 2 n)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(only-even 4)").unwrap();
    assert_eq!(disp(&rt, &v), "8");
}

#[test]
fn user_class_rejects_mismatching_arg() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-class even-number even?)
        (define-syntax-parser only-even
          ((_ n:even-number) (* 2 n)))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(only-even 3)")
        .expect_err("odd number violates predicate");
    let s = format!("{}", err);
    assert!(s.contains("expected even-number"), "got: {}", s);
}

#[test]
fn user_class_with_user_defined_predicate() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define (positive? x) (and (number? x) (> x 0)))
        (define-syntax-class positive positive?)
        (define-syntax-parser must-be-positive
          ((_ x:positive) x))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(must-be-positive 5)").unwrap();
    assert_eq!(disp(&rt, &v), "5");
    let err = rt
        .eval_str("<t>", "(must-be-positive -1)")
        .expect_err("negative violates positive class");
    let s = format!("{}", err);
    assert!(s.contains("expected positive"), "got: {}", s);
}

#[test]
fn multiple_user_classes_in_one_parser() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-class even-num even?)
        (define-syntax-class odd-num odd?)
        (define-syntax-parser pair-of
          ((_ e:even-num o:odd-num) (list e o)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(pair-of 4 7)").unwrap();
    assert_eq!(disp(&rt, &v), "(4 7)");
}

// ---- registry semantics ----

#[test]
fn redefining_class_overrides_predicate() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-class my-class even?)
        (define-syntax-class my-class odd?)
        (define-syntax-parser test
          ((_ n:my-class) 'matched))
        "#,
    )
    .unwrap();
    // After redefinition, my-class uses odd? not even?.
    let v = rt.eval_str("<t>", "(test 3)").unwrap();
    assert_eq!(disp(&rt, &v), "matched");
    let err = rt
        .eval_str("<t>", "(test 4)")
        .expect_err("4 is even, not odd");
    let s = format!("{}", err);
    assert!(s.contains("expected my-class"), "got: {}", s);
}

// ---- error cases ----

#[test]
fn define_syntax_class_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(define-syntax-class foo)").is_err());
    assert!(rt
        .eval_str("<t>", "(define-syntax-class foo even? odd?)")
        .is_err());
}

#[test]
fn define_syntax_class_rejects_non_symbol_name() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(define-syntax-class 42 even?)")
        .expect_err("name must be symbol");
    let s = format!("{}", err);
    assert!(s.contains("name must be a symbol"), "got: {}", s);
}

#[test]
fn define_syntax_class_rejects_non_symbol_predicate() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(define-syntax-class foo (lambda (x) x))")
        .expect_err("predicate must be a bare symbol");
    let s = format!("{}", err);
    assert!(s.contains("predicate must be"), "got: {}", s);
}

// ---- composition with built-in classes ----

#[test]
fn user_class_alongside_builtin() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-class even-num even?)
        (define-syntax-parser kvp
          ((_ k:id v:even-num) (list (quote k) v)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(kvp answer 42)").unwrap();
    assert_eq!(disp(&rt, &v), "(answer 42)");
    let err = rt
        .eval_str("<t>", "(kvp answer 3)")
        .expect_err("3 not even");
    let s = format!("{}", err);
    assert!(s.contains("expected even-num"), "got: {}", s);
}
