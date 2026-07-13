//! Countable-memory representation: `Gc<T>` as a single
//! tagged pointer.
//!
//! Default (no `regions` feature): every `Gc<T>` is Rc-backed
//! — the stored pointer is exactly what `Rc::into_raw` would
//! give, and `Gc<T>` is 8 bytes (pointer-sized).
//!
//! With `regions` feature on: the pointer's bit 0 doubles as
//! the arm discriminant. `0` means the (untagged) pointer is
//! an Rc-owned `T`; `1` means the (tag-cleared) pointer is a
//! [`RegionSlot<T>`] living in a [`crate::region::Region`]
//! bump arena. This is sound because every `T` `Gc<T>` is
//! instantiated with in this codebase has alignment >= 2 (see
//! `debug_assert`s in `Gc::new`/`Gc::new_in`), so bit 0 of a
//! well-aligned pointer to either backing is always free.
//! `region_id` is NOT duplicated in `Gc<T>` — it's read from
//! the `RegionSlot<T>` header (which already carries it, for
//! `from_raw_jit_region`'s benefit) whenever a region-arm
//! operation needs it. `Gc<T>` stays 8 bytes in both
//! configurations.
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

use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;
use std::rc::{Rc, Weak as RawWeak};

#[cfg(feature = "regions")]
use crate::region::{assert_region_live, is_region_live, Region, RegionId, RegionSlot};

/// Bit 0 of the packed pointer distinguishes an Rc-backed
/// allocation (0) from a region-backed one (1, tag set on the
/// pointer to the owning [`RegionSlot<T>`]).
#[cfg(feature = "regions")]
const REGION_TAG: usize = 1;

/// A heap-allocated, reference-counted value.
///
/// `Gc<T>` wraps a single tagged pointer that's either
/// Rc-backed (default; layer 2, countable-memory) or
/// region-backed (under `feature = "regions"`; layer 3,
/// region-memory spec) — see the module doc for the tagging
/// scheme. `Clone` is cheap (refcount bump); `Deref` exposes
/// `&T`.
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
pub struct Gc<T>(NonNull<u8>, PhantomData<T>);

impl<T> Gc<T> {
    /// Wrap a raw Rc-owned pointer to `T` (as returned by
    /// `Rc::into_raw`) as a `Gc<T>`. The pointer must own one
    /// strong reference count.
    #[inline]
    fn from_rc_ptr(ptr: NonNull<T>) -> Self {
        debug_assert!(
            std::mem::align_of::<T>() >= 2,
            "Gc<T>: T's alignment must be >= 2 to leave bit 0 free for the region tag"
        );
        Gc(ptr.cast(), PhantomData)
    }

    /// Wrap a raw pointer to a live [`RegionSlot<T>`] as a
    /// tagged `Gc<T>`.
    #[cfg(feature = "regions")]
    #[inline]
    fn from_region_ptr(ptr: NonNull<RegionSlot<T>>) -> Self {
        debug_assert!(
            std::mem::align_of::<T>() >= 2,
            "Gc<T>: T's alignment must be >= 2 to leave bit 0 free for the region tag"
        );
        let addr = (ptr.as_ptr() as usize) | REGION_TAG;
        // SAFETY: `ptr` is non-null, so `addr` (its address
        // with the low tag bit set) is non-zero too.
        Gc(
            unsafe { NonNull::new_unchecked(addr as *mut u8) },
            PhantomData,
        )
    }

    /// `true` if the stored pointer carries the region tag.
    #[cfg(feature = "regions")]
    #[inline]
    fn is_region_tagged(&self) -> bool {
        (self.0.as_ptr() as usize) & REGION_TAG != 0
    }

    #[cfg(not(feature = "regions"))]
    #[inline]
    fn is_region_tagged(&self) -> bool {
        false
    }

    /// Recover the Rc-owned `T` pointer (tag bit is always 0
    /// on this arm, so no masking needed beyond the cast).
    #[inline]
    fn rc_ptr(&self) -> NonNull<T> {
        #[cfg(feature = "regions")]
        {
            let addr = (self.0.as_ptr() as usize) & !REGION_TAG;
            // SAFETY: `self.0` is non-null and untagged here
            // is still non-zero (addresses 0/1 are never valid
            // allocations).
            unsafe { NonNull::new_unchecked(addr as *mut T) }
        }
        #[cfg(not(feature = "regions"))]
        {
            self.0.cast()
        }
    }

    /// Recover the region-slot pointer (mask off the tag bit).
    /// Caller must have checked [`Self::is_region_tagged`].
    #[cfg(feature = "regions")]
    #[inline]
    fn region_slot_ptr(&self) -> NonNull<RegionSlot<T>> {
        let addr = (self.0.as_ptr() as usize) & !REGION_TAG;
        // SAFETY: see `rc_ptr`.
        unsafe { NonNull::new_unchecked(addr as *mut RegionSlot<T>) }
    }
}

/// Read the owning `region_id` out of a region slot's header.
///
/// # Safety + trust boundary
///
/// `slot_ptr` must point at a still-*mapped* `RegionSlot<T>` —
/// i.e. reading through it must not fault. This is the same
/// trust boundary `Gc::from_raw_jit_region` already documents
/// for the JIT raw-handle ABI. It's weaker than "the region is
/// still alive": once a region drops, its bump-arena chunk is
/// deallocated and the allocator is free to reuse (and zero)
/// those bytes — observed empirically as `region_id` reading
/// back `0` immediately after a `Region` drop. That's exactly
/// the use-after-region-drop condition this function exists to
/// catch, so a `0` read is treated as "not live" (panics below)
/// rather than "corrupted", matching what the surviving,
/// memory-independent thread-local epoch-slab check
/// (`assert_region_live`/`is_region_live`) would say if the
/// (now-overwritten) real id were still readable.
#[cfg(feature = "regions")]
#[inline]
unsafe fn region_id_of<T>(slot_ptr: NonNull<RegionSlot<T>>) -> RegionId {
    let raw = unsafe { (*slot_ptr.as_ptr()).region_id };
    RegionId::from_raw_u32(raw).unwrap_or_else(|| {
        panic!(
            "cs_gc::Gc<T>: region dropped while a handle into it is still outstanding \
             (use-after-region-drop) — the slot's region_id read back as 0, which is what \
             happens once the owning region's bump-arena chunk is freed and reused/zeroed"
        )
    })
}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        #[cfg(feature = "regions")]
        if self.is_region_tagged() {
            let slot_ptr = self.region_slot_ptr();
            // SAFETY: see `region_id_of`.
            let region_id = unsafe { region_id_of(slot_ptr) };
            assert_region_live(region_id);
            // SAFETY: liveness just confirmed above.
            unsafe {
                let slot = slot_ptr.as_ref();
                slot.strong.set(
                    slot.strong
                        .get()
                        .checked_add(1)
                        .expect("Gc<T>::clone: region refcount overflow"),
                );
            }
            return Gc(self.0, PhantomData);
        }
        // SAFETY: `rc_ptr()` is a live Rc-owned pointer for
        // the lifetime of `self`; bumping its strong count
        // without constructing an owning `Rc` avoids an
        // extra raw round-trip.
        unsafe { Rc::increment_strong_count(self.rc_ptr().as_ptr() as *const T) };
        Gc(self.0, PhantomData)
    }
}

impl<T> Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        #[cfg(feature = "regions")]
        if self.is_region_tagged() {
            let slot_ptr = self.region_slot_ptr();
            // SAFETY: see `region_id_of`.
            let region_id = unsafe { region_id_of(slot_ptr) };
            // Iter 3: validate the region still exists before
            // dereferencing the in-arena value.
            assert_region_live(region_id);
            // SAFETY: liveness just confirmed above.
            return unsafe { &slot_ptr.as_ref().value };
        }
        // SAFETY: `rc_ptr()` is a live Rc-owned pointer for
        // the lifetime of `self`.
        unsafe { self.rc_ptr().as_ref() }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gc({:?})", self.deref())
    }
}

impl<T> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other)
    }
}

impl<T> Gc<T> {
    /// Pointer-equality test (analogous to `Rc::ptr_eq`) —
    /// useful for implementing `eq?` over GC-managed values.
    /// Returns true iff both handles refer to the same
    /// allocation (across both Rc and Region arms;
    /// inter-variant comparison is always false — the region
    /// tag bit makes the packed addresses differ).
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.0 == b.0
    }

    /// Stable opaque integer for the underlying allocation
    /// address. Cycle-detection visited-sets and `eq?`-identity
    /// hashing key off this. Stable across clones; differs
    /// between Rc and Region arms even for "equal" payloads
    /// (the region tag bit, and the addresses, are physically
    /// different).
    pub fn as_addr(this: &Self) -> usize {
        this.0.as_ptr() as usize
    }

    /// Live strong-count for the underlying allocation. For
    /// Rc-backed Gc, returns `Rc::strong_count`. For region-
    /// backed Gc, returns the in-line refcount header value;
    /// the count is informational only (region drop reclaims
    /// regardless).
    pub fn strong_count(this: &Self) -> usize {
        #[cfg(feature = "regions")]
        if this.is_region_tagged() {
            let slot_ptr = this.region_slot_ptr();
            // SAFETY: see `region_id_of`.
            let region_id = unsafe { region_id_of(slot_ptr) };
            assert_region_live(region_id);
            // SAFETY: liveness just confirmed above.
            return unsafe { slot_ptr.as_ref().strong.get() as usize };
        }
        // SAFETY: peek-only — `ManuallyDrop` suppresses the
        // reconstructed `Rc`'s destructor so we don't touch
        // the real strong count.
        let rc = ManuallyDrop::new(unsafe { Rc::from_raw(this.rc_ptr().as_ptr()) });
        Rc::strong_count(&rc)
    }

    /// `true` if this `Gc<T>` is region-allocated.
    ///
    /// Used by the cycle detector (`cs-runtime::countable_memory_cycle`)
    /// to skip detection on region-allocated mutation sites —
    /// region cycles reclaim via region drop, not via the
    /// per-mutation detector.
    pub fn is_region(this: &Self) -> bool {
        this.is_region_tagged()
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
    /// Calling `downgrade` on a region-backed `Gc<T>` **panics
    /// in all builds** (parallel-runtime C5.1 hardening,
    /// previously debug-only): region cells have no defined
    /// weak-ref semantics — the region's bulk drop is the
    /// reclamation event, not a refcount transition. Layer 5
    /// escape analysis is supposed to make this unreachable in
    /// compiled code; manual region users (and callers like
    /// `cs_core::WeakValue::from_value`) must `is_region`-check
    /// before calling.
    ///
    /// The previous "silently return a dead Weak" behavior
    /// masked latent bugs — values would appear reclaimed when
    /// they were actually still alive in the region. The
    /// upgrade-to-panic mirrors how `Vec::index` reports OOB.
    pub fn downgrade(this: &Self) -> Weak<T> {
        #[cfg(feature = "regions")]
        if this.is_region_tagged() {
            let slot_ptr = this.region_slot_ptr();
            // SAFETY: see `region_id_of`. Reading the id here
            // is purely for the panic message.
            let region_id = unsafe { region_id_of(slot_ptr) };
            panic!(
                "Gc<T>::downgrade: region-backed Gc (region_id={:?}) \
                 cannot be downgraded — region drop handles reclamation; \
                 weak refs to region allocations have no defined semantics. \
                 If you reached this from a layer-2 cycle-break path, the \
                 caller should `is_region`-check first \
                 (see WeakValue::from_value for the pattern); for direct \
                 region access use to_rc_deep + the resulting Rc-backed Gc.",
                region_id
            );
        }
        // SAFETY: peek-only reconstruction; `ManuallyDrop`
        // suppresses the destructor so `this`'s strong count
        // is untouched.
        let rc = ManuallyDrop::new(unsafe { Rc::from_raw(this.rc_ptr().as_ptr()) });
        Weak {
            inner: Rc::downgrade(&rc),
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
        let ptr = Rc::into_raw(Rc::new(value));
        // SAFETY: `Rc::into_raw` never returns null.
        Gc::from_rc_ptr(unsafe { NonNull::new_unchecked(ptr as *mut T) })
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
        if !this.is_region_tagged() {
            // Rc arm: nothing to do.
            return;
        }
        let slot_ptr = this.region_slot_ptr();
        // SAFETY: see `region_id_of`.
        let region_id = unsafe { region_id_of(slot_ptr) };
        assert_region_live(region_id);
        // SAFETY: region is alive (just checked); the slot is
        // in its arena and readable.
        let cloned: T = unsafe { slot_ptr.as_ref().value.clone() };
        // Mirror the old region-arm `Drop` bookkeeping before
        // overwriting `this.0` — the region still owns the
        // slot, so this just decrements the informational
        // in-arena refcount (skipped if the region already
        // dropped, matching `Drop`'s own guard).
        if is_region_live(region_id) {
            // SAFETY: liveness just confirmed.
            unsafe {
                let slot = slot_ptr.as_ref();
                slot.strong.set(slot.strong.get().saturating_sub(1));
            }
        }
        *this = Gc::new(cloned);
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
        Gc::from_region_ptr(ptr)
    }

    /// Take ownership of the inner `T` when this is the only
    /// strong holder, mirroring `Rc::into_inner`. Returns
    /// `None` for shared handles and for region-backed
    /// handles (the region owns the slot).
    ///
    /// Primary consumer is `Pair`'s iterative Drop, which
    /// walks long cdr chains without recursing.
    pub fn into_inner(this: Self) -> Option<T> {
        // Fast-reject the region variant first — region drop
        // owns reclamation; just let `this`'s natural Drop
        // run to decrement the in-arena strong count.
        #[cfg(feature = "regions")]
        if this.is_region_tagged() {
            return None;
        }
        // Rc variant: suppress `Gc<T>`'s own Drop and
        // reconstruct the owning `Rc` so `Rc::into_inner` gets
        // the strong count handoff.
        let this = ManuallyDrop::new(this);
        let rc = unsafe { Rc::from_raw(this.rc_ptr().as_ptr()) };
        Rc::into_inner(rc)
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

impl<T: 'static> Gc<T> {
    /// Hand off this `Gc<T>` as a raw handle. Pair every call
    /// with exactly one [`from_raw_jit`] (consumes the strong
    /// count, returns a fresh `Gc<T>`) or with [`raw_incref`]
    /// (bumps the strong count for a borrowing observer
    /// without transferring ownership).
    pub fn into_raw_jit(this: Self) -> *const () {
        // Take ownership of the raw pointer without running
        // `this`'s Drop — the strong count is transferred to
        // the raw pointer.
        let this = ManuallyDrop::new(this);
        #[cfg(feature = "regions")]
        if this.is_region_tagged() {
            // The in-line refcount was bumped when `this` was
            // created (or last cloned). Transferring it into
            // the raw handle is just a pointer cast — no
            // decrement, no increment.
            return this.region_slot_ptr().as_ptr() as *const ();
        }
        this.rc_ptr().as_ptr() as *const ()
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
        // SAFETY: contract above.
        let nn = unsafe { NonNull::new_unchecked(ptr as *mut T) };
        Gc::from_rc_ptr(nn)
    }

    /// Reconstitute a region-backed `Gc<T>` from a raw slot
    /// pointer produced by [`into_raw_jit`] on a Region-arm
    /// handle. Reads the `region_id` field of the in-arena
    /// slot to restore the region tag.
    ///
    /// # Safety
    ///
    /// `ptr` must be the result of a matching `into_raw_jit`
    /// call on a Region-backed `Gc<T>` for the same `T`, and
    /// the slot must still be alive (the owning region not
    /// dropped). Consumes one strong count from the in-arena
    /// refcount header.
    ///
    /// Used by the VM nanbox decoder when the low payload
    /// bit indicates the pointer was Region-allocated (see
    /// `cs_vm::vm::NanboxValue::from_value` / `to_value`).
    #[cfg(feature = "regions")]
    pub unsafe fn from_raw_jit_region(ptr: *const ()) -> Self
    where
        T: Sized,
    {
        let slot_ptr =
            NonNull::new(ptr as *mut RegionSlot<T>).expect("from_raw_jit_region: null ptr");
        // SAFETY: slot is live per the function's contract;
        // validated below purely to preserve the original
        // panic-on-corruption diagnostic.
        let raw_id = unsafe { (*slot_ptr.as_ptr()).region_id };
        RegionId::from_raw_u32(raw_id)
            .expect("from_raw_jit_region: slot region_id was 0 (corrupted slot or wrong tag)");
        Gc::from_region_ptr(slot_ptr)
    }

    /// Drop-context counterpart to [`from_raw_jit_region`]: instead
    /// of asserting the slot is live (panicking otherwise), this
    /// mirrors `Gc<T>::drop`'s own tolerance for an
    /// already-torn-down owning region — a region's bulk-arena free
    /// does not run any `T::drop`, so a raw owning payload whose
    /// region already dropped has nothing left to release, and that
    /// is not a bug worth panicking a destructor over (unlike
    /// `from_raw_jit_region`'s read/mutate contract, where reaching
    /// a dead region through a "live" handle IS the bug).
    ///
    /// Returns `None` when the slot's `region_id` reads back as `0`
    /// (freed/reused arena memory) or the region is no longer
    /// tracked live — in both cases there is nothing to release, the
    /// same as `Gc<T>::drop`'s region arm would conclude. Returns
    /// `Some` (transferring the strong count exactly like
    /// `from_raw_jit_region`) otherwise.
    ///
    /// # Safety
    ///
    /// Same as [`from_raw_jit_region`] except the "slot must still
    /// be alive" clause is relaxed to "slot must still be valid
    /// memory to *read* `region_id` from" — i.e. `ptr` must be the
    /// result of a matching `into_raw_jit` call on a Region-backed
    /// `Gc<T>`, but the owning region may have since dropped.
    ///
    /// This is the strict-vs-lenient split `cs-core`'s
    /// `decode_gc_handle_for_drop` documents in full: reads/mutates
    /// (`Clone`, `downgrade`, `strong_count`, [`from_raw_jit_region`])
    /// stay strict on purpose — reaching a dead region through a
    /// handle believed live is a real bug worth panicking loudly
    /// for. Drop/peek paths (this function, `Gc<T>::drop`'s own
    /// region arm) must instead be lenient — a destructor can't
    /// safely panic, and a stale payload surviving its region's
    /// bulk-free is often not a bug at this layer at all (recursive
    /// destructor chains can reach payloads whose region already
    /// reclaimed everything there was to reclaim). Consult that doc
    /// before picking which side a new call site belongs on.
    #[cfg(feature = "regions")]
    pub unsafe fn from_raw_jit_region_for_drop(ptr: *const ()) -> Option<Self>
    where
        T: Sized,
    {
        let slot_ptr = NonNull::new(ptr as *mut RegionSlot<T>)?;
        let raw_id = unsafe { (*slot_ptr.as_ptr()).region_id };
        let region_id = RegionId::from_raw_u32(raw_id)?;
        if !is_region_live(region_id) {
            return None;
        }
        Some(Gc::from_region_ptr(slot_ptr))
    }

    /// Bump the strong count for a raw Rc handle without
    /// consuming a reference.
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live allocation produced by
    /// [`into_raw_jit`] on the same `T`.
    ///
    /// Interprets `ptr` as an Rc-backed allocation — for
    /// Region-backed pointers, use [`raw_incref_region`].
    pub unsafe fn raw_incref(ptr: *const ()) {
        unsafe { Rc::increment_strong_count(ptr as *const T) }
    }

    /// Bump the in-arena strong count of a region-backed
    /// raw slot pointer. Counterpart to [`raw_incref`] for
    /// the Region arm.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live `RegionSlot<T>` pointer (slot
    /// still alive, region not dropped).
    #[cfg(feature = "regions")]
    pub unsafe fn raw_incref_region(ptr: *const ())
    where
        T: Sized,
    {
        let slot_ptr = ptr as *const crate::region::RegionSlot<T>;
        // SAFETY: slot alive per contract; strong is a Cell<u32>.
        unsafe {
            let slot = &*slot_ptr;
            let cur = slot.strong.get();
            slot.strong
                .set(cur.checked_add(1).expect("raw_incref_region: u32 overflow"));
        }
    }
}

impl<T> Drop for Gc<T> {
    fn drop(&mut self) {
        #[cfg(feature = "regions")]
        if self.is_region_tagged() {
            let slot_ptr = self.region_slot_ptr();
            // SAFETY: see `region_id_of`. If the id itself
            // decodes to the corrupted-slot sentinel (0),
            // there's nothing sound left to do — treat it the
            // same as an already-dropped region and skip.
            let Some(region_id) =
                (unsafe { RegionId::from_raw_u32((*slot_ptr.as_ptr()).region_id) })
            else {
                return;
            };
            // Iter 3: skip the slot decrement entirely if the
            // owning region already dropped. The slot memory
            // is already freed by the bump arena; touching
            // `slot.strong` would be UB. (In a
            // well-disciplined program this branch is never
            // taken — but if it is, this is the best we can do
            // in release without panicking.)
            if !is_region_live(region_id) {
                return;
            }
            // Decrement the in-line refcount. The region's own
            // Drop handles reclamation; we just bookkeep.
            // SAFETY: region is alive (just checked); slot is
            // in its arena.
            unsafe {
                let slot = slot_ptr.as_ref();
                let cur = slot.strong.get();
                slot.strong.set(cur.saturating_sub(1));
            }
            return;
        }
        // SAFETY: `self` owns one strong count on `rc_ptr()`
        // (every live `Gc<T>` does, by construction);
        // reconstructing the `Rc` and letting it drop here
        // runs the normal Rc teardown (decrement, free on
        // last drop).
        unsafe { drop(Rc::from_raw(self.rc_ptr().as_ptr())) };
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
/// use cs_gc::Gc;
/// let g = Gc::new(42_i64);
/// let w = Gc::downgrade(&g);
/// assert_eq!(w.upgrade().map(|g| *g), Some(42));
/// drop(g);
/// assert!(w.upgrade().is_none());
/// ```
pub struct Weak<T> {
    inner: RawWeak<T>,
}

impl<T> Clone for Weak<T> {
    fn clone(&self) -> Self {
        Weak {
            inner: RawWeak::clone(&self.inner),
        }
    }
}

impl<T> std::fmt::Debug for Weak<T> {
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

impl<T: 'static> Weak<T> {
    /// Attempt to upgrade to a strong `Gc<T>` handle. Returns
    /// `None` if the underlying allocation has been reclaimed.
    pub fn upgrade(&self) -> Option<Gc<T>> {
        self.inner.upgrade().map(|rc| {
            let ptr = Rc::into_raw(rc);
            // SAFETY: `Rc::into_raw` never returns null.
            Gc::from_rc_ptr(unsafe { NonNull::new_unchecked(ptr as *mut T) })
        })
    }

    /// Strong count of the underlying allocation, without
    /// upgrading. Unlike `upgrade().map(|g| Gc::strong_count(&g))`,
    /// this does NOT transiently add a strong reference — so the
    /// returned value is the true reachable strong count, which
    /// the Bacon-Rajan trial-deletion walk needs for correct
    /// external-vs-internal classification (parallel-runtime C4.3).
    pub fn strong_count(&self) -> usize {
        self.inner.strong_count()
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
    fn gc_value_is_pointer_sized() {
        // cs-7xg stage 1: Gc<T> collapses from a 2-variant
        // enum (16B) to a single tagged pointer (8B).
        assert_eq!(std::mem::size_of::<Gc<Leaf>>(), 8);
        assert_eq!(std::mem::size_of::<Option<Gc<Leaf>>>(), 8);
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

    /// parallel-runtime C5.1: `Gc::downgrade` on a region-backed
    /// Gc panics in all builds with a message that names the
    /// region_id and points at the recommended fix.
    #[cfg(feature = "regions")]
    #[test]
    #[should_panic(expected = "Gc<T>::downgrade: region-backed Gc")]
    fn downgrade_region_handle_panics() {
        let region = crate::Region::new();
        let g: Gc<Leaf> = Gc::new_in(&region, Leaf { n: 7 });
        // Should panic; the test catches it.
        let _ = Gc::downgrade(&g);
    }

    #[cfg(feature = "regions")]
    #[test]
    #[should_panic(expected = "weak refs to region allocations")]
    fn downgrade_region_panic_message_explains_why() {
        let region = crate::Region::new();
        let g: Gc<Leaf> = Gc::new_in(&region, Leaf { n: 11 });
        let _ = Gc::downgrade(&g);
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
