//! cs-4wk — integration smoke coverage alongside the CycleVisit
//! region-pointer routing fix in `cs-vm`'s `Bindings::visit_children`
//! (crates/cs-vm/src/vm.rs). The precise, deterministic regression
//! tests for that fix live as in-crate unit tests in
//! `cs-vm/src/vm.rs` (`bindings_cycle_visit_region_tests`), since
//! `Bindings`/`NanboxValue`/`decode_gc_handle` are crate-private and
//! only reachable directly from within `cs-vm`.
//!
//! **cs-f0k**: `cs-vm`'s `impl Procedure for VmClosure` now overrides
//! `visit_closure_children` (mirroring `cs-runtime`'s tree-walker
//! `Closure`), so `Value::Procedure`'s `CycleVisit` forward and the
//! layer-2 synchronous detector (`set-cdr!` / `set-car!` /
//! `vector-set!` / `hashtable-set!`) now do reach a VM closure's
//! captured `Env`/`Bindings` -> `VmClosure::visit_children` ->
//! `Env::visit_children` -> `Bindings::visit_children`. The
//! `cycle_detector_reaches_captured_env_through_vm_closure` test
//! below is the true end-to-end regression coverage for that: a
//! Scheme-level cycle closed *through* a VM closure's capture (not
//! just through the pair's own `car`/`cdr`) is now observed by the
//! detector, mirroring `cs-runtime/tests/closure_cycle.rs`'s
//! walker-tier `cycle_detector_observes_pair_holding_self_referential_closure`.
//!
//! The two tests below it are kept as general smoke coverage: a
//! self-cycle on an Rc-backed pair whose car is a VM closure
//! capturing a `cons-in-region` value, closed via `set-cdr!`, must
//! not panic and must compute the right answer.
#![cfg(feature = "regions")]

use cs_core::WriteMode;
use cs_runtime::countable_memory_cycle::{cycle_detection_count, reset_cycle_detection_count};
use cs_runtime::Runtime;

/// True end-to-end regression for cs-f0k: builds a pair `p` and a VM
/// closure `g` that captures `p` as a free variable, then
/// `set-cdr!`s `p`'s cdr to `g`. The cycle is `p -> (cdr p) = g ->
/// g's captured env -> binding for p -> p`, i.e. only reachable via
/// `VmClosure::visit_closure_children`, not via `p`'s own car/cdr
/// directly. Before the fix this mutation would never re-observe
/// `p`'s address (the walk into `g` was a no-op), so the detector
/// count would not advance; after the fix it does.
#[test]
fn cycle_detector_reaches_captured_env_through_vm_closure() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    rt.eval_str_via_vm(
        "<t>",
        r"
        (define p (cons 'box #f))
        (define (g) p)
        (set-cdr! p g)
    ",
    )
    .expect("eval ok");
    let after = cycle_detection_count();
    assert!(
        after > before,
        "expected detector to observe the cycle closed through g's captured env \
         (count: {before} -> {after})"
    );
}

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
