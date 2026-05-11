//! Inline-cache (IC) infrastructure for the Cranelift JIT.
//!
//! Implements the storage half of the per-call-site monomorphic-first
//! IC described in `docs/research/jit_inline_cache.md` and ratified by
//! ADR 0012 D-1. iter BR ships **only** the data structures — codegen
//! (the load-compare-call sequence baked into JIT bodies) lands in
//! later iters (BS onward) once this skeleton is verified.
//!
//! Shape (per ADR 0012 D-1):
//!
//! | Knob              | Choice                                         |
//! | ----------------- | ---------------------------------------------- |
//! | Slot location     | `Vec<IcSlot>` in the `Lowerer` (`IcTable`)     |
//! | Slot addressing   | Pointer constant baked into JIT body           |
//! | Cache key         | `u32` stable closure id from [`cs_vm::vm::VmClosure::closure_id`] |
//! | Cache value       | `(jit_ptr, arity, param_types: u32)`           |
//! | Polymorphism cap  | Deferred — iter BR only wires the mono slot    |
//!
//! Each [`IcSlot`] is `#[repr(C)]` so its field offsets are stable
//! across compilations. Future codegen (iter BS+) will load
//! `slot_ptr + offset_of!(cached_closure_id)` etc. directly via
//! Cranelift `load.i32` / `load.i64` instructions; freezing the
//! layout now means the lowering code can use plain immediate
//! offsets instead of going through Rust's `offset_of!`.

use std::sync::atomic::{AtomicPtr, AtomicU32};

/// One per-call-site monomorphic inline-cache slot.
///
/// All fields are atomic so the JIT body, which may eventually
/// race the miss-handler thread, observes coherent transitions
/// from "uninitialized" to "filled". Single-threaded execution
/// is the rule today (ADR 0011 §Negative), but the atomic shape
/// is free at the iter-BR scale and keeps future expansion open
/// — V8's FeedbackVector, JSC's StructureStubInfo, and
/// SpiderMonkey CacheIR all rely on the same "stable address,
/// mutable contents" discipline.
///
/// **Layout invariant**: `#[repr(C)]` plus field order
/// `(cached_closure_id, cached_jit_ptr, cached_arity,
/// cached_param_types, miss_count)`. JIT-emitted code in later
/// iters will hard-code byte offsets against this layout — see
/// the dispatch sequence in `docs/research/jit_inline_cache.md`
/// §3.4. Don't reorder without bumping the IC ABI.
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
    /// (future) miss helper; once it crosses the polymorphic
    /// promotion threshold the slot transitions to a chain
    /// (iter BU onward — out of scope for iter BR).
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
/// every IC-bearing call site allocated by lowering takes one
/// index into this table, and the slot's address (`&table.slots[i]`)
/// is the constant pointer the JIT body's load-compare-call sequence
/// uses (iter BS+).
///
/// The `Vec` is grown only at compile time (between lowering and
/// finalization); JIT bodies hold raw pointers into it, so the
/// table must not reallocate while a JIT body is live. The
/// `alloc_slot` API enforces this contract by returning an index
/// the caller can persist while still owning a mutable handle to
/// the table — i.e. all allocations happen before any JIT body
/// is finalized against the resulting addresses.
pub struct IcTable {
    slots: Vec<IcSlot>,
}

impl IcTable {
    /// Build a table with capacity for `n` slots. The slots
    /// themselves are not allocated until [`alloc_slot`] is
    /// called; the capacity hint just avoids a reallocation on
    /// the first push when the lowerer knows up front how many
    /// IC sites it will emit.
    pub fn new(n: usize) -> Self {
        Self {
            slots: Vec::with_capacity(n),
        }
    }

    /// Borrow the slot at `idx`. Panics on out-of-bounds — the
    /// lowerer obtains indices from `alloc_slot` and never
    /// fabricates them, so any out-of-bounds access is a bug.
    pub fn slot(&self, idx: usize) -> &IcSlot {
        &self.slots[idx]
    }

    /// Current number of allocated slots. Mirrors `Vec::len`
    /// semantics — the next [`alloc_slot`] call returns this
    /// value.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True if no slots have been allocated yet.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Push a fresh slot and return its index. Monotonic — each
    /// call returns `len() - 1` *after* the push, so indices
    /// double as compact identifiers for the call site. Future
    /// iter-BS lowering will hand the returned index back as a
    /// constant baked into the JIT body.
    pub fn alloc_slot(&mut self) -> usize {
        let idx = self.slots.len();
        self.slots.push(IcSlot::new());
        idx
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
    /// field. This is the minimum the (future) miss helper needs
    /// — store the live closure's id, and a subsequent load
    /// reads the same value back.
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

    /// `alloc_slot` returns monotonically increasing indices and
    /// each returned slot starts in the miss state. Future
    /// lowering relies on this to map call-site -> slot index
    /// 1:1.
    #[test]
    fn ic_table_grows_on_demand() {
        let mut table = IcTable::new(0);
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());

        let i0 = table.alloc_slot();
        let i1 = table.alloc_slot();
        let i2 = table.alloc_slot();
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        assert_eq!(i2, 2);
        assert_eq!(table.len(), 3);

        // Newly allocated slot is in the miss state.
        assert_eq!(table.slot(i1).cached_closure_id.load(Ordering::Relaxed), 0);

        // Writing to one slot does not bleed into another.
        table.slot(i1).cached_closure_id.store(5, Ordering::Relaxed);
        assert_eq!(table.slot(i0).cached_closure_id.load(Ordering::Relaxed), 0);
        assert_eq!(table.slot(i1).cached_closure_id.load(Ordering::Relaxed), 5);
        assert_eq!(table.slot(i2).cached_closure_id.load(Ordering::Relaxed), 0);
    }
}
