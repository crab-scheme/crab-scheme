//! IC slot layout shim — cs-vm side.
//!
//! `cs-jit-cranelift::ic::IcSlot` defines the authoritative
//! `#[repr(C)]` layout (BR + ADR 0012 D-1). cs-vm can't import it
//! directly without a circular dep (cs-jit-cranelift already depends
//! on cs-vm). The shim below has identical field order, types, and
//! `#[repr(C)]` discipline, so a raw pointer cast between the two is
//! ABI-safe. Both crates must update layout in lockstep — see the
//! "Layout invariant" comment on cs-jit-cranelift's IcSlot.
//!
//! ADR 0012 D-1 (iter BY) — used by `vm_call_general` to update the
//! per-call-site IC slot on dispatch.

use std::sync::atomic::{AtomicPtr, AtomicU32};

/// ABI-compatible mirror of `cs_jit_cranelift::ic::IcSlot`. Same
/// field order, same atomic shapes, same `#[repr(C)]`.
#[repr(C)]
pub struct IcSlotShim {
    pub cached_closure_id: AtomicU32,
    pub cached_jit_ptr: AtomicPtr<()>,
    pub cached_arity: AtomicU32,
    pub cached_param_types: AtomicU32,
    pub miss_count: AtomicU32,
}
