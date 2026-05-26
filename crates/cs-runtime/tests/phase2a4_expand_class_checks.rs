//! R6RS++ #32 follow-up — built-in syntax classes (`id`/`number`/
//! `string`) are checked at EXPAND time, not by a runtime `if`-wrap.
//!
//! The payoff is that the rule body is emitted UNWRAPPED, so a
//! `define-syntax-parser` macro whose body is a definition can finally
//! carry an `:id` on its name. Under the old design the body became
//! `(if (identifier? 'name) (define name val) (error ...))`, which put
//! the `(define ...)` in expression position and failed to expand.
//!
//! Built-in classes are *syntactic*: `:number` means "a number
//! literal", not "evaluates to a number". A runtime value guard is
//! what user-defined `define-syntax-class` predicates are for (those
//! stay runtime checks); see `phase2_syntax_class.rs`.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- the headline win: definition-bodied macro with :id ----

#[test]
fn id_class_macro_with_define_body_expands_and_runs() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser def-const
          ((_ name:id val) (define name val)))
        (def-const answer 42)
        "#,
    )
    .expect("define-bodied :id macro should expand (was: define in expression position)");
    let v = rt.eval_str("<t>", "answer").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn id_class_macro_with_multi_define_body() {
    // A multi-form body becomes a (begin ...) of definitions; with the
    // body unwrapped that splices at top level as expected.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser def-both
          ((_ a:id b:id v) (define a v) (define b v)))
        (def-both x y 7)
        "#,
    )
    .expect("multi-define body should expand");
    let v = rt.eval_str("<t>", "(+ x y)").unwrap();
    assert_eq!(disp(&rt, &v), "14");
}

// ---- the check fires at EXPAND time, not run time ----

#[test]
fn id_violation_is_an_expand_time_error() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        "(define-syntax-parser def-const ((_ name:id val) (define name val)))",
    )
    .unwrap();
    // The bad use sits in a lambda body that is NEVER called. A runtime
    // check could only fire on invocation; this errors merely by being
    // defined -- proof the class check runs during expansion.
    let err = rt
        .eval_str("<t>", "(define (never-called) (def-const 5 99))")
        .expect_err("non-identifier name must be rejected at expand time");
    let s = format!("{}", err);
    assert!(
        s.contains("expected id"),
        "class check should fire before the define-shape error; got: {s}"
    );
}

// ---- :number / :string are now syntactic (literal) checks ----

#[test]
fn number_class_is_a_literal_check_not_a_value_check() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        "(define-syntax-parser num-double ((_ n:number) (* 2 n)))",
    )
    .unwrap();
    // A number literal passes (and the body still computes).
    let v = rt.eval_str("<t>", "(num-double 21)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
    // A compound expression that would *evaluate* to a number is NOT a
    // number literal -- rejected at expand time. (This is the documented
    // semantic shift from the old runtime value-check.)
    let err = rt
        .eval_str("<t>", "(num-double (+ 1 2))")
        .expect_err(":number is a literal class, so (+ 1 2) is not a number");
    assert!(format!("{err}").contains("expected number"), "got: {err}");
}

#[test]
fn string_class_rejects_non_string_literal() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define-syntax-parser as-string ((_ s:string) s))")
        .unwrap();
    let v = rt.eval_str("<t>", r#"(as-string "hi")"#).unwrap();
    assert_eq!(disp(&rt, &v), "hi");
    let err = rt
        .eval_str("<t>", "(as-string foo)")
        .expect_err("a symbol is not a string literal");
    assert!(format!("{err}").contains("expected string"), "got: {err}");
}

// ---- multi-clause dispatch is unaffected; check pins the matched rule ----

#[test]
fn class_check_runs_only_on_the_matched_clause() {
    // First clause matches a 2-element call and demands an id; the
    // second matches a 1-element call with no constraint. The class
    // check applies to whichever clause structurally matched.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser tag
          ((_ name:id v) (list (quote name) v))
          ((_ v)         (list 'bare v)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(tag widget 5)").unwrap();
    assert_eq!(disp(&rt, &v), "(widget 5)");
    // 1-arg call routes to the second (unconstrained) clause.
    let v = rt.eval_str("<t>", "(tag 5)").unwrap();
    assert_eq!(disp(&rt, &v), "(bare 5)");
    // 2-arg call with a non-id first arg matches clause 1 structurally,
    // then fails its :id check (no silent fall-through to clause 2).
    let err = rt
        .eval_str("<t>", "(tag 9 5)")
        .expect_err(":id on the matched clause must fire");
    assert!(format!("{err}").contains("expected id"), "got: {err}");
}

// ---- regression: a classless parser macro still desugars normally ----

#[test]
fn classless_parser_macro_unchanged() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser my-swap
          ((_ a b) (list b a)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(my-swap 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "(2 1)");
}
