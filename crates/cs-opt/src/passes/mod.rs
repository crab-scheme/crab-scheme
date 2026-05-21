//! Built-in passes shipped with cs-opt (ADR 0014 iter 2).
//!
//! Three small but real passes. Their job is to exercise the
//! framework end-to-end and serve as worked references for
//! plugin authors. None is revolutionary; together they touch
//! every surface the framework exposes (traversal, mutation,
//! statistics, ordering).

pub mod constant_fold;
pub mod dead_block_elim;
pub mod escape_to_region;
pub mod inst_stats;

use std::sync::Arc;

use crate::{PassRegistry, RegisterError};

/// Register every shipped builtin into `registry`. Called once at
/// process startup by embedders (typically `cs_runtime::Runtime::new`
/// when iter 3 wires it through).
///
/// Returns the first registration error if any pass fails (e.g.,
/// the registry already has a pass with the same name).
/// Re-registering the same builtin set into the same registry is
/// rejected on the duplicate path — embedders should call this
/// exactly once per registry.
pub fn register_builtins(registry: &mut PassRegistry) -> Result<(), RegisterError> {
    registry.register(Arc::new(constant_fold::ConstantFold))?;
    registry.register(Arc::new(dead_block_elim::DeadBlockElim))?;
    registry.register(Arc::new(escape_to_region::EscapeToRegion))?;
    registry.register(Arc::new(inst_stats::InstStats))?;
    Ok(())
}

/// Names of every builtin, in registration order. Used by tests +
/// diagnostics that want to know what shipped.
pub const BUILTIN_NAMES: &[&str] = &[
    "constant-fold",
    "dead-block-elim",
    "escape-to-region",
    "inst-stats",
];
