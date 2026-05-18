//! R6RS++ Phase 2B.2 — `(-> dom rng)` contract + apply-contract
//! wrapper.
//!
//! Layered library at `lib/contract/contract.scm`. Each call to
//! a wrapped procedure checks args against dom-pred and result
//! against rng-pred; violation raises a &contract condition
//! (Phase 2D infra).

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

// ---- contract construction ----

#[test]
fn contract_predicate() {
    let mut rt = load_contract();
    let v = rt
        .eval_str("<t>", "(contract? (-> number? number?))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
    let v = rt.eval_str("<t>", "(contract? 42)").unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- apply-contract on simple procedures ----

#[test]
fn wrapped_proc_passes_valid_args() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define guarded (apply-contract (-> number? number?)
                                              (lambda (x) (* x 2))
                                              'double))
             (guarded 21)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn wrapped_proc_rejects_bad_argument() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> number? number?)
                                          (lambda (x) (* x 2))
                                          'double))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(guarded 'not-a-number)")
        .expect_err("non-number arg should fail");
    let s = format!("{}", err);
    // Contract violation surfaces as &contract; with-error-handler
    // catches it.
    assert!(
        s.contains("contract") || s.contains("&contract"),
        "got: {}",
        s
    );
}

#[test]
fn wrapped_proc_rejects_bad_return() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> number? string?)
                                          (lambda (x) (* x 2))
                                          'wrong-return))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(guarded 5)")
        .expect_err("range mismatch should fail (returns number, expected string)");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- catching via guard ----

#[test]
fn contract_violation_caught_via_guard() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> number? number?)
                                          (lambda (x) (* x 2))
                                          'double))",
    )
    .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((contract-violation? c)
                        (list 'caught
                              (contract-violation-source c)
                              (contract-violation-target c)
                              (contract-violation-value c))))
               (guarded 'bad))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(caught caller double bad)");
}

#[test]
fn contract_violation_range_blame_is_callee() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> number? string?)
                                          (lambda (x) (* x 2))
                                          'wrong-return))",
    )
    .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((contract-violation? c)
                        (contract-violation-source c)))
               (guarded 5))",
        )
        .unwrap();
    // Range violation -> callee blamed (the proc returned wrong type).
    assert_eq!(disp(&rt, &v), "callee");
}

// ---- multi-arg checks ----

#[test]
fn multi_arg_proc_checks_each_arg() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> number? number?)
                                          (lambda args (apply + args))
                                          'sum))",
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(guarded 1 2 3 4)").unwrap();
    assert_eq!(disp(&rt, &v), "10");
    let err = rt
        .eval_str("<t>", "(guarded 1 'oops 3)")
        .expect_err("middle bad arg");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- error cases for apply-contract ----

#[test]
fn apply_contract_rejects_non_contract() {
    let mut rt = load_contract();
    let err = rt
        .eval_str(
            "<t>",
            "(apply-contract 'not-a-contract (lambda (x) x) 'name)",
        )
        .expect_err("non-contract first arg");
    let s = format!("{}", err);
    assert!(s.contains("not a contract"), "got: {}", s);
}

#[test]
fn apply_contract_rejects_non_procedure() {
    let mut rt = load_contract();
    let err = rt
        .eval_str("<t>", "(apply-contract (-> number? number?) 42 'name)")
        .expect_err("non-procedure second arg");
    let s = format!("{}", err);
    assert!(s.contains("not a procedure"), "got: {}", s);
}

// ---- contract composition (real-world feel) ----

#[test]
fn contract_chain_compose() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define add1
               (apply-contract (-> number? number?)
                               (lambda (x) (+ x 1))
                               'add1))
             (define double
               (apply-contract (-> number? number?)
                               (lambda (x) (* x 2))
                               'double))
             (double (add1 (double 3)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "14"); // (3*2 + 1) * 2
}
