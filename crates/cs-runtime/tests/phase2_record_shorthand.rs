//! R6RS++ Phase 2C — record-definition shorthand.
//!
//! `define-record` and `define-record-mutable` are syntax-rules
//! wrappers over `define-record-type`. They cover the common case
//! of "auto-named accessors for every field" and let users skip
//! the verbose `(fields (immutable ...) ...)` clauses.
//!
//! The expander accepts a new `(mutable FIELD)` two-element form
//! inside `(fields ...)` that auto-names the accessor/mutator;
//! that's what the mutable shorthand expands into.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_record() -> Runtime {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/record/record.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<record>", &src).expect("load record.scm");
    rt
}

// ---- define-record (immutable) ----

#[test]
fn define_record_generates_constructor_predicate_accessors() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record point (x y))
             (define p (make-point 3 4))
             (list (point? p) (point-x p) (point-y p))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t 3 4)");
}

#[test]
fn define_record_predicate_rejects_other_records() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record point (x y))
             (define-record circle (r))
             (list (point? (make-circle 5))
                   (circle? (make-point 1 2)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f)");
}

#[test]
fn define_record_no_fields_works() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record sentinel ())
             (list (sentinel? (make-sentinel))
                   (sentinel? 42))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f)");
}

#[test]
fn define_record_immutable_has_no_setter() {
    let mut rt = load_record();
    rt.eval_str(
        "<t>",
        "(define-record point (x y))
         (define p (make-point 1 2))",
    )
    .unwrap();
    // No set-point-x! mutator should exist for immutable record.
    let err = rt
        .eval_str("<t>", "(set-point-x! p 99)")
        .expect_err("immutable: no mutator");
    let s = format!("{}", err);
    assert!(
        s.contains("undefined") || s.contains("unbound") || s.contains("not found"),
        "got: {}",
        s
    );
}

// ---- define-record-mutable ----

#[test]
fn define_record_mutable_auto_names_accessor_and_mutator() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record-mutable counter (n))
             (define c (make-counter 0))
             (set-counter-n! c 42)
             (counter-n c)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn define_record_mutable_multiple_fields() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record-mutable box (lo hi))
             (define b (make-box 0 100))
             (set-box-lo! b 10)
             (set-box-hi! b 90)
             (list (box-lo b) (box-hi b))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(10 90)");
}

#[test]
fn define_record_mutable_predicate_still_works() {
    let mut rt = load_record();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record-mutable counter (n))
             (list (counter? (make-counter 0))
                   (counter? 'not-a-record))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f)");
}

// ---- expander-level (mutable FIELD) shorthand used directly ----

#[test]
fn expander_mutable_shorthand_in_define_record_type() {
    // Skip the lib macro and use the underlying primitive directly
    // to confirm the (mutable FIELD) shorthand is recognized by
    // the expander even outside lib/record/record.scm.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define-record-type cell (fields (mutable n)))
             (define c (make-cell 1))
             (set-cell-n! c 99)
             (cell-n c)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "99");
}

// ---- composition with contracts ----

#[test]
fn record_predicate_usable_as_contract_domain() {
    let mut rt = load_record();
    rt.eval_str("<t>", "(define-record point (x y))").unwrap();
    // Load contract library too.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&path).unwrap();
    rt.eval_str("<contract>", &src).unwrap();

    let v = rt
        .eval_str(
            "<t>",
            "(define/contract origin-distance (-> point? number?)
               (lambda (p) (+ (point-x p) (point-y p))))
             (origin-distance (make-point 3 4))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "7");
    let err = rt
        .eval_str("<t>", "(origin-distance 'not-a-point)")
        .expect_err("contract should fire");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}
