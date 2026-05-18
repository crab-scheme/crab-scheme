//! R6RS++ §12 (#118) Iter E — hygiene-tracking surface.
//!
//! Full R6RS hygiene with per-macro-call marks requires a
//! `Value::Identifier { name, mark }` variant. That migration
//! touches ~45 files (every `match Value::Symbol` site) and is
//! deferred to a post-1.0 SyntaxObject track — see the Iter E
//! status section of `docs/milestones/r6rs-extensions-118-plan.md`
//! for the architecture sketch.
//!
//! What Iter E ships in this session:
//! * `make-variable-transformer` builtin (R6RS §12.3) -- stub
//!   that returns the procedure unchanged so user code that
//!   calls it doesn't error out with "undefined".
//! * Sharpened doc comments on the identifier-comparison
//!   builtins explaining today-vs-future semantics.
//! * Tests pinning what works today: `bound-identifier=?`
//!   discriminates by interned-symbol name (so two distinct
//!   reader symbols compare unequal even if visually similar).
//! * Tests pinning the gap that requires SyntaxObject (the Iter A
//!   `bound_id_eq_distinguishes_marked_identifiers` test
//!   remains `#[ignore]`d with an updated doc comment).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- make-variable-transformer ----

#[test]
fn make_variable_transformer_returns_proc() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((vt (make-variable-transformer (lambda (x) x))))
               (procedure? vt))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn make_variable_transformer_rejects_non_procedure() {
    let mut rt = Runtime::new();
    assert!(rt
        .eval_str("<t>", "(make-variable-transformer 42)")
        .is_err());
    assert!(rt
        .eval_str("<t>", "(make-variable-transformer 'sym)")
        .is_err());
    assert!(rt.eval_str("<t>", "(make-variable-transformer)").is_err());
    assert!(rt
        .eval_str(
            "<t>",
            "(make-variable-transformer (lambda (x) x) (lambda (y) y))",
        )
        .is_err());
}

#[test]
fn make_variable_transformer_proc_remains_callable() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((vt (make-variable-transformer (lambda (x) (* x 2)))))
               (vt 21))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

// ---- bound-identifier=? semantics pinned ----

#[test]
fn distinct_user_symbols_compare_unequal() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'bar)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn same_user_symbol_compares_equal() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'foo)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_identifier_passes_through_syntax_case_pvar() {
    // A pvar-bound identifier compares equal to itself when
    // looked up at two different points in the same syntax-case
    // body.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'my-id ()
               (id (bound-identifier=? (syntax id) (syntax id))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_identifier_distinguishes_different_pvars() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(a b) ()
               ((x y) (bound-identifier=? (syntax x) (syntax y))))",
        )
        .unwrap();
    // x and y bind to different user symbols 'a and 'b.
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- identifier? + datum->syntax round-trip ----

#[test]
fn datum_to_syntax_then_identifier_predicate() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(identifier? (datum->syntax 'ctx 'introduced))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn syntax_to_datum_round_trips_identifier() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(syntax->datum (datum->syntax 'ctx 'foo))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "foo");
}
