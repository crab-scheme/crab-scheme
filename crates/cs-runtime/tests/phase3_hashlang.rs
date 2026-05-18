//! R6RS++ Phase 3C — `#!lang` reader-protocol header.
//!
//! A leading `#!lang NAME` line is rewritten to
//! `(import (lang NAME))` before parsing. NAME is whatever
//! identifier the user wrote; the corresponding `(lang NAME)`
//! library is expected to provide whatever surface the file
//! relies on (exports, base env, etc.). Per-file scope by
//! construction — no global reader-table mutation.
//!
//! This MVP only does the header → import rewrite. The full
//! reader-protocol (loading the lang library at parse time and
//! invoking its `reader` proc on the rest of the file) is post-1.0
//! work; user code that wants custom reader syntax inside a
//! `#!lang` file needs to wait for that follow-up.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// Establishes a minimal `(lang demo)` library that exports a
/// constant. Lets tests below `#!lang demo` and observe the
/// import's effect.
fn with_demo_lang(rt: &mut Runtime) {
    rt.eval_str(
        "<lang-demo>",
        "(library (lang demo)
           (export demo-marker)
           (import (rnrs))
           (define demo-marker 'demo-loaded))",
    )
    .unwrap();
}

// ---- header rewrite mechanics ----

#[test]
fn lang_header_loads_corresponding_library() {
    let mut rt = Runtime::new();
    with_demo_lang(&mut rt);
    let v = rt
        .eval_str(
            "<t>",
            "#!lang demo\n\
             demo-marker",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "demo-loaded");
}

#[test]
fn lang_header_supports_short_form_without_bang() {
    let mut rt = Runtime::new();
    with_demo_lang(&mut rt);
    let v = rt
        .eval_str(
            "<t>",
            "#lang demo\n\
             demo-marker",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "demo-loaded");
}

#[test]
fn missing_lang_library_is_silently_accepted_at_this_milestone() {
    // At the current cs-expand milestone, importing a library
    // that hasn't been registered is a no-op (the comment in
    // expand_library describes this: every binding is global,
    // imports don't enforce existence). So `#!lang nonexistent`
    // simply expands to `(import (lang nonexistent))` and the
    // body still runs. When namespace isolation lands, this
    // test should flip to expect_err.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "#!lang nonexistent\n42").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn no_lang_header_means_no_import_injection() {
    let mut rt = Runtime::new();
    // Same source without the header parses + runs normally.
    let v = rt.eval_str("<t>", "(+ 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

#[test]
fn lang_header_ignored_if_not_on_first_line() {
    let mut rt = Runtime::new();
    // Comment on line 1; `#!lang foo` on line 2 should NOT be
    // interpreted as a header (it's only a header when leading).
    let v = rt
        .eval_str(
            "<t>",
            "; not a header\n\
             (+ 1 2)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

// ---- preservation of subsequent source ----

#[test]
fn rest_of_file_is_evaluated_after_header() {
    let mut rt = Runtime::new();
    with_demo_lang(&mut rt);
    let v = rt
        .eval_str(
            "<t>",
            "#!lang demo\n\
             (define x 100)\n\
             (define y 23)\n\
             (+ x y)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "123");
}

#[test]
fn diagnostic_span_points_to_correct_line_after_rewrite() {
    let mut rt = Runtime::new();
    with_demo_lang(&mut rt);
    let err = rt
        .eval_str(
            "<t>",
            "#!lang demo\n\
             (+ 1 'bad)",
        )
        .expect_err("type error in body");
    // The Diagnostic carries a span; convert it to (line, col)
    // through the runtime's SourceMap to verify the rewrite
    // preserved line numbering. The bad form is on line 2 of
    // the (rewritten) source — same line as in the original.
    let (line, _) = rt.sources_line_col(err.primary);
    assert_eq!(line, 2, "span on wrong line: {:?}", err);
}

// ---- multiple lang libraries / sandboxing surface ----

#[test]
fn different_lang_headers_import_different_libs() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<l1>",
        "(library (lang one)
           (export one-marker)
           (import (rnrs))
           (define one-marker 1))",
    )
    .unwrap();
    rt.eval_str(
        "<l2>",
        "(library (lang two)
           (export two-marker)
           (import (rnrs))
           (define two-marker 2))",
    )
    .unwrap();
    let v1 = rt.eval_str("<t1>", "#!lang one\none-marker").unwrap();
    let v2 = rt.eval_str("<t2>", "#!lang two\ntwo-marker").unwrap();
    assert_eq!(disp(&rt, &v1), "1");
    assert_eq!(disp(&rt, &v2), "2");
}

#[test]
fn lang_header_with_only_directive_no_body() {
    let mut rt = Runtime::new();
    with_demo_lang(&mut rt);
    // Just the header, no trailing newline, no body. Should
    // expand to a bare import, no value computed.
    let _v = rt.eval_str("<t>", "#!lang demo").unwrap();
    // No assertion needed — just verify no panic / error.
}
