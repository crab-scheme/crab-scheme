//! ADR 0019 — JIT proper tail calls (bounce trampoline).
//!
//! Pre-fix, the Cranelift JIT only converted *self*-recursion in tail
//! position to a constant-stack `return_call`. Cross-function / mutual
//! tail calls (and self-calls the translator routed through
//! `CallGeneral`) lowered to a regular dispatch + `return`, so once the
//! bodies tiered up to the JIT a tail-recursive chain grew the host
//! stack until it overflowed and ABORTED the process.
//!
//! These programs run deep enough to tier up to the JIT *and* deep
//! enough that, pre-fix, the JIT'd bodies overflow the 8 MB host stack.
//! A regression would abort the test runner (stack overflow is not
//! catchable), so reaching the assertion at all is most of the signal;
//! the assertions also pin the cross-tier-correct result.
//!
//! The VM and walker tiers always honoured proper tail calls, so the
//! expected values double as a differential oracle.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn jit_eval(src: &str) -> String {
    let mut rt = Runtime::new();
    rt.install_jit().expect("install_jit");
    // eval_str_via_vm runs on the bytecode VM and tiers hot bodies up to
    // the Cranelift JIT — the path these programs exercise.
    let v = rt
        .eval_str_via_vm("<tco>", src)
        .unwrap_or_else(|d| panic!("eval failed: {}", d.message));
    rt.format_value(&v, WriteMode::Display)
}

#[test]
fn mutual_recursion_runs_in_constant_stack_on_jit() {
    // ping <-> pong, 1,000,000 deep. Pre-fix: stack overflow + abort
    // once the bodies tier up. Post-fix: constant stack (trampoline).
    let out = jit_eval(
        "(define (ping n) (if (= n 0) 'even (pong (- n 1))))
         (define (pong n) (if (= n 0) 'odd (ping (- n 1))))
         (ping 1000000)",
    );
    assert_eq!(out, "even");
}

#[test]
fn nested_named_let_tail_loops_run_in_constant_stack_on_jit() {
    // The inner `col` loop tail-calls the outer `row` loop — the exact
    // shape that makes mandelbrot's JIT body O(n^2) host-stack pre-fix.
    // 2000 x 2000 = 4,000,000 iterations.
    let out = jit_eval(
        "(define (grid n)
           (let row ((y 0) (acc 0))
             (if (= y n)
                 acc
                 (let col ((x 0) (a acc))
                   (if (= x n)
                       (row (+ y 1) a)
                       (col (+ x 1) (+ a 1)))))))
         (grid 2000)",
    );
    assert_eq!(out, "4000000");
}

#[test]
fn self_tail_recursion_unchanged_on_jit() {
    // Regression guard for the pre-existing self-tail `return_call`
    // path — it must keep working alongside the new bounce path.
    let out = jit_eval(
        "(define (count i acc)
           (if (= i 1000000) acc (count (+ i 1) (+ acc 1))))
         (count 0 0)",
    );
    assert_eq!(out, "1000000");
}

#[test]
fn deep_tail_call_result_matches_vm() {
    // Differential: the JIT-tiered result equals the VM result for a
    // tail-recursive accumulator that crosses a function boundary.
    let src = "(define (sum-to n acc) (if (= n 0) acc (add1-step n acc)))
               (define (add1-step n acc) (sum-to (- n 1) (+ acc n)))
               (sum-to 100000 0)";
    let jit = jit_eval(src);
    let mut rt = Runtime::new();
    let vm = rt
        .eval_str_via_vm("<vm>", src)
        .map(|v| rt.format_value(&v, WriteMode::Display))
        .unwrap();
    assert_eq!(jit, vm, "JIT and VM disagree on cross-function tail sum");
    assert_eq!(jit, "5000050000");
}
