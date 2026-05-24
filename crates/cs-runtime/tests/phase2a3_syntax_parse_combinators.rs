//! R6RS++ Phase 2A.3 — `syntax-parse` combinators (`~or`,
//! `~optional`, `~once`) for `define-syntax-parser`.
//!
//! These need a backtracking pattern matcher that the plain
//! `syntax-rules` desugar can't express, so combinator-using
//! parsers route through a dedicated matcher (cs-expand's
//! `syntax_parse` module). Macros that use none of the combinators
//! keep the original syntax-rules desugar path unchanged.
//!
//! Issue #31. Semantics follow Racket's `syntax/parse`, adapted to
//! the syntax-rules-style template model (templates substitute
//! pattern variables directly rather than via `(attribute x)`).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// =====================================================================
// ~or — ordered alternation (a single head pattern that matches if any
// alternative matches; first match wins; backtracks on failure)
// =====================================================================

#[test]
fn or_picks_first_matching_alternative() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser first-of
          ((_ (~or (a) (a b))) a))
        "#,
    )
    .unwrap();
    // 1-element list -> first alt (a)
    let v = rt.eval_str("<t>", "(first-of (10))").unwrap();
    assert_eq!(disp(&rt, &v), "10");
    // 2-element list -> first alt fails (arity), second alt (a b) matches
    let v = rt.eval_str("<t>", "(first-of (10 20))").unwrap();
    assert_eq!(disp(&rt, &v), "10");
}

#[test]
fn or_alternation_with_distinct_shapes() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser unwrap
          ((_ (~or (wrapped v) v)) v))
        "#,
    )
    .unwrap();
    // bare value -> second alt binds v directly
    let v = rt.eval_str("<t>", "(unwrap 7)").unwrap();
    assert_eq!(disp(&rt, &v), "7");
    // 2-list -> first alt binds v to the second element
    let v = rt.eval_str("<t>", "(unwrap (wrapped 99))").unwrap();
    assert_eq!(disp(&rt, &v), "99");
}

#[test]
fn or_no_alternative_matches_is_error() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser only-lists
          ((_ (~or (a) (a b))) a))
        "#,
    )
    .unwrap();
    // bare atom matches neither (a) nor (a b)
    let err = rt
        .eval_str("<t>", "(only-lists 5)")
        .expect_err("no alternative should match a bare atom");
    let s = format!("{}", err);
    assert!(
        s.contains("no matching rule") || s.contains("only-lists"),
        "got: {}",
        s
    );
}

#[test]
fn or_among_other_pattern_elements() {
    // ~or as one element in a longer pattern; surrounding fixed
    // elements still bind normally.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser tag
          ((_ name (~or (val v) v)) (list (quote name) v)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(tag x 1)").unwrap();
    assert_eq!(disp(&rt, &v), "(x 1)");
    let v = rt.eval_str("<t>", "(tag y (val 2))").unwrap();
    assert_eq!(disp(&rt, &v), "(y 2)");
}

// =====================================================================
// ~optional — present or absent; #:defaults supplies the absent value
// =====================================================================

#[test]
fn optional_present_and_absent_with_defaults() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser opt
          ((_ a (~optional b #:defaults ((b 99)))) (+ a b)))
        "#,
    )
    .unwrap();
    // present
    let v = rt.eval_str("<t>", "(opt 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "3");
    // absent -> default 99
    let v = rt.eval_str("<t>", "(opt 5)").unwrap();
    assert_eq!(disp(&rt, &v), "104");
}

#[test]
fn optional_absent_unreferenced_is_fine() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser optok
          ((_ a (~optional b)) a))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(optok 1)").unwrap();
    assert_eq!(disp(&rt, &v), "1");
    // present-but-unused also fine
    let v = rt.eval_str("<t>", "(optok 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "1");
}

#[test]
fn optional_absent_referenced_without_defaults_errors() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser optbad
          ((_ a (~optional b)) (list a b)))
        "#,
    )
    .unwrap();
    // present is fine
    let v = rt.eval_str("<t>", "(optbad 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
    // absent + referenced + no #:defaults -> clear expand error
    let err = rt
        .eval_str("<t>", "(optbad 1)")
        .expect_err("absent optional var referenced without defaults");
    let s = format!("{}", err);
    assert!(
        s.contains("absent") || s.contains("#:defaults"),
        "got: {}",
        s
    );
}

// =====================================================================
// ~once / ellipsis-head patterns — the flagship: order-free keyword
// options, each required exactly once
// =====================================================================

#[test]
fn once_keyword_options_any_order() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser kvpair
          ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(kvpair #:a 1 #:b 2)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
    // reversed order -> same bindings
    let v = rt.eval_str("<t>", "(kvpair #:b 2 #:a 1)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
}

#[test]
fn once_missing_required_option_errors() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser kvpair
          ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(kvpair #:a 1)")
        .expect_err("#:b required exactly once");
    let s = format!("{}", err);
    assert!(
        s.contains("no matching rule") || s.contains("kvpair"),
        "got: {}",
        s
    );
}

#[test]
fn once_duplicate_option_errors() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser kvpair
          ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(kvpair #:a 1 #:a 3 #:b 2)")
        .expect_err("#:a appears twice");
    let s = format!("{}", err);
    assert!(
        s.contains("no matching rule") || s.contains("kvpair"),
        "got: {}",
        s
    );
}

#[test]
fn eh_optional_with_defaults_in_ellipsis() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser kvopt
          ((_ (~or (~once #:x x) (~optional #:y y #:defaults ((y 0)))) ...)
           (list x y)))
        "#,
    )
    .unwrap();
    // optional present
    let v = rt.eval_str("<t>", "(kvopt #:x 1 #:y 2)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
    // optional absent -> default 0
    let v = rt.eval_str("<t>", "(kvopt #:x 1)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 0)");
    // optional twice -> cardinality violation
    let err = rt
        .eval_str("<t>", "(kvopt #:x 1 #:y 2 #:y 3)")
        .expect_err("~optional at most once");
    let s = format!("{}", err);
    assert!(
        s.contains("no matching rule") || s.contains("kvopt"),
        "got: {}",
        s
    );
}

#[test]
fn plain_eh_or_repetition_accumulates_shared_var_in_order() {
    // ~or of plain alternatives under ellipsis: the shared variable
    // accumulates across BOTH alternatives in input order.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser collect
          ((_ (~or (left x) (right x)) ...) (list x ...)))
        "#,
    )
    .unwrap();
    let v = rt
        .eval_str("<t>", "(collect (left 1) (right 2) (left 3))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3)");
}

#[test]
fn once_combined_with_fixed_leading_element() {
    // ~once EH ellipsis after a fixed leading pattern element.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser named
          ((_ tag (~or (~once #:a a) (~once #:b b)) ...)
           (list (quote tag) a b)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(named widget #:b 20 #:a 10)").unwrap();
    assert_eq!(disp(&rt, &v), "(widget 10 20)");
}

// =====================================================================
// Composition with :class annotations (Phase 2A.1/2A.2). Works for a
// single pvar inside a combinator; conflicting per-alternative class
// annotations in ~or are unsupported (see syntax_parse module docs).
// =====================================================================

#[test]
fn class_annotation_inside_optional() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser checked-opt
          ((_ x (~optional n:number #:defaults ((n 1)))) (* x n)))
        "#,
    )
    .unwrap();
    // present + passes the :number check
    let v = rt.eval_str("<t>", "(checked-opt 5 3)").unwrap();
    assert_eq!(disp(&rt, &v), "15");
    // absent -> default 1 (which also satisfies :number)
    let v = rt.eval_str("<t>", "(checked-opt 5)").unwrap();
    assert_eq!(disp(&rt, &v), "5");
}

#[test]
fn class_annotation_inside_optional_rejects_bad_value() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser checked-opt
          ((_ (~optional n:number #:defaults ((n 1)))) n))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", r#"(checked-opt "not-a-number")"#)
        .expect_err(":number class check should fire on a string");
    let s = format!("{}", err);
    assert!(s.contains("expected number"), "got: {}", s);
}
