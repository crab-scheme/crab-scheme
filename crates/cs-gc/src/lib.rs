//! CrabScheme heap-pointer crate (layer 2 of the unified memory
//! architecture, ADR 0015).
//!
//! `Gc<T>` is a thin newtype around `Rc<T>` (the `rc_only` module);
//! cycle handling lives in a sibling [`cycle`] module that wraps the
//! synchronous detector + Bacon-Rajan trial deletion. Public surface
//! (`new`, `Clone`, `Deref`, `PartialEq`, `Debug`, `ptr_eq`,
//! `as_addr`, `into_raw_jit`, `from_raw_jit`, `raw_incref`,
//! `downgrade`, `strong_count`, `Weak<T>`) is what every consumer in
//! the workspace targets.
//!
//! Optional sibling modules add the other architecture layers:
//!
//! - [`region`] (feature `regions`, on by default) — layer 3 bump
//!   arenas (ADR 0016).
//! - [`cycle_registry`] (feature `tracing-cycle-collector`, off by
//!   default) — layer 4 residual-cycle sweep (ADR 0018).
//!
//! ADR 0014 (iter 12b) deleted the M5 Phase 1 precise tracing GC
//! that previously lived here; see `docs/adr/0006-gc-design.md` for
//! the superseded design and `docs/milestones/countable-memory-exit.md`
//! for the transition history.

#![allow(clippy::missing_safety_doc)]

mod rc_only;
pub use rc_only::{Gc, Weak};

pub mod alloc_telemetry;

pub mod cycle;

#[cfg(feature = "tracing-cycle-collector")]
pub mod cycle_registry;

#[cfg(feature = "regions")]
pub mod region;
#[cfg(feature = "regions")]
pub use region::{Region, RegionId};
