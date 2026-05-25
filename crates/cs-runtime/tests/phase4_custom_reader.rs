//! R6RS++ Phase 4 — full `#!lang` custom reader protocol (issue
//! #10). Phase 3C shipped only the header → `(import (lang NAME))`
//! rewrite; this milestone adds the parse-time reader hook: if
//! `(lang NAME)` exports a `reader` procedure, the host calls it
//! on the file body and feeds the returned datums to the expander
//! instead of running the host reader.
//!
//! Coverage:
//! - reader is invoked, drives expansion, sees the body text
//! - reader can return multiple top-level forms
//! - reader can call into the host reader via
//!   `open-input-string`+`read` (passthrough lang)
//! - body still imports the lang's other exports (helper visible)
//! - lang without a `reader` export falls back to MVP behaviour
//! - user-defined top-level `reader` is *not* mistaken for one
//!   the lang library exported
//! - reader raising or returning a non-datum produces a diagnostic
//!   that names the offending lang

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// Install a lang library inline; the file-based loader isn't
/// always wired in test environments, but the inline library form
/// produces the same registered library.
fn install(rt: &mut Runtime, src: &str) {
    rt.eval_str("<lang-install>", src).unwrap();
}

// ---- reader is invoked and drives expansion ----

#[test]
fn reader_replaces_body_with_its_output() {
    let mut rt = Runtime::new();
    // `(lang fixed)` ignores the actual body text and returns a
    // fixed list of two top-level forms. The body's literal text
    // (`THIS-WOULD-BE-A-SYNTAX-ERROR`) never reaches the host
    // reader, so it must be the reader's output that executes.
    install(
        &mut rt,
        "(library (lang fixed)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             '((define x 41) (+ x 1))))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang fixed\n\
             THIS-WOULD-BE-A-SYNTAX-ERROR-IF-THE-HOST-READER-RAN",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn reader_receives_body_text_as_string() {
    let mut rt = Runtime::new();
    // The reader stores its argument on a side table so the test
    // can assert on what string it actually received. Returns a
    // trivial body so the file evaluates to a known value.
    install(
        &mut rt,
        "(library (lang capture)
           (export reader last-body)
           (import (rnrs))
           (define last-body 'unset)
           (define (reader body-str)
             (set! last-body body-str)
             '(42)))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang capture\n\
             hello world contents",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
    let captured = rt.lookup("last-body").unwrap();
    let s = disp(&rt, &captured);
    assert!(
        s.contains("hello world contents"),
        "reader didn't receive the body text; got: {}",
        s
    );
}

#[test]
fn reader_can_return_multiple_top_level_forms() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang multi)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             '((define a 1)
               (define b 2)
               (define c 3)
               (+ a b c))))",
    );
    let v = rt.eval_str("<t>", "#!lang multi\n").unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

// ---- passthrough reader: the lang library defers to host read ----

#[test]
fn passthrough_reader_parses_host_syntax() {
    let mut rt = Runtime::new();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/lang/passthrough-reader.scm");
    let src = std::fs::read_to_string(&path).unwrap();
    rt.eval_str("<passthrough-load>", &src).unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "#!lang passthrough-reader\n\
             (define n 7)\n\
             (* n n)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "49");
}

// ---- body sees the lang's *other* exports too ----

#[test]
fn body_sees_other_exports_of_the_lang() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang with-helper)
           (export reader helper)
           (import (rnrs))
           (define (helper n) (* n 10))
           (define (reader body-str)
             '((helper 5))))",
    );
    let v = rt.eval_str("<t>", "#!lang with-helper\n").unwrap();
    assert_eq!(disp(&rt, &v), "50");
}

// ---- no reader exported: behaviour matches MVP ----

#[test]
fn lang_without_reader_falls_back_to_host_reader() {
    let mut rt = Runtime::new();
    // A lang that only exports a marker. No `reader` → the host
    // reader parses the body; the marker is still visible because
    // the import still ran (Phase 3C MVP behaviour).
    install(
        &mut rt,
        "(library (lang marker-only)
           (export marker)
           (import (rnrs))
           (define marker 'present))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang marker-only\n\
             marker",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "present");
}

// ---- snapshot logic: pre-existing user `reader` not mistaken ----

#[test]
fn user_defined_reader_not_used_when_lang_does_not_export_one() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang marker-only-2)
           (export marker2)
           (import (rnrs))
           (define marker2 'visible))",
    );
    // The user defines a top-level `reader` BEFORE the #!lang
    // call. If the host mistakenly picked it up, the body
    // `marker2` would never be looked up — instead we'd try to
    // call user-reader on the body string and fail (it returns a
    // non-list / non-procedure thing). The expected behaviour:
    // host reader runs, body sees `marker2`.
    rt.eval_str("<setup>", "(define reader (lambda (s) 'oops))")
        .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "#!lang marker-only-2\n\
             marker2",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "visible");
}

#[test]
fn lang_reader_overrides_prior_user_reader() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<setup>",
        "(define reader (lambda (s) 'user-defined-result))",
    )
    .unwrap();
    install(
        &mut rt,
        "(library (lang overrides)
           (export reader)
           (import (rnrs))
           (define (reader body-str) '(99)))",
    );
    // The lang's exported `reader` replaces the user's prior one
    // when the import runs; the snapshot detection sees the
    // change and routes through the lang's reader.
    let v = rt.eval_str("<t>", "#!lang overrides\n").unwrap();
    assert_eq!(disp(&rt, &v), "99");
}

// ---- error paths ----

#[test]
fn reader_returning_non_list_is_diagnosed() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang bad-return)
           (export reader)
           (import (rnrs))
           (define (reader body-str) 42))",
    );
    let err = rt
        .eval_str("<t>", "#!lang bad-return\n")
        .expect_err("non-list return should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang bad-return)") && s.contains("non-datum"),
        "diagnostic should name the offending lang and 'non-datum'; got: {}",
        s
    );
}

#[test]
fn reader_returning_non_proper_list_is_diagnosed() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang improper)
           (export reader)
           (import (rnrs))
           (define (reader body-str) (cons '(define x 1) 'tail)))",
    );
    let err = rt
        .eval_str("<t>", "#!lang improper\n")
        .expect_err("improper list return should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang improper)"),
        "diagnostic should name the offending lang; got: {}",
        s
    );
}

#[test]
fn reader_raising_propagates_with_named_lang() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang raiser)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             (error 'reader \"reader bailed\")))",
    );
    let err = rt
        .eval_str("<t>", "#!lang raiser\n")
        .expect_err("raising reader should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang raiser)"),
        "diagnostic should name the offending lang; got: {}",
        s
    );
}

// ---- multi-call: each eval_str routes through a fresh reader lookup ----

#[test]
fn lang_switch_between_eval_str_calls() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang la)
           (export reader)
           (import (rnrs))
           (define (reader b) '(11)))",
    );
    // First eval_str uses the only declared `reader` so far.
    let a = rt.eval_str("<a>", "#!lang la\n").unwrap();
    assert_eq!(disp(&rt, &a), "11");
    // Install lb, which (re)defines the global `reader`. The
    // current cs-expand milestone splices library bodies as
    // top-level defines (no namespace isolation), so this
    // overwrites la's binding. That's a documented milestone
    // limitation rather than a feature of the custom-reader
    // pipeline; pinning the behaviour here so a future
    // namespace-isolation pass can refine the assertion.
    install(
        &mut rt,
        "(library (lang lb)
           (export reader)
           (import (rnrs))
           (define (reader b) '(22)))",
    );
    let b = rt.eval_str("<b>", "#!lang lb\n").unwrap();
    assert_eq!(disp(&rt, &b), "22");
}
