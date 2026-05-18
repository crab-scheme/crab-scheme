//! Phase 4 — define/typed composes with the library system.
//!
//! No new code required: define/typed expands to (define name
//! (apply-contract ...)) so the bound name IS the wrapped
//! procedure. A library that exports `name` re-exports the
//! contract-protected version transparently — same property as
//! define/contract from Phase 2B.6.
//!
//! This test file confirms the composition rather than testing
//! new functionality. Documents the integration story.

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

#[test]
fn typed_define_inside_library_exports_wrapped() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(library (math typed)
               (export sq cube)
               (import (rnrs))
               (define/typed sq (-> Fixnum Fixnum)
                 (lambda (x) (* x x)))
               (define/typed cube (-> Fixnum Fixnum)
                 (lambda (x) (* x x x))))
             (list (sq 4) (cube 3))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(16 27)");
}

#[test]
fn typed_export_violation_caught_at_call_site() {
    let mut rt = load_typed_contract();
    rt.eval_str(
        "<t>",
        "(library (math typed)
           (export sq)
           (import (rnrs))
           (define/typed sq (-> Fixnum Fixnum)
             (lambda (x) (* x x))))",
    )
    .unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((contract-violation? c)
                        (contract-violation-target c)))
               (sq 'bad))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "sq");
}

#[test]
fn typed_export_with_higher_order_contract() {
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(library (functional)
               (export apply-twice)
               (import (rnrs))
               (define/typed apply-twice
                 (-> (-> Fixnum Fixnum) Fixnum Fixnum)
                 (lambda (f x) (f (f x)))))
             (apply-twice (lambda (n) (+ n 10)) 5)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "25");
}

#[test]
fn typed_submodule_can_test_parent_typed_exports() {
    // Composes with Phase 3B submodules: a submodule inside the
    // typed library can exercise typed exports and catch
    // violations.
    let mut rt = load_typed_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(library (geom)
               (export area)
               (import (rnrs))
               (define/typed area (-> Fixnum Fixnum Fixnum)
                 (lambda (w h) (* w h)))
               (submodule tests
                 (define rect-area (area 3 4))
                 (define bad-area
                   (guard (c ((contract-violation? c) 'caught))
                     (area 'a 4)))))
             (list rect-area bad-area)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(12 caught)");
}
