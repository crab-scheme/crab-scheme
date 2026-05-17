//! CrabScheme heap-pointer crate.
//!
//! Two representations of `Gc<T>` live here, selected at compile
//! time by the `countable-memory` feature:
//!
//! - **Default (feature off): tracing variant.** M5 Phase 1
//!   precise mark-sweep GC. `Gc<T>` wraps `Rc<Slot<T>>` where
//!   `Slot<T>` adds a mark cell consumed by the tracing layer.
//!   The crate exports `Heap`, `Trace`, `Marker`, `add_root`,
//!   `collect`. See `tracing.rs` and `docs/adr/0006-gc-design.md`.
//!
//! - **`countable-memory` on: Rc-only variant.** `Gc<T>` is a thin
//!   newtype around `Rc<T>` with no per-slot bookkeeping. Cycle
//!   handling moves out of this crate (to a sibling `cycle`
//!   module in iter 3 of the countable-memory spec; to `Weak<T>`
//!   back-edges in iter 8). See `rc_only.rs` and
//!   `.spec-workflow/specs/countable-memory/`.
//!
//! Both variants expose `Gc<T>`'s public API surface (`new`,
//! `Clone`, `Deref`, `PartialEq`, `Debug`, `ptr_eq`, `as_addr`,
//! `into_raw_jit`, `from_raw_jit`, `raw_incref`). The Rc-only
//! variant also exposes `downgrade`, `strong_count`, and `Weak<T>`.
//!
//! The two are mutually exclusive: iter 11 of the countable-memory
//! spec flips the feature default-on, iter 12 deletes the tracing
//! variant entirely.

#![allow(clippy::missing_safety_doc)]

#[cfg(not(feature = "countable-memory"))]
mod tracing;
#[cfg(not(feature = "countable-memory"))]
pub use tracing::{Gc, Heap, Marker, Trace};

#[cfg(feature = "countable-memory")]
mod rc_only;
#[cfg(feature = "countable-memory")]
pub use rc_only::{Gc, Weak};

#[cfg(feature = "countable-memory")]
pub mod cycle;

#[cfg(feature = "tracing-cycle-collector")]
pub mod cycle_registry;

#[cfg(feature = "regions")]
pub mod region;
#[cfg(feature = "regions")]
pub use region::{Region, RegionId};
