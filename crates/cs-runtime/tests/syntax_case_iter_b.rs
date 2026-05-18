//! R6RS++ §12 (#118) Iter B — syntax-case form parser.
//!
//! Iter B covers patterns: `_`, literal identifiers (via literals
//! list), pattern variables, self-quoting literals, `()`, dotted
//! pairs, and proper lists. No ellipsis (Iter C). No fenders
//! (Iter D). `(syntax T)` inside a clause body is rewritten to a
//! template-instantiation expression using the bound pvars.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- pattern: pattern variable ----

#[test]
fn pvar_pattern_binds_whole_subject() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 42 () (x (syntax x)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn pvar_pattern_with_list_subject() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case '(1 2 3) () (x (syntax x)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3)");
}

// ---- pattern: wildcard ----

#[test]
fn wildcard_matches_anything_no_binding() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 'foo () (_ 'matched))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched");
}

// ---- pattern: literal identifier ----

#[test]
fn literal_identifier_matches_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 'if (if) (if 'matched-if))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched-if");
}

#[test]
fn literal_identifier_no_match_falls_through() {
    let mut rt = Runtime::new();
    // First clause's `if` literal doesn't match; second wildcard
    // catches.
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'other (if) (if 'matched-if) (_ 'fallback))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "fallback");
}

#[test]
fn pvar_not_in_literals_binds() {
    // `foo` is NOT in the literals list -> it's a pattern var.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 'anything () (foo (syntax foo)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "anything");
}

// ---- pattern: self-quoting literal ----

#[test]
fn number_literal_pattern() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 42 () (42 'matched-42) (_ 'fallback))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched-42");
}

#[test]
fn number_literal_mismatch_falls_through() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case 99 () (42 'matched-42) (_ 'other))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "other");
}

#[test]
fn string_literal_pattern() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", r#"(syntax-case "hi" () ("hi" 'matched) (_ 'other))"#)
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched");
}

// ---- pattern: null ----

#[test]
fn null_pattern() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case '() () (() 'empty) (_ 'other))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "empty");
}

#[test]
fn null_pattern_no_match() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-case '(1) () (() 'empty) (_ 'non-empty))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "non-empty");
}

// ---- pattern: list ----

#[test]
fn list_pattern_extracts_elements() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3) ()
               ((a b c) (syntax (c b a))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3 2 1)");
}

#[test]
fn list_pattern_wrong_length_falls_through() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2) ()
               ((a b c) (syntax matched-3))
               ((a b) (syntax matched-2)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "matched-2");
}

// ---- pattern: dotted pair ----

#[test]
fn dotted_pair_pattern_binds_head_and_rest() {
    // (1 2 3 4) decomposed as head=1, rest=(2 3 4). Rebuilt with
    // (cons head rest) -> (1 2 3 4). Then we also check that
    // `rest` was bound to a proper sub-list so the spine survives
    // the destructuring/reconstructing roundtrip.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3 4) ()
               ((head . rest) (cons (syntax head) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3 4)");

    // Also verify the pieces independently.
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2 3 4) ()
               ((head . rest) (list (syntax head) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 (2 3 4))");
}

// ---- pattern: nested ----

#[test]
fn nested_list_pattern() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4)) ()
               (((a b) (c d)) (syntax (a c b d))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 3 2 4)");
}

// ---- template: (syntax T) reconstruction ----

#[test]
fn template_with_quoted_atoms_and_pvars() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(foo) ()
               ((x) (syntax (label x))))",
        )
        .unwrap();
    // `label` is not a pvar so it becomes a quoted symbol; `x` is.
    assert_eq!(disp(&rt, &v), "(label foo)");
}

#[test]
fn template_body_can_be_arbitrary_scheme() {
    // The clause body isn't restricted to `(syntax T)` -- pvars
    // are normal Scheme variables in scope, so we can mix them
    // with arithmetic, function calls, etc.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(10 20) ()
               ((a b) (+ a b 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "31");
}

#[test]
fn template_can_use_syntax_within_larger_expr() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(a b c) ()
               ((x y z) (list (syntax x) (syntax z))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(a c)");
}

// ---- clause selection ordering ----

#[test]
fn first_matching_clause_wins() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(1 2) ()
               ((a b) 'first)
               (_ 'fallback))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "first");
}

// ---- no-match diagnostics ----

#[test]
fn no_matching_clause_raises_error() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(syntax-case 42 () (() 'empty))")
        .expect_err("no matching pattern should raise");
    let s = format!("{}", err);
    assert!(
        s.contains("syntax-case") || s.contains("no matching pattern"),
        "got: {}",
        s
    );
}

// (Iter B used to reject 3-element clauses with an Iter D
// pointer; Iter D now implements fenders. See
// syntax_case_iter_d.rs for the success-side tests.)

// ---- standalone (syntax X) outside syntax-case ----

#[test]
fn standalone_syntax_form_is_like_quote() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(syntax foo)").unwrap();
    assert_eq!(disp(&rt, &v), "foo");
    let v = rt.eval_str("<t>", "(syntax (a b c))").unwrap();
    assert_eq!(disp(&rt, &v), "(a b c)");
}
