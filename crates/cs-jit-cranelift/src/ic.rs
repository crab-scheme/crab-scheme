//! Inline-cache (IC) infrastructure for the Cranelift JIT.
//!
//! Implements the storage half of the per-call-site monomorphic-first
//! IC described in `docs/research/jit_inline_cache.md` and ratified by
//! ADR 0012 D-1.
//!
//! Shape (per ADR 0012 D-1):
//!
//! | Knob              | Choice                                         |
//! | ----------------- | ---------------------------------------------- |
//! | Slot location     | keyed table in the `Lowerer` (`IcTable`)       |
//! | Slot addressing   | Pointer constant baked into JIT body           |
//! | Cache key         | `u32` stable closure id from [`cs_vm::vm::VmClosure::closure_id`] |
//! | Cache value       | `(jit_ptr, arity, param_types: u32)`           |
//! | Polymorphism cap  | Deferred — mono slot only                      |
//!
//! Each [`IcSlot`] is `#[repr(C)]` so its field offsets are stable
//! across compilations. Codegen loads `slot_ptr + offset_of!(cached_
//! closure_id)` etc. directly via Cranelift `load.i32` / `load.i64`
//! instructions; the frozen layout means the lowering code can use
//! plain immediate offsets instead of going through Rust's
//! `offset_of!`.
//!
//! cs-xop — [`IcTable`] is keyed by `(lambda_id, site_idx)` rather
//! than a flat `Vec` index. A recompile of the same lambda (e.g. a
//! deopt retry, see `cs_jit::DeoptState` / `MAX_DEOPT_RETRIES`) asks
//! for the same key and gets back the *same* slot, so warm inline-
//! cache state survives the recompile instead of resetting to cold.
//! It also means call sites no longer `Box::leak` a fresh slot per
//! compile — the table owns every slot and drops them when the
//! `Lowerer` (and thus the table) is dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicU32};

/// One per-call-site monomorphic inline-cache slot.
///
/// All fields are atomic so the JIT body, which may eventually
/// race the miss-handler thread, observes coherent transitions
/// from "uninitialized" to "filled". Single-threaded execution
/// is the rule today (ADR 0011 §Negative), but the atomic shape
/// is free at this scale and keeps future expansion open — V8's
/// FeedbackVector, JSC's StructureStubInfo, and SpiderMonkey
/// CacheIR all rely on the same "stable address, mutable
/// contents" discipline.
///
/// **Layout invariant**: `#[repr(C)]` plus field order
/// `(cached_closure_id, cached_jit_ptr, cached_arity,
/// cached_param_types, miss_count)`. JIT-emitted code hard-codes
/// byte offsets against this layout — see the dispatch sequence
/// in `docs/research/jit_inline_cache.md` §3.4. Don't reorder
/// without bumping the IC ABI.
#[repr(C)]
pub struct IcSlot {
    /// Cached [`cs_vm::vm::VmClosure::closure_id`] of the last
    /// callee that hit this site. `0` is the sentinel for
    /// "uninitialized / miss" — every live closure has a
    /// non-zero id (see `cs_vm::vm::alloc_closure_id`), so the
    /// zero state is unambiguous and the IC's mono hit-check
    /// can be a single `icmp.eq` against this field.
    pub cached_closure_id: AtomicU32,
    /// Cached native function pointer the JIT body jumps to on
    /// a hit. Mirrors `VmClosure::jit_ptr` at the moment the
    /// cache was filled. Stored as `AtomicPtr<()>` so the
    /// representation is bare-pointer-sized and the load can
    /// be a single instruction in JIT-emitted code.
    pub cached_jit_ptr: AtomicPtr<()>,
    /// Arity the cached `cached_jit_ptr` was compiled for.
    /// Mirrors `VmClosure::jit_arity`. The IC fast path uses
    /// this to skip the indirect closure-struct load.
    pub cached_arity: AtomicU32,
    /// Packed per-param JIT type tags as they appeared in
    /// `VmClosure::jit_param_types` at fill time. Same nibble
    /// encoding as `cs_vm::vm::JIT_RT_*` (4 bits per param,
    /// low nibble = arg 0). The closure-id check is the
    /// primary guard; this field is retained so the miss
    /// helper can short-circuit on signature change.
    pub cached_param_types: AtomicU32,
    /// Count of misses observed at this slot. Bumped by the
    /// miss helper; once it crosses the polymorphic promotion
    /// threshold the slot could transition to a chain (not yet
    /// implemented — out of scope here).
    pub miss_count: AtomicU32,
}

impl IcSlot {
    /// Fresh slot in the "miss / uninitialized" state. All
    /// fields zeroed; `cached_closure_id == 0` is the IC's
    /// sentinel for "no hit recorded".
    pub fn new() -> Self {
        Self {
            cached_closure_id: AtomicU32::new(0),
            cached_jit_ptr: AtomicPtr::new(std::ptr::null_mut()),
            cached_arity: AtomicU32::new(0),
            cached_param_types: AtomicU32::new(0),
            miss_count: AtomicU32::new(0),
        }
    }
}

impl Default for IcSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-module IC slot storage. Owned by the [`crate::Lowerer`];
/// every IC-bearing call site allocated by lowering asks for a
/// slot by `(lambda_id, site_idx)` — the process-wide unique
/// `LambdaProfile::lambda_id` of the lambda being compiled, and
/// this call site's ordinal position within that lambda's body.
/// The slot's address (a raw `*const IcSlot`) is the constant
/// pointer the JIT body's load-compare-call sequence uses.
///
/// Each slot is individually heap-allocated (`Box<IcSlot>`), so
/// growing the table (inserting a new key) never moves an
/// already-handed-out slot address — only the `HashMap`'s
/// internal bucket array moves, not the boxed slots it points
/// to. JIT bodies bake slot addresses in as `iconst` immediates
/// at lowering time, so this stability is load-bearing: a moved
/// slot would corrupt every previously-compiled body referencing
/// it.
pub struct IcTable {
    slots: HashMap<(u64, u32), Box<IcSlot>>,
}

impl IcTable {
    /// Build an empty table. `n` is a capacity hint (number of IC
    /// sites expected over the table's lifetime); it just avoids
    /// early `HashMap` reallocations.
    pub fn new(n: usize) -> Self {
        Self {
            slots: HashMap::with_capacity(n),
        }
    }

    /// Current number of allocated slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True if no slots have been allocated yet.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Fetch the stable address of the slot for `(lambda_id,
    /// site_idx)`, allocating a fresh miss-state slot on first
    /// use. A later call with the same key (e.g. a deopt
    /// recompile of the same lambda re-lowering the same call
    /// site) returns the *same* address — and thus the same
    /// accumulated warm state — instead of a fresh cold one.
    pub fn get_or_alloc(&mut self, lambda_id: u64, site_idx: u32) -> *const IcSlot {
        &**self
            .slots
            .entry((lambda_id, site_idx))
            .or_insert_with(|| Box::new(IcSlot::new())) as *const IcSlot
    }
}

impl Default for IcTable {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// A fresh `IcSlot` must represent the "miss/uninitialized"
    /// state. The IC reserves `cached_closure_id == 0` for this
    /// case, and the rest of the slot is zeroed in lockstep.
    #[test]
    fn ic_slot_default_is_miss() {
        let slot = IcSlot::new();
        assert_eq!(slot.cached_closure_id.load(Ordering::Relaxed), 0);
        assert!(slot.cached_jit_ptr.load(Ordering::Relaxed).is_null());
        assert_eq!(slot.cached_arity.load(Ordering::Relaxed), 0);
        assert_eq!(slot.cached_param_types.load(Ordering::Relaxed), 0);
        assert_eq!(slot.miss_count.load(Ordering::Relaxed), 0);

        // `Default` parity with `new` so the lowerer can use
        // either uniformly.
        let d = IcSlot::default();
        assert_eq!(d.cached_closure_id.load(Ordering::Relaxed), 0);
    }

    /// Round-trip a non-zero closure id through the atomic
    /// field. This is the minimum the miss helper needs — store
    /// the live closure's id, and a subsequent load reads the
    /// same value back.
    #[test]
    fn ic_slot_atomic_writes_are_visible() {
        let slot = IcSlot::new();
        slot.cached_closure_id.store(0xDEAD_BEEF, Ordering::Relaxed);
        slot.cached_arity.store(2, Ordering::Relaxed);
        slot.cached_param_types.store(0x12, Ordering::Relaxed);
        slot.miss_count.store(7, Ordering::Relaxed);
        // A small non-null pointer suffices — we're testing the
        // atomic, not dereferencing.
        let p = 0x1000usize as *mut ();
        slot.cached_jit_ptr.store(p, Ordering::Relaxed);

        assert_eq!(slot.cached_closure_id.load(Ordering::Relaxed), 0xDEAD_BEEF);
        assert_eq!(slot.cached_arity.load(Ordering::Relaxed), 2);
        assert_eq!(slot.cached_param_types.load(Ordering::Relaxed), 0x12);
        assert_eq!(slot.miss_count.load(Ordering::Relaxed), 7);
        assert_eq!(slot.cached_jit_ptr.load(Ordering::Relaxed), p);
    }

    /// `get_or_alloc` returns distinct addresses for distinct
    /// keys, each starting in the miss state.
    #[test]
    fn ic_table_grows_on_demand() {
        let mut table = IcTable::new(0);
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());

        let p0 = table.get_or_alloc(1, 0);
        let p1 = table.get_or_alloc(1, 1);
        let p2 = table.get_or_alloc(1, 2);
        assert_ne!(p0, p1);
        assert_ne!(p1, p2);
        assert_eq!(table.len(), 3);

        // Newly allocated slot is in the miss state.
        let s1 = unsafe { &*p1 };
        assert_eq!(s1.cached_closure_id.load(Ordering::Relaxed), 0);

        // Writing to one slot does not bleed into another.
        s1.cached_closure_id.store(5, Ordering::Relaxed);
        let s0 = unsafe { &*p0 };
        let s2 = unsafe { &*p2 };
        assert_eq!(s0.cached_closure_id.load(Ordering::Relaxed), 0);
        assert_eq!(s1.cached_closure_id.load(Ordering::Relaxed), 5);
        assert_eq!(s2.cached_closure_id.load(Ordering::Relaxed), 0);
    }

    /// The core cs-xop property: asking for the same `(lambda_id,
    /// site_idx)` key twice — modeling a deopt recompile re-
    /// lowering the same call site — returns the SAME address, so
    /// any warm state accumulated on the first compile survives.
    #[test]
    fn ic_table_reuses_slot_for_same_key() {
        let mut table = IcTable::new(0);
        let p_first = table.get_or_alloc(42, 3);
        unsafe { &*p_first }
            .cached_closure_id
            .store(99, Ordering::Relaxed);

        // Simulate a recompile: same lambda_id, same site_idx.
        let p_second = table.get_or_alloc(42, 3);
        assert_eq!(p_first, p_second, "recompile must reuse the warm slot");
        assert_eq!(table.len(), 1, "no new slot was allocated");
        assert_eq!(
            unsafe { &*p_second }
                .cached_closure_id
                .load(Ordering::Relaxed),
            99,
            "warm state survived the recompile"
        );

        // A different site (or lambda) still gets its own slot.
        let p_other_site = table.get_or_alloc(42, 4);
        let p_other_lambda = table.get_or_alloc(43, 3);
        assert_ne!(p_first, p_other_site);
        assert_ne!(p_first, p_other_lambda);
        assert_eq!(table.len(), 3);
    }

    /// Growing the table (inserting many new keys, forcing the
    /// `HashMap`'s bucket array to reallocate) must not move
    /// already-issued slot addresses — JIT bodies bake these in
    /// as compile-time constants.
    #[test]
    fn ic_table_slot_addresses_survive_growth() {
        let mut table = IcTable::new(0);
        let p0 = table.get_or_alloc(1, 0);
        for i in 1..500 {
            table.get_or_alloc(1, i);
        }
        let p0_again = table.get_or_alloc(1, 0);
        assert_eq!(p0, p0_again, "slot address must be stable across growth");
    }
}
