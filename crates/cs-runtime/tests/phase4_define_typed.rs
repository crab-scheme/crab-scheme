//! Phase 4 iter 4 — `define/typed` end-to-end.
//!
//! Brings the Phase 4 type→contract substrate to user-facing code.
//! `(define/typed NAME TYPE-ANN EXPR)` wraps EXPR with a contract
//! derived from TYPE-ANN using cs-typer's annotation syntax
//! (Fixnum, Flonum, (-> ...), (U ...), (Listof ...), etc.).
//!
//! Implementation lives in lib/contract/typed.scm; tests here
//! exercise the surface from a runtime perspective.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_typed_contract() -> Runtime {
    let contract_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let typed_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/typed.scm");
    let mut rt = Runtime::new();
    let contract_src = std::fs::read_to_string(&contract_path).unwrap();
    rt.eval_str("<contract>", &contract_src).unwrap();
    let typed_src = std::fs::read_to_string(&typed_path).unwrap();
    rt.eval_str("<typed>", &typed_src).unwrap();
    rt
}

// ---- atomic type annotations ----

#[test]
fn define_typed_with_fixnum_dom_and_range() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed sq (-> Fixnum Fixnum)
               (lambda (x) (* x x)))
             (sq 5)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "25");
}

#[test]
fn define_typed_violation_caught_via_guard() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed sq (-> Fixnum Fixnum)
               (lambda (x) (* x x)))
             (guard (c ((contract-violation? c)
                        (contract-violation-target c)))
               (sq 'not-an-int))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "sq");
}

#[test]
fn define_typed_with_mixed_atomic_types() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed describe (-> Fixnum String)
               (lambda (n) (if (> n 0) \"pos\" \"non-pos\")))
             (list (describe 5) (describe -1))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(pos non-pos)");
}

// ---- Any / Never ----

#[test]
fn any_type_admits_everything() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed id (-> Any Any)
               (lambda (x) x))
             (list (id 1) (id 'sym) (id \"hi\") (id '()))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 sym hi ())");
}

// ---- Union ----

#[test]
fn union_type_admits_alternatives() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed describe (-> (U Fixnum String) String)
               (lambda (x) (if (number? x) \"num\" \"str\")))
             (list (describe 5) (describe \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(num str)");
    let err = rt
        .eval_str("<t>", "(describe 'symbol-not-in-union)")
        .expect_err("symbol violates union");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- Listof / Vectorof ----

#[test]
fn listof_type_checks_every_element() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed sum-list (-> (Listof Fixnum) Fixnum)
               (lambda (xs) (apply + xs)))
             (sum-list (list 1 2 3 4))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "10");
    let err = rt
        .eval_str("<t>", "(sum-list (list 1 'bad 3))")
        .expect_err("non-int in Listof Fixnum violates");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

#[test]
fn vectorof_type_checks_every_element() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed first-str (-> (Vectorof String) String)
               (lambda (v) (vector-ref v 0)))
             (first-str (vector \"a\" \"b\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "a");
}

// ---- Higher-order arrows ----

#[test]
fn higher_order_arrow_type_works() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed apply-fn (-> (-> Fixnum Fixnum) Fixnum Fixnum)
               (lambda (f x) (f x)))
             (apply-fn (lambda (n) (* n 3)) 5)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "15");
}

// ---- Variadic tail ----

#[test]
fn variadic_tail_arrow_type_works() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed sum-all (->* () Fixnum Fixnum)
               (lambda args (apply + args)))
             (list (sum-all) (sum-all 1 2 3 4 5))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(0 15)");
}

#[test]
fn variadic_tail_with_mandatory_works() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed labeled-sum (->* (String) Fixnum Fixnum)
               (lambda (label . nums) (apply + nums)))
             (labeled-sum \"total\" 10 20 30)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "60");
}

// ---- Bare type variable lowers to Any ----

#[test]
fn bare_type_var_lowers_to_any() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/typed id (-> T T)
               (lambda (x) x))
             (list (id 1) (id 'sym))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 sym)");
}

// ---- Error cases ----

#[test]
fn malformed_listof_errors() {
    let mut rt = load_typed_contract();
    let err = rt
        .eval_str(
            "<t>",
            "(define/typed bad (-> (Listof Fixnum Fixnum) Fixnum)
               (lambda (xs) (car xs)))",
        )
        .expect_err("Listof takes one arg");
    let s = format!("{}", err);
    assert!(s.contains("Listof"), "got: {}", s);
}

#[test]
fn arrow_with_no_args_errors() {
    let mut rt = load_typed_contract();
    let err = rt
        .eval_str(
            "<t>",
            "(define/typed bad (->)
               (lambda () 42))",
        )
        .expect_err("(->) with no domains AND no range");
    let s = format!("{}", err);
    assert!(s.contains("(->"), "got: {}", s);
}

// `define/typed`'s `name` argument now carries an `:id` syntax class
// (R6RS++ #32 follow-up), so a non-identifier name is rejected at expand
// time -- the macro body is a `(define name ...)`, which is exactly the
// definition-bodied case that could not be class-validated before.
#[test]
fn define_typed_rejects_non_identifier_name() {
    let mut rt = load_typed_contract();
    let err = rt
        .eval_str("<t>", "(define/typed 5 (-> Fixnum Fixnum) (lambda (x) x))")
        .expect_err("a number is not a valid name");
    let s = format!("{}", err);
    assert!(s.contains("expected id"), "got: {}", s);
}
