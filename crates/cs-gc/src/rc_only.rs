//! Countable-memory representation: `Gc<T>` as a wrapper over
//! a discriminated union of backings.
//!
//! Default (no `regions` feature): `GcRepr` has one variant
//! `Rc(Rc<T>)`. `Gc<T>` is effectively a thin newtype over
//! `Rc<T>` (the compiler collapses the single-variant enum to
//! a transparent wrapper).
//!
//! With `regions` feature on: `GcRepr` adds a `Region` arm —
//! `NonNull<RegionSlot<T>> + RegionId` — pointing into a
//! [`crate::region::Region`] arena. Every Gc operation
//! dispatches on the variant transparently.
//!
//! Gated on `feature = "countable-memory"`. See
//! `.spec-workflow/specs/countable-memory/{requirements,design,tasks}.md`
//! for the layer-2 (RC) story, and
//! `.spec-workflow/specs/region-memory/` for the layer-3
//! (regions) extension.
//!
//! # API parity
//!
//! Every method the M5 Phase 1 `Gc<T>` exposed is preserved
//! here with byte-compatible semantics on the Rc arm:
//! - `new`, `Clone`, `Deref`, `PartialEq`, `Debug`
//! - `ptr_eq`, `as_addr`
//! - `into_raw_jit`, `from_raw_jit`, `raw_incref` — the
//!   Cranelift stack-map ABI (ADR 0012 D-2).
//!
//! Two methods are net-new for the cycle-collector module:
//! - `downgrade(&Self) -> Weak<T>`
//! - `strong_count(&Self) -> usize`
//!
//! And one for the region-memory spec iter 5:
//! - `is_region(&Self) -> bool`

use std::ops::Deref;
use std::rc::{Rc, Weak as RawWeak};

#[cfg(feature = "regions")]
use std::ptr::NonNull;

#[cfg(feature = "regions")]
use crate::region::{assert_region_live, is_region_live, Region, RegionId, RegionSlot};

/// Internal discriminated representation of a `Gc<T>`.
///
/// - `Rc(Rc<T>)`: global reference-counted allocation (layer 2,
///   countable-memory).
/// - `Region { ptr, region_id }` (under `feature = "regions"`):
///   a pointer into a [`crate::region::Region`] arena. The
///   `region_id` identifies the owning region for debug-mode
///   validity checking (iter 3 of the region-memory spec).
///
/// The Region arm holds an in-line per-allocation refcount in
/// `(*ptr).strong` — this exists for JIT raw-handle ABI
/// compatibility and to let [`Gc::strong_count`] report a
/// meaningful value, but **does NOT drive reclamation**. The
/// owning region's drop bulk-frees every allocation regardless
/// of the count.
enum GcRepr<T: ?Sized> {
    Rc(Rc<T>),
    #[cfg(feature = "regions")]
    #[allow(dead_code)] // wired into Gc::new_in in iter 3 of the region-memory spec
    Region {
        ptr: NonNull<RegionSlot<T>>,
        region_id: RegionId,
    },
}

impl<T: ?Sized> Clone for GcRepr<T> {
    fn clone(&self) -> Self {
        match self {
            GcRepr::Rc(rc) => GcRepr::Rc(Rc::clone(rc)),
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, region_id } => {
                // Iter 3: debug-mode use-after-region-drop
                // check. In release this is a no-op; soundness
                // relies on layer-5 escape analysis or manual
                // region discipline.
                assert_region_live(*region_id);
                // SAFETY: while `self` (a `Gc<T>`) exists, the
                // slot is alive (debug-mode check above
                // enforces in dev builds).
                unsafe {
                    let slot = ptr.as_ref();
                    slot.strong.set(
                        slot.strong
                            .get()
                            .checked_add(1)
                            .expect("Gc<T>::clone: region refcount overflow"),
                    );
                }
                GcRepr::Region {
                    ptr: *ptr,
                    region_id: *region_id,
                }
            }
        }
    }
}

/// A heap-allocated, reference-counted value.
///
/// `Gc<T>` wraps a [`GcRepr<T>`] that's either Rc-backed
/// (default; layer 2, countable-memory) or region-backed
/// (under `feature = "regions"`; layer 3, region-memory spec).
/// `Clone` is cheap (refcount bump); `Deref` exposes `&T`.
///
/// Rc-backed Gc reclaims deterministically when the last
/// `Gc<T>` drops. Region-backed Gc reclaims when its owning
/// `Region` drops (bulk free of all the region's allocations),
/// regardless of how many Gc handles remain.
///
/// Cycles are handled outside this type: mutation primitives
/// in `cs-runtime` invoke the synchronous cycle detector
/// (`cs_gc::cycle::check_and_break`) after operations that can
/// construct cycles, and structurally-known cycle shapes use
/// [`Weak`] back-edges. Region-allocated values participate in
/// no cycle detection (region drop handles them) — see iter 5
/// of the region-memory spec.
pub struct Gc<T: ?Sized>(GcRepr<T>);

impl<T: ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Self {
        Gc(self.0.clone())
    }
}

impl<T: ?Sized> Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.0 {
            GcRepr::Rc(rc) => rc,
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, region_id } => {
                // Iter 3: validate the region still exists
                // before dereferencing the in-arena pointer.
                assert_region_live(*region_id);
                // SAFETY: while `self` exists, the slot is
                // alive (debug-mode check above enforces).
                unsafe { &ptr.as_ref().value }
            }
        }
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gc({:?})", self.deref())
    }
}

impl<T: ?Sized> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other)
    }
}

impl<T: ?Sized> Gc<T> {
    /// Pointer-equality test (analogous to `Rc::ptr_eq`) —
    /// useful for implementing `eq?` over GC-managed values.
    /// Returns true iff both handles refer to the same
    /// allocation (across both Rc and Region arms;
    /// inter-variant comparison is always false).
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        match (&a.0, &b.0) {
            (GcRepr::Rc(x), GcRepr::Rc(y)) => Rc::ptr_eq(x, y),
            #[cfg(feature = "regions")]
            (GcRepr::Region { ptr: x, .. }, GcRepr::Region { ptr: y, .. }) => {
                std::ptr::addr_eq(x.as_ptr(), y.as_ptr())
            }
            #[cfg(feature = "regions")]
            _ => false,
        }
    }

    /// Stable opaque integer for the underlying allocation
    /// address. Cycle-detection visited-sets and `eq?`-identity
    /// hashing key off this. Stable across clones; differs
    /// between Rc and Region arms even for "equal" payloads
    /// (the addresses are physically different).
    pub fn as_addr(this: &Self) -> usize {
        match &this.0 {
            GcRepr::Rc(rc) => Rc::as_ptr(rc) as *const () as usize,
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, .. } => ptr.as_ptr() as *const () as usize,
        }
    }

    /// Live strong-count for the underlying allocation. For
    /// Rc-backed Gc, returns `Rc::strong_count`. For region-
    /// backed Gc, returns the in-line refcount header value;
    /// the count is informational only (region drop reclaims
    /// regardless).
    pub fn strong_count(this: &Self) -> usize {
        match &this.0 {
            GcRepr::Rc(rc) => Rc::strong_count(rc),
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, region_id } => {
                assert_region_live(*region_id);
                // SAFETY: while `this` exists, the slot is alive.
                unsafe { ptr.as_ref().strong.get() as usize }
            }
        }
    }

    /// `true` if this `Gc<T>` is region-allocated.
    ///
    /// Used by the cycle detector (`cs-runtime::countable_memory_cycle`)
    /// to skip detection on region-allocated mutation sites —
    /// region cycles reclaim via region drop, not via the
    /// per-mutation detector.
    #[cfg(feature = "regions")]
    pub fn is_region(this: &Self) -> bool {
        matches!(this.0, GcRepr::Region { .. })
    }

    /// Always `false` when the `regions` feature is off.
    #[cfg(not(feature = "regions"))]
    pub fn is_region(_this: &Self) -> bool {
        false
    }
}

impl<T: 'static> Gc<T> {
    /// Downgrade a strong reference to a weak one. The weak
    /// handle does not contribute to the strong count, so any
    /// cycle whose back-edge is `Weak<T>` is reclaimable when
    /// no other strong path reaches its members.
    ///
    /// Bound `T: Sized` because `std::rc::Weak::new()`
    /// requires it (used as the fallback for region-backed
    /// Gc, which has no defined weak-ref semantics — region
    /// drop handles reclamation, not weak edges).
    ///
    /// Note: only Rc-backed `Gc<T>` can be downgraded.
    /// Calling `downgrade` on a region-backed `Gc<T>` panics
    /// in debug builds and returns a non-upgradable Weak in
    /// release. Layer 5 (escape analysis) prevents this from
    /// happening in compiled code; manual region users must
    /// avoid the path themselves.
    pub fn downgrade(this: &Self) -> Weak<T> {
        match &this.0 {
            GcRepr::Rc(rc) => Weak {
                inner: Rc::downgrade(rc),
            },
            #[cfg(feature = "regions")]
            GcRepr::Region { .. } => {
                debug_assert!(
                    false,
                    "Gc<T>::downgrade: region-backed Gc cannot be downgraded \
                     (region drop handles reclamation; weak refs to region \
                     allocations have no defined semantics)"
                );
                Weak {
                    inner: RawWeak::new(),
                }
            }
        }
    }
}

impl<T: 'static> Gc<T> {
    /// Construct a new `Gc<T>` over `value` in the global Rc
    /// heap. Equivalent to `Rc::new`; the allocation has
    /// strong count 1.
    ///
    /// For region-allocated values, use
    /// [`Gc::new_in`](Self::new_in) (region-memory iter 3).
    pub fn new(value: T) -> Self {
        // Layer-4 auto-trigger (tracing-revival iter 4): when
        // the cycle-candidate registry has crossed its
        // threshold, the next allocation runs the sweep
        // before the new alloc lands. Single TLS read on the
        // hot path when the flag is false (the common case).
        #[cfg(feature = "tracing-cycle-collector")]
        if crate::cycle_registry::take_sweep_pending() {
            crate::cycle_registry::run_sweep();
        }
        // Gap A-1: alloc telemetry — one relaxed atomic
        // increment per `Gc::new` so `b_gc_stats` can report
        // real bytes/alloc-count instead of zeros.
        crate::alloc_telemetry::record_alloc::<T>();
        Gc(GcRepr::Rc(Rc::new(value)))
    }

    /// Promote a region-backed `Gc<T>` to Rc-backed (global
    /// heap), severing the dependency on the owning region's
    /// lifetime. No-op on already Rc-backed handles.
    ///
    /// Cloning happens through `T: Clone`. For values
    /// containing inner `Gc<U>` handles (e.g., `Pair` with
    /// region-allocated `car`/`cdr`), `T: Clone` is shallow —
    /// the inner handles are duplicated as region handles, not
    /// promoted. For deep promotion across a Value tree, use
    /// `cs_core::Promote::promote_deep` (layer wraps this on a
    /// per-variant basis).
    ///
    /// Called by layer 5 (escape analysis) at allocation
    /// sites where the value provably escapes its region.
    /// Manual region users invoke this directly when they
    /// know a value needs to outlive its region.
    #[cfg(feature = "regions")]
    pub fn promote(this: &mut Self)
    where
        T: Clone,
    {
        // Only mutate if currently Region-backed.
        if let GcRepr::Region { ptr, region_id } = &this.0 {
            assert_region_live(*region_id);
            // SAFETY: region is alive (just checked); the
            // slot is in its arena and readable.
            let cloned: T = unsafe { ptr.as_ref().value.clone() };
            // Replacing the variant runs Drop on the old
            // GcRepr (decrements the in-arena refcount; the
            // region still owns the slot, so this is fine).
            this.0 = GcRepr::Rc(Rc::new(cloned));
        }
        // Rc arm: nothing to do.
    }

    /// Construct a new `Gc<T>` over `value` allocated in
    /// `region`'s bump arena (layer 3 of the unified memory
    /// management architecture, ADR 0015).
    ///
    /// The returned handle's strong count starts at 1 (stored
    /// in the in-line per-allocation header). Clones bump the
    /// count; drops decrement. **The count does NOT drive
    /// reclamation** — the value lives until `region` drops,
    /// at which point all of the region's allocations free in
    /// one operation.
    ///
    /// # Safety + lifetime contract
    ///
    /// The returned `Gc<T>` must not outlive `region`. Layer
    /// 5 (escape analysis) statically enforces this for
    /// compiled programs; manual region users are
    /// responsible. In debug builds, accessing a `Gc<T>`
    /// whose region has already dropped panics with a clear
    /// diagnostic. In release, the access is undefined
    /// behaviour.
    #[cfg(feature = "regions")]
    pub fn new_in(region: &Region, value: T) -> Self {
        let ptr = region.alloc(value);
        Gc(GcRepr::Region {
            ptr,
            region_id: region.id(),
        })
    }
}

// === JIT raw-handle ABI (ADR 0012 D-2) ===
//
// Cranelift stack maps spill live `Gc<Value>` references to
// the host stack as opaque `i64` words. Each pair of helpers
// matches one Cranelift operation: handing a slot out as a
// raw pointer (and transferring one strong count with it),
// reclaiming it back, or sharing without taking ownership.
//
// For region-backed values, the raw pointer is the
// RegionSlot pointer; into_raw_jit bumps the in-line
// refcount, from_raw_jit decrements. The region's owning
// scope must outlive the JIT-emitted code that holds the
// raw pointer — this is the caller's responsibility (layer
// 5 escape analysis enforces in compiled programs).

impl<T: Sized + 'static> Gc<T> {
    /// Hand off this `Gc<T>` as a raw handle. Pair every call
    /// with exactly one [`from_raw_jit`] (consumes the strong
    /// count, returns a fresh `Gc<T>`) or with [`raw_incref`]
    /// (bumps the strong count for a borrowing observer
    /// without transferring ownership).
    pub fn into_raw_jit(this: Self) -> *const () {
        // Take ownership of the inner GcRepr without running
        // `this`'s Drop — the strong count is transferred to
        // the raw pointer.
        let this = std::mem::ManuallyDrop::new(this);
        // SAFETY: ManuallyDrop suppresses the original Drop;
        // ptr::read moves out the GcRepr exactly once.
        let repr = unsafe { std::ptr::read(&this.0) };
        match repr {
            GcRepr::Rc(rc) => Rc::into_raw(rc) as *const (),
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, .. } => {
                // The in-line refcount was bumped when `this`
                // was created (or last cloned). Transferring
                // it into the raw handle is just a pointer
                // cast — no decrement, no increment.
                ptr.as_ptr() as *const ()
            }
        }
    }

    /// Reconstitute a `Gc<T>` from a raw handle previously
    /// produced by [`into_raw_jit`]. Consumes one strong count
    /// from the allocation.
    ///
    /// # Safety
    ///
    /// `ptr` must be the result of a matching `into_raw_jit`
    /// call (or a `raw_incref` bump) for the same `T`, and
    /// ownership of one strong count must transfer here.
    /// Calling twice without an intervening `raw_incref` is a
    /// double-free.
    ///
    /// **Region caveat**: when the original `into_raw_jit` was
    /// on a region-backed `Gc<T>`, the resulting raw pointer
    /// addresses an in-arena slot. The Rc-vs-Region
    /// distinction is erased at the raw-pointer layer; for
    /// release-mode soundness, the caller must remember which
    /// variant the pointer came from. JIT-emitted code in
    /// CrabScheme always uses the Rc variant (escape-analysis
    /// emits region allocations directly, never through
    /// into_raw_jit/from_raw_jit pairs). To be explicit, this
    /// method always reconstitutes as the Rc variant.
    pub unsafe fn from_raw_jit(ptr: *const ()) -> Self {
        Gc(GcRepr::Rc(unsafe { Rc::from_raw(ptr as *const T) }))
    }

    /// Bump the strong count for a raw handle without
    /// consuming a reference.
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live allocation produced by
    /// [`into_raw_jit`] on the same `T`.
    ///
    /// Always interprets `ptr` as an Rc-backed allocation —
    /// see [`from_raw_jit`] for the rationale (the
    /// JIT/AOT ABI uses Rc only).
    pub unsafe fn raw_incref(ptr: *const ()) {
        unsafe { Rc::increment_strong_count(ptr as *const T) }
    }
}

impl<T: ?Sized> Drop for Gc<T> {
    fn drop(&mut self) {
        match &mut self.0 {
            GcRepr::Rc(_) => {
                // Inner Rc's Drop fires automatically; nothing
                // for us to do here.
            }
            #[cfg(feature = "regions")]
            GcRepr::Region { ptr, region_id } => {
                // Iter 3: skip the slot decrement entirely if
                // the owning region already dropped. The slot
                // memory is already freed by the bump arena;
                // touching `slot.strong` would be UB. (In a
                // well-disciplined program this branch is
                // never taken — but if it is, this is the best
                // we can do in release without panicking.)
                if !is_region_live(*region_id) {
                    return;
                }
                // Decrement the in-line refcount. The region's
                // own Drop handles reclamation; we just bookkeep.
                // SAFETY: region is alive (just checked); slot
                // is in its arena.
                unsafe {
                    let slot = ptr.as_ref();
                    let cur = slot.strong.get();
                    slot.strong.set(cur.saturating_sub(1));
                }
            }
        }
    }
}

/// A weak reference to a `Gc<T>` allocation. Does not
/// contribute to the strong count, so the allocation can be
/// reclaimed while a `Weak<T>` still exists — `upgrade` then
/// returns `None`.
///
/// Currently only supports Rc-backed `Gc<T>`. Region-backed
/// `Gc<T>` cannot be downgraded — see [`Gc::downgrade`].
///
/// # Example: upgrade-after-drop returns `None`
///
/// ```
/// # #[cfg(feature = "countable-memory")] {
/// use cs_gc::Gc;
/// let g = Gc::new(42_i64);
/// let w = Gc::downgrade(&g);
/// assert_eq!(w.upgrade().map(|g| *g), Some(42));
/// drop(g);
/// assert!(w.upgrade().is_none());
/// # }
/// ```
pub struct Weak<T: ?Sized> {
    inner: RawWeak<T>,
}

impl<T: ?Sized> Clone for Weak<T> {
    fn clone(&self) -> Self {
        Weak {
            inner: RawWeak::clone(&self.inner),
        }
    }
}

impl<T: ?Sized> std::fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Weak(<gc>)")
    }
}

impl<T> Default for Weak<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Weak<T> {
    /// Construct a `Weak<T>` that never upgrades — equivalent
    /// to `std::rc::Weak::new()`. Useful as a placeholder
    /// during two-phase letrec/closure allocation.
    pub fn new() -> Self {
        Weak {
            inner: RawWeak::new(),
        }
    }
}

impl<T: ?Sized> Weak<T> {
    /// Attempt to upgrade to a strong `Gc<T>` handle. Returns
    /// `None` if the underlying allocation has been reclaimed.
    pub fn upgrade(&self) -> Option<Gc<T>> {
        self.inner.upgrade().map(|rc| Gc(GcRepr::Rc(rc)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Clone)]
    struct Leaf {
        n: i64,
    }

    #[test]
    fn alloc_and_deref() {
        let g = Gc::new(Leaf { n: 7 });
        assert_eq!(g.n, 7);
        assert_eq!(Gc::strong_count(&g), 1);
    }

    #[test]
    fn clone_shares_strong_count() {
        let g = Gc::new(Leaf { n: 42 });
        let g2 = g.clone();
        assert!(Gc::ptr_eq(&g, &g2));
        assert_eq!(Gc::strong_count(&g), 2);
        drop(g2);
        assert_eq!(Gc::strong_count(&g), 1);
    }

    #[test]
    fn deref_returns_inner() {
        let g = Gc::new(Leaf { n: 13 });
        let r: &Leaf = &g;
        assert_eq!(r.n, 13);
    }

    #[test]
    fn debug_format_wraps_inner() {
        let g = Gc::new(Leaf { n: 1 });
        assert_eq!(format!("{:?}", g), "Gc(Leaf { n: 1 })");
    }

    #[test]
    fn as_addr_is_stable_across_clones() {
        let g = Gc::new(Leaf { n: 9 });
        let g2 = g.clone();
        assert_eq!(Gc::as_addr(&g), Gc::as_addr(&g2));
    }

    #[test]
    fn downgrade_then_upgrade_alive() {
        let g = Gc::new(Leaf { n: 100 });
        let w = Gc::downgrade(&g);
        let g2 = w.upgrade().expect("alive");
        assert_eq!(g2.n, 100);
        assert!(Gc::ptr_eq(&g, &g2));
    }

    #[test]
    fn weak_upgrade_after_drop_returns_none() {
        let g = Gc::new(Leaf { n: 200 });
        let w = Gc::downgrade(&g);
        drop(g);
        assert!(w.upgrade().is_none());
    }

    #[test]
    fn weak_default_never_upgrades() {
        let w: Weak<Leaf> = Weak::default();
        assert!(w.upgrade().is_none());
    }

    #[test]
    fn into_raw_jit_round_trip() {
        // ADR 0012 D-2 — the raw-handle ABI must preserve the
        // strong count across the round trip.
        let g = Gc::new(Leaf { n: 42 });
        assert_eq!(Gc::strong_count(&g), 1);
        let ptr = Gc::into_raw_jit(g);
        // SAFETY: ptr came from the matching into_raw_jit.
        let g2: Gc<Leaf> = unsafe { Gc::from_raw_jit(ptr) };
        assert_eq!(Gc::strong_count(&g2), 1);
        assert_eq!(g2.n, 42);
    }

    #[test]
    fn raw_incref_bumps_then_paired_release() {
        let g = Gc::new(Leaf { n: 7 });
        let ptr = Gc::into_raw_jit(g.clone()); // strong = 2
        assert_eq!(Gc::strong_count(&g), 2);
        // SAFETY: ptr points at a live allocation we own.
        unsafe { Gc::<Leaf>::raw_incref(ptr) };
        assert_eq!(Gc::strong_count(&g), 3);
        // Release both raw refs.
        let _ = unsafe { Gc::<Leaf>::from_raw_jit(ptr) };
        let _ = unsafe { Gc::<Leaf>::from_raw_jit(ptr) };
        assert_eq!(Gc::strong_count(&g), 1);
    }

    #[test]
    fn is_region_false_for_rc_backed() {
        let g = Gc::new(Leaf { n: 0 });
        assert!(!Gc::is_region(&g));
    }

    #[cfg(feature = "regions")]
    #[test]
    fn promote_rc_arm_is_noop() {
        let mut g = Gc::new(Leaf { n: 5 });
        let addr_before = Gc::as_addr(&g);
        Gc::promote(&mut g);
        assert!(!Gc::is_region(&g));
        // Address stable — no replacement, no clone.
        assert_eq!(Gc::as_addr(&g), addr_before);
    }

    #[cfg(feature = "regions")]
    #[test]
    fn promote_region_arm_switches_to_rc() {
        use crate::region::Region;
        let mut g = {
            let region = Region::new();
            let mut g = Gc::new_in(&region, Leaf { n: 99 });
            assert!(Gc::is_region(&g));
            Gc::promote(&mut g);
            assert!(!Gc::is_region(&g));
            // g now points into the global Rc heap; the region
            // can drop while we hold g.
            g
            // region drops here.
        };
        // Touch g after region dropped — would have UB-panicked
        // pre-promote; safe now because promote deep-cloned.
        assert_eq!(g.n, 99);
        assert_eq!(Gc::strong_count(&g), 1);
        // And further promote is a no-op.
        Gc::promote(&mut g);
        assert!(!Gc::is_region(&g));
        assert_eq!(g.n, 99);
    }
}
