//! R6RS++ Phase 2B.6 — define/contract and provide/contract macros.
//!
//! `define/contract` wraps a single definition's value in
//! apply-contract. `provide/contract` rebinds one or more already-
//! defined names to wrapped versions. Because the rebound name is
//! what any enclosing library would `(export ...)`, importers see
//! the wrapped procedure transparently.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_contract() -> Runtime {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<contract>", &src).expect("load contract.scm");
    rt
}

// ---- define/contract ----

#[test]
fn define_contract_binds_wrapped_procedure() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/contract double (-> number? number?)
               (lambda (x) (* x 2)))
             (double 21)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn define_contract_violation_caught_via_guard() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/contract double (-> number? number?)
               (lambda (x) (* x 2)))
             (guard (c ((contract-violation? c)
                        (contract-violation-target c)))
               (double 'oops))",
        )
        .unwrap();
    // Blame label is the bound name `double`.
    assert_eq!(disp(&rt, &v), "double");
}

#[test]
fn define_contract_blame_uses_definition_name() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/contract sq (-> number? number?)
               (lambda (x) (* x x)))
             (guard (c ((contract-violation? c)
                        (list (contract-violation-target c)
                              (contract-violation-source c))))
               (sq \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(sq caller)");
}

// ---- provide/contract ----

#[test]
fn provide_contract_wraps_single_binding() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define (inc x) (+ x 1))
             (provide/contract (inc (-> number? number?)))
             (inc 41)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn provide_contract_rebound_binding_is_contracted() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define (inc x) (+ x 1))
             (provide/contract (inc (-> number? number?)))
             (guard (c ((contract-violation? c)
                        (contract-violation-target c)))
               (inc 'oops))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "inc");
}

#[test]
fn provide_contract_wraps_multiple_bindings() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define (inc x) (+ x 1))
             (define (dbl x) (* x 2))
             (define (sqr x) (* x x))
             (provide/contract
               (inc (-> number? number?))
               (dbl (-> number? number?))
               (sqr (-> number? number?)))
             (list (inc 10) (dbl 10) (sqr 10))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(11 20 100)");
}

#[test]
fn provide_contract_each_binding_blamed_separately() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define (inc x) (+ x 1))
             (define (dbl x) (* x 2))
             (provide/contract
               (inc (-> number? number?))
               (dbl (-> number? number?)))
             (list
               (guard (c ((contract-violation? c)
                          (contract-violation-target c)))
                 (inc 'a))
               (guard (c ((contract-violation? c)
                          (contract-violation-target c)))
                 (dbl 'b)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(inc dbl)");
}

#[test]
fn provide_contract_empty_form_is_noop() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define x 42)
             (provide/contract)
             x",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

// ---- composition with combinators ----

#[test]
fn define_contract_with_combinator_domain() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define/contract describe
               (-> (or/c number? string?) string?)
               (lambda (x) (if (number? x) \"num\" \"str\")))
             (list (describe 42) (describe \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(num str)");
    let err = rt
        .eval_str("<t>", "(describe 'sym)")
        .expect_err("symbol matches neither");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}
