//! R6RS++ §12 (#118) Iter C6 — minimal nested ellipsis.
//!
//! Pattern shape `((p ...) ...)` where `p` is a single bare-symbol
//! pvar: p binds at depth 2 (a list-of-lists). The outer subject
//! must be a proper list whose every element is itself a proper
//! list. p captures the full list-of-lists since each inner
//! `(p ...)` binds p to the entire inner element.
//!
//! Template machinery: each ellipsis layer drops one depth level
//! for referenced pvars. So `(syntax ((p ...) ...))` with p at
//! depth 2:
//!   * Outer `(... ...)` rebinds p to depth 1 in inner template
//!   * Inner `(p ...)` with p at depth 1 splices the inner list
//!
//! Out of scope (still rejected, future iter):
//! * Nested ellipsis with prefix or compound inner: `((kw p ...) ...)`
//! * Three or more ellipsis levels
//! * Vector sub-patterns

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- pattern: ((p ...) ...) ----

#[test]
fn nested_ellipsis_binds_depth_two() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4 5)) ()
               (((p ...) ...) (syntax p)))",
        )
        .unwrap();
    // syntax p where p is depth-2 emits a list-of-lists.
    assert_eq!(disp(&rt, &v), "((1 2) (3 4 5))");
}

#[test]
fn nested_ellipsis_with_empty_inner_lists() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(() () ()) ()
               (((p ...) ...) (syntax p)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(() () ())");
}

#[test]
fn nested_ellipsis_with_empty_outer() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '() ()
               (((p ...) ...) (syntax p)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

// ---- shape failures fall through ----

#[test]
fn nested_ellipsis_rejects_non_list_outer_element() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) 99) ()
               (((p ...) ...) 'matched)
               (_ 'fallback))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "fallback");
}

#[test]
fn nested_ellipsis_rejects_dotted_outer_element() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 . 4)) ()
               (((p ...) ...) 'matched)
               (_ 'fallback))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "fallback");
}

// ---- template: nested ellipsis reconstruction ----

#[test]
fn template_nested_ellipsis_reconstructs_list_of_lists() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4 5)) ()
               (((p ...) ...)
                (syntax ((p ...) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((1 2) (3 4 5))");
}

#[test]
fn template_inner_ellipsis_with_prefix() {
    // Inner template splices p into a prefixed list.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '((1 2) (3 4)) ()
               (((p ...) ...)
                (syntax ((wrap p ...) ...))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "((wrap 1 2) (wrap 3 4))");
}

// ---- diagnostics ----

// (Iter C6's "compound inner / prefix inner rejected" tests
// removed -- Iter C7 generalized nested ellipsis to handle both
// via recursive compile_sc_pattern + depth bump. See
// syntax_case_iter_c7.rs for the success-side tests.)
