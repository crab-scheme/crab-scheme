//! Synchronous cycle-detection telemetry for countable-memory.
//!
//! Iter 7 wires the cycle detector into mutation builtins
//! (`set-car!`, `set-cdr!`, `vector-set!`, `hashtable-set!`).
//! When a cycle is found the runtime records it via
//! [`record_cycle_detected`] but does not yet flip the offending
//! storage edge to `Weak<T>` — that is the full iter-7
//! deliverable per `.spec-workflow/specs/countable-memory/design.md`
//! §"Component 5" and is tracked as a follow-up iter (7.1).
//!
//! For now the counter exists so:
//! - Tests can assert the detector fires on the right shapes
//!   (iter 9 cycle_break.rs regression suite).
//! - Embedders can introspect via [`cycle_detection_count`].
//!
//! The user-visible semantics of `(set-cdr! x x)` are unchanged
//! from M5 Phase 1: the operation succeeds and produces a cyclic
//! list. The cycle leaks at refcount-drop time. Iter 7.1 closes
//! that leak via Strong/Weak storage slot enums.

use std::cell::Cell;

thread_local! {
    static CYCLE_COUNT: Cell<u64> = const { Cell::new(0) };
    static CYCLE_BROKEN_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Increment the per-thread cycle-detection counter. Called from
/// the `break_at` callback in mutation builtins when the
/// synchronous detector reports a cycle.
pub fn record_cycle_detected() {
    CYCLE_COUNT.with(|c| c.set(c.get().saturating_add(1)));
}

/// Increment the per-thread cycle-broken counter. Called when
/// `Pair::break_*_cycle` returns true (the strong-count guard
/// permitted a safe demote and the slot was actually flipped to
/// a Weak tombstone — see iter 7.1.x).
pub fn record_cycle_broken() {
    CYCLE_BROKEN_COUNT.with(|c| c.set(c.get().saturating_add(1)));
}

/// Read the per-thread cycle-detection counter.
pub fn cycle_detection_count() -> u64 {
    CYCLE_COUNT.with(|c| c.get())
}

/// Read the per-thread cycle-broken counter. Lower than the
/// detection count for cycles the strong-count guard refused to
/// break (the unsafe case where the slot was the only strong
/// holder).
pub fn cycle_broken_count() -> u64 {
    CYCLE_BROKEN_COUNT.with(|c| c.get())
}

/// Reset both per-thread cycle counters to 0.
pub fn reset_cycle_detection_count() {
    CYCLE_COUNT.with(|c| c.set(0));
    CYCLE_BROKEN_COUNT.with(|c| c.set(0));
}

/// Record a detected cycle and, when the
/// `tracing-cycle-collector` feature is on, register the
/// candidate with the layer-4 sweep registry so a future
/// `(collect)` / auto-trigger pass can reclaim it if the
/// synchronous detector's break-attempt didn't succeed.
///
/// Region-allocated values are excluded — their cycles
/// reclaim via the region's bulk-free (layer 3).
///
/// When `tracing-cycle-collector` is OFF this is identical to
/// [`record_cycle_detected`] — the registration call compiles
/// to nothing. Mutation builtins (`set-car!`, `set-cdr!`,
/// `vector-set!`, `hashtable-set!`) call this unconditionally
/// from their cycle-break callbacks; the feature flag
/// controls whether the candidate is also registered.
/// `tracing-cycle-collector` ON: full bounds + registry call.
#[cfg(feature = "tracing-cycle-collector")]
pub fn record_cycle_with_candidate<T>(p: &cs_gc::Gc<T>)
where
    T: 'static
        + cs_gc::cycle::CycleVisit
        + cs_gc::cycle::BreakCycle
        + cs_gc::cycle_registry::CycleChildren,
{
    record_cycle_detected();
    #[cfg(feature = "regions")]
    if cs_gc::Gc::is_region(p) {
        // Region cycle — bulk-free handles it; no need to
        // register for the layer-4 sweep.
        return;
    }
    let addr = cs_gc::Gc::as_addr(p);
    let weak = cs_gc::Gc::downgrade(p);
    cs_gc::cycle_registry::register_cycle_candidate(addr, weak);
}

/// `tracing-cycle-collector` OFF: detection telemetry only,
/// no CycleChildren requirement on `T` (the trait doesn't
/// even exist in this build).
#[cfg(not(feature = "tracing-cycle-collector"))]
pub fn record_cycle_with_candidate<T>(p: &cs_gc::Gc<T>)
where
    T: 'static + cs_gc::cycle::CycleVisit + cs_gc::cycle::BreakCycle,
{
    record_cycle_detected();
    let _ = p;
}
