//! parallel-runtime spec C5.3 — contract boundary refuses
//! region values.
//!
//! A region pair returned from a contracted proc would
//! escape its `(with-region …)` scope as soon as the caller
//! stored the result. The contract library detects this via
//! `(gc-allocator …)` from C5.2 and raises a &contract
//! violation rather than letting the dangling handle leak.

#![cfg(feature = "regions")]

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

/// Sanity: with no region in the picture, the contract
/// wrapper still passes values through normally. Guards
/// against the new check accidentally rejecting Rc values.
#[test]
fn rc_returned_value_passes_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define guarded (apply-contract (-> number? pair?)
                                              (lambda (n) (cons n n))
                                              'rc-returner))
             (guarded 5)",
        )
        .expect("contract wrapper should accept rc pairs");
    assert!(disp(&rt, &v).contains('5'));
}

/// A contracted proc whose body opens its own `(with-region
/// …)` and returns the result observes auto-promotion: the
/// runtime promotes the region pair to Rc as the region
/// drops (`to_rc_deep`), so the contract sees a perfectly-
/// safe Rc value. This documents the belt-and-suspenders
/// relationship: C5.3's guard is the backstop, runtime
/// auto-promotion is the primary defense.
///
/// If auto-promotion is ever weakened (e.g., to optimize hot
/// paths) the C5.3 guard would start firing on this case.
#[test]
fn region_returned_then_promoted_passes() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define guarded (apply-contract (-> number? pair?)
                                              (lambda (n)
                                                (with-region
                                                 (lambda ()
                                                   (cons-in-region n n))))
                                              'leak-tester))
             (gc-allocator (guarded 7))",
        )
        .expect("auto-promotion should keep the call safe");
    // Runtime promoted the pair → contract accepts → caller
    // sees an Rc-tagged result.
    assert_eq!(disp(&rt, &v), "rc");
}

/// Symmetric: a contracted proc whose *argument* is a region
/// value also raises — the wrapper would retain the region
/// reference across the call boundary.
#[test]
fn region_argument_raises_contract() {
    let mut rt = load_contract();
    let result = rt.eval_str(
        "<t>",
        "(define guarded (apply-contract (-> pair? pair?)
                                          (lambda (p) p)
                                          'region-arg-accepter))
         (with-region
          (lambda ()
            (let ((rp (cons-in-region 1 2)))
              (guarded rp))))",
    );
    let err = result
        .err()
        .expect("contract should reject region argument");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("contract") || msg.contains("violation") || msg.contains("no-region-escape"),
        "expected contract-violation error, got: {msg}"
    );
}
