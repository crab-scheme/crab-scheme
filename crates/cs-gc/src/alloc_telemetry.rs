//! Per-allocation byte+count telemetry for the countable-
//! memory variant of `Gc<T>`.
//!
//! Closes Gap A-1 from the unified-memory-architecture
//! follow-on. The countable-memory `Gc::new` is just
//! `Rc::new(value)` with no hooks — so the M5-era
//! `Heap::bytes_allocated_total()` counter that benchmark
//! harnesses query reports 0 forever. This module adds two
//! process-global atomics that `Gc::new` bumps on every
//! allocation, and exposes them via three accessors the
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

/// Cumulative bytes allocated across every `Gc::new` call on
/// this process since program start. Bumped by `Gc::new<T>` by
/// `size_of::<T>()` plus a fixed Rc-header overhead constant.
pub(crate) static BYTES_ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Cumulative `Gc::new` invocations since process start. The
/// `bytes / count` ratio is the average allocation size — a
/// useful signal for distinguishing many-small from
/// few-large workloads.
pub(crate) static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Approximation of `Rc<T>`'s heap-header overhead — two
/// usize-sized refcount fields (strong + weak). Exact value
/// matches `std::rc::Rc`'s `RcBox` layout: `Cell<usize>` +
/// `Cell<usize>` = 16 bytes on 64-bit, 8 bytes on 32-bit.
/// Static rather than runtime-measured so the counter is
/// cheap.
const RC_HEADER_BYTES: u64 = (2 * std::mem::size_of::<usize>()) as u64;

/// Per-thread accumulator for the two counters. Bundled into
/// one struct (rather than two separate `thread_local!`
/// cells) so a single `Drop` impl flushes both into the
/// global atomics when the thread tears down — two
/// independent thread-locals have no guaranteed relative
/// destruction order, which would risk one flushing after the
/// other is already gone.
struct LocalTelemetry {
    bytes: Cell<u64>,
    count: Cell<u64>,
}

impl Drop for LocalTelemetry {
    fn drop(&mut self) {
        flush(self.bytes.get(), self.count.get());
    }
}

thread_local! {
    static LOCAL: LocalTelemetry = const {
        LocalTelemetry {
            bytes: Cell::new(0),
            count: Cell::new(0),
        }
    };
}

#[inline]
fn flush(bytes: u64, count: u64) {
    if bytes != 0 {
        BYTES_ALLOCATED.fetch_add(bytes, Ordering::Relaxed);
    }
    if count != 0 {
        ALLOC_COUNT.fetch_add(count, Ordering::Relaxed);
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
        flush(c.bytes.replace(0), c.count.replace(0));
    });
}

/// Record an allocation of `T` going through `Gc::new`. Adds
/// `size_of::<T>() + RC_HEADER_BYTES` to a thread-local byte
/// counter and increments a thread-local count counter — no
/// atomic RMW on the hot path. The thread-local pair is
/// folded into the process-global atomics by the accessors,
/// `reset()`, and thread teardown.
#[inline]
pub(crate) fn record_alloc<T>() {
    let bytes = std::mem::size_of::<T>() as u64 + RC_HEADER_BYTES;
    let ok = LOCAL.try_with(|c| {
        c.bytes.set(c.bytes.get() + bytes);
        c.count.set(c.count.get() + 1);
    });
    if ok.is_err() {
        // LOCAL already torn down (called from a TLS
        // destructor after ours ran) — fall back to a direct
        // atomic add so the allocation isn't lost.
        flush(bytes, 1);
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

/// Zero both counters. Bench harnesses call this after
/// warmup so per-iter measurements start from a clean
/// baseline. Mirrors `Heap::reset_stats` from the tracing
/// variant.
pub fn reset() {
    flush_local();
    BYTES_ALLOCATED.store(0, Ordering::Relaxed);
    ALLOC_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests share global state with every other test
    // in the process; they call `reset()` upfront so the
    // delta is deterministic, but two of them running in
    // parallel could see inflated counts. cargo test runs
    // tests in parallel by default — that's fine because
    // we're only asserting "count went up", not exact
    // numbers (except where we measure a delta within one
    // test).
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
        let _g: Gc<i64> = Gc::new(1);
        reset();
        assert_eq!(bytes_allocated_total(), 0);
        assert_eq!(alloc_count_total(), 0);
    }

    #[test]
    fn allocation_size_includes_payload_size() {
        reset();
        let _g: Gc<[u8; 1024]> = Gc::new([0u8; 1024]);
        let bytes = bytes_allocated_total();
        // 1024-byte payload + 16-byte header = 1040 on
        // 64-bit. Allow any value ≥ 1024.
        assert!(bytes >= 1024, "got {bytes} bytes for 1024-byte payload");
    }
}
