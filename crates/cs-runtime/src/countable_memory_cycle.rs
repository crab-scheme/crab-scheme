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

#![cfg(feature = "countable-memory")]

use std::cell::Cell;

thread_local! {
    static CYCLE_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Increment the per-thread cycle-detection counter. Called from
/// the `break_at` callback in mutation builtins when the
/// synchronous detector reports a cycle.
pub fn record_cycle_detected() {
    CYCLE_COUNT.with(|c| c.set(c.get().saturating_add(1)));
}

/// Read the per-thread cycle-detection counter.
pub fn cycle_detection_count() -> u64 {
    CYCLE_COUNT.with(|c| c.get())
}

/// Reset the per-thread cycle-detection counter to 0.
pub fn reset_cycle_detection_count() {
    CYCLE_COUNT.with(|c| c.set(0));
}
