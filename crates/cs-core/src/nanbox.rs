//! NaN-boxed value carrier (`NanboxValue`) — the canonical i64
//! "Any-lane" encoding shared by the bytecode VM and JIT tiers.
//!
//! Moved down from `cs-vm` into `cs-core` (cs-vnf.3 PR1, pure
//! refactor) so `cs-core` can encode/decode NB payloads directly —
//! a prerequisite for PR2's `Pair` car/cdr slots becoming native
//! `Cell<u64>` NB storage instead of `RefCell<Value>`. `cs-vm`
//! re-exports every name from this module unchanged so existing
//! call sites (JIT lowering, VM dispatch, etc.) are unaffected.

use crate::{CsStr, Hashtable, Pair, Port, Procedure, Promise, Symbol, Value};

// ===== NanboxValue (Stage 2 K1 step 2b — NaN-box encoding) =====
//
// `#[repr(transparent)]` newtype over `i64` carrying the canonical
// Any-lane value encoding. As of K1 step 2b this is also what the
// free functions `value_to_gc_i64` / `gc_i64_to_value` produce and
// consume, so JIT-emitted code and bytecode VM helpers all speak the
// same i64 representation. (The older 3-bit-low-tag `TaggedValue`
// newtype was retired in step 2b once the encodings unified.)
//
// **Encoding shape (sign-bit-set quiet NaN):**
//
// ```
// bit:  63    62..52   51    50..47   46..0
//      sign   exp=0x7FF quiet  tag(4)   payload(47)
//      1      all 1s    1
// ```
//
// - Tagged values: high 13 bits = `0xFFF8` (sign=1 + quiet NaN
//   signature). 16 tag values × 47-bit payload.
// - Real Flonums: any other f64 bit pattern. Sign=0 quiet NaNs
//   (the `0x7FF8…` range) stay distinct, so naturally-arising f64
//   NaN results from arithmetic do not collide with the tagged
//   range. Sign=1 NaNs are canonicalized at f64-producer sites.
//
// **47-bit pointer payloads** fit x86-64 user-space (canonical
// addresses are 47–48 bits, upper bits zero). This means heap
// values store their `Gc<T>` / `Rc<dyn …>` raw pointer **directly**
// with a per-variant tag — no `Gc<Value>` wrap, no allocation
// regression on the stack migration (which is the whole point of
// going with NaN-boxing over high-byte-only tagging).
//
// **47-bit Fixnum** range is ±2^46 = ±70 trillion. Practical Scheme
// programs never trip this; oversized values fall through to the
// BigInt path (which is heap-allocated regardless).

/// High 13-bit signature for our tagged-NaN range. A bit-pattern
/// `b` is in the tagged range iff `(b as u64) & SIGNATURE_MASK ==
/// SIGNATURE_BITS`.
pub const NB_SIGNATURE_MASK: u64 = 0xFFF8_0000_0000_0000;
pub const NB_SIGNATURE_BITS: u64 = 0xFFF8_0000_0000_0000;
pub const NB_TAG_SHIFT: u32 = 47;
pub const NB_TAG_MASK: u64 = 0xF << NB_TAG_SHIFT;
pub const NB_PAYLOAD_MASK: u64 = (1u64 << 47) - 1;

/// Variant tags. 16 values, one per top-level `Value` shape. Order
/// chosen so immediate-typed tags (the hot cases) cluster low.
pub const NB_TAG_FIXNUM: u64 = 0;
pub const NB_TAG_BOOLEAN: u64 = 1;
pub const NB_TAG_CHARACTER: u64 = 2;
pub const NB_TAG_SYMBOL: u64 = 3;
pub const NB_TAG_NULL: u64 = 4;
pub const NB_TAG_UNSPECIFIED: u64 = 5;
pub const NB_TAG_EOF: u64 = 6;
pub const NB_TAG_PAIR: u64 = 7;
pub const NB_TAG_VECTOR: u64 = 8;
pub const NB_TAG_STRING: u64 = 9;
pub const NB_TAG_BYTEVECTOR: u64 = 10;
pub const NB_TAG_PROCEDURE: u64 = 11;
pub const NB_TAG_HASHTABLE: u64 = 12;
pub const NB_TAG_PORT: u64 = 13;
pub const NB_TAG_PROMISE: u64 = 14;
/// Catchall for `Value` variants that don't have a dedicated NaN-
/// box tag — currently the wider Number forms (Flonum is *not*
/// here; it rides the raw-f64 path. The catchall is for
/// `Number::BigInt`, `Number::Rational`, `Number::Complex`). The
/// i64 payload is a `Gc<Value>` raw pointer wrapping the full
/// Value enum (the same shape the pre-K1-step-2b Any-lane used
/// for *all* pointer values; under NaN-boxing this stays only as
/// the catchall path).
pub const NB_TAG_GC_VALUE: u64 = 15;

/// Inclusive max Fixnum that fits the 47-bit signed payload.
pub const NB_FIXNUM_MAX: i64 = (1i64 << 46) - 1;
pub const NB_FIXNUM_MIN: i64 = -(1i64 << 46);

/// Canonical f64 NaN bit pattern — sign=0 quiet NaN, mantissa
/// payload zero. Outside our tagged range (sign=0, not sign=1).
/// Arithmetic that produces NaN should normalize to this pattern
/// at encode time so the same Scheme-level NaN compares bit-equal.
pub const NB_NAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// True iff `bits` falls in the tagged NaN range (not a regular
/// f64). One mask + compare; suitable for the hot path.
#[inline]
pub fn nb_is_tagged(bits: u64) -> bool {
    (bits & NB_SIGNATURE_MASK) == NB_SIGNATURE_BITS
}

#[inline]
pub fn nb_tag_of(bits: u64) -> u64 {
    (bits & NB_TAG_MASK) >> NB_TAG_SHIFT
}

#[inline]
pub fn nb_payload_of(bits: u64) -> u64 {
    bits & NB_PAYLOAD_MASK
}

#[inline]
pub fn nb_make(tag: u64, payload: u64) -> u64 {
    NB_SIGNATURE_BITS | ((tag & 0xF) << NB_TAG_SHIFT) | (payload & NB_PAYLOAD_MASK)
}

/// Bit 0 of pointer-typed nanbox payloads: 0 = Rc-allocated,
/// 1 = Region-allocated. The pointer-typed tags (NB_TAG_PAIR..
/// NB_TAG_PROMISE, NB_TAG_GC_VALUE) carry an 8-byte-aligned
/// pointer in the upper 46 bits; bit 0 is always zero in the
/// raw pointer, so we repurpose it to distinguish the GcRepr
/// arm.
///
/// Set by [`nb_encode_gc_ptr`] on encode; tested by
/// [`nb_decode_gc_ptr`] on decode to route to
/// `Gc::from_raw_jit` (Rc) or `Gc::from_raw_jit_region`
/// (Region). Closes the SIGSEGV-on-with-region root cause —
/// previously `from_raw_jit` always interpreted region
/// pointers as Rc allocations, hitting allocator UB on drop.
pub const NB_REGION_FLAG: u64 = 1;

/// Encode a `Gc<T>` raw-jit pointer into the pointer-typed nanbox
/// payload, tagging bit 0 with the Rc/Region origin flag.
///
/// The payload field is only 47 bits, but user-space pointers are up
/// to 48 bits (arm64 Linux places the heap with bit 47 set — unlike
/// macOS-arm64 and the x86-64 canonical lower half, which never set
/// it). Since `Gc`/`Rc` data pointers are ≥8-byte aligned (bits[2:0]
/// == 0), we shift-compress: `raw_ptr >> 1` losslessly fits a full
/// 48-bit address into 47 bits and leaves bit 0 free for the region
/// flag. See the platform-portability discussion in the type docs.
///
/// Caller must invoke before `Gc::into_raw_jit` consumes the
/// handle. `is_region` is `Gc::is_region(&g)`.
///
/// Safety: the returned u64 is suitable for `nb_make(tag, ...)`.
/// The decoder must invert via [`nb_decode_gc_ptr`].
#[inline]
pub fn nb_encode_gc_ptr(raw_ptr: u64, is_region: bool) -> u64 {
    debug_assert_eq!(
        raw_ptr & NB_REGION_FLAG,
        0,
        "Gc raw pointer must be 8-byte aligned; low bit reserved for region flag"
    );
    debug_assert!(
        raw_ptr < (1u64 << 48),
        "Gc raw pointer exceeds 48-bit VA; nanbox payload supports up to 48-bit addresses"
    );
    // Shift-compress: an 8-byte-aligned pointer has bit 0 == 0, so
    // `raw_ptr >> 1` is lossless and fits a 48-bit address into the
    // 47-bit payload, freeing bit 0 for the region flag.
    // [`nb_decode_gc_ptr`] inverts this with `<< 1`.
    (raw_ptr >> 1) | (is_region as u64)
}

/// Decode a pointer-typed nanbox payload into `(raw_ptr,
/// is_region)`. Strips the region flag from bit 0.
#[inline]
pub fn nb_decode_gc_ptr(payload: u64) -> (*const (), bool) {
    let is_region = (payload & NB_REGION_FLAG) != 0;
    // Invert `nb_encode_gc_ptr`'s shift-compression: clear the region
    // flag (bit 0), then `<< 1` to restore the original 48-bit pointer.
    let ptr = ((payload & !NB_REGION_FLAG) << 1) as *const ();
    (ptr, is_region)
}

/// Sign-extend the low 47 bits of `payload` to a full i64. Used to
/// decode the Fixnum tag (47-bit signed → i64).
#[inline]
pub fn nb_sign_extend_47(payload: u64) -> i64 {
    let shifted = (payload as i64) << 17;
    shifted >> 17
}

/// Dispatch a raw nanbox pointer + region flag to the right
/// `Gc::from_raw_jit` variant. Pulled out so every pointer-
/// typed decode arm in [`NanboxValue::to_value`],
/// [`vm_value_drop_gc`], etc. shares one impl.
///
/// # Safety
///
/// `ptr` must be a live raw handle for `T` previously
/// produced by `Gc::into_raw_jit`; `is_region` must reflect
/// whether the originating `Gc<T>` was Region-backed.
///
/// `pub` (not `pub(crate)`): cs-vm calls this from many sites
/// outside `NanboxValue`'s own impl now that both have moved out
/// of cs-vm together; cs-vm re-exports it `pub(crate)` to restore
/// the original crate-private encapsulation downstream.
#[inline]
pub unsafe fn decode_gc_handle<T: 'static + Sized>(
    ptr: *const (),
    is_region: bool,
) -> cs_gc::Gc<T> {
    #[cfg(feature = "regions")]
    if is_region {
        return unsafe { cs_gc::Gc::<T>::from_raw_jit_region(ptr) };
    }
    // Either Rc-backed, or the regions feature is off (then
    // the encoder never sets the flag — debug-asserted in
    // `nb_encode_gc_ptr` for the Rc path).
    #[cfg(not(feature = "regions"))]
    let _ = is_region;
    unsafe { cs_gc::Gc::<T>::from_raw_jit(ptr) }
}

/// Drop-context counterpart to [`decode_gc_handle`]: tolerant of an
/// already-torn-down owning region instead of panicking (see
/// `cs_gc::Gc::from_raw_jit_region_for_drop`). Use this — not
/// [`decode_gc_handle`] — anywhere an owning NB payload is being
/// released rather than read/mutated (e.g. `nb_drop_owned`,
/// `Pair`'s `Drop` cdr-walk): a region's bulk-arena free never runs
/// `T::drop`, so a raw owning payload whose region already dropped
/// has nothing left to release, and that's not a bug worth
/// panicking a destructor over.
///
/// Returns `None` when the region-tagged payload's owning region is
/// already gone — there is nothing to release. Returns `Some`
/// otherwise (transferring the strong count, same as
/// `decode_gc_handle`).
///
/// # Why this asymmetry exists — read before adding a new call site
///
/// `cs_gc` deliberately has TWO different answers to "what do I do
/// when a region-tagged handle's owning region turns out to already
/// be dead?", and picking the wrong one for a given call site is
/// exactly the mistake cs-vnf.3 PR2 made (three times, across two
/// crates) before this fix:
///
/// - **Live-code paths** (`Clone`, `downgrade`, `strong_count`, and
///   [`decode_gc_handle`] itself) stay STRICT — they panic via
///   `assert_region_live`/`from_raw_jit_region`'s `.expect`. Reaching
///   a dead region through a handle a caller believes is live is a
///   genuine use-after-region-drop bug (layer-5 escape analysis is
///   supposed to make it unreachable in compiled code); panicking
///   loudly is the correct, intentional behavior there, mirroring
///   `Vec::index`'s OOB panic.
/// - **Drop/peek paths** (this function, `nb_owning_payload_is_live`,
///   and `cs_gc::Gc<T>::drop`'s own region arm) are LENIENT — they
///   silently no-op instead of panicking. Two reasons this must be
///   the answer here, not the strict one: (1) a destructor can't
///   safely panic (a panic-in-Drop during unwind aborts the process
///   — this is literally how the original bug manifested: "panic in
///   a function that cannot unwind"); (2) it usually isn't even a
///   bug at this layer — a stale owning payload surviving its
///   region's bulk-free is an ordinary, expected side effect of
///   destructor recursion (dropping one `Pair` can trigger the
///   layer-4 cycle sweep, which walks OTHER live pairs' slots — see
///   `Drop for Pair` in `value.rs`), and the region's bulk-free
///   already reclaimed everything there was to reclaim regardless of
///   whether every payload pointing at it gets individually visited.
///
/// The two paths are NOT interchangeable substitutes for each other:
/// swapping a live-code read/mutate site to the lenient decoder
/// would silently mask a real UAF; swapping a Drop/peek site to the
/// strict decoder is exactly the bug this fix closes. When adding a
/// new call site that reconstructs a `Gc<T>` from a raw NB payload,
/// ask "if this fires from inside someone else's destructor, could
/// panicking here abort the process?" — if yes, it belongs on this
/// side.
///
/// # Safety
/// Same as [`decode_gc_handle`], except the "region must still be
/// alive" clause is relaxed — the region may have since dropped.
#[inline]
pub unsafe fn decode_gc_handle_for_drop<T: 'static + Sized>(
    ptr: *const (),
    is_region: bool,
) -> Option<cs_gc::Gc<T>> {
    #[cfg(feature = "regions")]
    if is_region {
        return unsafe { cs_gc::Gc::<T>::from_raw_jit_region_for_drop(ptr) };
    }
    #[cfg(not(feature = "regions"))]
    let _ = is_region;
    Some(unsafe { cs_gc::Gc::<T>::from_raw_jit(ptr) })
}

/// True iff `bits` is a pointer-typed (owning) NB encoding — i.e. it
/// owns a `Gc<T>`/`Rc<T>`-equivalent allocation (or a `proc_table`
/// slot, for `NB_TAG_PROCEDURE`) and needs incref/decref/drop
/// bookkeeping. The inverse of `cs_vm::vm::any_i64_is_inline`, which
/// stays defined separately in cs-vm — this is a small pure
/// predicate, not worth a cross-crate re-export ceremony for.
#[inline]
fn nb_is_owning(bits: u64) -> bool {
    nb_is_tagged(bits) && nb_tag_of(bits) >= NB_TAG_PAIR
}

/// True iff an owning NB payload's target is still safe to touch —
/// i.e. either it isn't region-tagged (Rc/proc_table payloads are
/// always "live" as far as this check is concerned; a genuinely
/// dangling Rc is a different, pre-existing class of bug this
/// doesn't attempt to catch), or it is region-tagged and that
/// region hasn't dropped yet.
///
/// For `NB_TAG_PROCEDURE` (proc_table slots, not region-backed at
/// all) this is always `true`.
///
/// Exists so `Pair::peek_car`/`peek_cdr` — the cycle-break/Drop
/// machinery's non-upgrading slot read, which can run during
/// TLS-teardown-triggered cycle sweeps well after the payload's
/// owning region has bulk-freed — can skip the decode entirely
/// instead of reconstructing (and thereby touching) a handle into
/// freed arena memory. Doesn't consume or mutate anything: for the
/// region case it borrows `decode_gc_handle_for_drop`'s liveness
/// check and immediately `mem::forget`s the temporary handle it
/// hands back on success, so the slot's strong count is untouched
/// either way (`None` case never constructed the handle at all).
///
/// Belongs on the LENIENT side of the strict-vs-lenient split
/// documented on [`decode_gc_handle_for_drop`] — read that doc
/// before adding a new caller of this function or its strict
/// counterpart [`decode_gc_handle`].
///
/// # Safety
/// `raw` must be a live owning NB payload (or an inline immediate) —
/// same contract as [`nb_clone_owned`]/[`nb_drop_owned`].
#[inline]
pub(crate) unsafe fn nb_owning_payload_is_live(raw: i64) -> bool {
    let bits = raw as u64;
    if !nb_is_owning(bits) {
        return true;
    }
    let tag = nb_tag_of(bits);
    if tag == NB_TAG_PROCEDURE {
        return true;
    }
    let (ptr, is_region) = nb_decode_gc_ptr(nb_payload_of(bits));
    if !is_region {
        return true;
    }
    #[cfg(feature = "regions")]
    {
        // Route through the same tag dispatch `to_value`/
        // `nb_drop_owned` use, just with a throwaway `T` (the
        // liveness check itself is untyped — only the leading
        // `region_id` field of the `RegionSlot<T>` header is read,
        // regardless of `T`) so this stays a single call site
        // rather than re-deriving `RegionSlot`'s layout here.
        let live = unsafe { decode_gc_handle_for_drop::<Value>(ptr, is_region) };
        if let Some(gc) = live {
            // Peek-only: undo the (no-op for region handles, but
            // let's not depend on that) ownership transfer by
            // forgetting rather than dropping.
            std::mem::forget(gc);
            true
        } else {
            false
        }
    }
    #[cfg(not(feature = "regions"))]
    {
        true
    }
}

/// Bump the strong refcount behind an owning NB payload, returning
/// the same bits as an independent, additional owner of the same
/// allocation. Immediate/Flonum payloads own nothing — the
/// identity. Mirrors `cs_vm::vm::vm_value_clone_gc`; introduced in
/// PR2 (cs-vnf.3) so `Pair`'s raw `Cell<u64>` car/cdr slots can
/// clone-out their contents without cs-core depending on cs-vm.
/// `pub(crate)`: only `Pair`'s accessors (in `value.rs`) call this.
///
/// # Safety
/// `raw` must be a live owning NB payload (or an inline immediate).
#[inline]
pub(crate) unsafe fn nb_clone_owned(raw: i64) -> i64 {
    let bits = raw as u64;
    if !nb_is_owning(bits) {
        return raw;
    }
    let tag = nb_tag_of(bits);
    let payload = nb_payload_of(bits);
    if tag == NB_TAG_PROCEDURE {
        proc_table::incref(payload as u32);
        return raw;
    }
    let (ptr, is_region) = nb_decode_gc_ptr(payload);
    #[cfg(feature = "regions")]
    if is_region {
        unsafe { cs_gc::Gc::<Value>::raw_incref_region(ptr) };
        return raw;
    }
    #[cfg(not(feature = "regions"))]
    let _ = is_region;
    unsafe { cs_gc::Gc::<Value>::raw_incref(ptr) };
    raw
}

/// Decrement the strong refcount behind an owning NB payload,
/// running the correct destructor for its tag if this was the last
/// owner. Immediate/Flonum payloads own nothing — a no-op. Mirrors
/// `cs_vm::vm::vm_value_drop_gc`; see `nb_clone_owned` for why this
/// exists separately in cs-core.
///
/// # Safety
/// `raw` must be a live owning NB payload this caller exclusively
/// owns (or an inline immediate). Calling this twice on the same
/// owning payload without an intervening `nb_clone_owned` is a
/// double-free.
pub(crate) unsafe fn nb_drop_owned(raw: i64) {
    let bits = raw as u64;
    if !nb_is_owning(bits) {
        return;
    }
    let tag = nb_tag_of(bits);
    let payload = nb_payload_of(bits);
    if tag == NB_TAG_PROCEDURE {
        proc_table::decref(payload as u32);
        return;
    }
    let (ptr, is_region) = nb_decode_gc_ptr(payload);
    // `decode_gc_handle_for_drop` (not `decode_gc_handle`): this is
    // a release, not a read/mutate — an owning region-tagged
    // payload whose region already tore down has nothing left to
    // release (region bulk-free never runs `T::drop`), so we must
    // tolerate that the same way `Gc<T>::drop` does, not panic the
    // way `decode_gc_handle`/`from_raw_jit_region` do for a
    // caller that expects a genuinely live handle.
    match tag {
        t if t == NB_TAG_PAIR => drop(unsafe { decode_gc_handle_for_drop::<Pair>(ptr, is_region) }),
        t if t == NB_TAG_VECTOR => drop(unsafe {
            decode_gc_handle_for_drop::<std::cell::RefCell<Vec<Value>>>(ptr, is_region)
        }),
        t if t == NB_TAG_STRING => {
            drop(unsafe { decode_gc_handle_for_drop::<std::cell::RefCell<CsStr>>(ptr, is_region) })
        }
        t if t == NB_TAG_BYTEVECTOR => drop(unsafe {
            decode_gc_handle_for_drop::<std::cell::RefCell<Vec<u8>>>(ptr, is_region)
        }),
        t if t == NB_TAG_HASHTABLE => {
            drop(unsafe { decode_gc_handle_for_drop::<Hashtable>(ptr, is_region) })
        }
        t if t == NB_TAG_PORT => drop(unsafe { decode_gc_handle_for_drop::<Port>(ptr, is_region) }),
        t if t == NB_TAG_PROMISE => {
            drop(unsafe { decode_gc_handle_for_drop::<Promise>(ptr, is_region) })
        }
        // NB_TAG_GC_VALUE and any other pointer-typed tag — Gc<Value>
        // wrap (mirrors `vm_value_drop_gc`'s default arm). Never
        // region-tagged (catchall is only BigInt/Rational/Complex,
        // none of which are region-allocatable today), so the
        // plain Rc decode stays here unchanged.
        _ => drop(unsafe { cs_gc::Gc::<Value>::from_raw_jit(ptr) }),
    }
}

/// Encode an `f64` as a NaN-boxed `i64`. Real flonums map to their
/// `to_bits()` directly; bit patterns that would collide with the
/// tagged range (sign=1 quiet NaN) are canonicalized to the
/// reserved [`NB_NAN_BITS`] sign=0 NaN.
#[inline]
pub fn nb_encode_flonum(f: f64) -> i64 {
    let bits = f.to_bits();
    if nb_is_tagged(bits) {
        NB_NAN_BITS as i64
    } else {
        bits as i64
    }
}

/// Phase 6 Stage B2 — thread-local registry of `Rc<dyn Procedure>`
/// values for the thin-procedure NB encoding. Each Procedure
/// passed through the NB lane gets a u32 slot index encoded in
/// the NB_TAG_PROCEDURE payload, replacing the previous direct
/// `Gc::new(Value::Procedure(p))` heap allocation per encoding.
///
/// Lifetime semantics mirror the Gc<Value> handle:
/// - `alloc(p)` registers `p` with refcount 1, returns slot idx.
/// - `incref(idx)` bumps refcount (called by `vm_value_clone_gc`).
/// - `decref(idx)` decrements; frees slot at 0 (called by
///   `vm_value_drop_gc`, and by `to_value` when it consumes the NB).
/// - `peek(idx)` clones the Rc without changing refcount (used by
///   helpers that need to inspect without consuming).
///
/// **GC tracing posture:** matches the pre-B2 encoding exactly.
/// `Value::Procedure(_)`'s `Trace` impl is a no-op (line ~321 of
/// cs-core/src/value.rs); captures are rooted via the VM stack /
/// caller env path, not through the NB carrier. Switching to
/// ProcTable doesn't change which Gc handles get traced.
///
/// **Slot reuse:** free list keeps the table compact under
/// steady-state encode/decode churn (nbody-style ~70k encodings/ms
/// settles to a small in-flight count).
// `pub` (not `pub(crate)`): cs-vm calls `proc_table::{alloc,incref,
// decref,peek,take}` from many sites outside `NanboxValue`'s own
// impl (vm_value_clone_gc/drop_gc, call dispatch, tests, …), so the
// module and its free functions need cross-crate reach now that
// they've moved out of cs-vm. cs-vm re-exports the module `pub(crate)`
// to restore the original crate-private encapsulation for its own
// downstream consumers (e.g. cs-jit-cranelift never saw this before
// and shouldn't now either).
//
// SAFETY-ADJACENT: this is a thread-local refcounted slot table; a
// `take`/`decref` from outside the NanboxValue lifecycle can free a
// slot a live NB_TAG_PROCEDURE payload still indexes (index-checked
// panic, not UB). Hidden from docs — not a public API.
#[doc(hidden)]
pub mod proc_table {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::Procedure;

    struct ProcSlot {
        // Some when the slot is live; None when freed and on the free list.
        proc: Option<Rc<Box<dyn Procedure>>>,
        // Live NB carriers pointing at this slot. Slot freed at 0.
        refcount: u32,
        // Free-list link (u32::MAX = tail).
        next_free: u32,
    }

    struct ProcTable {
        slots: Vec<ProcSlot>,
        free_head: u32,
    }

    impl ProcTable {
        const NIL: u32 = u32::MAX;

        const fn new() -> Self {
            Self {
                slots: Vec::new(),
                free_head: Self::NIL,
            }
        }

        pub(super) fn alloc(&mut self, p: Rc<Box<dyn Procedure>>) -> u32 {
            if self.free_head != Self::NIL {
                let idx = self.free_head;
                let slot = &mut self.slots[idx as usize];
                self.free_head = slot.next_free;
                slot.proc = Some(p);
                slot.refcount = 1;
                slot.next_free = Self::NIL;
                idx
            } else {
                let idx = self.slots.len() as u32;
                self.slots.push(ProcSlot {
                    proc: Some(p),
                    refcount: 1,
                    next_free: Self::NIL,
                });
                idx
            }
        }

        pub(super) fn incref(&mut self, idx: u32) {
            let slot = &mut self.slots[idx as usize];
            debug_assert!(slot.proc.is_some(), "incref on freed slot");
            slot.refcount = slot
                .refcount
                .checked_add(1)
                .expect("proc_table refcount overflow");
        }

        /// Decrement refcount; free slot if it hits 0.
        ///
        /// Returns the freed `Rc` (if any) instead of dropping it here:
        /// dropping it inline would run its destructor while `self` is
        /// still under the caller's `RefCell` borrow, and a closure's
        /// destructor can recursively reach `proc_table::decref` (e.g.
        /// via a captured `Bindings` holding another procedure slot),
        /// which would re-borrow the same thread-local and panic. The
        /// caller must drop the returned value only after releasing the
        /// borrow.
        #[must_use]
        pub(super) fn decref(&mut self, idx: u32) -> Option<Rc<Box<dyn Procedure>>> {
            let slot = &mut self.slots[idx as usize];
            debug_assert!(slot.proc.is_some(), "decref on freed slot");
            slot.refcount -= 1;
            if slot.refcount == 0 {
                slot.next_free = self.free_head;
                self.free_head = idx;
                slot.proc.take()
            } else {
                None
            }
        }

        /// Borrow without consuming. The returned Rc is a fresh clone;
        /// the slot's refcount is unchanged.
        pub(super) fn peek(&self, idx: u32) -> Rc<Box<dyn Procedure>> {
            self.slots[idx as usize]
                .proc
                .as_ref()
                .expect("peek on freed slot")
                .clone()
        }
    }

    thread_local! {
        static PROC_TABLE: RefCell<ProcTable> = const { RefCell::new(ProcTable::new()) };
    }

    /// Register `p` and return the new slot index (refcount = 1).
    pub fn alloc(p: Rc<Box<dyn Procedure>>) -> u32 {
        PROC_TABLE.with(|t| t.borrow_mut().alloc(p))
    }

    pub fn incref(idx: u32) {
        // `try_with` tolerates TLS-destruction during process exit:
        // if `PROC_TABLE` has already been destroyed (Bindings drops
        // running after PROC_TABLE's drop) we silently skip — the
        // slot's Rc is leaked but the process is exiting anyway.
        // Without this guard, `vm_value_drop_gc` during the
        // shutdown path panics on `cannot access TLS during
        // destruction`.
        let _ = PROC_TABLE.try_with(|t| t.borrow_mut().incref(idx));
    }

    pub fn decref(idx: u32) {
        // Deferred drop (see `ProcTable::decref`): the freed `Rc`, if
        // any, comes out of `try_with` and is dropped here — after the
        // `RefCell` borrow has been released — so a reentrant decref
        // triggered by its destructor doesn't double-borrow.
        let freed = PROC_TABLE
            .try_with(|t| t.borrow_mut().decref(idx))
            .ok()
            .flatten();
        drop(freed);
    }

    pub fn peek(idx: u32) -> Rc<Box<dyn Procedure>> {
        PROC_TABLE.with(|t| t.borrow().peek(idx))
    }

    /// Consume the NB encoding: clone the Rc out + decrement refcount
    /// (freeing the slot when it was the last NB owner).
    pub fn take(idx: u32) -> Rc<Box<dyn Procedure>> {
        // Deferred drop (see `ProcTable::decref`): `freed` must be
        // dropped only after the borrow below is released.
        let (p, freed) = PROC_TABLE.with(|t| {
            let mut t = t.borrow_mut();
            let p = t.peek(idx);
            let freed = t.decref(idx);
            (p, freed)
        });
        drop(freed);
        p
    }
}

/// NaN-boxed value carrier — the migration target for the bytecode
/// VM dispatch stack (K1 step 3). Layout-identical to `i64`. See
/// the module-level NanboxValue encoding documentation above.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct NanboxValue(pub i64);

impl NanboxValue {
    /// The bit pattern for `Value::Null`. `const fn`-friendly.
    pub const NULL: NanboxValue =
        NanboxValue((NB_SIGNATURE_BITS | (NB_TAG_NULL << NB_TAG_SHIFT)) as i64);
    /// The bit pattern for `Value::Unspecified`.
    pub const UNSPECIFIED: NanboxValue =
        NanboxValue((NB_SIGNATURE_BITS | (NB_TAG_UNSPECIFIED << NB_TAG_SHIFT)) as i64);
    /// The bit pattern for `Value::Eof`.
    pub const EOF: NanboxValue =
        NanboxValue((NB_SIGNATURE_BITS | (NB_TAG_EOF << NB_TAG_SHIFT)) as i64);
    /// `Value::Boolean(false)`.
    pub const FALSE: NanboxValue =
        NanboxValue((NB_SIGNATURE_BITS | (NB_TAG_BOOLEAN << NB_TAG_SHIFT)) as i64);
    /// `Value::Boolean(true)`.
    pub const TRUE: NanboxValue =
        NanboxValue((NB_SIGNATURE_BITS | (NB_TAG_BOOLEAN << NB_TAG_SHIFT) | 1) as i64);

    #[inline]
    pub fn fixnum(n: i64) -> Self {
        if n >= NB_FIXNUM_MIN && n <= NB_FIXNUM_MAX {
            NanboxValue(nb_make(NB_TAG_FIXNUM, (n as u64) & NB_PAYLOAD_MASK) as i64)
        } else {
            // Oversized: wrap in `Gc<Value>` directly via the
            // Wrap as Gc<Value>. (Calling back through `from_value`
            // would re-enter `fixnum` → infinite recursion.)
            let g = cs_gc::Gc::new(Value::Fixnum(n));
            let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
            NanboxValue(nb_make(NB_TAG_GC_VALUE, nb_encode_gc_ptr(ptr, false)) as i64)
        }
    }

    #[inline]
    pub fn boolean(b: bool) -> Self {
        if b {
            Self::TRUE
        } else {
            Self::FALSE
        }
    }

    #[inline]
    pub fn character(c: char) -> Self {
        NanboxValue(nb_make(NB_TAG_CHARACTER, c as u32 as u64) as i64)
    }

    #[inline]
    pub fn symbol(s: Symbol) -> Self {
        NanboxValue(nb_make(NB_TAG_SYMBOL, s.0 as u64) as i64)
    }

    #[inline]
    pub fn flonum(f: f64) -> Self {
        NanboxValue(nb_encode_flonum(f))
    }

    /// Encode a `Value` into the NaN-box. Pointer variants store
    /// their raw `Gc<T>` / `Rc<dyn …>` handle directly in the
    /// 47-bit payload with the matching variant tag — no extra
    /// `Gc<Value>` wrap. Non-Fixnum Number variants (BigInt /
    /// Rational / Complex) take the `NB_TAG_GC_VALUE` catchall
    /// path which DOES allocate a wrapper for these rare cases.
    pub fn from_value(v: Value) -> Self {
        match v {
            Value::Null => Self::NULL,
            Value::Unspecified => Self::UNSPECIFIED,
            Value::Eof => Self::EOF,
            Value::Boolean(b) => Self::boolean(b),
            Value::Character(c) => Self::character(c),
            Value::Symbol(s) => Self::symbol(s),
            Value::Fixnum(n) => Self::fixnum(n),
            Value::Flonum(f) => Self::flonum(f),
            // For pointer variants, take the Gc/Rc raw pointer
            // directly. Pointers must have low bits zero (8-byte
            // align) and upper 16 bits zero (x86-64 user-space),
            // so they fit in 47-bit signed canonical addresses
            // (and our 47-bit payload).
            Value::Pair(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_PAIR, tagged) as i64)
            }
            Value::Vector(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_VECTOR, tagged) as i64)
            }
            Value::String(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_STRING, tagged) as i64)
            }
            Value::ByteVector(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_BYTEVECTOR, tagged) as i64)
            }
            // Phase 6 Stage B2 — thin-procedure NB encoding.
            // `Rc<dyn Procedure>` is a fat pointer (data + vtable,
            // 16 bytes) that doesn't fit the 47-bit payload, so the
            // pre-B2 path wrapped it in `Gc<Value>` (one heap alloc
            // per encoding — nbody's measured ~250M alloc storm).
            //
            // B2 registers `p` in a thread-local `proc_table` and
            // encodes the returned u32 slot index with
            // `NB_TAG_PROCEDURE`. Decoding (`to_value` /
            // `vm_value_clone_gc` / `vm_value_drop_gc`) special-
            // cases this tag against the table. Net effect: no heap
            // alloc per Procedure encoding; ~1 Vec push (or free-
            // list pop) instead.
            //
            // GC tracing: unchanged from the pre-B2 path. The
            // `Value::Procedure(_)` `Trace` impl in cs-core/value.rs
            // is intentionally a no-op (line ~321); captures are
            // already rooted through the VM stack / caller env,
            // never reached via this NB carrier.
            Value::Procedure(p) => {
                let idx = proc_table::alloc(p);
                NanboxValue(nb_make(NB_TAG_PROCEDURE, idx as u64) as i64)
            }
            Value::Hashtable(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_HASHTABLE, tagged) as i64)
            }
            Value::Port(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_PORT, tagged) as i64)
            }
            Value::Promise(g) => {
                let is_region = cs_gc::Gc::is_region(&g);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                let tagged = nb_encode_gc_ptr(ptr, is_region);
                NanboxValue(nb_make(NB_TAG_PROMISE, tagged) as i64)
            }
            // Number variants outside Fixnum/Flonum (BigInt,
            // Rational, Complex), plus hygienic Identifier (a
            // (Symbol, u64-mark) pair that doesn't fit in the
            // 47-bit NB payload alongside its tag). All wrap in
            // Gc<Value> — these are rare in performance-
            // sensitive code. A future iter could carve out a
            // dedicated NB_TAG_IDENTIFIER if identifier-heavy
            // code shows up in the hot path.
            other @ (Value::BigNumber(_) | Value::Rational(_) | Value::Identifier { .. }) => {
                let g = cs_gc::Gc::new(other);
                let ptr = cs_gc::Gc::into_raw_jit(g) as u64;
                NanboxValue(nb_make(NB_TAG_GC_VALUE, nb_encode_gc_ptr(ptr, false)) as i64)
            }
        }
    }

    /// Decode this NaN-box back into a `Value`. Pointer-typed tags
    /// consume one strong refcount (`Rc::from_raw` semantics) and
    /// clone the inner value out. Inline immediates are pure value
    /// extractions.
    ///
    /// # Safety
    ///
    /// `self.0` must be a live, owned encoding from `from_value`
    /// or the equivalent. Passing the same `NanboxValue` to
    /// `to_value` twice is a double-free for the pointer tags.
    pub unsafe fn to_value(self) -> Value {
        let bits = self.0 as u64;
        if !nb_is_tagged(bits) {
            // Not in the tagged range — it's a raw f64.
            return Value::Flonum(f64::from_bits(bits));
        }
        let tag = nb_tag_of(bits);
        let payload = nb_payload_of(bits);
        match tag {
            t if t == NB_TAG_FIXNUM => {
                Value::Fixnum(nb_sign_extend_47(payload))
            }
            t if t == NB_TAG_BOOLEAN => Value::Boolean(payload != 0),
            t if t == NB_TAG_CHARACTER => {
                let cp = (payload as u32) & 0x1F_FFFF;
                Value::Character(char::from_u32(cp).unwrap_or('\u{FFFD}'))
            }
            t if t == NB_TAG_SYMBOL => Value::Symbol(Symbol(payload as u32)),
            t if t == NB_TAG_NULL => Value::Null,
            t if t == NB_TAG_UNSPECIFIED => Value::Unspecified,
            t if t == NB_TAG_EOF => Value::Eof,
            // Each pointer-typed decode reconstitutes the typed
            // `Gc<T>` from the raw pointer payload via
            // `from_raw_jit` (which takes over one strong refcount).
            // We then wrap it in the matching `Value::T(Gc<T>)`
            // variant, **preserving that single strong ref** —
            // no deep clone, no extra `Gc::new` allocation. The
            // returned `Value` owns the same allocation the input
            // `NanboxValue` did; consumer can clone or drop as
            // usual.
            //
            // (`Value::clone` for these variants is an Rc-clone, so
            // even if the caller clones the resulting Value, no deep
            // copy happens.)
            t if t == NB_TAG_PAIR => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<Pair> = decode_gc_handle(ptr, is_region);
                Value::Pair(g)
            }
            t if t == NB_TAG_VECTOR => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<std::cell::RefCell<Vec<Value>>> =
                    decode_gc_handle(ptr, is_region);
                Value::Vector(g)
            }
            t if t == NB_TAG_STRING => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<std::cell::RefCell<CsStr>> =
                    decode_gc_handle(ptr, is_region);
                Value::String(g)
            }
            t if t == NB_TAG_BYTEVECTOR => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<std::cell::RefCell<Vec<u8>>> =
                    decode_gc_handle(ptr, is_region);
                Value::ByteVector(g)
            }
            // Phase 6 Stage B2 — thin-procedure decode. Symmetric
            // to `from_value`'s `Value::Procedure(p)` arm: the
            // payload is the u32 ProcTable slot index. `take`
            // clones the Rc out and decrements the slot's refcount
            // (freeing the slot when this was the last NB pointing
            // at it), matching `to_value`'s consuming-the-NB
            // semantics shared with the other pointer-tag arms.
            t if t == NB_TAG_PROCEDURE => {
                let idx = payload as u32;
                Value::Procedure(proc_table::take(idx))
            }
            t if t == NB_TAG_HASHTABLE => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<Hashtable> = decode_gc_handle(ptr, is_region);
                Value::Hashtable(g)
            }
            t if t == NB_TAG_PORT => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<Port> = decode_gc_handle(ptr, is_region);
                Value::Port(g)
            }
            t if t == NB_TAG_PROMISE => {
                let (ptr, is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<Promise> = decode_gc_handle(ptr, is_region);
                Value::Promise(g)
            }
            _t /* NB_TAG_GC_VALUE or unknown */ => {
                let (ptr, _is_region) = nb_decode_gc_ptr(payload);
                let g: cs_gc::Gc<Value> = unsafe { cs_gc::Gc::from_raw_jit(ptr) };
                (*g).clone()
            }
        }
    }

    /// Raw i64 carrier.
    #[inline]
    pub fn into_raw(self) -> i64 {
        self.0
    }

    /// True iff this is a tagged value (not a raw f64).
    #[inline]
    pub fn is_tagged(self) -> bool {
        nb_is_tagged(self.0 as u64)
    }

    /// True iff this is a Flonum (carries an f64 bit pattern).
    #[inline]
    pub fn is_flonum(self) -> bool {
        !self.is_tagged()
    }

    /// Variant tag (only meaningful when `is_tagged()` is true).
    #[inline]
    pub fn tag(self) -> u64 {
        nb_tag_of(self.0 as u64)
    }

    /// Truthiness for `if` — false iff this is `Value::Boolean(false)`.
    #[inline]
    pub fn is_truthy(self) -> bool {
        self.0 != Self::FALSE.0
    }

    /// Extract a Fixnum payload if applicable.
    #[inline]
    pub fn as_fixnum(self) -> Option<i64> {
        if self.is_tagged() && self.tag() == NB_TAG_FIXNUM {
            Some(nb_sign_extend_47(nb_payload_of(self.0 as u64)))
        } else {
            None
        }
    }

    /// Extract a Boolean payload if applicable.
    #[inline]
    pub fn as_boolean(self) -> Option<bool> {
        if self.is_tagged() && self.tag() == NB_TAG_BOOLEAN {
            Some(nb_payload_of(self.0 as u64) != 0)
        } else {
            None
        }
    }

    /// Extract an f64 if this is a Flonum.
    #[inline]
    pub fn as_flonum(self) -> Option<f64> {
        if self.is_flonum() {
            Some(f64::from_bits(self.0 as u64))
        } else {
            None
        }
    }
}

impl PartialEq for NanboxValue {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for NanboxValue {}
