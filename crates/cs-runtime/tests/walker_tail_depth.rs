//! Regression tests for the walker's recursion-depth accounting.
//!
//! `EvalCtx::depth` used to be incremented once per closure
//! tail-call and never decremented — a monotonic total-call
//! counter that spuriously tripped `max_depth` (1M) on any
//! long-running program (GitHub issue #3). It now tracks live
//! `eval` nesting, so a tail-recursive loop holds depth constant
//! regardless of how many iterations it runs.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// A tail-recursive loop of 5M iterations is properly TCO'd —
/// constant host stack, constant eval-nesting depth. It must
/// complete, not trip the depth guard at 1M.
#[test]
fn deep_tail_loop_does_not_trip_depth_guard() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<walker-tail-depth>",
            "(let loop ((i 0)) (if (< i 5000000) (loop (+ i 1)) i))",
        )
        .expect("5M-iteration tail loop should complete on the walker");
    assert_eq!(disp(&rt, &v), "5000000");
}

/// Walker and VM tiers must agree on a 2M-iteration accumulator
/// loop — both are well past the old 1M counter ceiling.
#[test]
fn deep_tail_loop_walker_and_vm_agree() {
    let prog = "(let loop ((i 0) (acc 0)) (if (< i 2000000) (loop (+ i 1) (+ acc 1)) acc))";
    let mut rt_w = Runtime::new();
    let w = rt_w.eval_str("<walker>", prog).expect("walker tail loop");
    let mut rt_v = Runtime::new();
    let v = rt_v.eval_str_via_vm("<vm>", prog).expect("vm tail loop");
    assert_eq!(disp(&rt_w, &w), "2000000");
    assert_eq!(disp(&rt_w, &w), disp(&rt_v, &v));
}

/// Mutual tail recursion across two closures is also constant-
/// depth — neither `even?` nor `odd?` should accumulate depth.
#[test]
fn mutual_tail_recursion_stays_constant_depth() {
    let prog = "(define (ev? n) (if (= n 0) #t (od? (- n 1)))) \
                (define (od? n) (if (= n 0) #f (ev? (- n 1)))) \
                (ev? 3000000)";
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<walker>", prog)
        .expect("3M-deep mutual tail recursion should complete");
    assert_eq!(disp(&rt, &v), "#t");
}
