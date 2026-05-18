//! Phase 4 iter 3 — `(->* mandatory rest-pred rng)` variadic-tail
//! arrow contract.
//!
//! Used by the cs-typer contract lowering when a procedure type
//! has a rest-parameter type (ProcType.rest is Some). Mandatory
//! leading args are checked positionally; every additional arg
//! must satisfy rest-pred.

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

// ---- happy paths ----

#[test]
fn arrow_star_zero_mandatories_pure_rest() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define sum-nums
               (apply-contract (->* '() number? number?)
                               (lambda args (apply + args))
                               'sum-nums))
             (list (sum-nums)
                   (sum-nums 1)
                   (sum-nums 1 2 3 4 5))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(0 1 15)");
}

#[test]
fn arrow_star_mandatories_plus_rest() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define prefixed-sum
               (apply-contract (->* (list string?) number? number?)
                               (lambda (label . nums) (apply + nums))
                               'prefixed-sum))
             (list (prefixed-sum \"a\")
                   (prefixed-sum \"b\" 10 20)
                   (prefixed-sum \"c\" 1 2 3 4))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(0 30 10)");
}

// ---- arity check ----

#[test]
fn arrow_star_rejects_too_few_args() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (->* (list string? string?) number? number?)
                                    (lambda (a b . nums) (apply + nums))
                                    'f))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(f \"only-one\")")
        .expect_err("missing mandatory arg");
    let s = format!("{}", err);
    assert!(s.contains("contract") || s.contains("arity"), "got: {}", s);
}

// ---- mandatory-arg violation ----

#[test]
fn arrow_star_mandatory_arg_must_match_dom() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (->* (list string?) number? number?)
                                    (lambda (s . nums) (apply + nums))
                                    'f))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(f 42)")
        .expect_err("first arg must be string");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- rest-arg violation ----

#[test]
fn arrow_star_rest_arg_must_match_rest_pred() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (->* '() number? number?)
                                    (lambda args (apply + args))
                                    'f))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(f 1 2 'bad 4)")
        .expect_err("rest arg must be number");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- range still enforced ----

#[test]
fn arrow_star_range_is_enforced() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (->* '() number? string?)
                                    (lambda args (apply + args))
                                    'f))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(f 1 2 3)")
        .expect_err("proc returns number, contract says string");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- error cases at construction ----

#[test]
fn arrow_star_rejects_non_list_mandatory_doms() {
    let mut rt = load_contract();
    let err = rt
        .eval_str("<t>", "(->* number? number? number?)")
        .expect_err("mandatory-doms must be a list");
    let s = format!("{}", err);
    assert!(s.contains("must be a list"), "got: {}", s);
}

#[test]
fn arrow_star_rejects_bogus_rest_pred() {
    let mut rt = load_contract();
    let err = rt
        .eval_str("<t>", "(->* '() 42 number?)")
        .expect_err("rest-pred must be predicate or contract");
    let s = format!("{}", err);
    assert!(s.contains("rest-pred must be"), "got: {}", s);
}
