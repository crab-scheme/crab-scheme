//! R6RS++ Phase 2A.1 — `define-syntax-parser` + built-in
//! syntax classes (id / expr / number / string).
//!
//! Iter 1 surface:
//! - `(define-syntax-parser name (pat body ...) ...)`
//! - Pattern symbols may carry `:class` annotations: `id:id`,
//!   `n:number`, `s:string`, `e:expr` (the last is no-op-class).
//! - On class mismatch the expanded code raises an `error` at
//!   RUNTIME (the macro expansion site reports the bad arg).
//!   Phase 2A.4 lifts this to expand-time pinpointing.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- baseline: define-syntax-parser without annotations ----

#[test]
fn parser_without_classes_acts_like_syntax_rules() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser my-double
          ((_ x) (* 2 x)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(my-double 21)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn parser_multi_clause_dispatch() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser shape
          ((_ (a b)) 'pair)
          ((_ ())    'empty)
          ((_ x)     'atom))
        "#,
    )
    .unwrap();
    let v = rt
        .eval_str("<t>", "(list (shape (1 2)) (shape ()) (shape 99))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "(pair empty atom)");
}

// ---- class annotations ----

#[test]
fn id_class_accepts_symbol() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser id-only
          ((_ x:id) (list 'got (quote x))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(id-only foo)").unwrap();
    assert_eq!(disp(&rt, &v), "(got foo)");
}

#[test]
fn id_class_rejects_non_identifier_at_runtime() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser id-only
          ((_ x:id) 'ok))
        "#,
    )
    .unwrap();
    // Passing a number where an identifier is expected -- the
    // class check fires at runtime of the expansion.
    let err = rt
        .eval_str("<t>", "(id-only 42)")
        .expect_err("class violation should error");
    let s = format!("{}", err);
    assert!(
        s.contains("expected id") || s.contains("id-only"),
        "got: {}",
        s
    );
}

#[test]
fn number_class_accepts_number() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser num-double
          ((_ n:number) (* 2 n)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(num-double 7)").unwrap();
    assert_eq!(disp(&rt, &v), "14");
}

#[test]
fn number_class_rejects_non_number() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser num-double
          ((_ n:number) (* 2 n)))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(num-double 'not-a-num)")
        .expect_err("non-number should fail");
    let s = format!("{}", err);
    assert!(s.contains("expected number"), "got: {}", s);
}

#[test]
fn string_class_accepts_string() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser quote-str
          ((_ s:string) s))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", r#"(quote-str "hello")"#).unwrap();
    assert_eq!(disp(&rt, &v), "hello");
}

#[test]
fn expr_class_accepts_anything() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser identity-form
          ((_ e:expr) e))
        "#,
    )
    .unwrap();
    // Number, symbol, list, string -- all accepted.
    let v = rt.eval_str("<t>", "(identity-form 42)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
    let v = rt.eval_str("<t>", "(identity-form (+ 1 2))").unwrap();
    assert_eq!(disp(&rt, &v), "3");
    let v = rt.eval_str("<t>", r#"(identity-form "str")"#).unwrap();
    assert_eq!(disp(&rt, &v), "str");
}

// ---- multiple class annotations in one pattern ----

#[test]
fn multiple_class_annotations() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser tagged
          ((_ name:id count:number)
           (list (quote name) count)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(tagged widget 5)").unwrap();
    assert_eq!(disp(&rt, &v), "(widget 5)");
}

#[test]
fn multiple_class_annotations_check_order() {
    // First failing check fires its error first.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser tagged
          ((_ name:id count:number) 'ok))
        "#,
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(tagged 42 \"not-num\")")
        .expect_err("name fails first");
    let s = format!("{}", err);
    assert!(s.contains("expected id"), "got: {}", s);
}

// ---- error class for unknown class names ----

#[test]
fn unknown_class_name_fails_at_macro_definition() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            r#"
        (define-syntax-parser bad
          ((_ x:bogus) 'never))
        "#,
        )
        .expect_err("unknown class should fail to define");
    let s = format!("{}", err);
    assert!(
        s.contains("unknown syntax class") || s.contains("bogus"),
        "got: {}",
        s
    );
}

// ---- composition with ellipsis ----

#[test]
fn parser_supports_ellipsis_in_pattern() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax-parser my-list
          ((_ x ...) (list x ...)))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(my-list 1 2 3 4)").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3 4)");
}
