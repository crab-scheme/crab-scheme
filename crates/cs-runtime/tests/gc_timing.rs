//! Cycle-detector pause-time measurement harness.
//!
//! The legacy precise tracing GC (M5 Phase 1) measured `collect()`
//! pauses; it's gone. Reclamation now runs through `Rc::drop` plus
//! the synchronous Bacon-Rajan cycle detector in `cs_gc::cycle`,
//! which fires from inside mutation primitives that could close a
//! cycle (see ADR 0014 iter 12b and
//! `crates/cs-gc/src/cycle.rs`).
//!
//! This harness measures the cost of those cycle-closing
//! mutations as the "GC pause" surrogate. The bound is loose
//! (10 ms p99 across modest heaps) — primary goal is regression
//! detection on the detector's bounded-DFS implementation.
//!
//! Not a criterion bench. Plain test that records durations and
//! asserts a sanity ceiling. Useful as the second leg of the
//! nightly `m5-fuzz` workflow.

use std::time::Instant;

use cs_runtime::Runtime;

fn percentile(samples: &mut [u128], pct: f64) -> u128 {
    samples.sort_unstable();
    let idx = ((samples.len() as f64) * pct).round() as usize;
    samples[idx.min(samples.len() - 1)]
}

#[test]
fn p99_cycle_close_under_10ms_on_modest_heap() {
    let mut rt = Runtime::new();

    // Build a moderate heap: 100 cons pairs we'll cycle-close
    // one-by-one. Each `set-cdr!` triggers the synchronous cycle
    // detector.
    rt.eval_str(
        "<setup>",
        r#"
          (define pairs
            (let loop ((i 0) (acc '()))
              (if (= i 100) acc (loop (+ i 1) (cons (cons i i) acc)))))
        "#,
    )
    .unwrap();

    // Walk the 100 pairs; close a self-cycle on each via
    // `set-cdr!`. Time each individual mutation.
    let mut samples = Vec::with_capacity(100);
    rt.eval_str("<setup>", "(define iter pairs)").unwrap();
    for _ in 0..100 {
        // Each iteration both reads (car iter) and mutates its
        // cdr — the cycle detector fires on the set-cdr! call.
        let start = Instant::now();
        let _ = rt.eval_str(
            "<bench>",
            "(let ((p (car iter))) (set-cdr! p p)) (set! iter (cdr iter))",
        );
        samples.push(start.elapsed().as_nanos());
    }

    let p50 = percentile(&mut samples.clone(), 0.5);
    let p99 = percentile(&mut samples.clone(), 0.99);
    let max = *samples.iter().max().unwrap();

    eprintln!(
        "cycle-close pause samples (n={}): p50={}ns p99={}ns max={}ns",
        samples.len(),
        p50,
        p99,
        max
    );

    // Loose bound. The detector's per-call work is O(visited
    // children) bounded by `cs_gc::cycle::get_limit()` (default
    // 10_000); a self-loop on a 2-element pair should be well
    // under 10 µs in practice. 10 ms leaves room for first-call
    // warmup and CI scheduling jitter without losing the ability
    // to catch a regression that puts the detector into a
    // pathological code path.
    assert!(
        p99 < 10_000_000,
        "p99 cycle-close pause exceeded 10ms ceiling: {}ns",
        p99
    );
}

#[test]
fn collect_shim_is_cheap() {
    // `Runtime::collect()` is a documented no-op shim post-iter
    // 12b. The bound checks that the shim itself stays cheap —
    // anyone reintroducing real work on this path needs to
    // either justify it or rename the method.
    let rt = Runtime::new();
    let mut samples = Vec::with_capacity(50);
    for _ in 0..50 {
        let start = Instant::now();
        rt.collect();
        samples.push(start.elapsed().as_nanos());
    }
    let p99 = percentile(&mut samples.clone(), 0.99);
    eprintln!("collect-shim p99={}ns", p99);
    // No-op shim must stay sub-millisecond.
    assert!(p99 < 1_000_000, "collect-shim p99 > 1ms: {}ns", p99);
}
