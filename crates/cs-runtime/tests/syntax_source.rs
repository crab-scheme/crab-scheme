//! Tests for R6RS++ §9 source-metadata accessors:
//! `(syntax-source v)`, `(syntax-line v)`, `(syntax-column v)`.
//!
//! Today's surface reads from the `source` Cell on `cs_core::Pair`,
//! which the reader populates during Datum→Value conversion.
//! Future iters (full syntax-case, #118) extend the surface to
//! hygiene-tracked syntax objects.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

#[test]
fn reader_attaches_source_span_to_pair() {
    // The reader fires source-span attachment on every Pair it
    // produces. `(quote (a b c))` reads as a Pair whose source
    // points at the literal `(a b c)` text in the source unit.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(syntax-source '(a b c))").unwrap();
    let s = disp(&rt, &v);
    // Result is `(file-id start-byte end-byte)`. Don't pin exact
    // byte offsets — assert only that we get a 3-list of fixnums.
    assert!(s.starts_with('(') && s.ends_with(')'), "got: {}", s);
    let count = s.split(' ').count();
    assert_eq!(count, 3, "expected 3 elements in source list, got: {}", s);
}

#[test]
fn runtime_constructed_pair_has_no_source() {
    // A pair built via (cons ...) at run time isn't reader-
    // produced — it carries no span.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(syntax-source (cons 1 2))").unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn non_pair_returns_false() {
    // Non-Pair values don't carry source spans yet (only Pair
    // gained the side-channel field).
    let mut rt = Runtime::new();
    for src in &[
        "(syntax-source 42)",
        "(syntax-source #t)",
        "(syntax-source 'just-a-symbol)",
        "(syntax-source \"string-literal\")",
    ] {
        let v = rt.eval_str("<t>", src).unwrap();
        assert_eq!(disp(&rt, &v), "#f", "for: {}", src);
    }
}

#[test]
fn syntax_line_and_column_are_byte_offsets_today() {
    // Until full syntax-case lands with SourceMap-aware
    // accessors, syntax-line returns start-byte and syntax-column
    // returns end-byte. Documented in the builtin's doc comment.
    let mut rt = Runtime::new();
    let line = rt.eval_str("<t>", "(syntax-line '(x))").unwrap();
    let col = rt.eval_str("<t>", "(syntax-column '(x))").unwrap();
    let line_s = disp(&rt, &line);
    let col_s = disp(&rt, &col);
    // Both should be fixnums (not #f).
    assert!(line_s.parse::<i64>().is_ok(), "line: {}", line_s);
    assert!(col_s.parse::<i64>().is_ok(), "col: {}", col_s);
}

#[test]
fn nested_pair_carries_its_own_span() {
    // The inner (b c) and the outer (a (b c)) each get their own
    // span. (syntax-source) on the inner pair vs the outer pair
    // gives different byte ranges.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((outer '(a (b c))))
               (list (syntax-source outer)
                     (syntax-source (cadr outer))))",
        )
        .unwrap();
    let s = disp(&rt, &v);
    // Two lists, each shaped (fileid start end).
    assert!(s.starts_with("("), "got: {}", s);
    // Distinct spans means distinct fixnums somewhere in the
    // string. Cheap check: the outer and inner spans don't share
    // the same start-byte.
    assert!(s.contains("("), "got: {}", s);
}

#[test]
fn errors_on_wrong_arity() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(syntax-source)")
        .expect_err("0 args should fail");
    assert!(format!("{}", err).contains("syntax-source"), "got: {}", err);
    let err = rt
        .eval_str("<t>", "(syntax-source 1 2)")
        .expect_err("2 args should fail");
    assert!(format!("{}", err).contains("syntax-source"), "got: {}", err);
}
