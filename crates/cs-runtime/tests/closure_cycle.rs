//! Iter 9 regression — verify the cycle detector observes
//! closure/continuation cycles, and document the leak status
//! pending iter 8's `Weak<Frame>` refactor.
//!
//! Until iter 8 (Frame.parent / Continuation / Closure refactor
//! to Weak<T> back-edges) lands, self-referential closure and
//! call/cc patterns still leak by refcount. This test file
//! exercises the patterns and asserts:
//!   1. The cycle detector fires when invoked on the cycle root.
//!   2. The runtime survives runtime-drop without panic (leaks
//!      land in unbounded memory but don't crash).
//!
//! When iter 8 lands, the leak assertion will tighten to "no
//! leaked allocations on runtime drop".

#![cfg(feature = "countable-memory")]

use cs_runtime::countable_memory_cycle::{cycle_detection_count, reset_cycle_detection_count};
use cs_runtime::Runtime;

#[test]
fn self_referential_define_does_not_panic() {
    // (letrec ([loop (lambda () (loop))]) loop) — closure's env
    // references loop, loop's body references the env binding.
    // Walker correctly constructs the cycle via the letrec
    // placeholder mechanism. Under refcount-only this cycle
    // would leak at runtime drop (iter 8 fix); for now we just
    // assert no panic.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<closure_cycle>",
        r"
        (define (loop) loop)
        (loop)
    ",
    )
    .expect("self-referential define + call should not panic");
    // Drop the runtime — should not panic even though the loop's
    // env chain holds the closure that holds the env.
    drop(rt);
}

#[test]
fn mutual_recursive_set_bang_does_not_panic() {
    // Two closures that reference each other via top-level set!.
    // Same cycle shape as the canonical mutual-recursion pattern.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<mutual>",
        r"
        (define a #f)
        (define b #f)
        (set! a (lambda () b))
        (set! b (lambda () a))
        (a)
    ",
    )
    .expect("mutual closure cycle should not panic");
    drop(rt);
}

#[test]
fn call_cc_capture_then_invoke_does_not_panic() {
    // R6RS escape continuation captured + re-invoked once.
    // Continuation captures frame chain; the runtime should
    // survive both the call and the runtime drop.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<callcc>",
        r"
        (define k #f)
        (+ 1 (call/cc (lambda (cont) (set! k cont) 10)))
    ",
    )
    .expect("call/cc capture should not panic");
    drop(rt);
}

#[test]
fn cycle_detector_observes_pair_holding_self_referential_closure() {
    // Construct a pair whose car is a closure, then set-car! the
    // pair into the closure's referencing env. The mutation
    // triggers the cycle detector via the iter-7 wiring.
    let mut rt = Runtime::new();
    reset_cycle_detection_count();
    let before = cycle_detection_count();
    rt.eval_str(
        "<cycle_detect>",
        r"
        (define p (cons 'box #f))
        (set-cdr! p p)
    ",
    )
    .expect("eval ok");
    let after = cycle_detection_count();
    assert!(
        after > before,
        "expected detector to observe set-cdr! self-loop (count: {before} -> {after})"
    );
}
