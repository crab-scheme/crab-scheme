//! Tail-safe continuation marks (issue #36).
//!
//! The naive parameter/`dynamic-wind` implementation grew the mark
//! chain by one frame per `with-continuation-mark`, so a tail loop
//! that installs a mark each iteration used O(n) space (and the
//! `dynamic-wind` cleanup frames defeated tail-call elimination
//! outright). The native implementation installs the mark on the
//! current continuation frame, which a tail call reuses — so a wcm
//! reached through tail calls REPLACES the frame's mark for that key
//! and the loop runs in constant mark-space.
//!
//! These tests exercise the property on both evaluation tiers (the
//! tree-walker via `eval_str`, the bytecode VM via `eval_str_via_vm`)
//! and check the two tiers agree, since they have independent
//! implementations (walker: depth-tagged `EvalCtx` stack; VM:
//! per-`Frame` mark slot).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// A tail loop that installs `'k -> i` each iteration and, at the
/// base case, reads back the marks for `'k`. Because every wcm is in
/// tail position they all share one continuation frame, so the read
/// sees only the final value — and the loop runs in constant space.
const TAIL_LOOP: &str = "(define (loop i) \
     (with-continuation-mark 'k i \
       (if (= i 0) \
           (current-continuation-marks 'k) \
           (loop (- i 1)))))";

#[test]
fn tail_loop_marks_are_constant_space_walker() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", TAIL_LOOP).unwrap();
    // 200k iterations: under the naive impl this is O(n) marks (and
    // the dynamic-wind frames would overflow); tail-safe it is O(1).
    let v = rt.eval_str("<t>", "(loop 200000)").unwrap();
    assert_eq!(disp(&rt, &v), "(0)", "tail wcm should replace, not grow");
}

#[test]
fn tail_loop_marks_are_constant_space_vm() {
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<t>", TAIL_LOOP).unwrap();
    let v = rt.eval_str_via_vm("<t>", "(loop 200000)").unwrap();
    assert_eq!(disp(&rt, &v), "(0)", "tail wcm should replace, not grow");
}

/// A non-tail call between two marks for the same key must NOT
/// collapse them — the callee gets a fresh frame, so both marks are
/// live and visible innermost-first.
const NON_TAIL_NESTING: &str = "(define (g) \
     (with-continuation-mark 'k 2 (current-continuation-marks 'k))) \
   (define (f) \
     (with-continuation-mark 'k 1 \
       (cons 'sum (g))))";

#[test]
fn non_tail_call_accumulates_marks_walker() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", NON_TAIL_NESTING).unwrap();
    // `(g)` is a non-tail operand of `cons`, so it runs in a fresh
    // frame: both 'k=1 (f's frame) and 'k=2 (g's frame) are live.
    let v = rt.eval_str("<t>", "(f)").unwrap();
    assert_eq!(disp(&rt, &v), "(sum 2 1)");
}

#[test]
fn non_tail_call_accumulates_marks_vm() {
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<t>", NON_TAIL_NESTING).unwrap();
    let v = rt.eval_str_via_vm("<t>", "(f)").unwrap();
    assert_eq!(disp(&rt, &v), "(sum 2 1)");
}

/// The two tiers must agree on every continuation-mark program.
#[test]
fn walker_and_vm_agree() {
    let programs = [
        "(with-continuation-mark 'k 42 (current-continuation-marks 'k))",
        "(with-continuation-mark 'a 1 (with-continuation-mark 'b 2 (current-continuation-marks)))",
        "(with-continuation-mark 'k 1 (with-continuation-mark 'k 2 (current-continuation-marks 'k)))",
        "(current-continuation-marks)",
        "(current-continuation-marks 'absent)",
        TAIL_LOOP_AND_CALL,
    ];
    for prog in programs {
        let mut w = Runtime::new();
        let wv = w.eval_str("<t>", prog).unwrap();
        let ws = disp(&w, &wv);

        let mut v = Runtime::new();
        let vv = v.eval_str_via_vm("<t>", prog).unwrap();
        let vs = disp(&v, &vv);

        assert_eq!(
            ws, vs,
            "tier mismatch for program: {prog}\nwalker={ws} vm={vs}"
        );
    }
}

/// A define + invocation in one program (used in the agreement loop).
const TAIL_LOOP_AND_CALL: &str = "(define (loop i) \
     (with-continuation-mark 'k i \
       (if (= i 0) (current-continuation-marks 'k) (loop (- i 1))))) \
   (loop 1000)";

#[test]
fn body_value_is_returned() {
    // The form's value is the body's value, on both tiers.
    let prog = "(with-continuation-mark 'k 99 (+ 1 2))";
    let mut w = Runtime::new();
    let wv = w.eval_str("<t>", prog).unwrap();
    assert_eq!(disp(&w, &wv), "3");
    let mut v = Runtime::new();
    let vv = v.eval_str_via_vm("<t>", prog).unwrap();
    assert_eq!(disp(&v, &vv), "3");
}
