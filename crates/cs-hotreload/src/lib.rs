//! `cs-hotreload` — two-version code dispatch + state migration.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md` (and the
//! Rust/Scheme split clarification in commit 051efd4):
//!
//! - **Rust** (this crate): the **version table**. Holds at most
//!   two versions per module — `old` (just-superseded) and
//!   `current` (most recently loaded). Loading a third version
//!   purges `old` (any actor still holding it will fail on its
//!   next call into that version's exports). Export entries are
//!   opaque [`Export`] handles (`Arc<dyn Any + Send + Sync>`)
//!   that the Scheme primop layer cast to its closure / fn-ptr
//!   shape.
//!
//! - **Scheme** (cs-runtime + a prelude file): the **dispatch
//!   policy** — when an actor enters version N's code, it pins
//!   that version for the duration of its call; subsequent
//!   `code-soft-purge!` checks "is anyone still pinned to the old
//!   version?" Scheme also drives the `code-change` callback
//!   that migrates per-actor state across versions.
//!
//! ## API surface
//!
//! - [`VersionRegistry::register`] — first-time module load.
//! - [`VersionRegistry::load`] — replace current; demote previous
//!   current to old.
//! - [`VersionRegistry::lookup`] — get the current version's
//!   export for `(module, fn_name)`.
//! - [`VersionRegistry::lookup_old`] — same but for the old
//!   version, if any. Used by Scheme code that wants to finish
//!   an in-flight call in the old version.
//! - [`VersionRegistry::soft_purge`] — drops `old` iff `holders`
//!   is empty (Scheme tracks the holder set).
//! - [`VersionRegistry::purge`] — drops `old` unconditionally
//!   (caller is asserting "I know nothing's still using it").
//! - [`VersionRegistry::epochs`] — `(old?, current)` epoch
//!   numbers, for diagnostics + the Scheme-facing
//!   `(code-versions 'mod)` primop.
//!
//! ## Status
//!
//! Phase **B6** — version table works end-to-end. B7 will wire
//! JIT invalidation (drop cs-jit-cranelift's JITModule entries
//! for transitive callers of the reloaded module) and provide
//! Scheme-callable `code-change` hooks for state migration. AOT
//! is fundamentally out — once compiled to a Rust source +
//! built, an AOT module is a frozen artifact and `load` will
//! error on it.

#![allow(dead_code)]

use std::sync::Arc;

use dashmap::DashMap;
use rustc_hash::FxHashMap;

// Re-export FxHashMap so downstream crates (cs-runtime's
// `actor`-gated beam module) can build the exports map without
// adding rustc_hash to their own deps.
pub use rustc_hash::FxHashMap as ExportsMap;
use thiserror::Error;

// ---------- Types ----------

/// Opaque function-or-data handle stored in the version table.
///
/// The Scheme primop layer wraps each module export in an Arc<>
/// of whatever shape that export takes — a VmClosure, an AOT fn
/// pointer, a constant value, etc. cs-hotreload doesn't care
/// about the shape; it just stores and returns the Arc.
pub type Export = Arc<dyn std::any::Any + Send + Sync>;

/// One loaded version of a module's exports.
#[derive(Clone)]
pub struct ModuleVersion {
    /// Monotonic version number. 1, 2, 3, ... per module.
    pub epoch: u32,
    /// All exports keyed by name (e.g., function name as a
    /// String). Cheap to clone (Arc<HashMap> via cloning the
    /// containing ModuleVersion).
    pub exports: Arc<FxHashMap<String, Export>>,
}

impl ModuleVersion {
    /// Lookup an export by name.
    pub fn get(&self, name: &str) -> Option<Export> {
        self.exports.get(name).cloned()
    }
}

/// The (old, current) pair for one module.
#[derive(Clone, Default)]
struct ModuleSlot {
    old: Option<ModuleVersion>,
    current: Option<ModuleVersion>,
    /// Next epoch to issue. Starts at 1; bumped on every load.
    next_epoch: u32,
}

// ---------- Errors ----------

#[derive(Debug, Error)]
pub enum ReloadError {
    #[error("module {module:?} not loaded")]
    NotLoaded { module: String },
    #[error("cannot soft-purge {module:?}: {holder_count} actor(s) still pinned to old version")]
    StillHeld { module: String, holder_count: usize },
    #[error("module {module:?} has no old version to purge")]
    NoOldVersion { module: String },
}

// ---------- Registry ----------

/// Process-wide version table. Cloneable cheaply (Arc inside).
#[derive(Clone, Default)]
pub struct VersionRegistry {
    modules: Arc<DashMap<String, ModuleSlot>>,
}

impl VersionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the first version of a new module, or load a new
    /// version of an existing module.
    ///
    /// On a fresh module:
    ///   - `current` is set to the new version (epoch 1).
    ///   - `old` is None.
    ///
    /// On an existing module:
    ///   - The prior `current` is demoted to `old`. Any prior
    ///     `old` is purged (BEAM's two-version rule: a third
    ///     load forces the oldest out).
    ///   - The new version becomes `current` with bumped epoch.
    ///
    /// Returns the new `current` ModuleVersion's epoch for
    /// downstream tracking (Scheme actors note "I entered code
    /// at epoch N").
    pub fn load(&self, module: &str, exports: FxHashMap<String, Export>) -> u32 {
        let mut slot = self
            .modules
            .entry(module.to_string())
            .or_insert_with(ModuleSlot::default);
        let epoch = slot.next_epoch.max(1);
        slot.next_epoch = epoch + 1;
        let new_version = ModuleVersion {
            epoch,
            exports: Arc::new(exports),
        };
        // Demote previous current → old (purging any older old).
        slot.old = slot.current.take();
        slot.current = Some(new_version);
        epoch
    }

    /// Look up an export from the **current** version.
    pub fn lookup(&self, module: &str, name: &str) -> Option<Export> {
        let slot = self.modules.get(module)?;
        slot.current.as_ref()?.get(name)
    }

    /// Look up an export from the **old** version (if any).
    /// Returns `None` if the module doesn't exist or has no
    /// old version (most modules at most times).
    pub fn lookup_old(&self, module: &str, name: &str) -> Option<Export> {
        let slot = self.modules.get(module)?;
        slot.old.as_ref()?.get(name)
    }

    /// Drop the old version if `holder_count == 0`. Returns
    /// `Err(StillHeld)` if any actor is still pinned to it.
    ///
    /// Holder tracking lives in Scheme: each actor records the
    /// epoch of any module it has called into on its call stack;
    /// the soft-purge driver iterates registered actors and
    /// passes the count.
    pub fn soft_purge(&self, module: &str, holder_count: usize) -> Result<(), ReloadError> {
        let mut slot = self
            .modules
            .get_mut(module)
            .ok_or_else(|| ReloadError::NotLoaded {
                module: module.to_string(),
            })?;
        if slot.old.is_none() {
            return Err(ReloadError::NoOldVersion {
                module: module.to_string(),
            });
        }
        if holder_count > 0 {
            return Err(ReloadError::StillHeld {
                module: module.to_string(),
                holder_count,
            });
        }
        slot.old = None;
        Ok(())
    }

    /// Drop the old version unconditionally. The caller asserts
    /// that any actor still pinned to the old version is OK to
    /// fail on its next call (Scheme delivers a 'killed Exit
    /// signal to those actors).
    pub fn purge(&self, module: &str) -> Result<(), ReloadError> {
        let mut slot = self
            .modules
            .get_mut(module)
            .ok_or_else(|| ReloadError::NotLoaded {
                module: module.to_string(),
            })?;
        if slot.old.is_none() {
            return Err(ReloadError::NoOldVersion {
                module: module.to_string(),
            });
        }
        slot.old = None;
        Ok(())
    }

    /// `(old_epoch?, current_epoch?)` for diagnostics. None when
    /// the module is unloaded.
    pub fn epochs(&self, module: &str) -> Option<(Option<u32>, Option<u32>)> {
        let slot = self.modules.get(module)?;
        Some((
            slot.old.as_ref().map(|v| v.epoch),
            slot.current.as_ref().map(|v| v.epoch),
        ))
    }

    /// All loaded modules (snapshot of names).
    pub fn modules(&self) -> Vec<String> {
        self.modules.iter().map(|e| e.key().clone()).collect()
    }

    /// Drop a module entirely (both versions, gone).
    pub fn unload(&self, module: &str) {
        self.modules.remove(module);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(value: i64) -> Export {
        Arc::new(value)
    }

    fn exports(pairs: &[(&str, i64)]) -> FxHashMap<String, Export> {
        let mut m = FxHashMap::default();
        for (k, v) in pairs {
            m.insert(k.to_string(), export(*v));
        }
        m
    }

    fn read(e: Option<Export>) -> Option<i64> {
        e.and_then(|e| e.downcast_ref::<i64>().copied())
    }

    #[test]
    fn first_load_sets_current_no_old() {
        let r = VersionRegistry::new();
        let ep = r.load("m", exports(&[("f", 10)]));
        assert_eq!(ep, 1);
        assert_eq!(r.epochs("m"), Some((None, Some(1))));
        assert_eq!(read(r.lookup("m", "f")), Some(10));
        assert_eq!(read(r.lookup_old("m", "f")), None);
    }

    #[test]
    fn second_load_demotes_current_to_old() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        let ep2 = r.load("m", exports(&[("f", 20)]));
        assert_eq!(ep2, 2);
        assert_eq!(r.epochs("m"), Some((Some(1), Some(2))));
        // current sees v2; old sees v1.
        assert_eq!(read(r.lookup("m", "f")), Some(20));
        assert_eq!(read(r.lookup_old("m", "f")), Some(10));
    }

    #[test]
    fn third_load_purges_oldest() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)])); // v1
        r.load("m", exports(&[("f", 20)])); // v2
        r.load("m", exports(&[("f", 30)])); // v3
                                            // After v3: current=v3, old=v2, v1 is gone.
        assert_eq!(r.epochs("m"), Some((Some(2), Some(3))));
        assert_eq!(read(r.lookup("m", "f")), Some(30));
        assert_eq!(read(r.lookup_old("m", "f")), Some(20));
    }

    #[test]
    fn soft_purge_succeeds_when_no_holders() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        r.load("m", exports(&[("f", 20)]));
        r.soft_purge("m", 0).unwrap();
        // Old gone, current intact.
        assert_eq!(r.epochs("m"), Some((None, Some(2))));
    }

    #[test]
    fn soft_purge_blocked_when_holders() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        r.load("m", exports(&[("f", 20)]));
        let err = r.soft_purge("m", 3).unwrap_err();
        assert!(matches!(
            err,
            ReloadError::StillHeld {
                holder_count: 3,
                ..
            }
        ));
        // Old still there; soft_purge did NOT drop.
        assert_eq!(r.epochs("m"), Some((Some(1), Some(2))));
    }

    #[test]
    fn purge_drops_old_unconditionally() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        r.load("m", exports(&[("f", 20)]));
        r.purge("m").unwrap();
        assert_eq!(r.epochs("m"), Some((None, Some(2))));
    }

    #[test]
    fn purge_with_no_old_errors() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        let err = r.purge("m").unwrap_err();
        assert!(matches!(err, ReloadError::NoOldVersion { .. }));
    }

    #[test]
    fn lookup_unknown_module_returns_none() {
        let r = VersionRegistry::new();
        assert!(r.lookup("nope", "f").is_none());
        assert!(r.epochs("nope").is_none());
    }

    #[test]
    fn unload_drops_module() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10)]));
        r.unload("m");
        assert!(r.epochs("m").is_none());
    }

    #[test]
    fn modules_lists_loaded() {
        let r = VersionRegistry::new();
        r.load("a", exports(&[("f", 1)]));
        r.load("b", exports(&[("g", 2)]));
        let mut ms = r.modules();
        ms.sort();
        assert_eq!(ms, vec!["a", "b"]);
    }

    #[test]
    fn multiple_exports_per_version() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("f", 10), ("g", 20), ("h", 30)]));
        assert_eq!(read(r.lookup("m", "f")), Some(10));
        assert_eq!(read(r.lookup("m", "g")), Some(20));
        assert_eq!(read(r.lookup("m", "h")), Some(30));
        assert_eq!(read(r.lookup("m", "missing")), None);
    }

    #[test]
    fn new_version_can_have_different_export_set() {
        let r = VersionRegistry::new();
        r.load("m", exports(&[("a", 1), ("b", 2)]));
        // v2 drops `b`, adds `c`.
        r.load("m", exports(&[("a", 10), ("c", 30)]));
        assert_eq!(read(r.lookup("m", "a")), Some(10));
        assert_eq!(read(r.lookup("m", "b")), None); // gone from current
        assert_eq!(read(r.lookup("m", "c")), Some(30));
        // Old still has the original shape.
        assert_eq!(read(r.lookup_old("m", "a")), Some(1));
        assert_eq!(read(r.lookup_old("m", "b")), Some(2));
        assert_eq!(read(r.lookup_old("m", "c")), None);
    }
}
