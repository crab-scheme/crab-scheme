//! Issue #11 ext-4 — eta-elision for monomorphic typed contracts.
//!
//! Phase 2B.7 (issue #150) added a fast-path to the contract
//! library: when every dom spec and the range spec are plain
//! predicates (not sub-contracts), `apply-contract` returns the
//! specialized `__apply-contract-fast-fixed` /
//! `__apply-contract-fast-variadic` wrapper instead of the
//! generic dispatch wrapper. Detection happens once at
//! construction time so the per-call hot path only invokes the
//! predicate directly.
//!
//! ext-4 asks: does the typed-derived contract from ext-2's
//! auto-contract pass take the fast path? Answer: **yes**,
//! automatically. The typed lowering emits contracts using the
//! same `->` constructor, with every spec a plain predicate
//! (`integer?`, `string?`, etc.). The `__all-simple-preds?`
//! detector sees plain predicates and returns true, so the
//! fast path triggers.
//!
//! These tests demonstrate the wiring end-to-end:
//!   1. A typed monomorphic library export still produces
//!      correct results.
//!   2. The exported binding's wrapper exhibits the fast-path
//!      shape (we can observe this by stress-calling and
//!      asserting no `&contract-violation` is raised on
//!      conforming inputs).
//!
//! A direct "did the fast path fire?" assertion would require
//! exposing internal contract-library state; we settle for
//! behavioural correctness on a tight loop instead.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_contract(rt: &mut Runtime) {
    let contract_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&contract_path).unwrap();
    rt.eval_str("<contract>", &src).unwrap();
}

#[test]
fn monomorphic_typed_export_uses_fast_path_correctly() {
    // `(-> Fixnum Fixnum)` lowers to `(-> integer? integer?)`.
    // Both specs are plain predicates → `__all-simple-preds?`
    // returns #t → fast path kicks in.
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    rt.eval_str(
        "<lib>",
        "(library (math) \
           (export square) \
           (import (rnrs)) \
           (: square (-> Fixnum Fixnum)) \
           (define (square x) (* x x)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (math))").unwrap();
    // Tight loop: 100 calls. If the fast path didn't trigger,
    // we'd still get correct results — but the perf delta on
    // hot paths motivates the test.
    let v = rt
        .eval_str(
            "<loop>",
            "(let loop ((i 0) (acc 0)) \
               (if (= i 10) acc (loop (+ i 1) (+ acc (square i)))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "285"); // 0² + 1² + … + 9² = 285
}

#[test]
fn higher_order_typed_contract_falls_through_to_slow_path() {
    // `(-> (-> Fixnum Fixnum) Fixnum)` lowers to a contract
    // whose first dom is a SUB-CONTRACT (the inner arrow),
    // not a plain predicate. `__all-simple-preds?` returns #f
    // → slow path. This test confirms we still get correct
    // behavior; it doesn't directly assert "slow path was
    // taken" (would require internal-state inspection).
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    rt.eval_str(
        "<lib>",
        "(library (math) \
           (export apply-twice) \
           (import (rnrs)) \
           (: apply-twice (-> (-> Fixnum Fixnum) Fixnum)) \
           (define (apply-twice f) (f (f 1))))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (math))").unwrap();
    let v = rt
        .eval_str("<call>", "(apply-twice (lambda (x) (* x 3)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "9"); // 1 → 3 → 9
}

#[test]
fn union_typed_contract_does_not_use_fast_path_but_still_works() {
    // `(-> (U Fixnum Flonum) (U Fixnum Flonum))` lowers to
    // `(-> (or/c integer? real?) (or/c integer? real?))`. The
    // `(or/c …)` form is a SUB-CONTRACT, not a plain
    // predicate. Slow path; still correct.
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    rt.eval_str(
        "<lib>",
        "(library (math) \
           (export double) \
           (import (rnrs)) \
           (: double (-> (U Fixnum Flonum) (U Fixnum Flonum))) \
           (define (double x) (* x 2)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (math))").unwrap();
    let v = rt.eval_str("<int>", "(double 21)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
    let v = rt.eval_str("<float>", "(double 1.5)").unwrap();
    assert_eq!(disp(&rt, &v), "3.0");
}

#[test]
fn monomorphic_multi_arg_typed_contract_works() {
    // `(-> Fixnum Fixnum Fixnum)` lowers to `(-> integer?
    // integer? integer?)` — fast path for multi-domain fixed-
    // arity (`__apply-contract-fast-fixed`).
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    rt.eval_str(
        "<lib>",
        "(library (math) \
           (export add) \
           (import (rnrs)) \
           (: add (-> Fixnum Fixnum Fixnum)) \
           (define (add a b) (+ a b)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (math))").unwrap();
    let v = rt.eval_str("<good>", "(add 12 30)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
    let v = rt
        .eval_str(
            "<bad-arity>",
            "(guard (c ((contract-violation? c) 'caught)) (add 1))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}
