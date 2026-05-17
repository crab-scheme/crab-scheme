//! `cs-hotreload` — two-version code dispatch + state migration.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`:
//! - Each module holds (old, current) function pointers.
//! - Remote calls (fully-qualified `(my-module my-fn args)`) hit
//!   the current version via the export table.
//! - Local calls (lexically inside the module body) bind to the
//!   version the calling actor entered — lets in-flight calls
//!   finish in old code while new calls hit new code.
//! - `code-soft-purge!` is non-destructive; `code-purge!` kills
//!   actors still holding the old version.
//!
//! ## Status
//!
//! Phase **B6** scaffold only. JIT invalidation lands in B7.

#![allow(dead_code)]

use thiserror::Error;

/// Identifies a code version.
///
/// The pair `(module-name, epoch)` uniquely tags a loaded module
/// version. Epochs increment monotonically per module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodeVersion {
    pub module: cs_core::Symbol,
    pub epoch: u32,
}

/// Per-module slot holding (old, current) versions.
///
/// At most two versions exist at once. Loading a third purges old.
#[derive(Debug)]
pub struct ModuleSlot {
    pub old: Option<CodeVersion>,
    pub current: Option<CodeVersion>,
}

#[derive(Debug, Error)]
pub enum ReloadError {
    #[error("module {module:?} not loaded")]
    NotLoaded { module: cs_core::Symbol },
    #[error("cannot purge {module:?}: {n} actors still on old version")]
    Busy { module: cs_core::Symbol, n: usize },
    #[error("module load failed: {reason}")]
    LoadFailed { reason: String },
}
