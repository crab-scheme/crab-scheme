//! R6RS++ Phase 1.5 Iter F — `datum->syntax` propagates the
//! context identifier's mark.
//!
//! `(datum->syntax template-id datum)` walks `datum` and
//! converts every bare `Symbol` leaf to a `Value::Identifier`
//! carrying `template-id`'s mark. Existing `Identifier` leaves
//! pass through unchanged. Non-identifier atoms pass through.
//! Pairs and vectors recurse.
//!
//! The inverse operation is `syntax->datum`, which strips
//! identifiers back to symbols (extended in this iter to recurse
//! into compound structures so round-trips compose).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- leaf stamping ----

#[test]
fn datum_to_syntax_wraps_symbol_with_zero_mark_from_symbol_context() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx 'introduced)))
               (and (identifier? stx)
                    (not (symbol? stx))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_inherits_mark_from_identifier_context() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((ctx (make-identifier 'tmpl 99)))
               (let ((stx (datum->syntax ctx 'introduced)))
                 (bound-identifier=? stx (make-identifier 'introduced 99))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_non_identifier_pass_through() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(datum->syntax 'ctx 42)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
    let v = rt.eval_str("<t>", "(datum->syntax 'ctx #t)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
    let v = rt.eval_str("<t>", "(datum->syntax 'ctx \"str\")").unwrap();
    assert_eq!(disp(&rt, &v), "str");
}

// ---- compound recursion ----

#[test]
fn datum_to_syntax_recurses_into_pair() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((ctx (make-identifier 'tmpl 7)))
               (let ((stx (datum->syntax ctx '(a b c))))
                 (and (identifier? (car stx))
                      (identifier? (cadr stx))
                      (identifier? (caddr stx)))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_recursive_marks_inherit_consistently() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((ctx (make-identifier 'tmpl 13)))
               (let ((stx (datum->syntax ctx '(a b))))
                 (bound-identifier=? (car stx)
                                     (make-identifier 'a 13))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_pair_with_mixed_content() {
    // Symbols become identifiers, numbers stay as numbers.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx '(name 42 \"str\"))))
               (list (identifier? (car stx))
                     (number? (cadr stx))
                     (string? (caddr stx))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t)");
}

#[test]
fn datum_to_syntax_nested_pair() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx '((a b) (c d)))))
               (identifier? (caar stx)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_existing_identifier_passes_through() {
    // If the datum already contains an Identifier value, it
    // keeps its original mark (the user explicitly chose it);
    // datum->syntax doesn't re-stamp.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((existing (make-identifier 'pre-marked 77)))
               (let ((stx (datum->syntax 'ctx (list existing))))
                 (bound-identifier=? (car stx) existing)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- syntax->datum inverts datum->syntax (round-trip) ----

#[test]
fn syntax_to_datum_strips_compound_structure() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx '(a b c))))
               (syntax->datum stx))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(a b c)");
}

#[test]
fn round_trip_preserves_datum_shape() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((d '(a (b 42) c)))
               (equal? d (syntax->datum (datum->syntax 'ctx d))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn syntax_to_datum_strips_nested_identifiers() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx '((a 1) (b 2)))))
               (let ((stripped (syntax->datum stx)))
                 (symbol? (caar stripped))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- composition with syntax-case ----

#[test]
fn datum_to_syntax_with_syntax_case_template_context() {
    // Use a template-id from a syntax-case to stamp a datum.
    // The introduced identifier shares the template's mark.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (let ((tmpl (syntax tmpl-id)))
                    (bound-identifier=? (datum->syntax tmpl 'foo)
                                        (datum->syntax tmpl 'foo)))))",
        )
        .unwrap();
    // Both call sites of datum->syntax inherit the same mark
    // from `tmpl`, so the two introduced identifiers compare
    // equal.
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn datum_to_syntax_with_distinct_template_marks_differ() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((a (syntax-case 'whatever () (_ (syntax tmpl))))
                   (b (syntax-case 'whatever () (_ (syntax tmpl)))))
               (bound-identifier=? (datum->syntax a 'introduced)
                                   (datum->syntax b 'introduced)))",
        )
        .unwrap();
    // Two distinct syntax-case forms produce identifiers with
    // distinct marks; using them as template-ids transfers the
    // distinction to the datum->syntax results.
    assert_eq!(disp(&rt, &v), "#f");
}
