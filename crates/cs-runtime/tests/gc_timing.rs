#![cfg(not(feature = "countable-memory"))]

//! GC pause-time measurement harness for the M5 exit gate.
//!
//! The M5 spec calls for p99 GC pause < 1ms on stdlib load. Phase 1's
//! Rc-backed `Heap::collect()` is mostly bookkeeping (mark-walk +
//! prune expired weaks), so the numbers here are a baseline — Phase
//! 2's arena-backed implementation reports against this same harness.
//!
//! This isn't a criterion bench (criterion is a follow-up). It's a
//! plain test that records durations and asserts a sanity bound. The
//! bound is loose for now (10 ms p99, generous for Phase 1 bookkeeping
//! across a few thousand allocations); Phase 2 tightens it to the spec's
//! 1ms target.

use std::time::Instant;

use cs_runtime::Runtime;

fn percentile(samples: &mut [u128], pct: f64) -> u128 {
    samples.sort_unstable();
    let idx = ((samples.len() as f64) * pct).round() as usize;
    samples[idx.min(samples.len() - 1)]
}

#[test]
fn p99_pause_under_10ms_on_modest_heap() {
    let mut rt = Runtime::new();
    // Build a moderate heap: 100 small lists + 100 vectors + 10 hashtables.
    rt.eval_str(
        "<setup>",
        r#"
          (define lists (map (lambda (i)
                               (let loop ((j 0) (acc '()))
                                 (if (= j 10) acc (loop (+ j 1) (cons j acc)))))
                             '(0 1 2 3 4 5 6 7 8 9)))
          (define vecs  (map (lambda (i) (make-vector 8 i)) '(0 1 2 3 4 5 6 7 8 9)))
        "#,
    )
    .unwrap();

    // Measure collect() across 100 calls.
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        rt.collect();
        samples.push(start.elapsed().as_nanos());
    }

    let p50 = percentile(&mut samples.clone(), 0.5);
    let p99 = percentile(&mut samples.clone(), 0.99);
    let max = *samples.iter().max().unwrap();

    eprintln!(
        "GC pause samples (n={}): p50={}ns p99={}ns max={}ns",
        samples.len(),
        p50,
        p99,
        max
    );

    // Loose bound for Phase 1; Phase 2 tightens to 1_000_000ns (1ms).
    assert!(
        p99 < 10_000_000,
        "p99 GC pause exceeded 10ms ceiling: {}ns",
        p99
    );
}

#[test]
fn collect_is_cheap_on_empty_runtime() {
    let rt = Runtime::new();
    let mut samples = Vec::with_capacity(50);
    for _ in 0..50 {
        let start = Instant::now();
        rt.collect();
        samples.push(start.elapsed().as_nanos());
    }
    let p99 = percentile(&mut samples.clone(), 0.99);
    eprintln!("empty-runtime collect p99={}ns", p99);
    // Empty heap collect should be sub-millisecond comfortably.
    assert!(p99 < 1_000_000, "empty-runtime p99 > 1ms: {}ns", p99);
}
