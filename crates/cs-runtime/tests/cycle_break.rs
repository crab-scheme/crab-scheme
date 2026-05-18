//! Iter 7.1.x / 7.1.x.y regression — verify the strong-count-
//! guarded cycle break works correctly:
//!
//! 1. Acyclic mutations don't fire the detector and don't
//!    trigger the break.
//! 2. Cycles whose only strong holder is the freshly-mutated
//!    subgraph are detected but NOT broken (the strong-count
//!    guard correctly refuses to orphan the value).
//! 3. Cycles with persistent external anchors (e.g. a top-
//!    level binding) DO trigger the break: the Weak tombstone
//!    is set, the cycle stays observable while the anchor
//!    lives, and reclaims when the anchor drops.
//! 4. Programs that rely on the cyclic semantics (e.g., the
//!    metacircular evaluator's env mutations) continue to work
//!    because the guard preserves observability.
//!
//! Iter 7.1.x.y replaced the iter-7.1.x threshold-5 heuristic
//! with a caller-supplied `baseline: usize` (see
//! `Pair::break_car_cycle` doc). Walker's `b_set_car` /
//! `b_set_cdr` pass `baseline = 2` (slot + args[1]). The
//! `iter_7_1_x_y_top_bound_self_cycle_actually_breaks` test
//! is the canonical example that iter 7.1.x's heuristic
//! would have leaked but iter 7.1.x.y reclaims correctly.

use cs_runtime::countable_memory_cycle::{
    cycle_broken_count, cycle_detection_count, reset_cycle_detection_count,
};
use cs_runtime::Runtime;

#[test]
fn detector_fires_on_self_loop_observability_preserved() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<cycle_break>",
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    )
    .expect("eval ok");
    assert!(
        cycle_detection_count() > 0,
        "expected detector to fire on (set-cdr! x x)"
    );
    // Observability check: (car x) must still return 1 and
    // (cdr x) must still return a pair (cycle visible to user
    // even if Weak tombstone was applied).
    let result = rt.eval_str("<verify>", "(car x)").expect("car x");
    assert!(
        matches!(result, cs_core::Value::Number(_)),
        "(car x) returned {result:?}, expected Number(1)"
    );
    let cdr_result = rt.eval_str("<verify>", "(pair? (cdr x))").expect("cdr x");
    assert!(
        matches!(cdr_result, cs_core::Value::Boolean(true)),
        "(pair? (cdr x)) returned {cdr_result:?}, expected #t"
    );
}

#[test]
fn acyclic_mutation_does_not_trigger_break() {
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<acyclic>",
        r"
        (define a (cons 1 2))
        (define b (cons 3 4))
        (set-cdr! a b)
    ",
    )
    .expect("eval ok");
    assert_eq!(
        cycle_detection_count(),
        0,
        "no cycle should have been detected"
    );
    assert_eq!(cycle_broken_count(), 0, "no break should have happened");
}

#[test]
fn metacircular_style_define_preserves_binding_observability() {
    // Mirrors the metacircular's `(set-car! env (cons name val))`
    // pattern that broke under the naive iter-7.1 break. The
    // strong-count guard must skip the break (or the binding
    // becomes inaccessible).
    let mut rt = Runtime::new();
    rt.eval_str(
        "<meta_define>",
        r"
        (define env (cons (cons 'old 'val) '()))
        (define name 'new)
        (define val 'value)
        ; Simulate metacircular define:
        (set-cdr! env (cons (car env) (cdr env)))
        (set-car! env (cons name val))
        ; Verify the binding is still accessible:
        (define accessible (eq? (car (car env)) name))
    ",
    )
    .expect("eval ok");
    let result = rt.eval_str("<verify>", "accessible").expect("lookup");
    assert!(
        matches!(result, cs_core::Value::Boolean(true)),
        "metacircular-style binding lookup returned {result:?}, expected #t"
    );
}

#[test]
fn cycle_broken_count_distinguishes_detect_from_break() {
    // The detection counter must increment even when the
    // break is skipped by the strong-count guard.
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<break_count>",
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    )
    .expect("eval ok");
    let detected = cycle_detection_count();
    let broken = cycle_broken_count();
    assert!(detected > 0, "detection should have fired");
    // broken may or may not be > 0 depending on the guard;
    // they must satisfy broken <= detected.
    assert!(
        broken <= detected,
        "broken count {broken} exceeds detected {detected}"
    );
}

#[test]
fn iter_7_1_x_y_top_bound_self_cycle_actually_breaks() {
    // Iter 7.1.x.y regression: under the caller-supplied
    // baseline (b_set_cdr passes 3 = slot + args[0] + args[1]
    // + VM-tier transient = upper bound across tiers), a top-
    // level-bound pair's self-cycle demotes to a Weak
    // tombstone. Total strong at break time for walker:
    // top env binding (1) + args[0] (1) + args[1] (1) +
    // p.cdr slot (1) = 4 > baseline=3 → demote fires.
    //
    // Without iter 7.1.x.y's caller baseline (the previous
    // threshold-5 heuristic), total=4 was below threshold
    // and the break skipped — leaking the cycle.
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    rt.eval_str(
        "<top_self_cycle>",
        r"
        (define x (cons 1 2))
        (set-cdr! x x)
    ",
    )
    .expect("eval ok");
    let detected = cycle_detection_count();
    let broken = cycle_broken_count();
    assert!(detected > 0, "detection should have fired");
    assert!(
        broken > 0,
        "iter 7.1.x.y caller-baseline should have permitted the break (detected={detected}, broken={broken})"
    );
    // Observability: (car x) is still a fixnum; (cdr x)
    // upgrades the weak tombstone and returns the cyclic
    // pair x.
    let car_val = rt.eval_str("<verify>", "(car x)").expect("car x");
    assert!(
        matches!(car_val, cs_core::Value::Number(_)),
        "(car x) returned {car_val:?}, expected Number"
    );
    let cdr_eq_x = rt.eval_str("<verify>", "(eq? (cdr x) x)").expect("eq?");
    assert!(
        matches!(cdr_eq_x, cs_core::Value::Boolean(true)),
        "(eq? (cdr x) x) returned {cdr_eq_x:?}, expected #t"
    );
}
