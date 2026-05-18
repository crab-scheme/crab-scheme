//! R6RS++ §12 (#118) Iter C3 — compound + multi-pvar ellipsis.
//!
//! Builds on Iter C2 (`(prefix… pvar …)`). Adds:
//!
//! * Pattern `(sub …)` where `sub` is a proper list of bare-symbol
//!   pvars (e.g., `((a b) …)`). Each pvar in `sub` becomes a
//!   depth-1 list capturing the per-element extraction.
//! * Template `(sub …)` zip-map: when `sub` contains multiple
//!   depth-1 pvars they're mapped in parallel.
//!
//! Out of scope (rejected with pointer to a future iter):
//! * Nested ellipsis `((p …) …)`
//! * Dotted sub-patterns
//! * Literals inside compound sub-pattern
//! * Vector patterns

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- pattern: compound sub-pattern ----

#[test]
fn pair_pattern_under_ellipsis_binds_two_lists() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4) (5 6)) ()
               (((a b) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    // a → (1 3 5), b → (2 4 6)
    assert_eq!(disp(&rt, &v), "((1 3 5) (2 4 6))");
}

#[test]
fn triple_pattern_under_ellipsis_binds_three_lists() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((a 1 x) (b 2 y) (c 3 z)) ()
               (((k v t) ...) (list (syntax k) (syntax v) (syntax t))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((a b c) (1 2 3) (x y z))");
}

#[test]
fn empty_list_under_compound_ellipsis() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '() ()
               (((a b) ...) (list (syntax a) (syntax b))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(() ())");
}

#[test]
fn wrong_shape_falls_through() {
    // Subject has elements that are not proper 2-element lists.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4 5)) ()
               (((a b) ...) 'matched-pairs)
               (_ 'mixed))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "mixed");
}

#[test]
fn non_list_elements_fall_through() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) 99) ()
               (((a b) ...) 'matched-pairs)
               (_ 'fallback))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "fallback");
}

// ---- pattern: prefix + compound ellipsis ----

#[test]
fn prefix_plus_compound_ellipsis() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(header (1 2) (3 4)) ()
               ((h (a b) ...) (list (syntax h) (syntax a) (syntax b))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(header (1 3) (2 4))");
}

// ---- template: zip-map ----

#[test]
fn template_zip_map_reconstructs_pairs() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4)) ()
               (((a b) ...)
                (syntax ((a b) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 2) (3 4))");
}

#[test]
fn template_zip_map_with_literal_inner() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((x 1) (y 2)) ()
               (((name val) ...)
                (syntax ((let-binding name val) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((let-binding x 1) (let-binding y 2))");
}

#[test]
fn template_zip_map_into_prefix() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((x 1) (y 2)) ()
               (((name val) ...)
                (syntax (let-form (name val) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(let-form (x 1) (y 2))");
}

// ---- composition: emulated let-style macro ----

#[test]
fn let_style_macro_emulation() {
    // Given `(let-shape ((var val) ...) body)`, produce a
    // `((lambda (var ...) body) val ...)` form -- the textbook
    // `let` macro rewrite.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((var val) ((x 1) (y 2)) (+ x y)) ()
               ((_ ((name binding) ...) body)
                (syntax ((lambda (name ...) body) binding ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((lambda (x y) (+ x y)) 1 2)");
}

// ---- diagnostics ----

// (Iter C3 used to reject nested ellipsis; Iter C6 now handles
// the `((p ...) ...)` form -- see syntax_case_iter_c6.rs.)

// (Iter C3 used to reject literals inside compound sub-patterns;
// Iter C4 handles them -- see syntax_case_iter_c4.rs.)
