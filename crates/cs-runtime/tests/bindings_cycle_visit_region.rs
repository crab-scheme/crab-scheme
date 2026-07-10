//! cs-4wk — integration smoke coverage alongside the CycleVisit
//! region-pointer routing fix in `cs-vm`'s `Bindings::visit_children`
//! (crates/cs-vm/src/vm.rs). The precise, deterministic regression
//! tests for that fix live as in-crate unit tests in
//! `cs-vm/src/vm.rs` (`bindings_cycle_visit_region_tests`), since
//! `Bindings`/`NanboxValue`/`decode_gc_handle` are crate-private and
//! only reachable directly from within `cs-vm`.
//!
//! **Investigation note (why this file doesn't itself exercise
//! `Bindings::visit_children`):** the natural way to reach a VM
//! closure's captured `Env`/`Bindings` from Scheme is through
//! `cs_core::Procedure::visit_closure_children` — `Value::Procedure`'s
//! `CycleVisit` impl forwards to it, and the layer-2 synchronous
//! detector (`set-cdr!` / `set-car!` / `vector-set!` /
//! `hashtable-set!`) walks through `Value` unconditionally. However,
//! `cs-vm`'s `impl Procedure for VmClosure` does not override
//! `visit_closure_children` (it inherits the trait's empty default —
//! contrast with `cs-runtime`'s tree-walker `Closure`, which does
//! override it). So today, no Scheme-level mutation actually reaches
//! `VmClosure::visit_children` -> `Env::visit_children` ->
//! `Bindings::visit_children`; that path is unreachable until
//! `VmClosure` grows its own override. That gap is separate from
//! cs-4wk's scope (tracked as a follow-up) but was discovered while
//! building this test.
//!
//! These two tests are kept anyway as general smoke coverage: a
//! self-cycle on an Rc-backed pair whose car is a VM closure
//! capturing a `cons-in-region` value, closed via `set-cdr!`, must
//! not panic and must compute the right answer.
#![cfg(feature = "regions")]

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

#[test]
fn set_cdr_self_cycle_with_region_captured_closure_is_sound() {
    let mut rt = Runtime::new();
    let prog = "(with-region (lambda ()
                   (let ((v (cons-in-region 11 22)))
                     (define (g) v)
                     (define p (cons g 'anchor))
                     (set-cdr! p p)
                     (set-cdr! p '())
                     (+ (car v) (cdr v)))))";
    let result = rt.eval_str_via_vm("<t>", prog).unwrap();
    assert_eq!(disp(&rt, &result), "33");
}

/// Same shape but with a bigger frame (14 locals, past `cs-vm`'s
/// `SMALL_THRESHOLD` of 12), so if/when `VmClosure` grows a
/// `visit_closure_children` override, this would additionally cover
/// the `Bindings::Large` (`SymbolMap`) storage tier.
#[test]
fn set_cdr_self_cycle_with_region_captured_closure_large_frame_is_sound() {
    let mut rt = Runtime::new();
    let prog = "(with-region (lambda ()
                   (let ((a1 1) (a2 2) (a3 3) (a4 4) (a5 5) (a6 6)
                         (a7 7) (a8 8) (a9 9) (a10 10) (a11 11)
                         (a12 12) (a13 13) (a14 14)
                         (v (cons-in-region 11 22)))
                     (define (g) (+ a1 a2 a3 a4 a5 a6 a7 a8 a9 a10 a11
                                     a12 a13 a14 (car v) (cdr v)))
                     (define p (cons g 'anchor))
                     (set-cdr! p p)
                     (set-cdr! p '())
                     (g))))";
    let result = rt.eval_str_via_vm("<t>", prog).unwrap();
    // a1..a14 sum to 105; (car v)+(cdr v) = 33.
    assert_eq!(disp(&rt, &result), "138");
}
