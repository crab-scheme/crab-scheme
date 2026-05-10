//! M6 Phase 4 iter BE — JIT stack-map registry + frame scanner.
//!
//! ADR 0012 D-2 commits to Cranelift's user-stack-maps API for
//! precise rooting of JIT-allocated `Gc<Value>`s. This module is
//! the runtime side: a registry mapping each safepoint PC to the
//! list of SP-relative offsets that hold raw `Gc<Value>` handles,
//! plus the routine that walks one frame and hands each live
//! handle to a caller-supplied visitor.
//!
//! No JIT code uses these yet. Iter BF wires `Inst::Cons` to
//! emit `declare_value_needs_stack_map` and populates the registry
//! from `compiled_function.user_stack_maps()` at install time. The
//! visitor is hooked into `Heap::collect()` in a later iter.
//!
//! Why callback-based: the cs-gc `Marker::new` is crate-private, so
//! a unit test can't construct a `Marker` directly. Taking a
//! `FnMut(*const ())` keeps this module independent of the rooting
//! callee — production code wraps the closure with the
//! `raw_incref` + `from_raw_jit` + `Marker::mark` dance.

use std::collections::HashMap;

/// Per-JIT-compiled-function stack-map registry. Maps each safepoint
/// PC (as an offset from the function's start address) to the list
/// of SP-relative byte offsets that hold raw `Gc<Value>` handles
/// at that PC.
///
/// One `JitStackMaps` per `VmClosure` once we wire it into the
/// closure struct in iter BF.
#[derive(Debug)]
pub struct JitStackMaps {
    /// PC-offset (return-address minus function-base) -> list of
    /// SP-relative byte offsets that hold raw `Gc` handles.
    by_pc: HashMap<u32, Vec<i32>>,
    /// Function's start address. PCs are computed as
    /// `return_pc - base`.
    base: *const u8,
}

// SAFETY: the `*const u8` carries no aliasing or interior
// mutability — it's a numeric address used to compute PC offsets.
unsafe impl Send for JitStackMaps {}
unsafe impl Sync for JitStackMaps {}

impl JitStackMaps {
    /// Construct an empty registry anchored at `base`.
    pub fn new(base: *const u8) -> Self {
        Self {
            by_pc: HashMap::new(),
            base,
        }
    }

    /// Record the SP offsets that hold roots at the given PC offset
    /// (PC measured from `base`). Called by the JIT installer after
    /// reading `compiled_function.user_stack_maps()`.
    pub fn insert(&mut self, pc_offset: u32, sp_offsets: Vec<i32>) {
        self.by_pc.insert(pc_offset, sp_offsets);
    }

    /// Total number of safepoint records.
    pub fn len(&self) -> usize {
        self.by_pc.len()
    }

    /// Whether the registry has any safepoints.
    pub fn is_empty(&self) -> bool {
        self.by_pc.is_empty()
    }

    /// Function base address — used by `scan_frame` to convert a
    /// return PC into a key for `by_pc`.
    pub fn base(&self) -> *const u8 {
        self.base
    }
}

/// Walk one JIT'd frame's safepoint metadata and hand each live raw
/// `Gc<Value>` handle to `visit`.
///
/// The contract: `return_pc` is the address the frame would return
/// to (one past the call site). `frame_sp` is the stack pointer at
/// the moment of the safepoint — for x86_64 Cranelift this is the
/// callee's SP at the time of the call. SP-relative offsets in the
/// map are negative when slots sit above (older addresses, lower SP
/// arithmetic), positive when below.
///
/// `visit` receives each non-null raw handle. The callback is
/// responsible for any refcount bookkeeping (typically
/// `raw_incref` + `from_raw_jit` + use + drop).
///
/// # Safety
///
/// - `frame_sp` must point at a live JIT frame on the host stack
///   whose layout matches the map recorded at `return_pc`.
/// - `maps` must be the registry that was active when this frame
///   was compiled.
/// - The visitor must not move or free the handles' allocations.
pub unsafe fn scan_frame<F: FnMut(*const ())>(
    frame_sp: *const u8,
    return_pc: *const u8,
    maps: &JitStackMaps,
    mut visit: F,
) {
    let pc_off = (return_pc as usize).wrapping_sub(maps.base() as usize);
    let pc_off = pc_off as u32;
    let Some(offsets) = maps.by_pc.get(&pc_off) else {
        return;
    };
    for &off in offsets {
        let slot = unsafe { frame_sp.offset(off as isize) } as *const *const ();
        let handle = unsafe { *slot };
        if handle.is_null() {
            continue;
        }
        visit(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a hand-crafted frame on the test thread's stack, register
    /// a fake stack-map record that says "slot @ +0 and @ +8 hold
    /// Gc handles", run scan_frame, and verify the visitor saw both.
    #[test]
    fn scan_frame_visits_recorded_slots() {
        // Allocate two fake handles. We'll use sentinel non-null
        // pointers — scan_frame doesn't dereference them in this
        // test (it just calls the visitor with them).
        let h1 = 0xDEAD_BEEF_usize as *const ();
        let h2 = 0xCAFE_F00D_usize as *const ();
        let null = std::ptr::null::<()>();

        // Pretend the frame has three slots: [h1, null, h2].
        let frame: [*const (); 3] = [h1, null, h2];
        let frame_sp = frame.as_ptr() as *const u8;

        // PC offset 42 is a safepoint that roots slots 0 and 2.
        // Slots are pointer-sized (8 bytes on 64-bit).
        let base = 0x1000 as *const u8;
        let return_pc = unsafe { base.add(42) };
        let mut maps = JitStackMaps::new(base);
        maps.insert(42, vec![0, 16]);

        let mut visited = Vec::new();
        unsafe {
            scan_frame(frame_sp, return_pc, &maps, |h| visited.push(h));
        }

        assert_eq!(visited.len(), 2);
        assert_eq!(visited[0], h1);
        assert_eq!(visited[1], h2);
    }

    /// A PC without any recorded map is silently skipped (the JIT
    /// emits safepoints only at calls; unrelated PCs reach here
    /// during frame walks if the GC fires mid-non-call code).
    #[test]
    fn scan_frame_ignores_unmapped_pc() {
        let frame: [*const (); 1] = [0x1234 as *const ()];
        let frame_sp = frame.as_ptr() as *const u8;
        let base = 0x1000 as *const u8;
        let return_pc = unsafe { base.add(100) };
        let mut maps = JitStackMaps::new(base);
        maps.insert(50, vec![0]); // recorded for PC 50, not 100

        let mut visited = Vec::new();
        unsafe {
            scan_frame(frame_sp, return_pc, &maps, |h| visited.push(h));
        }
        assert!(visited.is_empty());
    }

    /// Null spill slots are skipped — represents "this slot was
    /// optimized away or hasn't been written yet".
    #[test]
    fn scan_frame_skips_null_handles() {
        let frame: [*const (); 3] = [
            std::ptr::null(),
            0xDEAD_BEEF_usize as *const (),
            std::ptr::null(),
        ];
        let frame_sp = frame.as_ptr() as *const u8;
        let base = 0x1000 as *const u8;
        let return_pc = unsafe { base.add(0) };
        let mut maps = JitStackMaps::new(base);
        maps.insert(0, vec![0, 8, 16]);

        let mut visited = Vec::new();
        unsafe {
            scan_frame(frame_sp, return_pc, &maps, |h| visited.push(h));
        }
        assert_eq!(visited.len(), 1);
        assert_eq!(visited[0], 0xDEAD_BEEF_usize as *const ());
    }
}
