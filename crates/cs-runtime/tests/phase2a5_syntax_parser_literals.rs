//! R6RS++ Phase 2A.5 (#32) — `#:literals` support for
//! `define-syntax-parser`.
//!
//! Before this, `define-syntax-parser` always desugared to
//! `(syntax-rules () ...)` (empty literals), so a keyword-driven macro
//! could not match its keywords *by name* — they bound as pattern
//! variables instead. The `#:literals (lit ...)` clause closes that
//! gap, which is what unblocks migrating the in-tree keyword macros
//! (`match`, channel `select`, gen-server callbacks, web routing).

use cs_core::WriteMode;
use cs_runtime::Runtime;

/// Evaluate `src` and return its displayed value. Sequential
/// (`&mut` eval, then `&` format) to avoid an overlapping borrow.
fn eval(rt: &mut Runtime, src: &str) -> String {
    let v = rt.eval_str("<t>", src).unwrap();
    rt.format_value(&v, WriteMode::Display)
}

/// A literal listed in `#:literals` matches *by name*; it does not
/// bind. So `yes`/`no` discriminate clauses rather than each greedily
/// matching the first clause as a pattern variable would.
#[test]
fn literal_discriminates_clauses() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser classify
          #:literals (yes no)
          ((_ yes) 'affirmative)
          ((_ no)  'negative)
          ((_ x)   'other))
        "#,
    )
    .unwrap();
    assert_eq!(eval(&mut rt, "(classify yes)"), "affirmative");
    // The decisive case: without `#:literals`, `(_ yes)` would bind
    // `yes` as a pvar and swallow this too.
    assert_eq!(eval(&mut rt, "(classify no)"), "negative");
    assert_eq!(eval(&mut rt, "(classify 42)"), "other");
}

/// Keyword identifiers (`#:tag`) work as literals — they carry an
/// internal colon but are never mistaken for a `:class` annotation.
#[test]
fn keyword_literal_matches() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser kw-test
          #:literals (#:tag)
          ((_ #:tag v) (list 'tagged v))
          ((_ v)       (list 'plain v)))
        "#,
    )
    .unwrap();
    assert_eq!(eval(&mut rt, "(kw-test #:tag 5)"), "(tagged 5)");
    assert_eq!(eval(&mut rt, "(kw-test 5)"), "(plain 5)");
}

/// `#:literals` composes with `:class` annotations in the same clause.
/// (The `:class` check wraps the body in an `if`, so the body must be
/// an expression — hence the `list` template rather than a `define`.)
#[test]
fn literals_compose_with_class_annotations() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser tagged
          #:literals (=>)
          ((_ name:id => val) (list (quote name) val)))
        "#,
    )
    .unwrap();
    assert_eq!(eval(&mut rt, "(tagged foo => 10)"), "(foo 10)");
    // `name:id` still validates even with literals present.
    let err = rt
        .eval_str("<t>", "(tagged 42 => 10)")
        .expect_err("non-identifier name should error");
    let s = format!("{err}");
    assert!(
        s.contains("expected id") || s.contains("tagged"),
        "got: {s}"
    );
}

/// Regression: a parser with no `#:literals` clause behaves exactly as
/// before (every pattern symbol binds).
#[test]
fn no_literals_clause_still_binds_all_symbols() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser swap2
          ((_ a b) (list b a)))
        "#,
    )
    .unwrap();
    assert_eq!(eval(&mut rt, "(swap2 1 2)"), "(2 1)");
}

/// A recursive keyword macro — the shape `match-clauses` relies on:
/// the literal `when` selects the guarded clause, and the macro
/// references itself in the template.
#[test]
fn recursive_keyword_macro() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser pick
          #:literals (when)
          ((_ (when c) v rest ...) (if c v (pick rest ...)))
          ((_ v rest ...)          v)
          ((_)                     'none))
        "#,
    )
    .unwrap();
    assert_eq!(eval(&mut rt, "(pick (when #f) 1 2 3)"), "2");
    assert_eq!(eval(&mut rt, "(pick (when #t) 1 2 3)"), "1");
}
