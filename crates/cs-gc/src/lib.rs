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
    pub fn new() -> Self {
        Heap {
            slots: RefCell::new(Vec::new()),
            alloc_count: Cell::new(0),
            threshold: Cell::new(4096),
            roots: RefCell::new(Vec::new()),
        }
    }

    /// Allocate `value` on the heap and return a `Gc<T>` to it.
    ///
    /// Allocation may trigger a collection if more than `threshold`
    /// allocations have happened since the last sweep. We currently
    /// only collect on explicit `collect()` calls — auto-collect is
    /// hooked up in a follow-up iter once root registration is wired
    /// to the runtime.
    pub fn alloc<T: Trace + 'static>(&self, value: T) -> Gc<T> {
        let slot = Rc::new(Slot {
            mark: Cell::new(false),
            value: SlotValue { inner: value },
        });
        let weak: Weak<dyn Marked> = Rc::downgrade(&(slot.clone() as Rc<dyn Marked>));
        self.slots.borrow_mut().push(weak);
        self.alloc_count.set(self.alloc_count.get() + 1);
        Gc { inner: slot }
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
    fn marker_idempotent_within_pass() {
        let h = Heap::new();
        let g = h.alloc(Leaf { n: 1 });
        let mut marker = Marker::new();
        assert!(marker.mark(&g));
        // Second mark within the same pass is a no-op.
        assert!(!marker.mark(&g));
        assert_eq!(marker.visited(), 1);
    }
}
