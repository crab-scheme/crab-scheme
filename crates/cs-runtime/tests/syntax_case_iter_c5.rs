//! R6RS++ §12 (#118) Iter C5 — nested compound sub-patterns.
//!
//! Refactor that introduces a recursive walker
//! (`walk_sub_pattern` in cs-expand) handling arbitrarily nested
//! compound sub-patterns under `…`. Replaces the flat
//! `classify_compound_sub` from Iter C3/C4.
//!
//! What lands:
//! * `((a (b c)) …)` — pvar at outer position + nested compound
//! * `((a (b . c)) …)` — nested dotted-tail in sub
//! * `((kw (a b)) …)` — literal kw + nested compound
//! * `((a (_ c)) …)` — wildcards at any depth
//! * Deeper nesting: `((a (b (c d))) …)`
//!
//! Still rejected (Iter C6):
//! * Nested ellipsis: `((p …) …)` — the inner ellipsis section
//!   needs a per-element matcher loop, not a flat shape check.
//! * Vector sub-patterns.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- nested compound: list inside list ----

#[test]
fn nested_compound_extracts_inner_pvars() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (2 3)) (4 (5 6))) ()
               (((a (b c)) ...)
                (list (syntax a) (syntax b) (syntax c))))",
        )
        .unwrap();
    // a → (1 4), b → (2 5), c → (3 6)
    assert_eq!(disp(&rt, &v), "((1 4) (2 5) (3 6))");
}

#[test]
fn deeply_nested_compound() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (2 (3 4))) (5 (6 (7 8)))) ()
               (((a (b (c d))) ...)
                (list (syntax a) (syntax b) (syntax c) (syntax d))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 5) (2 6) (3 7) (4 8))");
}

#[test]
fn nested_compound_with_wildcards() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (2 99)) (3 (4 88))) ()
               (((a (b _)) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 3) (2 4))");
}

#[test]
fn nested_compound_with_inner_literal() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (tag v1)) (2 (tag v2))) (tag)
               (((a (tag v)) ...) (list (syntax a) (syntax v))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 2) (v1 v2))");
}

#[test]
fn nested_compound_with_dotted_inner() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (a . b)) (2 (c . d))) ()
               (((k (h . t)) ...)
                (list (syntax k) (syntax h) (syntax t))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 2) (a c) (b d))");
}

// ---- shape mismatch falls through ----

#[test]
fn nested_shape_mismatch_falls_through() {
    // Subject's second outer element doesn't have a 2-elem
    // inner list -- pattern should fail to match.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (2 3)) (4 (5))) ()
               (((a (b c)) ...) 'all-shape)
               (_ 'mixed))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "mixed");
}

#[test]
fn nested_compound_empty_outer_list() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '() ()
               (((a (b c)) ...)
                (list (syntax a) (syntax b) (syntax c))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(() () ())");
}

// ---- template zip-map over nested pvars ----

#[test]
fn template_zip_map_reconstructs_nested() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 (2 3)) (4 (5 6))) ()
               (((a (b c)) ...)
                (syntax ((a b c) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 2 3) (4 5 6))");
}

// ---- composition: define-record-type-style macro emulation ----

#[test]
fn record_field_extraction_emulation() {
    // Pattern shape for an R6RS-style record clause:
    // `(define-record-type name (constructor field ...) predicate
    //   (field accessor) ...)`
    // We don't macro-expand it; we just exercise the shape via
    // syntax-case to confirm field-list zip-map.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((id id-of) (name name-of) (age age-of)) ()
               (((field accessor) ...)
                (syntax ((define-accessor field accessor) ...))))",
        )
        .unwrap();
    assert_eq!(
        disp(&rt, &v),
        "((define-accessor id id-of) (define-accessor name name-of) (define-accessor age age-of))"
    );
}

// ---- diagnostics ----

// (Iter C5's "nested ellipsis still rejected" test removed --
// Iter C6 now handles `((p ...) ...)`. See syntax_case_iter_c6.rs.)
