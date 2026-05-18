//! Region-memory: layer 3 of the unified memory management
//! architecture (ADR 0015).
//!
//! A [`Region`] is a bump arena that owns its allocations.
//! Values allocated through [`Region::alloc`] live until the
//! region drops, at which point all of them free in one
//! operation — no per-allocation refcount cycle, no per-object
//! tracing.
//!
//! # When to use a region
//!
//! Region allocation is faster than `Rc<T>` for both
//! per-allocation cost (just a bump pointer increment) and
//! reclamation (one bulk free vs. one `Rc::drop` per object).
//! It's the right choice when:
//!
//! - The allocation's lifetime is provably bounded by some
//!   surrounding dynamic scope (a `let` body, a function call,
//!   a `map`/`filter` pipeline).
//! - The value never escapes its region — i.e., never gets
//!   stored in a longer-lived holder. Layer 5 (escape
//!   analysis) proves this property; without it, manual
//!   region use requires the programmer's own discipline.
//! - Cycles inside the region are fine: the bulk free handles
//!   them regardless of internal references.
//!
//! # Per-allocation header
//!
//! Each region allocation gets an 8-byte in-line header
//! containing a 32-bit refcount. The count exists for ABI
//! compatibility with the JIT raw-handle ABI (ADR 0012 D-2)
//! and to let [`Gc::strong_count`] report a meaningful value
//! on region-allocated handles. The count does NOT drive
//! reclamation — the region's bulk free runs regardless.
//!
//! # Validity check (iter 3 + Gap E-6)
//!
//! A thread-local `LIVE_REGION_IDS` set: every Region-arm
//! `Gc<T>` operation (`Clone`, `Deref`, `strong_count`)
//! checks that the region the value was allocated from is
//! still alive. Use-after-region-drop panics with a clear
//! diagnostic.
//!
//! Iter 3 originally gated this check on
//! `#[cfg(debug_assertions)]` and accepted release-mode UB.
//! Gap E-6 closed that window: the check runs unconditionally
//! in every build. Cost is one HashSet lookup per Region-arm
//! Deref (~5ns on a warm cache). The trade-off is favourable —
//! protecting against UB in production is worth ~5ns/deref,
//! and layer-5 escape analysis (when fully wired) eliminates
//! the path entirely for compiled programs (all Region
//! allocations become statically safe).
//!
//! # Status
//!
//! - **Iter 1** (this file): `Region`, `RegionId`, `RegionSlot<T>`,
//!   `Region::alloc` returning a raw `NonNull<RegionSlot<T>>`.
//!   `Gc::new_in` wiring lands in iter 2 alongside the
//!   `Gc<T>` discriminated-union refactor.
//! - **Iter 2**: `Gc<T>` discriminated union over Rc and
//!   Region backings.
//! - **Iter 3**: `Gc::new_in` + debug-mode validity.
//! - **Iter 4**: `Gc::promote` for escape-to-Rc.
//! - **Iter 5**: cycle-detector region skip.
//! - **Iter 6**: flip default-on + ADR 0016 + exit report.

#![cfg(feature = "regions")]

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use bumpalo::Bump;

/// Per-region identity used to validate region membership in
/// debug builds and to disambiguate handles from sibling
/// regions.
///
/// The id is a 32-bit non-zero counter minted from a global
/// atomic. Sized to u32 so it fits the `region_id` field of
/// [`RegionSlot`] — that field stores the owning region's id
/// inline so `from_raw_jit_region` can reconstruct
/// `GcRepr::Region` from a raw slot pointer alone (no
/// thread-local lookup required). Roll-over after 2³² regions
/// per process is a theoretical concern only — realistically
/// the counter never approaches u32::MAX in any program.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct RegionId(NonZeroU32);

static REGION_ID_COUNTER: AtomicU32 = AtomicU32::new(1);

impl RegionId {
    /// Mint a fresh `RegionId` distinct from every other id
    /// produced this process.
    fn fresh() -> Self {
        let raw = REGION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        // Counter starts at 1 and only ever increments; the
        // NonZero invariant holds.
        RegionId(NonZeroU32::new(raw).expect("RegionId counter overflowed to 0"))
    }

    /// Raw u32 form for diagnostic printing and `RegionSlot`
    /// storage.
    pub fn as_u32(self) -> u32 {
        self.0.get()
    }

    /// Construct a `RegionId` from a raw u32 (as read from a
    /// `RegionSlot`'s in-arena `region_id` field). Returns
    /// `None` if the value is zero (the sentinel for "no
    /// region" / corrupted slot).
    pub fn from_raw_u32(raw: u32) -> Option<Self> {
        NonZeroU32::new(raw).map(RegionId)
    }
}

/// Per-allocation header carrying a region-local strong count.
///
/// The header sits immediately before the value payload. The
/// `strong` count tracks how many `Gc<T>` handles point at this
/// slot (incremented on `Clone`, decremented on `Drop`); the
/// count exists for ABI compatibility with the JIT raw-handle
/// ABI and to let `Gc::strong_count` report a meaningful value.
/// **Reclamation is driven by the owning `Region`'s drop, not
/// by this count reaching zero.**
///
/// Layout: `#[repr(C)]` ensures the header fields come before
/// the payload at predictable offsets. `strong` + `region_id`
/// together are 8 bytes, keeping `value` 8-byte aligned for
/// the common case of `T` containing pointers; types with
/// stricter alignment (>8) are over-aligned by the arena's
/// alignment guarantee.
///
/// `region_id` carries the raw u32 from the owning
/// [`RegionId`]. [`Gc::from_raw_jit_region`] (in `rc_only.rs`)
/// reads this field to reconstruct a Region-arm `GcRepr`
/// from a raw slot pointer alone — needed because the VM's
/// nanbox encoding (low-bit region flag on pointer-typed
/// tags) round-trips raw pointers without preserving the
/// `RegionId` separately.
#[repr(C)]
#[allow(dead_code)] // wired into Gc<T> in iter 2 of the region-memory spec
pub(crate) struct RegionSlot<T: ?Sized> {
    pub(crate) strong: Cell<u32>,
    pub(crate) region_id: u32,
    pub(crate) value: T,
}

/// A bump-allocator arena that owns all values allocated
/// through [`Region::alloc`]. Dropping the region bulk-frees
/// every allocation regardless of outstanding `Gc<T>` handles.
///
/// Single-threaded only (`!Send`, `!Sync`): the
/// `_not_send` marker prevents accidental cross-thread moves.
pub struct Region {
    id: RegionId,
    arena: Bump,
    /// Gap E-7: callbacks to run when the region drops.
    /// Lets non-POD payloads (file handles, FFI resources,
    /// anything with cleanup logic) be safely allocated in
    /// a region — the dtor releases the resource before
    /// bumpalo bulk-frees the buffer underneath. Each dtor
    /// runs once, in LIFO registration order.
    ///
    /// The vec is `RefCell`-wrapped so `register_dtor` can
    /// take `&self` (matching the `Region::alloc` convention).
    /// `!Send` already prevents cross-thread races.
    dtors: std::cell::RefCell<Vec<Box<dyn FnOnce()>>>,
    _not_send: PhantomData<*const ()>,
}

impl Default for Region {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Set of `RegionId`s for regions currently alive on this
    /// thread. `Region::new` inserts; `Region::Drop` removes
    /// **before** the underlying Bump arena frees, so the
    /// validity check in `Gc<T>` methods can detect
    /// use-after-region-drop.
    static LIVE_REGION_IDS: RefCell<HashSet<RegionId>> = RefCell::new(HashSet::new());
}

/// Check that `region_id` corresponds to a region currently
/// alive on this thread. Panics with a clear diagnostic if
/// not.
///
/// **Gap E-6:** runs unconditionally in every build (debug
/// and release) — protecting against use-after-region-drop
/// UB in production is worth the ~5ns/deref cost. Layer-5
/// escape analysis, once fully wired, eliminates this path
/// for compiled programs (all `Gc::new_in` sites become
/// statically safe).
#[inline]
pub(crate) fn assert_region_live(region_id: RegionId) {
    LIVE_REGION_IDS.with(|s| {
        if !s.borrow().contains(&region_id) {
            panic!(
                "cs_gc::Gc<T>: region {region_id:?} dropped while \
                 a handle into it is still outstanding (use-after-region-drop)"
            );
        }
    });
}

/// `true` if `region_id` is currently alive on this thread.
/// Available in both debug and release; used by `Gc::Drop`
/// to skip the slot decrement when the region has already
/// freed (release-mode best-effort to avoid UB).
#[inline]
pub(crate) fn is_region_live(region_id: RegionId) -> bool {
    LIVE_REGION_IDS.with(|s| s.borrow().contains(&region_id))
}

/// Test-only accessor for the live-region-id count. Used by
/// region.rs's own unit tests to verify Region::new/Drop
/// bookkeeping; not exposed beyond the crate.
#[cfg(test)]
pub(crate) fn live_region_count() -> usize {
    LIVE_REGION_IDS.with(|s| s.borrow().len())
}

impl Region {
    /// Create a fresh region with a unique id and an empty
    /// bump arena. The arena grows on demand as allocations
    /// arrive. Registers the region's id with the per-thread
    /// `LIVE_REGION_IDS` set so debug-mode `Gc<T>` operations
    /// can validate that handles into this region are still
    /// in scope.
    pub fn new() -> Self {
        let id = RegionId::fresh();
        LIVE_REGION_IDS.with(|s| {
            s.borrow_mut().insert(id);
        });
        Region {
            id,
            arena: Bump::new(),
            dtors: std::cell::RefCell::new(Vec::new()),
            _not_send: PhantomData,
        }
    }

    /// Gap E-7: register a closure to run when this region
    /// drops. Lets non-POD payloads (file handles, FFI
    /// resources, anything with Drop-like cleanup) be
    /// safely allocated in a region — the dtor releases the
    /// resource before bumpalo bulk-frees the underlying
    /// buffer. Dtors run in LIFO registration order (last
    /// registered runs first).
    ///
    /// Use case: allocating a `Port::FileOutput` (which
    /// holds a file path + buffer to flush on close) in a
    /// region. Register a dtor that flushes + closes the
    /// file when the region drops; otherwise the OS file
    /// handle leaks and the buffered writes are silently
    /// dropped.
    ///
    /// Takes `&self` not `&mut self` to match the `alloc`
    /// convention — the dtor list lives in a RefCell. The
    /// `!Send` constraint prevents cross-thread races.
    pub fn register_dtor(&self, dtor: Box<dyn FnOnce()>) {
        self.dtors.borrow_mut().push(dtor);
    }

    /// The region's per-process unique id.
    pub fn id(&self) -> RegionId {
        self.id
    }

    /// Total bytes allocated through this region's arena (a
    /// monotonically-growing counter; doesn't shrink on
    /// individual allocations being dropped, only on region
    /// drop).
    pub fn allocated_bytes(&self) -> usize {
        self.arena.allocated_bytes()
    }

    /// Allocate `value` in this region and return a
    /// `NonNull<RegionSlot<T>>` pointing at the in-arena slot.
    ///
    /// `pub(crate)` because the public API for callers is
    /// `Gc::new_in(region, value)` (lands in iter 2). The
    /// raw slot pointer is an implementation detail of the
    /// `Gc<T>` discriminated-union representation.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid for reads and writes
    /// until this region drops. Callers must not retain the
    /// pointer past the region's lifetime.
    #[allow(dead_code)] // wired into Gc::new_in in iter 2 of the region-memory spec
    pub(crate) fn alloc<T: 'static>(&self, value: T) -> NonNull<RegionSlot<T>> {
        let slot = self.arena.alloc(RegionSlot {
            strong: Cell::new(1),
            region_id: self.id.as_u32(),
            value,
        });
        // `bumpalo::Bump::alloc` returns `&mut T` with a
        // lifetime tied to the Bump. We convert to NonNull to
        // erase the borrow so callers can hold the pointer
        // across the Region's life; correctness depends on
        // the region outliving every outstanding handle (iter
        // 3 adds debug-mode validation; iter 5 / escape
        // analysis enforce statically).
        NonNull::from(slot)
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // Gap E-7: run registered dtors BEFORE the Bump
        // arena frees the slot memory. Dtors run in LIFO
        // registration order — the last registered cleanup
        // sees the most-recently-allocated state. Each dtor
        // is `FnOnce` so we drain the vec by consuming it.
        let dtors = std::mem::take(&mut *self.dtors.borrow_mut());
        for dtor in dtors.into_iter().rev() {
            dtor();
        }
        // Unregister this region from the live-id set BEFORE
        // the underlying Bump arena frees its memory.
        // `Gc<T>` operations check this set; an outstanding
        // handle accessed after region drop would panic if
        // we'd missed this step (which is correct — the
        // access would otherwise hit freed memory).
        //
        // Field-drop order: fields drop in declaration order
        // after this Drop fn returns. `id` is Copy so it's
        // unaffected; the Bump arena drops last (when this fn
        // returns), freeing all allocations.
        LIVE_REGION_IDS.with(|s| {
            s.borrow_mut().remove(&self.id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_ids_are_unique() {
        let r1 = Region::new();
        let r2 = Region::new();
        let r3 = Region::new();
        assert_ne!(r1.id(), r2.id());
        assert_ne!(r2.id(), r3.id());
        assert_ne!(r1.id(), r3.id());
    }

    #[test]
    fn region_id_is_nonzero() {
        let r = Region::new();
        assert!(r.id().as_u32() > 0);
    }

    #[test]
    fn region_alloc_returns_distinct_addresses() {
        let r = Region::new();
        let s1 = r.alloc(10_i64);
        let s2 = r.alloc(20_i64);
        let s3 = r.alloc(30_i64);
        let addrs = [
            s1.as_ptr() as usize,
            s2.as_ptr() as usize,
            s3.as_ptr() as usize,
        ];
        // All distinct.
        assert_ne!(addrs[0], addrs[1]);
        assert_ne!(addrs[1], addrs[2]);
        assert_ne!(addrs[0], addrs[2]);
    }

    #[test]
    fn region_alloc_payload_is_readable() {
        let r = Region::new();
        let slot_ptr = r.alloc(42_i64);
        // SAFETY: slot is alive while `r` is in scope.
        let value = unsafe { (*slot_ptr.as_ptr()).value };
        assert_eq!(value, 42);
    }

    #[test]
    fn region_alloc_strong_count_initialized_to_one() {
        let r = Region::new();
        let slot_ptr = r.alloc("hello".to_string());
        // SAFETY: slot alive while r in scope.
        let strong = unsafe { (*slot_ptr.as_ptr()).strong.get() };
        assert_eq!(strong, 1);
    }

    #[test]
    fn allocated_bytes_grows_monotonically() {
        let r = Region::new();
        let b0 = r.allocated_bytes();
        for i in 0..100_i64 {
            let _ = r.alloc(i);
        }
        let b1 = r.allocated_bytes();
        assert!(b1 > b0, "allocated_bytes should grow after 100 allocs");
        // Allocate more, ensure monotone.
        for i in 0..1000_i64 {
            let _ = r.alloc(i);
        }
        let b2 = r.allocated_bytes();
        assert!(b2 >= b1, "allocated_bytes must be monotone");
    }

    #[test]
    fn region_drop_releases_arena() {
        // Indirect test: a region allocates a Vec<u8> wrapping
        // a large buffer; after region drop, the system
        // should have reclaimed the buffer. We can't directly
        // observe the free (Bump's drop is opaque), so we
        // just exercise the path and assert no panic.
        {
            let r = Region::new();
            for _ in 0..1000_u64 {
                let _ = r.alloc(vec![0_u8; 1024]);
            }
            // r drops at end of scope.
        }
    }

    // ---- Gap E-7: dtor registry ----

    #[test]
    fn dtor_runs_on_region_drop() {
        use std::rc::Rc;
        let counter = Rc::new(Cell::new(0_usize));
        {
            let r = Region::new();
            let c = Rc::clone(&counter);
            r.register_dtor(Box::new(move || c.set(c.get() + 1)));
            // Counter still 0 — dtor hasn't run yet.
            assert_eq!(counter.get(), 0);
        }
        // Region dropped — dtor ran.
        assert_eq!(counter.get(), 1);
    }

    #[test]
    fn dtors_run_in_lifo_order() {
        use std::rc::Rc;
        let order: Rc<std::cell::RefCell<Vec<u8>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let r = Region::new();
            for i in 0..5_u8 {
                let o = Rc::clone(&order);
                r.register_dtor(Box::new(move || o.borrow_mut().push(i)));
            }
        }
        // LIFO: last-registered ran first.
        assert_eq!(*order.borrow(), vec![4, 3, 2, 1, 0]);
    }

    #[test]
    fn multiple_dtors_all_run() {
        use std::rc::Rc;
        let count = Rc::new(Cell::new(0_usize));
        {
            let r = Region::new();
            for _ in 0..10 {
                let c = Rc::clone(&count);
                r.register_dtor(Box::new(move || c.set(c.get() + 1)));
            }
        }
        assert_eq!(count.get(), 10);
    }

    #[test]
    fn no_dtors_registered_is_fine() {
        // Region drop must not error when dtors vec is empty.
        let _r = Region::new();
    }
}
