//! R6RS++ Phase 2B.4 — per-arg domain combinators + higher-order
//! contracts.
//!
//! Extends Phase 2B.2's single-domain `(-> dom rng)` form to:
//! - Per-arg fixed-arity: `(-> dom1 dom2 ... rng)`
//! - Higher-order: a domain or range may itself be a contract,
//!   in which case the matching arg/result is wrapped via
//!   apply-contract (and blame transfers naturally to the
//!   inner wrapper).

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

// ---- per-arg fixed-arity domains ----

#[test]
fn fixed_arity_each_arg_position_checked() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define f (apply-contract (-> number? string? number?)
                                        (lambda (n s) n)
                                        'f))
             (f 5 \"hello\")",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "5");
}

#[test]
fn fixed_arity_first_arg_violation() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (-> number? string? number?)
                                    (lambda (n s) n)
                                    'f))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(f 'bad \"hi\")")
        .expect_err("first arg wrong type");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

#[test]
fn fixed_arity_second_arg_violation() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (-> number? string? number?)
                                    (lambda (n s) n)
                                    'f))",
    )
    .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((contract-violation? c)
                        (contract-violation-value c)))
               (f 5 42))",
        )
        .unwrap();
    // The second arg `42` violates string?; the value field
    // carries the bad input.
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn fixed_arity_arity_mismatch_caught() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define f (apply-contract (-> number? string? number?)
                                    (lambda (n s) n)
                                    'f))",
    )
    .unwrap();
    let err = rt.eval_str("<t>", "(f 5)").expect_err("missing second arg");
    let s = format!("{}", err);
    assert!(s.contains("contract") || s.contains("arity"), "got: {}", s);
}

#[test]
fn three_arg_fixed_arity() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define f (apply-contract (-> number? number? number? number?)
                                        (lambda (a b c) (+ a b c))
                                        'sum3))
             (f 1 2 3)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

// ---- higher-order: contract as domain ----

#[test]
fn higher_order_domain_wraps_procedure_arg() {
    // map-like: takes a procedure and a number; calls the proc
    // on the number. The proc arg is itself contracted.
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define apply-fn
               (apply-contract (-> (-> number? number?) number? number?)
                               (lambda (f x) (f x))
                               'apply-fn))
             (apply-fn (lambda (n) (* n 2)) 5)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "10");
}

#[test]
fn higher_order_inner_proc_violation_caught() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define apply-fn
           (apply-contract (-> (-> number? number?) number? number?)
                           (lambda (f x) (f x))
                           'apply-fn))",
    )
    .unwrap();
    let err = rt
        .eval_str(
            "<t>",
            // The inner proc returns a non-number, violating its
            // own (-> number? number?) contract.
            "(apply-fn (lambda (n) \"oops\") 5)",
        )
        .expect_err("inner proc returns wrong type");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

#[test]
fn higher_order_rejects_non_procedure_arg() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define apply-fn
           (apply-contract (-> (-> number? number?) number? number?)
                           (lambda (f x) (f x))
                           'apply-fn))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(apply-fn 42 5)")
        .expect_err("non-procedure where procedure expected");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- higher-order: contract as range ----

#[test]
fn higher_order_range_wraps_returned_procedure() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            // make-adder returns a procedure; the returned proc
            // is contracted to (-> number? number?).
            "(define make-adder
               (apply-contract (-> number? (-> number? number?))
                               (lambda (n) (lambda (x) (+ n x)))
                               'make-adder))
             (define add5 (make-adder 5))
             (add5 10)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "15");
}

#[test]
fn higher_order_range_returned_proc_inherits_contract() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define make-adder
           (apply-contract (-> number? (-> number? number?))
                           (lambda (n) (lambda (x) (+ n x)))
                           'make-adder))
         (define add5 (make-adder 5))",
    )
    .unwrap();
    // The returned add5 is wrapped with (-> number? number?).
    // Passing a non-number should fire the contract.
    let err = rt
        .eval_str("<t>", "(add5 'bad)")
        .expect_err("wrapped returned proc rejects non-number");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- error cases ----

#[test]
fn arrow_requires_at_least_two_args() {
    let mut rt = load_contract();
    assert!(rt.eval_str("<t>", "(->)").is_err());
    assert!(rt.eval_str("<t>", "(-> number?)").is_err());
}

#[test]
fn domain_spec_must_be_predicate_or_contract() {
    let mut rt = load_contract();
    rt.eval_str(
        "<t>",
        "(define bad (apply-contract (vector '__contract__ (list 'not-callable) number?)
                                      (lambda (x) x)
                                      'bad))",
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(bad 5)")
        .expect_err("bogus domain spec");
    let s = format!("{}", err);
    assert!(
        s.contains("domain spec must be") || s.contains("predicate or contract"),
        "got: {}",
        s
    );
}
