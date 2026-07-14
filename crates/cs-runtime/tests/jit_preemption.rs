//! #30 iter-1 — JIT-compiled tail loops tick reductions so a CPU-bound
//! JIT'd actor preempts.
//!
//! Reduction-based preemption already worked in the bytecode VM
//! (`vm_tick_reductions` fires the yield hook every `budget` ops in the
//! dispatch loop), but JIT-compiled native code bypassed the dispatch
//! loop entirely — so once a hot tail loop tiered up, it stopped ticking
//! and could hold its worker thread indefinitely. The fix emits a
//! `vm_jit_tick_reductions` call at the JIT tail-self back-edge.
//!
//! `yield_count()` only increments when the budget is hit AND a hook is
//! installed, so we install a no-op hook and read the per-thread count
//! after running a tail loop that tiers up (n well past the 1024-call
//! threshold). All globals are thread-local and restored after.

use cs_runtime::Runtime;

fn noop_yield_hook() {}

#[test]
fn jit_tail_loop_ticks_reductions() {
    let prev_hook = cs_vm::vm::install_yield_hook(Some(noop_yield_hook));
    let prev_budget = cs_vm::vm::reduction_budget();
    cs_vm::vm::set_reduction_budget(50);
    cs_vm::vm::reset_yield_count();

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<t>",
        "(define (count n) (let loop ((i 0)) (if (< i n) (loop (+ i 1)) i)))",
    )
    .unwrap();
    // `loop` tiers up at ~1024 self-calls; the remaining ~49k iterations
    // run JIT-compiled.
    let v = rt.eval_str_via_vm("<t>", "(count 50000)").unwrap();
    let yields = cs_vm::vm::yield_count();

    // Restore thread-locals before asserting.
    cs_vm::vm::install_yield_hook(prev_hook);
    cs_vm::vm::set_reduction_budget(prev_budget);

    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "50000");
    // Pre-fix, only the ~1024 pre-tier-up VM iterations ticked, so the
    // count would cap near 1024/50 ≈ 20. With the back-edge tick every
    // iteration ticks → ~50000/50 ≈ 1000. cs-845.6 (judge fix): the old
    // threshold of 100 had gone stale — a neutered tick still produces
    // ~167 yields (a bit more than the naive ~20 estimate, since the VM
    // dispatch loop itself keeps ticking on every bytecode op right up to
    // tier-up, and per-tier-up-attempt overhead adds a few more), so 100
    // no longer has teeth. 500 cleanly separates "no JIT-side tick"
    // (~167) from "JIT-side tick present" (~1000).
    assert!(
        yields > 500,
        "JIT-compiled tail loop must tick reductions (got {yields} yields; \
         a neutered JIT-side tick caps near ~167 here, not the full ~1000)"
    );
}

/// Control: a NON-tail self-recursion (factorial-style, not a loop) takes
/// the regular `CallSelf` path, which is intentionally NOT ticked (it is
/// bounded and returns). This documents the scope choice — it should
/// still compute correctly under the JIT.
#[test]
fn jit_non_tail_recursion_correct_and_untouched() {
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<t>",
        "(define (sumto n) (if (= n 0) 0 (+ n (sumto (- n 1)))))",
    )
    .unwrap();
    // Warm past tier-up with a depth the debug-build stack tolerates.
    rt.eval_str_via_vm(
        "<t>",
        "(let w ((k 0)) (if (= k 2000) 'done (begin (sumto 100) (w (+ k 1)))))",
    )
    .unwrap();
    let v = rt.eval_str_via_vm("<t>", "(sumto 100)").unwrap();
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "5050");
}
