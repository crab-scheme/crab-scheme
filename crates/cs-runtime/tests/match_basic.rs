//! End-to-end tests for the `(match …)` library at
//! `lib/match/match.scm`. Loads the library source into a fresh
//! Runtime via `eval_str` and exercises each supported pattern
//! shape.
//!
//! Spec: `docs/research/r6rs_extensions_spec.md` §1.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_match() -> Runtime {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/match/match.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<match>", &src).expect("load match.scm");
    rt
}

#[test]
fn wildcard_matches_anything() {
    let mut rt = load_match();
    let v = rt.eval_str("<t>", "(match 42 (_ 'matched))").unwrap();
    assert_eq!(disp(&rt, &v), "matched");
}

#[test]
fn syntax_rules_dotted_pair_pattern() {
    // Regression for #111 (pattern side): cs-expand's syntax-rules
    // now accepts dotted-pair patterns `(x . y)`. The pattern walks
    // via collect_pair_chain instead of bailing on
    // collect_proper_list_strict.
    //
    // Note: the macros emit `(quote ...)` to keep the test focused
    // on expansion correctness rather than on what evaluating an
    // unquoted list does.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"
        (define-syntax car-of
          (syntax-rules ()
            ((_ (x . y)) (quote x))))
        (define-syntax tail-of
          (syntax-rules ()
            ((_ (x . y)) (quote y))))
        (list (car-of  (1 2 3))
              (tail-of (1 2 3)))
    "#,
        )
        .expect("dotted pattern should match proper lists");
    assert_eq!(
        rt.format_value(&v, cs_core::WriteMode::Display),
        "(1 (2 3))"
    );
}

#[test]
fn syntax_rules_dotted_template() {
    // Regression for #111 (template side): templates can now
    // contain dotted-pair forms, e.g. (quote a . b) — though this
    // particular shape is unusual because the dotted tail must
    // itself be a quoted list. Use a simpler form: a template
    // that produces a dotted pair via a pattern variable in the
    // tail position.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"
        (define-syntax dotted-quote
          (syntax-rules ()
            ((_ a b) '(a . b))))
        (dotted-quote head tail)
    "#,
        )
        .expect("dotted template should instantiate");
    assert_eq!(
        rt.format_value(&v, cs_core::WriteMode::Display),
        "(head . tail)"
    );
}

#[test]
fn syntax_rules_underscore_in_literals_is_literal_not_wildcard() {
    // Regression for #112: previously, `_` was always a wildcard
    // in match-pattern, even when listed as a syntax-rules
    // literal. That broke any catch-all rule that came after a
    // rule using `_` as a literal.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"
        (define-syntax kind
          (syntax-rules (_)
            ((_ _ body)     'literal-underscore)
            ((_ var body)   'pattern-variable)))
        (list (kind _ first)    ;; literal _ matches → rule 1
              (kind x second))  ;; x ≠ literal _ → rule 2 binds var
    "#,
        )
        .expect("literal `_` should match literal `_`");
    assert_eq!(
        rt.format_value(&v, cs_core::WriteMode::Display),
        "(literal-underscore pattern-variable)"
    );
}

#[test]
fn identifier_binds_subject() {
    let mut rt = load_match();
    let v = rt.eval_str("<t>", "(match 42 (x (+ x 1)))").unwrap();
    assert_eq!(disp(&rt, &v), "43");
}

#[test]
fn quoted_literal_matches_via_equal() {
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match 'hello ('hello 'yes) (_ 'no))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "yes");

    let v = rt
        .eval_str("<t>", "(match 'world ('hello 'yes) (_ 'no))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "no");
}

#[test]
fn empty_list_pattern() {
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match '() (() 'empty) (_ 'nonempty))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "empty");

    let v = rt
        .eval_str("<t>", "(match '(1) (() 'empty) (_ 'nonempty))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "nonempty");
}

#[test]
fn predicate_no_binding() {
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match 42 ((? number?) 'num) (_ 'other))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "num");

    let v = rt
        .eval_str("<t>", "(match 'foo ((? number?) 'num) (_ 'other))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "other");
}

#[test]
fn predicate_with_binding() {
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match 42 ((? number? n) (+ n 8)) (_ 'other))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "50");
}

#[test]
fn pair_pattern_destructures() {
    let mut rt = load_match();
    // Both the Racket-style (cons …) and the native (a . b) forms
    // work — the former is sugar for the latter.
    let v = rt
        .eval_str("<t>", "(match '(1 . 2) ((cons a b) (+ a b)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");

    let v = rt
        .eval_str("<t>", "(match '(1 . 2) ((a . b) (+ a b)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

#[test]
fn bare_list_pattern_native_form() {
    // With the #111 fix, bare (a b c) list patterns work directly
    // (no need for (list a b c) wrapping).
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match '(1 2 3) ((a b c) (+ a b c)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn bare_list_pattern_with_dotted_tail() {
    // Native (head . rest) form binds rest to the remainder.
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match '(1 2 3 4)
               ((head . rest) (cons head (length rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 . 3)");
}

#[test]
fn list_pattern_three_elements() {
    let mut rt = load_match();
    let v = rt
        .eval_str("<t>", "(match '(1 2 3) ((list a b c) (+ a b c)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn list_pattern_length_mismatch_falls_through() {
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match '(1 2)
               ((list a b c) 'three)
               ((list a b)   'two)
               (_            'other))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "two");
}

#[test]
fn vector_pattern() {
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match (vector 1 2 3) ((vector a b c) (+ a b c)) (_ 'no))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");

    // Length mismatch falls through.
    let v = rt
        .eval_str(
            "<t>",
            "(match (vector 1 2) ((vector a b c) 'three) ((vector a b) 'two) (_ 'no))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "two");
}

#[test]
fn nested_pair_in_list() {
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match '(point (1 . 2))
               ((list 'point (cons x y)) (+ x y)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

#[test]
fn guard_clause() {
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match 10
               (x (when (negative? x)) 'neg)
               (x (when (zero? x))     'zero)
               (x (when (positive? x)) 'pos))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "pos");

    let v = rt
        .eval_str(
            "<t>",
            "(match -5
               (x (when (negative? x)) 'neg)
               (x (when (zero? x))     'zero)
               (x (when (positive? x)) 'pos))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "neg");
}

#[test]
fn guard_fails_falls_through() {
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(match 0
               (x (when (positive? x)) 'pos)
               (_ 'other))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "other");
}

#[test]
fn no_match_raises() {
    let mut rt = load_match();
    let err = rt
        .eval_str("<t>", "(match 42 ('foo 'a) ('bar 'b))")
        .expect_err("no clause should error");
    let formatted = format!("{}", err);
    assert!(formatted.contains("match"), "got: {}", formatted);
}

#[test]
fn match_evaluates_subject_once() {
    // The subject is let-bound at the top of match expansion, so
    // a side-effecting expression should fire exactly once even
    // when multiple clauses backtrack.
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            r#"
            (define counter 0)
            (define (bump!) (set! counter (+ counter 1)) counter)
            (match (bump!)
              ('a 'a)
              ('b 'b)
              (_  counter))
        "#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "1");
}

#[test]
fn tag_dispatch_idiom() {
    // The use case the spec calls out at the top of §1:
    let mut rt = load_match();
    let v = rt
        .eval_str(
            "<t>",
            "(define (eval-expr e)
               (match e
                 ((list 'add x y) (+ x y))
                 ((list 'sub x y) (- x y))
                 ((list 'mul x y) (* x y))
                 (_ (error 'eval-expr \"unknown form\" e))))
             (eval-expr '(add 3 4))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "7");

    let v = rt.eval_str("<t>", "(eval-expr '(sub 10 7))").unwrap();
    assert_eq!(disp(&rt, &v), "3");
}
