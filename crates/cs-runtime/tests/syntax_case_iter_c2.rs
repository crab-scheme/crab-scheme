//! R6RS++ §12 (#118) Iter C2 — minimal ellipsis (`...`) support.
//!
//! Iter C2 handles the canonical shape: `(prefix... pvar ...)`
//! where `pvar` is a single bare symbol. The pvar binds to the
//! tail of the subject list (after consuming `prefix...`). On
//! the template side, `(prefix... pvar ...)` splices the bound
//! list into the rebuilt structure -- emitted as a `cons` chain
//! terminated by the pvar.
//!
//! Compound sub-patterns under `...`, multiple pvars under
//! ellipsis, and nested ellipsis defer to a follow-up iter; we
//! reject them up front with a clear pointer so users don't get
//! silent misinterpretation.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- pattern: (pvar ...) ----

#[test]
fn ellipsis_alone_binds_full_list() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3) ()
               ((xs ...) (syntax xs)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3)");
}

#[test]
fn ellipsis_alone_matches_empty_list() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '() ()
               ((xs ...) (syntax xs)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn ellipsis_rejects_non_proper_list() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 . 2) ()
               ((xs ...) (syntax xs)))",
        )
        .expect_err("dotted-tail should not match (xs ...)");
    let s = format!("{}", err);
    assert!(
        s.contains("syntax-case") || s.contains("no matching pattern"),
        "got: {}",
        s
    );
}

// ---- pattern: (prefix ... pvar ...) ----

#[test]
fn one_prefix_plus_ellipsis_binds_remainder() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(head 1 2 3) ()
               ((h rest ...) (list (syntax h) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(head (1 2 3))");
}

#[test]
fn two_prefix_plus_ellipsis() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(a b c d e) ()
               ((x y rest ...) (list (syntax x) (syntax y) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(a b (c d e))");
}

#[test]
fn prefix_plus_ellipsis_empty_rest() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(only) ()
               ((x rest ...) (list (syntax x) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(only ())");
}

#[test]
fn prefix_count_mismatch_falls_through() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(a) ()
               ((x y rest ...) 'matched-two-plus)
               ((x rest ...) 'matched-one-plus))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched-one-plus");
}

// ---- template: (prefix... pvar ...) splicing ----

#[test]
fn template_splices_pvar_list_into_prefix() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3) ()
               ((args ...)
                (syntax (call-with args ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(call-with 1 2 3)");
}

#[test]
fn template_splices_into_longer_prefix() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(x y z) ()
               ((args ...)
                (syntax (define foo (lambda args ...)))))",
        )
        .unwrap();
    // Walks to (define foo (lambda x y z))
    assert_eq!(disp(&rt, &v), "(define foo (lambda x y z))");
}

#[test]
fn template_splices_empty_list() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '() ()
               ((args ...)
                (syntax (no-args args ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(no-args)");
}

// ---- composition: define-syntax-style macro emulation ----

#[test]
fn function_definition_pattern_works() {
    // Common pattern: bind a name + an arglist + a body, build
    // a (define (name args...) body) form.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(my-fn (a b c) (+ a b c)) ()
               ((name args body)
                (syntax (define name (lambda args body)))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(define my-fn (lambda (a b c) (+ a b c)))");
}

#[test]
fn ellipsis_inside_with_syntax() {
    // with-syntax should also support `(p ...)` patterns since it
    // desugars to syntax-case.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax (((xs ...) '(1 2 3)))
               (syntax (sum xs ...)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(sum 1 2 3)");
}

// ---- diagnostics ----

#[test]
fn compound_pvar_under_ellipsis_rejected() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4)) ()
               (((a b) ...) (syntax 0)))",
        )
        .expect_err("compound sub-pattern under ... not supported in Iter C2");
    let s = format!("{}", err);
    assert!(s.contains("follow-up"), "got: {}", s);
}
