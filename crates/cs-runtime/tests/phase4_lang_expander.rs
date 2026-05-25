//! R6RS++ Phase 4 — `#!lang` `expander` export (issue #71).
//!
//! Option 2 MVP: when `(lang NAME)` declares `(export expander)`
//! and binds it to a procedure, the host calls that procedure
//! with the datum list produced by the host reader (or the
//! lang's `reader`, if also exported) and feeds the returned
//! datums to the standard host expander. Effectively a
//! datum→datum macro pass between read and expand.
//!
//! Coverage:
//! - expander alone (no reader) — host reader output runs
//!   through the user expander before host expansion
//! - reader + expander composition — pipeline is
//!   read → user-expand → host-expand
//! - expander returning multiple forms — body can be expanded
//!   into several top-level forms
//! - expander not a procedure (e.g. an integer) → silent
//!   degradation (the lang library is misconfigured; we fall
//!   back to plain host expansion rather than panic)
//! - expander raises → diagnostic names the offending lang
//! - expander returns non-list / non-datum → typed error

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn install(rt: &mut Runtime, src: &str) {
    rt.eval_str("<lang-install>", src).unwrap();
}

// ---- expander alone (no reader) ----

#[test]
fn expander_alone_rewrites_host_reader_output() {
    let mut rt = Runtime::new();
    // The expander wraps each top-level datum in (begin DATUM)
    // — a trivial transformation that proves it ran on the host
    // reader's output. Body is `(+ 10 20)`; without the
    // expander, the program evaluates to 30 anyway. To prove
    // the expander ran, we have it REPLACE the body entirely
    // with a constant form.
    install(
        &mut rt,
        "(library (lang fixed-expand)
           (export expander)
           (import (rnrs))
           (define (expander datums) '((+ 100 100))))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang fixed-expand\n\
             (this would be (+ 10 20) but the expander discards it)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "200");
}

#[test]
fn expander_receives_host_reader_datum_list() {
    let mut rt = Runtime::new();
    // Capture how many top-level forms the expander saw so we
    // can prove it received the host-parsed datums (not the
    // empty list, not a string, etc.).
    install(
        &mut rt,
        "(library (lang counter)
           (export expander seen)
           (import (rnrs))
           (define seen -1)
           (define (expander datums)
             (set! seen (length datums))
             '(99)))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang counter\n\
             (define a 1)\n\
             (define b 2)\n\
             (+ a b)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "99");
    let seen = rt.lookup("seen").unwrap();
    // Three top-level forms in the body — two defines and the
    // trailing expression.
    assert_eq!(disp(&rt, &seen), "3");
}

// ---- reader + expander composition ----

#[test]
fn reader_then_expander_pipeline() {
    let mut rt = Runtime::new();
    // Reader produces the datum (+ 1 2 3). Expander rewrites
    // each top-level form by prepending `* 10` to the head, so
    // (+ 1 2 3) becomes (* 10 (+ 1 2 3)) = 60. Tests the
    // pipeline read → user-expand → host-expand.
    install(
        &mut rt,
        "(library (lang pipe)
           (export reader expander)
           (import (rnrs))
           (define (reader body-str) '((+ 1 2 3)))
           (define (expander datums)
             (map (lambda (d) (list '* 10 d)) datums)))",
    );
    let v = rt.eval_str("<t>", "#!lang pipe\n").unwrap();
    assert_eq!(disp(&rt, &v), "60");
}

#[test]
fn expander_can_emit_multiple_forms() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang multiform)
           (export expander)
           (import (rnrs))
           (define (expander datums)
             '((define x 50) (define y 50) (+ x y))))",
    );
    let v = rt.eval_str("<t>", "#!lang multiform\nignored\n").unwrap();
    assert_eq!(disp(&rt, &v), "100");
}

// ---- degradation: expander not a procedure ----

#[test]
fn non_procedure_expander_falls_through_silently() {
    let mut rt = Runtime::new();
    // Library exports `expander` but binds it to an integer.
    // Treat as misconfigured library — degrade to plain host
    // expansion rather than panic. Body should evaluate
    // normally.
    install(
        &mut rt,
        "(library (lang bad-expand)
           (export expander)
           (import (rnrs))
           (define expander 42))",
    );
    let v = rt.eval_str("<t>", "#!lang bad-expand\n(+ 1 2 3)").unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

// ---- error paths ----

#[test]
fn expander_raising_propagates_with_named_lang() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang raiser-expand)
           (export expander)
           (import (rnrs))
           (define (expander datums)
             (error 'expander \"expand bailed\")))",
    );
    let err = rt
        .eval_str("<t>", "#!lang raiser-expand\n(+ 1 2)")
        .expect_err("raising expander should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang raiser-expand)") && s.contains("expander"),
        "diag should name the lang and 'expander': {}",
        s
    );
}

#[test]
fn expander_returning_non_list_is_diagnosed() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang non-list-expand)
           (export expander)
           (import (rnrs))
           (define (expander datums) 42))",
    );
    let err = rt
        .eval_str("<t>", "#!lang non-list-expand\n(+ 1 2)")
        .expect_err("non-list return should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang non-list-expand)") && s.contains("non-datum"),
        "diag should name the lang and 'non-datum': {}",
        s
    );
}
