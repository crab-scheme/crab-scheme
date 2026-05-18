//! M5 Phase 1 precise tracing GC: `Rc<Slot<T>>`-backed `Gc<T>` plus
//! a stop-the-world mark-sweep collector. Gated on the
//! `countable-memory` feature being **off**; under the new RC-only
//! representation (see `rc_only.rs` and
//! `.spec-workflow/specs/countable-memory/`) this module is excluded
//! from the build.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::{Rc, Weak};

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

    /// Roots — closures that mark their reachable set when called.
    /// Each closure is invoked once per `collect()`.
    roots: RefCell<Vec<Box<dyn Fn(&mut Marker)>>>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge stub for BEAM's `Heap::activate` API. The full
/// implementation in main's lib.rs maintains a per-thread
/// `ACTIVE_HEAP` pointer so `Gc::new` can attribute
/// allocations to the right Heap. Under the countable-memory
/// variant (this branch's default) that machinery lives in
/// `cs_gc::alloc_telemetry` (Gap A-1) as process-global
/// atomics — the tracing variant's per-Heap accounting
/// hasn't been ported yet. Stub returns a no-op guard so
/// `cs_runtime::active::Runtime::with_active`'s tracing-tier
/// code path compiles; per-Heap stats reads still report 0
/// until the port lands.
pub struct ActiveHeapGuard {
    _private: (),
}
impl Drop for ActiveHeapGuard {
    fn drop(&mut self) {}
}

impl Heap {
    /// Stub for BEAM's `Heap::activate` — see `ActiveHeapGuard`
    /// docs. Tracing-variant integration is a follow-on port;
    /// the countable-memory variant uses `cs_gc::alloc_telemetry`
    /// (process-global) instead of per-Heap active-heap
    /// thread-local.
    pub fn activate(&self) -> ActiveHeapGuard {
        ActiveHeapGuard { _private: () }
    }

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
    }

    /// Set the auto-collect threshold (number of allocations between
    /// automatic collections). Default is 4096.
    pub fn set_threshold(&self, n: usize) {
        self.threshold.set(n);
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
}
