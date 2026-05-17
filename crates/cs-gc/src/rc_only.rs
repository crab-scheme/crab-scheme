//! Countable-memory representation: `Gc<T>` as a thin newtype over
//! `std::rc::Rc<T>`. No `Slot`/mark wrapper; no `Heap`; no
//! `Trace`/`Marker`. Reclamation is pure Rust reference counting,
//! and cycle handling lives in the `cycle` sibling module
//! (introduced in iter 3) plus `Weak<T>` back-edges at known cycle
//! sites (iter 8).
//!
//! Gated on `feature = "countable-memory"`. See
//! `.spec-workflow/specs/countable-memory/{requirements,design,tasks}.md`
//! for the full migration plan.
//!
//! # API parity with the tracing variant
//!
//! Every method the M5 Phase 1 `Gc<T>` exposed is preserved here
//! with byte-compatible semantics:
//! - `new`, `Clone`, `Deref`, `PartialEq`, `Debug`
//! - `ptr_eq`, `as_addr`
//! - `into_raw_jit`, `from_raw_jit`, `raw_incref` — the Cranelift
//!   stack-map ABI (ADR 0012 D-2). Under this representation each
//!   delegates to `Rc::into_raw`/`Rc::from_raw`/
//!   `Rc::increment_strong_count` directly (no `Slot<T>`
//!   indirection).
//!
//! Two methods are net-new for the cycle-collector module:
//! - `downgrade(&Self) -> Weak<T>`
//! - `strong_count(&Self) -> usize`

use std::ops::Deref;
use std::rc::{Rc, Weak as RawWeak};

/// A heap-allocated, reference-counted value.
///
/// `Gc<T>` is a thin newtype around `std::rc::Rc<T>`. `Clone` is an
/// `Rc::clone` (refcount bump); `Deref` exposes `&T`. The slot is
/// reclaimed deterministically when the last `Gc<T>` drops.
///
/// Cycles are handled outside this type: mutation primitives in
/// `cs-runtime` invoke the synchronous cycle detector
/// (`cs_gc::cycle::check_and_break`, introduced in iter 3) after
/// any operation that can construct a cycle, and structurally-
/// known cycle shapes (continuation parent chains, self-referential
/// closures) use [`Weak`] back-edges so cycles never form.
pub struct Gc<T: ?Sized> {
    inner: Rc<T>,
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
        &self.inner
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

    /// Stable opaque integer for the underlying allocation address.
    /// Cycle-detection visited-sets and `eq?`-identity hashing key
    /// off this. Survives the M5 → countable-memory transition: the
    /// tracing variant returned the `Slot<T>` address, this variant
    /// returns the bare `Rc<T>` allocation address — call sites use
    /// the value only for identity, never for arithmetic.
    pub fn as_addr(this: &Self) -> usize {
        Rc::as_ptr(&this.inner) as *const () as usize
    }

    /// Live strong-count for the underlying allocation. Used by the
    /// cycle collector (`cs_gc::cycle`) to detect when an outside
    /// reference still holds the root after walking its transitive
    /// children.
    pub fn strong_count(this: &Self) -> usize {
        Rc::strong_count(&this.inner)
    }
}

impl<T: ?Sized + 'static> Gc<T> {
    /// Downgrade a strong reference to a weak one. The weak handle
    /// does not contribute to the strong count, so any cycle whose
    /// back-edge is `Weak<T>` is reclaimable when no other strong
    /// path reaches its members.
    ///
    /// Used by the iter-8 refactor of `Frame.parent`,
    /// `Continuation`'s parent-frame chain, and
    /// `Closure`/`VmClosure`'s self-referential bindings.
    pub fn downgrade(this: &Self) -> Weak<T> {
        Weak {
            inner: Rc::downgrade(&this.inner),
        }
    }
}

impl<T: 'static> Gc<T> {
    /// Construct a new `Gc<T>` over `value`. Equivalent to
    /// `Rc::new`; the allocation has strong count 1.
    pub fn new(value: T) -> Self {
        Gc {
            inner: Rc::new(value),
        }
    }
}

// === JIT raw-handle ABI (ADR 0012 D-2) ===
//
// Cranelift stack maps spill live `Gc<Value>` references to the
// host stack as opaque `i64` words. Each pair of helpers below
// matches one Cranelift operation: handing a slot out as a raw
// pointer (and transferring one strong count with it), reclaiming
// it back (consuming one strong count), or sharing without taking
// ownership (incrementing the strong count for a borrowing
// observer like the root walker).
//
// The byte-level semantics are identical to the M5 Phase 1 variant
// because both delegate to `Rc::into_raw` / `Rc::from_raw` /
// `Rc::increment_strong_count`. The only difference is the pointee
// type: there `*const Slot<T>`, here `*const T`. Call sites use
// `into_raw_jit` and `from_raw_jit` as a matched pair, so the
// pointee type stays consistent end-to-end and the swap is
// invisible at the ABI surface.

impl<T: Sized + 'static> Gc<T> {
    /// Hand off this `Gc<T>` as a raw handle. Pair every call with
    /// exactly one [`from_raw_jit`] (which consumes the strong
    /// count and returns a fresh `Gc<T>`) or with [`raw_incref`]
    /// (which bumps the strong count for a borrowing observer
    /// without transferring ownership).
    pub fn into_raw_jit(this: Self) -> *const () {
        Rc::into_raw(this.inner) as *const ()
    }

    /// Reconstitute a `Gc<T>` from a raw handle previously produced
    /// by [`into_raw_jit`]. Consumes one strong count from the
    /// allocation.
    ///
    /// # Safety
    ///
    /// `ptr` must be the result of a matching `into_raw_jit` call
    /// (or a `raw_incref` bump) for the same `T`, and ownership of
    /// one strong count must transfer here. Calling twice on the
    /// same pointer without an intervening `raw_incref` is a
    /// double-free.
    pub unsafe fn from_raw_jit(ptr: *const ()) -> Self {
        Gc {
            inner: unsafe { Rc::from_raw(ptr as *const T) },
        }
    }

    /// Bump the strong count for a raw handle without consuming a
    /// reference. Used by the Cranelift stack-map root walker
    /// when it needs to borrow a spilled slot for inspection —
    /// the caller does **not** own the resulting reference until
    /// it pairs the bump with [`from_raw_jit`].
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live allocation produced by
    /// [`into_raw_jit`] on the same `T`.
    pub unsafe fn raw_incref(ptr: *const ()) {
        unsafe { Rc::increment_strong_count(ptr as *const T) }
    }
}

/// A weak reference to a `Gc<T>` allocation. Does not contribute to
/// the strong count, so the allocation can be reclaimed while a
/// `Weak<T>` still exists — `upgrade` then returns `None`.
///
/// Used to break the cycle in `Frame.parent` (the leaf frame holds
/// strong refs; ancestors are walked via `upgrade`), in continuation
/// captures, and in self-referential closure bindings.
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

impl<T> Default for Weak<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Weak<T> {
    /// Construct a `Weak<T>` that never upgrades — equivalent to
    /// `std::rc::Weak::new()`. Useful as a placeholder during
    /// two-phase letrec/closure allocation.
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
        self.inner.upgrade().map(|rc| Gc { inner: rc })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
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
}
