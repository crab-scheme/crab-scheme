//! R6RS++ §12 (#118) Iter D — fender expressions in syntax-case.
//!
//! 3-element clauses `(pattern fender body)` evaluate `fender`
//! (a regular Scheme expression) after the pattern matches; if
//! the fender returns `#f` the clause is treated as non-matching
//! and the next clause is tried. Fender expressions have access
//! to the bound pattern variables.
//!
//! Implementation note: since our syntax-case is a runtime form,
//! the fender runs at runtime — there's no expand-time evaluator
//! requirement. The next-clause expression is shared between the
//! test-failure branch and the fender-failure branch via a
//! `Letrec`-bound 0-arity thunk so the CoreExpr tree doesn't
//! duplicate exponentially for nested fender clauses.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- basic fender behavior ----

#[test]
fn fender_true_uses_clause_body() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 42 ()
               (x #t (syntax x)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn fender_false_falls_through_to_next_clause() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 42 ()
               (x #f 'first)
               (y 'second))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "second");
}

#[test]
fn fender_references_bound_pvar() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 10 ()
               (x (> x 5) 'big)
               (x 'small))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "big");
}

#[test]
fn fender_references_pvar_falls_through_when_predicate_fails() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 3 ()
               (x (> x 5) 'big)
               (x 'small))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "small");
}

// ---- destructured + fender ----

#[test]
fn fender_uses_destructured_pvars() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2) ()
               ((a b) (= a b) 'equal)
               ((a b) (< a b) 'ascending)
               (_ 'other))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "ascending");
}

#[test]
fn fender_can_use_arbitrary_scheme() {
    // The fender is arbitrary Scheme; it can call user functions,
    // do arithmetic, etc.
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define (positive-pair? a b) (and (> a 0) (> b 0)))")
        .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(3 4) ()
               ((a b) (positive-pair? a b) 'both-positive)
               (_ 'no))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "both-positive");
}

// ---- fender + ellipsis ----

#[test]
fn fender_with_ellipsis_pvar() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3) ()
               ((xs ...) (> (length (syntax xs)) 2) 'three-plus)
               ((xs ...) 'few))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "three-plus");
}

#[test]
fn fender_with_ellipsis_falls_through_when_short() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2) ()
               ((xs ...) (> (length (syntax xs)) 2) 'three-plus)
               ((xs ...) 'few))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "few");
}

// ---- multiple fender clauses cascade ----

#[test]
fn cascading_fenders() {
    let mut rt = Runtime::new();
    for (subject, expected) in &[
        ("0", "zero"),
        ("5", "small"),
        ("50", "medium"),
        ("500", "large"),
    ] {
        let src = format!(
            "(syntax-case {} ()
               (x (zero? x) 'zero)
               (x (< x 10) 'small)
               (x (< x 100) 'medium)
               (x 'large))",
            subject
        );
        let v = rt.eval_str("<t>", &src).unwrap();
        assert_eq!(disp(&rt, &v), *expected, "for subject {}", subject);
    }
}

// ---- fender + template uses (syntax X) ----

#[test]
fn fender_template_uses_syntax_form() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(double 5) ()
               ((op n) (eq? (syntax op) 'double)
                       (* 2 n))
               (_ 'unknown))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "10");
}

// ---- mixed fender + non-fender clauses ----

#[test]
fn mixed_fender_and_nonfender_clauses() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 7 ()
               (x (even? x) 'even-fender)
               (x 'odd-default))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "odd-default");
}

// ---- diagnostics ----

#[test]
fn four_element_clause_rejected() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(syntax-case 42 () (x #t y (syntax x)))")
        .expect_err("4-element clause is not valid");
    let s = format!("{}", err);
    assert!(
        s.contains("(pattern template)") || s.contains("fender"),
        "got: {}",
        s
    );
}
