//! R6RS++ §12 (#118) Iter C4 — compound-sub extensions.
//!
//! Builds on Iter C3 (compound proper-list of bare-symbol pvars
//! under ellipsis). Iter C4 widens the compound sub-pattern to
//! also accept:
//!
//! * Literal identifiers (from the literals list) — eq?-checked
//!   at their position; no binding.
//! * `_` wildcards — accept any value at that position; no binding.
//! * Dotted-tail sub-patterns: `((a . b) ...)`, `((a b . rest) ...)`.
//!
//! Out of scope (still rejected, see Iter C5+):
//! * Nested ellipsis: `((p …) …)`
//! * Nested compound sub-patterns: `((a (b c)) …)`
//! * Vector sub-patterns

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- literals inside compound sub-pattern ----

#[test]
fn literal_kw_in_sub_matches_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((kw 1) (kw 2) (kw 3)) (kw)
               (((kw v) ...) (syntax v)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3)");
}

#[test]
fn literal_kw_mismatch_falls_through() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((kw 1) (other 2)) (kw)
               (((kw v) ...) 'all-kw)
               (_ 'mixed))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "mixed");
}

#[test]
fn literal_at_multiple_positions() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((open 1 close) (open 2 close)) (open close)
               (((open v close) ...) (syntax v)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
}

// ---- wildcards inside compound sub-pattern ----

#[test]
fn wildcard_skips_position() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 ignored 2) (3 whatever 4)) ()
               (((a _ b) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 3) (2 4))");
}

#[test]
fn wildcard_only_sub_pattern() {
    // Sub-pattern is all wildcards: shape-check only, no
    // bindings. (At least we must check the structure.)
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((a b) (c d) (e f)) ()
               (((_ _) ...) 'all-pairs)
               (_ 'mixed))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "all-pairs");
}

// ---- dotted-tail sub-pattern ----

#[test]
fn dotted_tail_sub_binds_head_and_tail() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 . a) (2 . b) (3 . c)) ()
               (((h . t) ...) (list (syntax h) (syntax t))))",
        )
        .unwrap();
    // h → (1 2 3), t → (a b c)
    assert_eq!(disp(&rt, &v), "((1 2 3) (a b c))");
}

#[test]
fn dotted_tail_with_proper_list_arg() {
    // `((h . t) ...)` against a list whose elements are proper
    // lists works: t binds to the cdr of each element.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2 3) (4 5 6)) ()
               (((h . t) ...) (list (syntax h) (syntax t))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 4) ((2 3) (5 6)))");
}

#[test]
fn dotted_tail_with_prefix_in_sub() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2 a b) (3 4 c d)) ()
               (((x y . rest) ...) (list (syntax x) (syntax y) (syntax rest))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 3) (2 4) ((a b) (c d)))");
}

// ---- composition: case-style + tagged binding ----

#[test]
fn case_lambda_clause_extraction() {
    // Common shape for `case-lambda` rewrites: each clause is
    // `(args body)`. Extract args-list and body-list separately.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(((a) (+ a 1)) ((a b) (+ a b))) ()
               (((args body) ...)
                (syntax (case-lambda (args body) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(case-lambda ((a) (+ a 1)) ((a b) (+ a b)))");
}

#[test]
fn cond_clause_extraction_with_arrow_literal() {
    // Realistic case-style: each clause is `(test => proc)`,
    // with `=>` a literal. Extract test and proc lists.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((t1 => proc1) (t2 => proc2)) (=>)
               (((test => proc) ...)
                (list (syntax test) (syntax proc))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((t1 t2) (proc1 proc2))");
}

// ---- diagnostics ----

// (Iter C4 used to reject nested ellipsis; Iter C6 handles the
// `((p ...) ...)` form. Iter C6's own test file pins the
// remaining rejections for compound/prefixed inner sections.)

// (Iter C4 used to reject nested compound sub-patterns; Iter C5
// handles them via the recursive sub-pattern walker -- see
// syntax_case_iter_c5.rs.)
