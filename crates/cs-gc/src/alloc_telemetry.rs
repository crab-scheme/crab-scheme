//! Per-allocation byte+count telemetry for the countable-
//! memory variant of `Gc<T>`.
//!
//! Closes Gap A-1 from the unified-memory-architecture
//! follow-on. The countable-memory `Gc::new` is just
//! `Rc::new(value)` with no hooks — so the M5-era
//! `Heap::bytes_allocated_total()` counter that benchmark
//! harnesses query reports 0 forever. This module adds
//! process-global atomics that `Gc::new` bumps on every
//! allocation, and exposes them via accessors the
//! `cs-runtime::b_gc_stats` countable-memory arm consumes.
//!
//! ## Cost
//!
//! `Gc::new` bumps a pair of thread-local `Cell<u64>`s (no
//! atomic RMW on the hot path); the accessors and the
//! per-thread `Drop` guard fold the thread-local pair into
//! the process-global atomics. Always on under
//! `feature = "countable-memory"`; no separate feature flag
//! since the cost is negligible and the value (every benchmark
//! reports real numbers) is high.
//!
//! ## Deallocation tracking (cs-i6p.1)
//!
//! Symmetric bytes/count atomics on the dealloc side, bumped
//! from `Gc<T>`'s `Drop` impl (and `Gc::into_inner`) only when
//! the drop is actually the *last* strong reference — i.e.
//! only when the `RcBox` is really about to free. Intermediate
//! clone-drops (strong count still > 1 afterwards) don't touch
//! the counters, matching the fact that `record_alloc` only
//! fires once per `Rc::new`, not once per `Gc<T>` clone.
//! `live_bytes()`/`live_count()` are `alloc - dealloc`
//! (saturating — see their doc comments for why that can't
//! actually go negative on the Rc arm).
//!
//! **Region arm excluded.** `Gc::new_in` (the region-backed
//! constructor in `rc_only.rs`) never calls `record_alloc` —
//! region allocations were never counted on the alloc side to
//! begin with (bump-arena slots don't have a natural "freed"
//! moment the way an `Rc`'s last-drop does, and
//! `Region::allocated_bytes()` already exposes the arena's own
//! monotonic byte count for callers that want it). Recording
//! *dealloc* at region-drop without a matching *alloc* would
//! make `live = alloc - dealloc` go negative for any program
//! that mixes Rc- and region-backed `Gc<T>`, which is worse
//! than not tracking regions at all. So region slots stay
//! outside this module on both sides, symmetrically.
//!
//! ## Why a global counter
//!
//! CrabScheme is single-threaded today and runs one Runtime
//! per process. A static atomic is the simplest correct
//! structure; per-Runtime counters become interesting only
//! when multi-tenant embedding is on the table, which it
//! isn't. The accessors expose monotonic-since-process-start
//! values; the runtime's `Heap::reset_stats`-equivalent
//! (under tracing) snapshots a baseline and subtracts.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "alloc-histogram")]
pub mod histogram;

/// Cumulative bytes allocated across every `Gc::new` call on
/// this process since program start. Bumped by `Gc::new<T>` by
/// `size_of::<T>()` plus a fixed Rc-header overhead constant.
pub(crate) static BYTES_ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Cumulative `Gc::new` invocations since process start. The
/// `bytes / count` ratio is the average allocation size — a
/// useful signal for distinguishing many-small from
/// few-large workloads.
pub(crate) static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Cumulative bytes freed across every last-strong-reference
/// `Gc<T>` drop (or `Gc::into_inner`) since process start.
/// Mirrors `BYTES_ALLOCATED`'s accounting exactly — same
/// `size_of::<T>() + RC_HEADER_BYTES` formula — so `alloc -
/// dealloc` is a meaningful live-byte count rather than
/// comparing apples to oranges.
pub(crate) static BYTES_DEALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Cumulative count of last-strong-reference `Gc<T>` drops
/// (the RcBox actually freeing) since process start.
pub(crate) static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Approximation of `Rc<T>`'s heap-header overhead — two
/// usize-sized refcount fields (strong + weak). Exact value
/// matches `std::rc::Rc`'s `RcBox` layout: `Cell<usize>` +
/// `Cell<usize>` = 16 bytes on 64-bit, 8 bytes on 32-bit.
/// Static rather than runtime-measured so the counter is
/// cheap.
const RC_HEADER_BYTES: u64 = (2 * std::mem::size_of::<usize>()) as u64;

/// Per-thread accumulator for the four counters. Bundled into
/// one struct (rather than four separate `thread_local!`
/// cells) so a single `Drop` impl flushes all of them into the
/// global atomics when the thread tears down — independent
/// thread-locals have no guaranteed relative destruction
/// order, which would risk one flushing after the other is
/// already gone.
struct LocalTelemetry {
    alloc_bytes: Cell<u64>,
    alloc_count: Cell<u64>,
    dealloc_bytes: Cell<u64>,
    dealloc_count: Cell<u64>,
}

impl Drop for LocalTelemetry {
    fn drop(&mut self) {
        flush(
            self.alloc_bytes.get(),
            self.alloc_count.get(),
            self.dealloc_bytes.get(),
            self.dealloc_count.get(),
        );
    }
}

thread_local! {
    static LOCAL: LocalTelemetry = const {
        LocalTelemetry {
            alloc_bytes: Cell::new(0),
            alloc_count: Cell::new(0),
            dealloc_bytes: Cell::new(0),
            dealloc_count: Cell::new(0),
        }
    };
}

#[inline]
fn flush(alloc_bytes: u64, alloc_count: u64, dealloc_bytes: u64, dealloc_count: u64) {
    if alloc_bytes != 0 {
        BYTES_ALLOCATED.fetch_add(alloc_bytes, Ordering::Relaxed);
    }
    if alloc_count != 0 {
        ALLOC_COUNT.fetch_add(alloc_count, Ordering::Relaxed);
    }
    if dealloc_bytes != 0 {
        BYTES_DEALLOCATED.fetch_add(dealloc_bytes, Ordering::Relaxed);
    }
    if dealloc_count != 0 {
        DEALLOC_COUNT.fetch_add(dealloc_count, Ordering::Relaxed);
    }
}

/// Fold this thread's pending local counters into the global
/// atomics and zero them. `try_with` rather than `with`
/// because this can run from a context where `LOCAL` itself
/// is already torn down (e.g. another TLS destructor running
/// after `LOCAL`'s in unspecified inter-thread_local order);
/// in that case there's nothing pending to flush.
#[inline]
fn flush_local() {
    let _ = LOCAL.try_with(|c| {
        flush(
            c.alloc_bytes.replace(0),
            c.alloc_count.replace(0),
            c.dealloc_bytes.replace(0),
            c.dealloc_count.replace(0),
        );
    });
}

/// Record an allocation of `T` going through `Gc::new`. Adds
/// `size_of::<T>() + RC_HEADER_BYTES` to a thread-local byte
/// counter and increments a thread-local count counter — no
/// atomic RMW on the hot path. The thread-local values are
/// folded into the process-global atomics by the accessors,
/// `reset()`, and thread teardown.
#[inline]
pub(crate) fn record_alloc<T>() {
    #[cfg(feature = "alloc-histogram")]
    histogram::record::<T>();
    let bytes = std::mem::size_of::<T>() as u64 + RC_HEADER_BYTES;
    let ok = LOCAL.try_with(|c| {
        c.alloc_bytes.set(c.alloc_bytes.get() + bytes);
        c.alloc_count.set(c.alloc_count.get() + 1);
    });
    if ok.is_err() {
        // LOCAL already torn down (called from a TLS
        // destructor after ours ran) — fall back to a direct
        // atomic add so the allocation isn't lost.
        flush(bytes, 1, 0, 0);
    }
}

/// Record a deallocation of `T` — called from `Gc<T>`'s `Drop`
/// impl (and `Gc::into_inner`) exactly when the drop is the
/// last strong reference, i.e. exactly when `record_alloc<T>`
/// would have logically been "undone". Uses the identical
/// `size_of::<T>() + RC_HEADER_BYTES` formula so bytes
/// reported here always subtract cleanly from
/// `bytes_allocated_total()`.
#[inline]
pub(crate) fn record_dealloc<T>() {
    let bytes = std::mem::size_of::<T>() as u64 + RC_HEADER_BYTES;
    let ok = LOCAL.try_with(|c| {
        c.dealloc_bytes.set(c.dealloc_bytes.get() + bytes);
        c.dealloc_count.set(c.dealloc_count.get() + 1);
    });
    if ok.is_err() {
        flush(0, 0, bytes, 1);
    }
}

/// Cumulative bytes since process start (or since the last
/// `reset()` call). Flushes this thread's pending local
/// counter first, then a cheap atomic load.
pub fn bytes_allocated_total() -> u64 {
    flush_local();
    BYTES_ALLOCATED.load(Ordering::Relaxed)
}

/// Cumulative allocation count. Flushes this thread's pending
/// local counter first, then a cheap atomic load.
pub fn alloc_count_total() -> u64 {
    flush_local();
    ALLOC_COUNT.load(Ordering::Relaxed)
}

/// Cumulative bytes freed since process start (or since the
/// last `reset()` call).
pub fn bytes_deallocated_total() -> u64 {
    flush_local();
    BYTES_DEALLOCATED.load(Ordering::Relaxed)
}

/// Cumulative deallocation count.
pub fn dealloc_count_total() -> u64 {
    flush_local();
    DEALLOC_COUNT.load(Ordering::Relaxed)
}

/// Bytes currently live: `alloc - dealloc`. Saturating because
/// the two sides are only guaranteed non-negative when read
/// under a single flush — `flush_local` folds this thread's
/// pending pair before each load, but a *different* thread's
/// dealloc can still be mid-flight relative to this thread's
/// alloc read in a genuinely multi-threaded embedding.
/// CrabScheme runs one Runtime per process today (see the
/// module doc), so in practice this is exact.
pub fn live_bytes() -> u64 {
    bytes_allocated_total().saturating_sub(bytes_deallocated_total())
}

/// Allocations currently live: `alloc_count - dealloc_count`.
/// See [`live_bytes`] for the saturating-subtraction rationale.
pub fn live_count() -> u64 {
    alloc_count_total().saturating_sub(dealloc_count_total())
}

/// Zero all four counters. Bench harnesses call this after
/// warmup so per-iter measurements start from a clean
/// baseline. Mirrors `Heap::reset_stats` from the tracing
/// variant.
pub fn reset() {
    flush_local();
    BYTES_ALLOCATED.store(0, Ordering::Relaxed);
    ALLOC_COUNT.store(0, Ordering::Relaxed);
    BYTES_DEALLOCATED.store(0, Ordering::Relaxed);
    DEALLOC_COUNT.store(0, Ordering::Relaxed);
    #[cfg(feature = "alloc-histogram")]
    histogram::reset();
}

/// Test-only helper (cs-i6p.1): re-invoke this same test binary
/// as a fresh subprocess, running exactly one (possibly
/// `#[ignore]`d) test by its full path.
///
/// Every unit test across this crate that calls `Gc::new`/drop
/// bumps the *same* process-global atomics this module exposes
/// (see the module doc's "why a global counter" section) —
/// `cargo test`'s default thread-per-test parallelism means an
/// exact-value assertion (`reset()` then `assert_eq!(..., 0)`,
/// or a before/after pair that must NOT change) can be falsified
/// by an unrelated test's allocation landing in the same window.
/// A fresh process has its own copy of every `static`, so running
/// the real check there — instead of inline in the parallel
/// suite — makes the assertion deterministic without slowing
/// down or serializing anything else. Verified: the exact same
/// checks below reliably fail under normal parallel `cargo test`
/// but never under `--test-threads=1`, confirming this is
/// cross-test interference on shared statics, not a logic bug.
#[cfg(test)]
pub(crate) fn run_isolated(test_path: &str) {
    let exe = std::env::current_exe().expect("run_isolated: current_exe");
    let status = std::process::Command::new(exe)
        .args(["--exact", "--include-ignored", test_path])
        .status()
        .expect("run_isolated: failed to spawn subprocess");
    assert!(
        status.success(),
        "isolated test {test_path} failed in its subprocess (see output above)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The first two tests below only assert monotonic deltas
    // ("this call's own contribution pushed the counter up by
    // at least X") — safe under `cargo test`'s default
    // parallelism, since concurrent unrelated allocations can
    // only add to a shared counter, never subtract. Every test
    // past them needs an *exact* value (post-`reset()`, or a
    // before/after pair that must NOT move) and runs itself via
    // [`run_isolated`] in a fresh subprocess instead — see that
    // function's doc for why.
    use crate::Gc;

    #[test]
    fn alloc_counter_bumps_on_gc_new() {
        let before = alloc_count_total();
        let _g: Gc<i64> = Gc::new(42);
        let after = alloc_count_total();
        assert!(after > before, "count={before} → {after}");
    }

    #[test]
    fn bytes_counter_bumps_on_gc_new() {
        let before = bytes_allocated_total();
        let _g: Gc<i64> = Gc::new(0);
        let after = bytes_allocated_total();
        // i64 is 8 bytes + 16-byte Rc header = 24 bytes on
        // 64-bit. Just assert the delta is at least
        // `size_of::<i64>()` so the test works on 32-bit too.
        assert!(
            after - before >= std::mem::size_of::<i64>() as u64,
            "delta={}",
            after - before
        );
    }

    #[test]
    fn reset_zeroes_both_counters() {
        run_isolated("alloc_telemetry::tests::reset_zeroes_both_counters_isolated");
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn reset_zeroes_both_counters_isolated() {
        let _g: Gc<i64> = Gc::new(1);
        reset();
        assert_eq!(bytes_allocated_total(), 0);
        assert_eq!(alloc_count_total(), 0);
    }

    #[test]
    fn allocation_size_includes_payload_size() {
        run_isolated("alloc_telemetry::tests::allocation_size_includes_payload_size_isolated");
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn allocation_size_includes_payload_size_isolated() {
        reset();
        let _g: Gc<[u8; 1024]> = Gc::new([0u8; 1024]);
        let bytes = bytes_allocated_total();
        // 1024-byte payload + 16-byte header = 1040 on
        // 64-bit. Allow any value ≥ 1024.
        assert!(bytes >= 1024, "got {bytes} bytes for 1024-byte payload");
    }

    #[test]
    fn dealloc_counter_bumps_when_last_ref_drops() {
        run_isolated("alloc_telemetry::tests::dealloc_counter_bumps_when_last_ref_drops_isolated");
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn dealloc_counter_bumps_when_last_ref_drops_isolated() {
        reset();
        let g: Gc<i64> = Gc::new(7);
        assert_eq!(dealloc_count_total(), 0);
        drop(g);
        assert_eq!(dealloc_count_total(), 1);
        assert_eq!(bytes_deallocated_total(), bytes_allocated_total());
    }

    #[test]
    fn dealloc_counter_does_not_bump_on_clone_drop() {
        run_isolated(
            "alloc_telemetry::tests::dealloc_counter_does_not_bump_on_clone_drop_isolated",
        );
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn dealloc_counter_does_not_bump_on_clone_drop_isolated() {
        reset();
        let g: Gc<i64> = Gc::new(7);
        let g2 = g.clone();
        drop(g2);
        // Two live strong refs became one; the RcBox is still
        // alive, so nothing should have been "deallocated".
        assert_eq!(dealloc_count_total(), 0);
        drop(g);
        assert_eq!(dealloc_count_total(), 1);
    }

    #[test]
    fn live_reflects_alloc_minus_dealloc() {
        run_isolated("alloc_telemetry::tests::live_reflects_alloc_minus_dealloc_isolated");
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn live_reflects_alloc_minus_dealloc_isolated() {
        reset();
        assert_eq!(live_count(), 0);
        assert_eq!(live_bytes(), 0);
        let g: Gc<i64> = Gc::new(1);
        assert_eq!(live_count(), 1);
        assert!(live_bytes() > 0);
        drop(g);
        assert_eq!(live_count(), 0);
        assert_eq!(live_bytes(), 0);
    }

    #[test]
    fn alloc_always_at_least_dealloc() {
        run_isolated("alloc_telemetry::tests::alloc_always_at_least_dealloc_isolated");
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn alloc_always_at_least_dealloc_isolated() {
        reset();
        let mut handles = Vec::new();
        for i in 0..16i64 {
            handles.push(Gc::new(i));
        }
        // Drop half.
        handles.truncate(8);
        assert!(alloc_count_total() >= dealloc_count_total());
        assert!(bytes_allocated_total() >= bytes_deallocated_total());
        drop(handles);
        assert!(alloc_count_total() >= dealloc_count_total());
    }
}
