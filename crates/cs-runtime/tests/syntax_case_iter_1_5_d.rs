//! R6RS++ Phase 1.5 Iter D — mark-aware bound-identifier=? and
//! free-identifier=?.
//!
//! After Iter 1.5.C wired the syntax-case template instantiator
//! to create marked Identifier values, this iter upgrades the
//! two R6RS hygiene predicates to honor the marks:
//!
//! * `bound-identifier=?`: (name, mark) comparison. Symbol
//!   treated as mark=0. Distinguishes identifiers introduced
//!   at different macro-form invocations.
//! * `free-identifier=?`: name-only comparison (mark ignored).
//!   Two identifiers refer to the same binding if they have
//!   the same name, regardless of marks.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- bound-identifier=? ----

#[test]
fn bound_id_eq_symbol_to_symbol_same_name() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'foo)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_id_eq_symbol_to_symbol_diff_name() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'bar)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn bound_id_eq_symbol_to_mark_zero_identifier() {
    // Symbol implicitly has mark=0; identifier explicitly mark=0
    // -> equal if names match.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo (make-identifier 'foo 0))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_id_eq_symbol_to_marked_identifier_unequal() {
    // Symbol = mark 0; identifier has mark != 0 -> not equal.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo (make-identifier 'foo 5))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn bound_id_eq_same_mark_diff_name() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(bound-identifier=? (make-identifier 'foo 7)
                                  (make-identifier 'bar 7))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn bound_id_eq_same_name_diff_mark() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(bound-identifier=? (make-identifier 'foo 7)
                                  (make-identifier 'foo 8))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn bound_id_eq_same_name_same_mark() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(bound-identifier=? (make-identifier 'foo 7)
                                  (make-identifier 'foo 7))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- bound-identifier=? via syntax-case (the hygiene point) ----

#[test]
fn bound_id_eq_distinguishes_syntax_case_form_evaluations() {
    // Two separate (syntax-case ...) forms each produce a
    // distinct mark; identifiers from each must compare unequal
    // under bound-identifier=?. This is the test that lived as
    // #[ignore]d in syntax_case_iter_a.rs through Iter A-E of
    // the original #118 sequence -- now activated.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((a (syntax-case 'whatever () (_ (syntax foo))))
                   (b (syntax-case 'whatever () (_ (syntax foo)))))
               (bound-identifier=? a b))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn bound_id_eq_same_syntax_form_two_references() {
    // Two (syntax foo) inside one form share the mark -> equal.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (bound-identifier=? (syntax foo) (syntax foo))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_id_eq_via_function_call_distinguishes_invocations() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define (intro)
               (syntax-case 'whatever () (_ (syntax foo))))
             (bound-identifier=? (intro) (intro))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- free-identifier=? semantics ----

#[test]
fn free_id_eq_ignores_marks_when_names_match() {
    // Two identifiers with the same name but different marks
    // (different macro expansion sites) refer to the "same
    // binding" in the surrounding scope, so free-identifier=?
    // says equal even though bound-identifier=? says not.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(free-identifier=? (make-identifier 'foo 7)
                                 (make-identifier 'foo 8))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn free_id_eq_diff_names_unequal_regardless_of_mark() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(free-identifier=? (make-identifier 'foo 0)
                                 (make-identifier 'bar 0))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn free_id_eq_syntax_case_two_form_evaluations_same_name() {
    // The Racket-style ergonomic: macros that introduce a
    // reference to a known top-level name compare equal under
    // free-identifier=? even though they came from different
    // expansions. This is the predicate Racket's match uses
    // for keyword recognition.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((a (syntax-case 'whatever () (_ (syntax foo))))
                   (b (syntax-case 'whatever () (_ (syntax foo)))))
               (free-identifier=? a b))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn free_id_eq_symbol_to_identifier_with_same_name() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(free-identifier=? 'foo (make-identifier 'foo 42))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- type errors ----

#[test]
fn bound_id_eq_rejects_non_identifier() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(bound-identifier=? 1 'foo)")
        .expect_err("non-identifier first arg should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("bound-identifier=?") || s.contains("identifier"),
        "got: {}",
        s
    );
}

#[test]
fn free_id_eq_rejects_non_identifier() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(free-identifier=? 'foo \"str\")")
        .expect_err("non-identifier second arg should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("free-identifier=?") || s.contains("identifier"),
        "got: {}",
        s
    );
}
