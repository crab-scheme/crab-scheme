//! R6RS++ Phase 4 — `#!lang` reader span threading (issue #72).
//!
//! A reader proc that wraps a returned form with
//! `(syntax-datum d start end)` causes the bridge in
//! `crates/cs-runtime/src/lang_reader.rs` to attach a `Span`
//! pointing at the named byte range of the body file, instead of
//! collapsing every reader-produced datum to byte 0. Diagnostics
//! about reader-produced forms then point at the form's logical
//! location.
//!
//! Coverage:
//! - the `syntax-datum` constructor + `syntax-datum?` predicate
//!   round-trip
//! - a reader emitting `syntax-datum`-wrapped forms produces
//!   diagnostics whose span line/col reflects the wrapped offset
//! - non-wrapped reader output keeps today's coarse anchor
//!   (backward compatible)
//! - nested wrappers re-anchor only the inner subtree
//! - argument validation: 3 args required, non-neg integers, end
//!   >= start

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn install(rt: &mut Runtime, src: &str) {
    rt.eval_str("<lang-install>", src).unwrap();
}

// ---- constructor + predicate ----

#[test]
fn syntax_datum_constructor_returns_a_record_recognised_by_predicate() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax-datum 'foo 3 6)")
        .expect("syntax-datum should succeed with valid args");
    let pred = rt
        .eval_str("<t2>", "(syntax-datum? (syntax-datum 'foo 3 6))")
        .unwrap();
    assert_eq!(disp(&rt, &pred), "#t");
    // Plain datums are NOT syntax-datum records.
    let plain = rt.eval_str("<t3>", "(syntax-datum? 'foo)").unwrap();
    assert_eq!(disp(&rt, &plain), "#f");
    // The record shape is the documented tagged vector — its
    // first slot is the sentinel string. Verify by ref.
    let s = disp(&rt, &v);
    assert!(
        s.contains("__syntax-datum__"),
        "record shape should include the sentinel tag; got: {}",
        s
    );
}

#[test]
fn syntax_datum_arity_and_type_checks() {
    let mut rt = Runtime::new();
    // Wrong arity.
    let e1 = rt
        .eval_str("<t>", "(syntax-datum 'foo 0)")
        .expect_err("arity");
    assert!(
        format!("{}", e1).contains("syntax-datum"),
        "diag should name syntax-datum: {}",
        e1
    );
    // Non-integer start.
    let e2 = rt
        .eval_str("<t>", "(syntax-datum 'foo \"bad\" 5)")
        .expect_err("non-int");
    assert!(format!("{}", e2).contains("syntax-datum"));
    // Negative start.
    let e3 = rt
        .eval_str("<t>", "(syntax-datum 'foo -1 5)")
        .expect_err("neg");
    assert!(format!("{}", e3).contains("syntax-datum"));
    // end < start.
    let e4 = rt
        .eval_str("<t>", "(syntax-datum 'foo 8 4)")
        .expect_err("order");
    assert!(format!("{}", e4).contains("syntax-datum"));
}

// ---- span actually rides through to the diagnostic ----

#[test]
fn wrapped_reader_output_reports_diagnostic_at_wrapped_offset() {
    let mut rt = Runtime::new();
    // The reader returns a single form that references an unbound
    // identifier `nope-undefined`. By wrapping the symbol with
    // syntax-datum the reader claims it lives at byte range
    // 12..27 of the body file. The diagnostic's span line/col
    // should reflect that wrapped offset (i.e. NOT line 1 col 1,
    // which is where every unwrapped reader output collapses).
    install(
        &mut rt,
        "(library (lang positioner)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             (list (list (syntax-datum 'nope-undefined 12 27)))))",
    );
    // Body has enough bytes to give 12..27 a real position. We
    // arrange so byte 12 is on line 2 (after the newline at
    // position N), so the resolved line is > 1.
    let body = "\n0123456789ab nope-undefined-here\n";
    let src = format!("#!lang positioner{}", body);
    let err = rt
        .eval_str("<t>", &src)
        .expect_err("unbound symbol from reader output");
    let (line, _col) = rt.sources_line_col(err.primary);
    // The span we asked for starts at byte 12 of `body`, which
    // is on line 2 (the newline at body[0] ends line 1). If span
    // threading is broken, every reader datum would collapse to
    // line 1.
    assert_eq!(
        line, 2,
        "wrapped span should resolve to line 2; got line {} (full err: {:?})",
        line, err
    );
}

#[test]
fn unwrapped_reader_output_collapses_to_anchor() {
    let mut rt = Runtime::new();
    // Same shape but no syntax-datum wrap; the span should
    // collapse to byte 0 (line 1) as documented.
    install(
        &mut rt,
        "(library (lang unwrapped)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             (list (list 'nope-undefined-2))))",
    );
    let body = "\n0123456789ab nope-undefined-2-here\n";
    let src = format!("#!lang unwrapped{}", body);
    let err = rt
        .eval_str("<t>", &src)
        .expect_err("unbound symbol from reader output");
    let (line, _col) = rt.sources_line_col(err.primary);
    assert_eq!(
        line, 1,
        "without syntax-datum, anchor collapses to line 1; got line {} (err: {:?})",
        line, err
    );
}

// ---- nesting ----

#[test]
fn nested_syntax_datum_re_anchors_only_inner_subtree() {
    let mut rt = Runtime::new();
    // Outer form sits at body bytes 5..50; the inner offending
    // symbol sits at 18..32. The diagnostic should point at the
    // inner range, not the outer.
    install(
        &mut rt,
        "(library (lang nested)
           (export reader)
           (import (rnrs))
           (define (reader body-str)
             (list (syntax-datum
                     (list (syntax-datum 'inner-undef 18 32))
                     5 50))))",
    );
    // Body needs >=50 bytes total.
    let body = "\n0123456789abcdef inner-undef-bad-token continues..\n";
    let src = format!("#!lang nested{}", body);
    let err = rt
        .eval_str("<t>", &src)
        .expect_err("unbound symbol from reader output");
    // The reported span should be the inner one (18..32),
    // which on `body` (with `\n` at byte 0) lands on line 2.
    assert_eq!(
        err.primary.start, 18,
        "inner syntax-datum should win; got primary={:?}",
        err.primary
    );
    assert_eq!(err.primary.end, 32);
}
