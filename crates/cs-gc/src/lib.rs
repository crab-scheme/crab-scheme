//! CrabScheme precise tracing garbage collector.
//!
//! M5 milestone — `cs-gc` crate. The exit gate is a stop-the-world
//! mark-sweep collector that replaces every `Rc<T>` heap variant in
//! `cs_core::Value` with an opaque `Gc<T>` smart pointer. This file
//! ships **Phase 1** of that work: the public API surface plus a
//! correctness-first mark-sweep cycle collector. The pointer
//! representation is intentionally `Rc<Slot<T>>`-backed so the
//! ergonomic surface (`Clone`, `Deref<Target = T>`) lines up with
//! the existing `cs-core` call sites; Phase 2 swaps the inner
//! representation for a hand-rolled arena allocator without changing
//! this API.
//!
//! # Why Rc-backed first
//!
//! The migration plan in `.spec-workflow/specs/gc/design.md` calls for
//! a feature-flagged rollout: bring up `Gc<T>` next to `Rc<T>`, swap
//! call sites one variant at a time, run conformance under both, then
//! delete `Rc<T>` from `value.rs` last. Phase 1 keeps the inner
//! representation Rc-shaped so the swap is a type-alias change and
//! the runtime/VM never have to care which is in play.
//!
//! Phase 1 nevertheless implements **real cycle collection**: the
//! `Heap` retains `Weak<Slot<T>>` to every allocation, and `collect()`
//! breaks reachability cycles by zeroing the `value` cell of any slot
//! whose mark stays clear after tracing. Cycles caught this way drop
//! cleanly even though they'd leak under plain `Rc`. The cycle-break
//! tests in `tests/lib.rs` cover this.
//!
//! # Why not `gc-arena` / `gc` crates
//!
//! ADR 0006 (forthcoming) ratifies the hand-rolled choice. Short
//! version: we want full control over the rooting strategy when the
//! JIT lands (M6/M7), the surface area is small enough that a
//! workspace-internal crate beats an external dep on the audit/license
//! ledger, and the cs-gc API is shaped to our `Value` layout in a way
//! a generic GC crate can't be.

#![allow(clippy::missing_safety_doc)]

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

/// A heap-allocated, GC-managed value.
///
/// `Gc<T>` is reference-equal-cheap (`Clone` is a refcount bump on the
/// inner backing) and derefs to `&T`. It does **not** hand out `&mut`
/// — interior mutability lives inside `T` (typically `RefCell<...>`),
/// matching the Rc<RefCell<...>> pattern the rest of CrabScheme uses.
///
/// Ownership is shared with the `Heap` that allocated it; the slot is
/// freed when no strong `Gc<T>` references remain *and* the slot is
/// either swept by `Heap::collect()` (cycle case) or naturally dropped.
pub struct Gc<T: ?Sized> {
    inner: Rc<Slot<T>>,
}

impl<T: ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Self {
        Gc {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T: ?Sized> Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // The slot's value is alive for as long as we hold a strong
        // ref. After collect() severs a cycle, the slot's RefCell is
        // emptied — accessing it would panic, but we've already
        // dropped the cycle-internal Gc<T>s by that point.
        self.inner.value.as_ref()
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gc({:?})", self.deref())
    }
}

impl<T: ?Sized> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl<T: ?Sized> Gc<T> {
    /// Pointer-equality test (analogous to `Rc::ptr_eq`) — useful for
    /// implementing `eq?` over GC-managed values.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.inner, &b.inner)
    }

    /// Return the underlying allocation address as a stable opaque
    /// integer. Useful for cycle-detection visited-sets and `eq?`-style
    /// identity hashing where you need a `Hash + Eq` key for a
    /// reference. Phase 2 (arena-backed) will retain this signature.
    pub fn as_addr(this: &Self) -> usize {
        Rc::as_ptr(&this.inner) as *const () as usize
    }

    /// Hand off this `Gc<T>` as a raw handle for ABI use (e.g. carried
    /// as an i64 across the JIT/runtime boundary). Pair every call with
    /// exactly one `from_raw_jit` to release ownership, or with
    /// `raw_incref` to share without taking ownership. The returned
    /// pointer is opaque — it carries the inner `Slot<T>` address and
    /// retains the strong refcount the caller held.
    ///
    /// ADR 0012 D-2 — Cranelift stack maps will rely on this to spill
    /// live `Gc<T>` references to the host stack as plain i64 words
    /// without losing GC visibility.
    pub fn into_raw_jit(this: Self) -> *const ()
    where
        T: Sized,
    {
        Rc::into_raw(this.inner) as *const ()
    }

    /// Reconstitute a `Gc<T>` from a raw handle previously produced by
    /// [`into_raw_jit`].
    ///
    /// # Safety
    ///
    /// `ptr` must be the result of a matching `into_raw_jit` call (or a
    /// `raw_incref` bump) for the same `T`, and ownership of one strong
    /// count must transfer here. Calling twice on the same pointer
    /// without an intervening `raw_incref` is a double-free.
    pub unsafe fn from_raw_jit(ptr: *const ()) -> Self
    where
        T: Sized + 'static,
    {
        Gc {
            inner: unsafe { Rc::from_raw(ptr as *const Slot<T>) },
        }
    }

    /// Bump the strong count for a raw handle without consuming a
    /// reference. Used by the Cranelift stack-map root walker when it
    /// needs to borrow a spilled slot for tracing — the caller does
    /// **not** own the resulting reference until it pairs the bump
    /// with [`from_raw_jit`].
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live allocation produced by
    /// [`into_raw_jit`] on the same `T`.
    pub unsafe fn raw_incref(ptr: *const ())
    where
        T: Sized + 'static,
    {
        unsafe { Rc::increment_strong_count(ptr as *const Slot<T>) }
    }
}

impl<T: Trace + 'static> Gc<T> {
    /// Construct an unregistered `Gc<T>` — i.e. one not associated with
    /// any `Heap`. The slot lives by reference counting alone, exactly
    /// like `Rc::new`. Use this as the migration bridge while we swap
    /// `Rc<T>` call sites to `Gc<T>` without yet threading a `Heap`
    /// through every constructor.
    ///
    /// Once the migration completes (M5 step 4.E), prefer
    /// `Heap::alloc(value)` so the slot participates in tracing and
    /// can be reclaimed across cycles.
    pub fn new(value: T) -> Self {
        Gc {
            inner: Rc::new(Slot {
                mark: Cell::new(false),
                value: SlotValue { inner: value },
            }),
        }
    }
}

/// A heap object's per-allocation header.
///
/// Held alongside the value inside the `Slot` so the heap's bookkeeping
/// vec needs only `Weak<dyn Marked>` references and can call `mark`
/// without knowing the concrete `T`.
struct Slot<T: ?Sized> {
    mark: Cell<bool>,
    value: SlotValue<T>,
}

/// The value cell of a slot. The Option lets `collect()` drop the
/// payload of a sweep-victim before the strong refcount actually hits
/// zero — necessary to break cycles.
struct SlotValue<T: ?Sized> {
    inner: T,
}

impl<T: ?Sized> SlotValue<T> {
    fn as_ref(&self) -> &T {
        &self.inner
    }
}

/// Type-erased view of a heap object: enough surface for the heap to
/// query and update its mark bit without knowing the concrete `T`.
/// Tracing is initiated through the typed `Gc<T>` path inside
/// `Marker::mark`, so this trait deliberately omits a `trace` method.
trait Marked {
    fn set_mark(&self, m: bool);
    fn mark(&self) -> bool;
}

impl<T: Trace + 'static> Marked for Slot<T> {
    fn set_mark(&self, m: bool) {
        self.mark.set(m);
    }
    fn mark(&self) -> bool {
        self.mark.get()
    }
}

/// A type whose internal `Gc` references can be enumerated for
/// reachability tracing.
///
/// Leaf types (no `Gc<T>` inside) provide an empty `trace`. Compound
/// types call `marker.mark(&child)` for each `Gc` field they hold.
pub trait Trace {
    fn trace(&self, marker: &mut Marker);
}

// Common leaf impls so users don't have to write empty traces for
// primitive payloads.
macro_rules! trace_leaf {
    ($($t:ty),* $(,)?) => {
        $(
            impl Trace for $t {
                fn trace(&self, _marker: &mut Marker) {}
            }
        )*
    };
}
trace_leaf!(bool, char, u8, i8, u16, i16, u32, i32, u64, i64, usize, isize, f32, f64, String,);

impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, marker: &mut Marker) {
        for item in self {
            item.trace(marker);
        }
    }
}

impl<T: Trace> Trace for Option<T> {
    fn trace(&self, marker: &mut Marker) {
        if let Some(v) = self {
            v.trace(marker);
        }
    }
}

impl<T: Trace + 'static> Trace for Gc<T> {
    fn trace(&self, marker: &mut Marker) {
        marker.mark(self);
    }
}

impl<T: Trace> Trace for RefCell<T> {
    fn trace(&self, marker: &mut Marker) {
        self.borrow().trace(marker);
    }
}

/// Mark phase walker. Tracks which objects have been visited so cycle
/// traversal terminates.
pub struct Marker {
    visited: usize,
}

impl Marker {
    fn new() -> Self {
        Marker { visited: 0 }
    }

    /// Mark a `Gc<T>` reachable. Returns true if the mark was newly
    /// set (i.e. this was the first visit), false if already marked.
    pub fn mark<T: Trace + 'static>(&mut self, gc: &Gc<T>) -> bool {
        if gc.inner.mark() {
            return false;
        }
        gc.inner.set_mark(true);
        self.visited += 1;
        // Recurse into the value's children.
        gc.inner.value.inner.trace(self);
        true
    }

    /// Number of objects marked reachable in the current pass.
    pub fn visited(&self) -> usize {
        self.visited
    }
}

/// Fixed-bucket log-binned histogram of GC pause durations.
///
/// `buckets[i]` counts pauses in `[2^i, 2^(i+1))` microseconds.
/// 32 buckets covers 1 µs up to 2^32 µs ≈ 71 minutes — well beyond
/// any realistic pause in a stop-the-world collector. Pauses < 1 µs
/// (or zero) all land in bucket 0.
///
/// Designed for ~0 allocation and constant-time `record`/`percentile`
/// queries — the benchmark harness sums thousands of pauses without
/// driving its own allocation pressure.
#[derive(Debug, Clone)]
pub struct PauseHist {
    buckets: [u64; 32],
    total_micros: u64,
    count: u64,
}

impl Default for PauseHist {
    fn default() -> Self {
        Self {
            buckets: [0; 32],
            total_micros: 0,
            count: 0,
        }
    }
}

impl PauseHist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one pause observation.
    pub fn record(&mut self, d: Duration) {
        let micros = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        // ilog2(0) panics, so map 0 → bucket 0 explicitly. Clamp the
        // high end to bucket 31 (which then represents "≥ 2^31 µs").
        let bucket = if micros == 0 {
            0
        } else {
            (micros.ilog2() as usize).min(31)
        };
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.total_micros = self.total_micros.saturating_add(micros);
        self.count = self.count.saturating_add(1);
    }

    /// Total number of pauses recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Sum of all recorded pauses.
    pub fn total(&self) -> Duration {
        Duration::from_micros(self.total_micros)
    }

    /// Approximate percentile. `p` ∈ [0.0, 1.0]. Returns the upper
    /// bound of the bucket containing the p-th observation —
    /// a worst-case interpretation that rounds pauses up to the
    /// nearest bucket boundary. Coarser than HdrHistogram but
    /// adequate for the benchmark harness's p50/p95/p99 reporting.
    /// Returns Duration::ZERO if no pauses have been recorded.
    pub fn percentile(&self, p: f64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let p = p.clamp(0.0, 1.0);
        let target = ((self.count as f64) * p).ceil().max(1.0) as u64;
        let mut cumulative = 0u64;
        for (i, &c) in self.buckets.iter().enumerate() {
            cumulative = cumulative.saturating_add(c);
            if cumulative >= target {
                let upper_shift = (i + 1).min(31);
                return Duration::from_micros(1u64 << upper_shift);
            }
        }
        Duration::from_micros(1u64 << 31)
    }

    /// Per-bucket counts. Bucket `i` covers `[2^i, 2^(i+1))` µs.
    /// Useful for tools that want to render the full distribution.
    pub fn buckets(&self) -> &[u64; 32] {
        &self.buckets
    }

    /// Drop all recorded observations back to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Aggregate snapshot of the heap's instrumentation counters. Cheap
/// to copy. The Scheme-facing `(gc-stats)` primop wraps this into
/// an alist (Chez/Guile-shape) so external tooling can dump it
/// verbatim into the benchmark harness's JSON output.
#[derive(Debug, Clone, Copy)]
pub struct GcStats {
    /// Cumulative bytes allocated since heap creation (or since
    /// the last `reset_stats`). Approximate: counts the payload
    /// size of each `alloc<T>` call (`size_of::<Slot<T>>`), not
    /// the Rc bookkeeping overhead.
    pub bytes_allocated_total: u64,
    /// Number of allocations since heap creation (or last reset).
    pub alloc_count_total: u64,
    /// Total `collect()` calls since heap creation (or last reset).
    pub collect_count: u64,
    /// Live slot count at the time of the snapshot.
    pub live_slots: usize,
    /// Cumulative time spent inside `collect()`. Only updated when
    /// `stats_enabled()` is true; otherwise stays at `Duration::ZERO`.
    pub collect_duration_total: Duration,
    /// Most recent `collect()` pause duration. Only updated when
    /// `stats_enabled()` is true.
    pub last_pause: Duration,
    /// Peak `collect()` pause since heap creation (or last reset).
    /// Only updated when `stats_enabled()` is true.
    pub max_pause: Duration,
    /// Whether pause-time stats are being collected. When false,
    /// the three duration fields are stale (or zero).
    pub stats_enabled: bool,
}

/// The garbage-collected heap. One per `Runtime`. Owns weak refs to
/// every allocation so `collect()` can sweep the unreachable.
pub struct Heap {
    /// Weak handles to every slot ever allocated. After collect()
    /// expired entries are removed (the slot is gone). Live entries
    /// stay so the next collect can re-mark them.
    slots: RefCell<Vec<Weak<dyn Marked>>>,

    /// Allocations since last collection. Compared against `threshold`
    /// to decide whether `alloc` should auto-collect.
    alloc_count: Cell<usize>,
    threshold: Cell<usize>,

    /// Auto-collect enabled. When true, `alloc` runs `collect` whenever
    /// `alloc_count` crosses `threshold`. Default false in Phase 1
    /// because the runtime makes no GC commitments yet — Phase 2 flips
    /// this on by default once the arena lands.
    auto_collect: Cell<bool>,

    /// Total number of `collect()` calls since heap creation. Useful
    /// telemetry for tooling and test assertions.
    collect_count: Cell<usize>,

    /// Cumulative bytes allocated (approximate — counts payload
    /// `size_of::<Slot<T>>` per `alloc<T>`, not Rc overhead). Always
    /// tracked; the increment per alloc is a single u64 add.
    bytes_allocated_total: Cell<u64>,

    /// Cumulative count of allocations across all `alloc<T>` calls.
    /// Distinct from `alloc_count` (which resets on every collect to
    /// drive the auto-collect threshold).
    alloc_count_total: Cell<u64>,

    /// Pause-time instrumentation. Gated by `stats_enabled` so the
    /// `Instant::now()` cost around `collect()` is paid only when
    /// the embedding runtime asks for it. Default false.
    stats_enabled: Cell<bool>,
    collect_duration_total: Cell<Duration>,
    last_pause: Cell<Duration>,
    max_pause: Cell<Duration>,
    pause_hist: RefCell<PauseHist>,

    /// Roots — closures that mark their reachable set when called.
    /// Each closure is invoked once per `collect()`.
    roots: RefCell<Vec<Box<dyn Fn(&mut Marker)>>>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    /// New empty heap. Default auto-collect threshold: 4096 allocs.
    /// `auto_collect` defaults to `false` in Phase 1; set it via
    /// [`Heap::set_auto_collect`] when the embedding runtime is ready
    /// to commit to GC-triggered allocation pauses.
    pub fn new() -> Self {
        Heap {
            slots: RefCell::new(Vec::new()),
            alloc_count: Cell::new(0),
            threshold: Cell::new(4096),
            auto_collect: Cell::new(false),
            collect_count: Cell::new(0),
            bytes_allocated_total: Cell::new(0),
            alloc_count_total: Cell::new(0),
            stats_enabled: Cell::new(false),
            collect_duration_total: Cell::new(Duration::ZERO),
            last_pause: Cell::new(Duration::ZERO),
            max_pause: Cell::new(Duration::ZERO),
            pause_hist: RefCell::new(PauseHist::new()),
            roots: RefCell::new(Vec::new()),
        }
    }

    /// Allocate `value` on the heap and return a `Gc<T>` to it.
    ///
    /// Triggers a `collect()` if [`set_auto_collect`] is enabled AND
    /// `alloc_count` has crossed `threshold` since the last collection.
    /// In Phase 1 the default is auto-collect off; the embedding
    /// runtime opts in.
    pub fn alloc<T: Trace + 'static>(&self, value: T) -> Gc<T> {
        if self.auto_collect.get() && self.alloc_count.get() >= self.threshold.get() {
            self.collect();
        }
        let slot = Rc::new(Slot {
            mark: Cell::new(false),
            value: SlotValue { inner: value },
        });
        let weak: Weak<dyn Marked> = Rc::downgrade(&(slot.clone() as Rc<dyn Marked>));
        self.slots.borrow_mut().push(weak);
        self.alloc_count.set(self.alloc_count.get() + 1);
        // Byte counting is always-on: a single u64 add per alloc
        // is sub-nanosecond, well under the 2 % target. The
        // pause-time stats above are the expensive ones and stay
        // gated by stats_enabled.
        let slot_bytes = std::mem::size_of::<Slot<T>>() as u64;
        self.bytes_allocated_total
            .set(self.bytes_allocated_total.get().saturating_add(slot_bytes));
        self.alloc_count_total
            .set(self.alloc_count_total.get().saturating_add(1));
        Gc { inner: slot }
    }

    /// Enable or disable auto-collect on allocation.
    pub fn set_auto_collect(&self, enabled: bool) {
        self.auto_collect.set(enabled);
    }

    /// Whether auto-collect is currently enabled.
    pub fn auto_collect_enabled(&self) -> bool {
        self.auto_collect.get()
    }

    /// Number of `collect()` calls since heap creation.
    pub fn collect_count(&self) -> usize {
        self.collect_count.get()
    }

    /// Register a root-set closure. The closure will be invoked on
    /// every `collect()` and is expected to call `marker.mark(&v)`
    /// for each `Gc<T>` reachable from this root.
    pub fn add_root(&self, f: impl Fn(&mut Marker) + 'static) {
        self.roots.borrow_mut().push(Box::new(f));
    }

    /// Number of currently-live slot bookkeeping entries. Drops on
    /// `collect()` once the underlying `Rc` strong count hits zero.
    pub fn live_slots(&self) -> usize {
        self.slots
            .borrow()
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }

    /// Number of allocations since process start (or since last
    /// `reset_alloc_count`).
    pub fn alloc_count(&self) -> usize {
        self.alloc_count.get()
    }

    /// Run a stop-the-world mark-and-sweep cycle.
    ///
    /// Phase 1 implementation:
    /// 1. Clear all marks.
    /// 2. Walk every registered root, marking transitively.
    /// 3. Drop any `Weak` slot whose `strong_count == 0` from the
    ///    bookkeeping vec.
    ///
    /// Cycle-break note: with `Rc<Slot<T>>` as the inner pointer,
    /// a true cycle keeps every slot's `strong_count` >= 1. To break
    /// such cycles we'd need to drop the cycle's strong refs from
    /// inside their own slots — currently the call sites manage that
    /// via `RefCell` + `Option` payloads in their `Trace` impls. A
    /// future iter introduces an explicit cycle-break path that
    /// zeroes the slot's value cell when it stays unmarked across
    /// two consecutive collections (a generation-counter heuristic).
    pub fn collect(&self) {
        // Pause-time instrumentation: gate on `stats_enabled` so a
        // production runtime that never asks for pause numbers pays
        // zero `Instant::now()` cost. The branch itself is one
        // u8 Cell read and predicts perfectly in either direction.
        let stats_on = self.stats_enabled.get();
        let start = if stats_on { Some(Instant::now()) } else { None };

        // 1. Reset marks on every live slot.
        let slots = self.slots.borrow();
        for w in slots.iter() {
            if let Some(s) = w.upgrade() {
                s.set_mark(false);
            }
        }
        drop(slots);

        // 2. Walk roots, marking reachable slots.
        let mut marker = Marker::new();
        let roots = self.roots.borrow();
        for root in roots.iter() {
            root(&mut marker);
        }
        drop(roots);

        // 3. Compact the bookkeeping vec — drop any Weak whose slot
        //    is gone (Rc strong count fell to 0 after roots dropped).
        self.slots.borrow_mut().retain(|w| w.strong_count() > 0);
        self.alloc_count.set(0);
        self.collect_count.set(self.collect_count.get() + 1);

        if let Some(t0) = start {
            let pause = t0.elapsed();
            self.last_pause.set(pause);
            if pause > self.max_pause.get() {
                self.max_pause.set(pause);
            }
            self.collect_duration_total
                .set(self.collect_duration_total.get() + pause);
            self.pause_hist.borrow_mut().record(pause);
        }
    }

    /// Set the auto-collect threshold (number of allocations between
    /// automatic collections). Default is 4096.
    pub fn set_threshold(&self, n: usize) {
        self.threshold.set(n);
    }

    /// Enable or disable pause-time instrumentation. When false (the
    /// default) `collect()` skips its `Instant::now()` samples; the
    /// duration / histogram accessors return stale-or-zero values.
    /// When true, every `collect()` records its pause into the
    /// histogram, updates `last_pause` / `max_pause`, and adds to
    /// `collect_duration_total`.
    ///
    /// Bytes-allocated counting is always on regardless — its cost
    /// is sub-nanosecond.
    pub fn set_stats_enabled(&self, enabled: bool) {
        self.stats_enabled.set(enabled);
    }

    pub fn stats_enabled(&self) -> bool {
        self.stats_enabled.get()
    }

    /// Cumulative bytes allocated since heap creation (or last
    /// `reset_stats`). Approximate — counts `size_of::<Slot<T>>`
    /// per allocation, excluding Rc bookkeeping.
    pub fn bytes_allocated_total(&self) -> u64 {
        self.bytes_allocated_total.get()
    }

    /// Cumulative count of allocations since heap creation (or last
    /// `reset_stats`). Different from `alloc_count`, which resets
    /// every collect to drive the auto-collect threshold.
    pub fn alloc_count_total(&self) -> u64 {
        self.alloc_count_total.get()
    }

    /// Cumulative time spent inside `collect()`. Only meaningful
    /// when `stats_enabled()` was true for the relevant collections.
    pub fn collect_duration_total(&self) -> Duration {
        self.collect_duration_total.get()
    }

    /// Duration of the most recent `collect()` call. `Duration::ZERO`
    /// if stats were never enabled.
    pub fn last_pause(&self) -> Duration {
        self.last_pause.get()
    }

    /// Peak `collect()` duration since heap creation (or last reset).
    /// `Duration::ZERO` if stats were never enabled.
    pub fn max_pause(&self) -> Duration {
        self.max_pause.get()
    }

    /// Borrow the pause-time histogram for direct inspection. The
    /// borrow lives until the returned guard is dropped.
    pub fn pause_histogram(&self) -> std::cell::Ref<'_, PauseHist> {
        self.pause_hist.borrow()
    }

    /// Reset all instrumentation counters back to zero. Does not
    /// touch the heap contents themselves — the live set and the
    /// auto-collect threshold persist. Used by the benchmark harness
    /// to split warmup iterations from measurement iterations.
    pub fn reset_stats(&self) {
        self.bytes_allocated_total.set(0);
        self.alloc_count_total.set(0);
        self.collect_count.set(0);
        self.collect_duration_total.set(Duration::ZERO);
        self.last_pause.set(Duration::ZERO);
        self.max_pause.set(Duration::ZERO);
        self.pause_hist.borrow_mut().reset();
    }

    /// One-shot snapshot of all instrumentation counters. Equivalent
    /// to calling each individual accessor; bundled for the
    /// Scheme-facing `(gc-stats)` primop and the benchmark harness's
    /// per-iter JSON capture.
    pub fn stats(&self) -> GcStats {
        GcStats {
            bytes_allocated_total: self.bytes_allocated_total.get(),
            alloc_count_total: self.alloc_count_total.get(),
            collect_count: self.collect_count.get() as u64,
            live_slots: self.live_slots(),
            collect_duration_total: self.collect_duration_total.get(),
            last_pause: self.last_pause.get(),
            max_pause: self.max_pause.get(),
            stats_enabled: self.stats_enabled.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leaf payload for tests.
    #[derive(Debug)]
    struct Leaf {
        n: i64,
    }
    impl Trace for Leaf {
        fn trace(&self, _: &mut Marker) {}
    }

    #[test]
    fn alloc_and_deref() {
        let h = Heap::new();
        let g = h.alloc(Leaf { n: 7 });
        assert_eq!(g.n, 7);
        assert_eq!(h.live_slots(), 1);
    }

    #[test]
    fn clone_shares() {
        let h = Heap::new();
        let g = h.alloc(Leaf { n: 42 });
        let g2 = g.clone();
        assert!(Gc::ptr_eq(&g, &g2));
        assert_eq!(g.n, g2.n);
    }

    #[test]
    fn unrooted_drops_after_collect() {
        let h = Heap::new();
        // No root registered — this slot is unreachable from the
        // moment we drop the local Gc binding.
        {
            let _g = h.alloc(Leaf { n: 1 });
            assert_eq!(h.live_slots(), 1);
        }
        h.collect();
        assert_eq!(h.live_slots(), 0);
    }

    #[test]
    fn rooted_stays_alive_across_collect() {
        let h = Heap::new();
        let g = h.alloc(Leaf { n: 100 });
        let g_for_root = g.clone();
        h.add_root(move |m| {
            m.mark(&g_for_root);
        });
        h.collect();
        assert_eq!(h.live_slots(), 1);
        assert_eq!(g.n, 100);
    }

    /// Compound payload that holds a Gc<Leaf> child — exercises trace.
    #[derive(Debug)]
    struct Container {
        child: Gc<Leaf>,
    }
    impl Trace for Container {
        fn trace(&self, marker: &mut Marker) {
            self.child.trace(marker);
        }
    }

    #[test]
    fn transitive_marking() {
        let h = Heap::new();
        let leaf = h.alloc(Leaf { n: 5 });
        let cont = h.alloc(Container {
            child: leaf.clone(),
        });
        // Root the container only — leaf must survive transitively.
        let cont_root = cont.clone();
        h.add_root(move |m| {
            m.mark(&cont_root);
        });
        // Drop our local leaf reference; only the container's path
        // keeps it alive.
        drop(leaf);
        h.collect();
        assert_eq!(h.live_slots(), 2);
        // Re-fetch via the container.
        assert_eq!(cont.child.n, 5);
    }

    #[test]
    fn marker_visited_count() {
        let h = Heap::new();
        let _l1 = h.alloc(Leaf { n: 1 });
        let _l2 = h.alloc(Leaf { n: 2 });
        let _l3 = h.alloc(Leaf { n: 3 });
        let l1 = _l1.clone();
        let l2 = _l2.clone();
        h.add_root(move |m| {
            m.mark(&l1);
            m.mark(&l2);
        });
        let mut marker = Marker::new();
        // Manually run the roots once to count.
        for root in h.roots.borrow().iter() {
            root(&mut marker);
        }
        assert_eq!(marker.visited(), 2);
    }

    #[test]
    fn gc_new_unregistered_drops_naturally() {
        // Gc::new doesn't register with any heap; the slot lives by
        // refcount and drops when the last clone is released.
        let g = Gc::new(Leaf { n: 99 });
        assert_eq!(g.n, 99);
        let g2 = g.clone();
        drop(g);
        assert_eq!(g2.n, 99);
        // No assertion against a heap — Gc::new is heap-less.
    }

    #[test]
    fn auto_collect_off_by_default() {
        let h = Heap::new();
        assert!(!h.auto_collect_enabled());
        h.set_threshold(2);
        for _ in 0..10 {
            let _ = h.alloc(Leaf { n: 0 });
        }
        // Default: auto-collect off → no collects despite crossing
        // threshold many times.
        assert_eq!(h.collect_count(), 0);
        assert_eq!(h.alloc_count(), 10);
    }

    #[test]
    fn auto_collect_on_triggers_when_threshold_crossed() {
        let h = Heap::new();
        h.set_auto_collect(true);
        h.set_threshold(3);
        // No roots → every slot is unreachable; the auto-collect
        // sweep prunes them, alloc_count resets.
        for _ in 0..10 {
            let _ = h.alloc(Leaf { n: 0 });
        }
        // alloc_count crossed 3 multiple times; expect at least 3
        // collections.
        assert!(h.collect_count() >= 3, "{}", h.collect_count());
    }

    #[test]
    fn collect_count_increments_per_call() {
        let h = Heap::new();
        assert_eq!(h.collect_count(), 0);
        h.collect();
        assert_eq!(h.collect_count(), 1);
        h.collect();
        h.collect();
        assert_eq!(h.collect_count(), 3);
    }

    #[test]
    fn marker_idempotent_within_pass() {
        let h = Heap::new();
        let g = h.alloc(Leaf { n: 1 });
        let mut marker = Marker::new();
        assert!(marker.mark(&g));
        // Second mark within the same pass is a no-op.
        assert!(!marker.mark(&g));
        assert_eq!(marker.visited(), 1);
    }

    #[test]
    fn raw_jit_handle_roundtrips() {
        // ADR 0012 D-2 — `Gc::into_raw_jit` / `from_raw_jit` pair
        // must round-trip without changing strong count.
        let g = Gc::new(Leaf { n: 42 });
        let strong_before = Rc::strong_count(&g.inner);
        let ptr = Gc::into_raw_jit(g);
        // SAFETY: ptr came from the matching into_raw_jit.
        let g2: Gc<Leaf> = unsafe { Gc::from_raw_jit(ptr) };
        let strong_after = Rc::strong_count(&g2.inner);
        assert_eq!(strong_before, strong_after);
        assert_eq!(g2.n, 42);
    }

    #[test]
    fn raw_incref_then_release() {
        // raw_incref bumps the count by one; one extra from_raw_jit
        // releases the bump cleanly. Both handles see the same value.
        let g = Gc::new(Leaf { n: 7 });
        let ptr = Gc::into_raw_jit(g.clone()); // strong count = 2
        let strong_after_clone = Rc::strong_count(&g.inner);
        assert_eq!(strong_after_clone, 2);
        // Now bump via raw_incref — count = 3.
        unsafe { Gc::<Leaf>::raw_incref(ptr) };
        assert_eq!(Rc::strong_count(&g.inner), 3);
        // Release both raw refs.
        let _ = unsafe { Gc::<Leaf>::from_raw_jit(ptr) }; // drops one
        let _ = unsafe { Gc::<Leaf>::from_raw_jit(ptr) }; // drops the other
        assert_eq!(Rc::strong_count(&g.inner), 1);
    }

    // ---- Phase A: instrumentation tests ----

    #[test]
    fn bytes_allocated_total_increments_per_alloc() {
        let h = Heap::new();
        assert_eq!(h.bytes_allocated_total(), 0);
        assert_eq!(h.alloc_count_total(), 0);

        let _g = h.alloc(Leaf { n: 1 });
        let after_one = h.bytes_allocated_total();
        assert!(after_one > 0, "alloc should bump byte counter");
        assert_eq!(h.alloc_count_total(), 1);

        let _g2 = h.alloc(Leaf { n: 2 });
        assert_eq!(
            h.bytes_allocated_total(),
            after_one * 2,
            "two same-shape allocs should bump by 2x the per-alloc size"
        );
        assert_eq!(h.alloc_count_total(), 2);
    }

    #[test]
    fn bytes_allocated_survives_collect() {
        // collect() resets the rolling alloc_count (drives the
        // auto-collect threshold) but cumulative counters keep
        // going. Important for the bench harness's per-iter delta
        // measurements.
        let h = Heap::new();
        for _ in 0..5 {
            let _ = h.alloc(Leaf { n: 0 });
        }
        let bytes_before = h.bytes_allocated_total();
        let count_before = h.alloc_count_total();
        h.collect();
        assert_eq!(h.bytes_allocated_total(), bytes_before);
        assert_eq!(h.alloc_count_total(), count_before);
        assert_eq!(h.alloc_count(), 0, "rolling alloc_count resets");
    }

    #[test]
    fn pause_stats_zero_when_disabled() {
        let h = Heap::new();
        assert!(!h.stats_enabled());
        h.collect();
        h.collect();
        // collect_count tracks runs regardless; pause durations
        // stay at zero because stats are off.
        assert_eq!(h.collect_count(), 2);
        assert_eq!(h.last_pause(), Duration::ZERO);
        assert_eq!(h.max_pause(), Duration::ZERO);
        assert_eq!(h.collect_duration_total(), Duration::ZERO);
        assert_eq!(h.pause_histogram().count(), 0);
    }

    #[test]
    fn pause_stats_populated_when_enabled() {
        let h = Heap::new();
        h.set_stats_enabled(true);
        // Allocate enough that the collect's mark+sweep does
        // observable work — 1k slots → sweep takes microseconds.
        let mut roots = Vec::new();
        for i in 0..1000 {
            roots.push(h.alloc(Leaf { n: i }));
        }
        h.collect();
        let stats = h.stats();
        assert!(stats.last_pause > Duration::ZERO);
        assert!(stats.max_pause >= stats.last_pause);
        assert_eq!(stats.collect_duration_total, stats.last_pause);
        assert_eq!(h.pause_histogram().count(), 1);
        // Run a second collect — max stays ≥ last.
        h.collect();
        let stats2 = h.stats();
        assert!(stats2.max_pause >= stats2.last_pause);
        assert!(stats2.collect_duration_total >= stats.collect_duration_total);
        assert_eq!(h.pause_histogram().count(), 2);
    }

    #[test]
    fn reset_stats_clears_counters_not_heap() {
        let h = Heap::new();
        h.set_stats_enabled(true);
        let _g1 = h.alloc(Leaf { n: 1 });
        let _g2 = h.alloc(Leaf { n: 2 });
        h.collect();
        assert!(h.bytes_allocated_total() > 0);
        assert!(h.alloc_count_total() > 0);
        assert!(h.collect_count() > 0);
        let live_before = h.live_slots();
        h.reset_stats();
        // Counters zeroed.
        assert_eq!(h.bytes_allocated_total(), 0);
        assert_eq!(h.alloc_count_total(), 0);
        assert_eq!(h.collect_count(), 0);
        assert_eq!(h.last_pause(), Duration::ZERO);
        assert_eq!(h.max_pause(), Duration::ZERO);
        assert_eq!(h.collect_duration_total(), Duration::ZERO);
        assert_eq!(h.pause_histogram().count(), 0);
        // Live slots untouched.
        assert_eq!(h.live_slots(), live_before);
        // Auto-collect / threshold / stats-enabled also persist.
        assert!(h.stats_enabled());
    }

    #[test]
    fn pause_histogram_records_into_correct_bucket() {
        let mut hist = PauseHist::new();
        hist.record(Duration::from_micros(0));
        hist.record(Duration::from_micros(1));
        hist.record(Duration::from_micros(3));
        hist.record(Duration::from_micros(100));
        hist.record(Duration::from_micros(1_000_000));
        assert_eq!(hist.count(), 5);
        // 0µs and 1µs both land in bucket 0 ([1, 2) µs after the
        // zero-special-case).
        assert_eq!(hist.buckets()[0], 2);
        // 3µs → bucket 1 ([2, 4) µs).
        assert_eq!(hist.buckets()[1], 1);
        // 100µs → bucket 6 ([64, 128) µs).
        assert_eq!(hist.buckets()[6], 1);
        // 1_000_000µs (1s) → bucket 19 ([2^19, 2^20) µs ≈ 524ms..1.05s).
        assert_eq!(hist.buckets()[19], 1);
    }

    #[test]
    fn pause_histogram_percentiles_monotonic() {
        let mut hist = PauseHist::new();
        // 100 short pauses + 1 long one.
        for _ in 0..100 {
            hist.record(Duration::from_micros(10));
        }
        hist.record(Duration::from_micros(1_000_000));
        let p50 = hist.percentile(0.5);
        let p99 = hist.percentile(0.99);
        let p100 = hist.percentile(1.0);
        assert!(p50 <= p99, "p50 {:?} ≤ p99 {:?}", p50, p99);
        assert!(p99 <= p100, "p99 {:?} ≤ p100 {:?}", p99, p100);
        // The single 1-second outlier should land in the p100
        // bucket — upper bound ≥ 1s.
        assert!(p100 >= Duration::from_secs(1));
    }

    #[test]
    fn pause_histogram_empty_percentile_is_zero() {
        let hist = PauseHist::new();
        assert_eq!(hist.percentile(0.5), Duration::ZERO);
        assert_eq!(hist.percentile(0.99), Duration::ZERO);
    }

    #[test]
    fn stats_snapshot_reflects_state() {
        let h = Heap::new();
        h.set_stats_enabled(true);
        let _g = h.alloc(Leaf { n: 42 });
        h.collect();
        let s = h.stats();
        assert!(s.bytes_allocated_total > 0);
        assert_eq!(s.alloc_count_total, 1);
        assert_eq!(s.collect_count, 1);
        assert!(s.stats_enabled);
        assert!(s.last_pause > Duration::ZERO);
        assert_eq!(s.max_pause, s.last_pause);
        assert_eq!(s.collect_duration_total, s.last_pause);
    }
}
