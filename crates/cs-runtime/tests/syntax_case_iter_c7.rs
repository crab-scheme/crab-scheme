//! R6RS++ §12 (#118) Iter C7 — nested ellipsis with compound /
//! prefixed inner.
//!
//! Generalizes Iter C6's bare-pvar-only nested-ellipsis handler
//! to arbitrary inner ellipsis shapes. Implementation: when the
//! compound-sub branch of `compile_sc_pattern` detects a sub
//! that's itself a `(... ...)` form, it recursively compiles the
//! inner ellipsis pattern against a synthetic `__sc-inner-elem__`
//! key, then wraps every inner test in `(every (lambda (e) …))`
//! and every inner pvar extractor in `(map (lambda (e) …) walking)`,
//! bumping each inner pvar's depth by 1.
//!
//! Shapes that now land:
//! * `(((a b) …) …)` — compound inner: a, b at depth 2
//! * `((kw p …) …)` — prefixed inner with literal: p at depth 2
//! * `((h p …) …)` — prefixed inner with pvar: h at depth 2 (one
//!   per outer element), p at depth 2 (a list per outer element)
//!
//! Iter C7 still doesn't support templates whose pvars cross
//! ellipsis depths (e.g., using a depth-2 pvar inside only one
//! template-level ellipsis) — the depth-bookkeeping fix from
//! Iter C6 handles balanced cases.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- pattern: prefixed inner ----

#[test]
fn prefixed_inner_with_literal_kw() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((kw 1 2) (kw 3 4 5)) (kw)
               (((kw p ...) ...) (syntax p)))",
        )
        .unwrap();
    // For each outer element, p binds to the rest after `kw`.
    // p at depth 2 → ((1 2) (3 4 5)).
    assert_eq!(disp(&rt, &v), "((1 2) (3 4 5))");
}

#[test]
fn prefixed_inner_with_pvar_head() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((a 1 2) (b 3 4)) ()
               (((h p ...) ...) (list (syntax h) (syntax p))))",
        )
        .unwrap();
    // h at depth 2 → (a b), p at depth 2 → ((1 2) (3 4)).
    assert_eq!(disp(&rt, &v), "((a b) ((1 2) (3 4)))");
}

#[test]
fn prefixed_inner_with_kw_falls_through_on_mismatch() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((kw 1) (other 2)) (kw)
               (((kw p ...) ...) 'all-kw)
               (_ 'mixed))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "mixed");
}

// ---- pattern: compound inner ----

#[test]
fn compound_inner_binds_depth_two_pvars() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(((1 2) (3 4)) ((5 6))) ()
               ((((a b) ...) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    // First outer elem: ((1 2) (3 4)) → a-inner=(1 3), b-inner=(2 4)
    // Second outer elem: ((5 6)) → a-inner=(5), b-inner=(6)
    // At depth 2: a = ((1 3) (5)), b = ((2 4) (6))
    assert_eq!(disp(&rt, &v), "(((1 3) (5)) ((2 4) (6)))");
}

#[test]
fn compound_inner_with_empty_outer_or_inner() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(() ((1 2))) ()
               ((((a b) ...) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    // First outer = (): inner matches empty -> a-inner=(), b-inner=()
    // Second outer = ((1 2)): a-inner=(1), b-inner=(2)
    // At depth 2: a = (() (1)), b = (() (2))
    // (list (syntax a) (syntax b)) = ((() (1)) (() (2)))
    assert_eq!(disp(&rt, &v), "((() (1)) (() (2)))");
}

// ---- template: nested zip-map reconstruction ----

#[test]
fn template_reconstructs_prefixed_inner() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((kw 1 2) (kw 3 4)) (kw)
               (((kw p ...) ...)
                (syntax ((tag p ...) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((tag 1 2) (tag 3 4))");
}

#[test]
fn template_reconstructs_compound_inner() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(((1 2) (3 4)) ((5 6))) ()
               ((((a b) ...) ...)
                (syntax (((wrap a b) ...) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(((wrap 1 2) (wrap 3 4)) ((wrap 5 6)))");
}

// ---- composition: realistic macro shape ----

#[test]
fn nested_let_pattern_extraction() {
    // Pattern shape for `(my-let* (((var val) ...) ...) body)`
    // where each outer ellipsis section is one let-binding group.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(((x 1) (y 2)) ((a 3) (b 4))) ()
               ((((name val) ...) ...)
                (list (syntax name) (syntax val))))",
        )
        .unwrap();
    // name at depth 2 = ((x y) (a b))
    // val at depth 2 = ((1 2) (3 4))
    assert_eq!(disp(&rt, &v), "(((x y) (a b)) ((1 2) (3 4)))");
}
