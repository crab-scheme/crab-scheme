//! Allocation-lifetime tag for cs-rir SSA values (layer 5 of
//! the unified memory architecture — see ADR 0015 and the
//! `.spec-workflow/specs/escape-analysis/` spec).
//!
//! Each allocating instruction's result carries a `Lifetime`
//! that lowering consumes to pick the right allocator tier:
//!
//! - `Lifetime::Rc` — default; use `Gc::new(…)` (global Rc
//!   heap).
//! - `Lifetime::Region(RegionTag)` — use `Gc::new_in(region,
//!   …)` against the named region. The `RegionTag` is a
//!   per-function-scope id that lowering resolves to a
//!   concrete `cs_gc::Region` instance held on the runtime's
//!   region-scope stack (iter 5).
//! - `Lifetime::Stack` — reserved for future stack-allocation
//!   work; today the runtime treats Stack the same as Region
//!   (no stack-alloc dispatch yet).
//! - `Lifetime::Traced` — opt-in tracing GC tier (depends on
//!   the forthcoming `tracing-revival` spec). Used when the
//!   inferencer flagged the allocation as `may_cycle` and a
//!   tracing collector is preferable to the cycle detector.
//!
//! Iter 4 (this file) only introduces the type. Lowering
//! consumers (iter 6 — allocation dispatch in cs-runtime,
//! cs-vm, cs-aot) consume them.

/// Per-function-scope region identifier. A function may have
/// at most `2^32` distinct regions; in practice the typer
/// mints one per `let`/`begin`/lambda-body scope that carries
/// at least one `Lifetime::Region` allocation.
///
/// Wrapping a `u32` rather than the `cs_gc::RegionId` type
/// keeps cs-rir independent of cs-gc — the lowering layer
/// (cs-runtime) maps each `RegionTag` to a concrete
/// `cs_gc::Region` at allocation time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RegionTag(pub u32);

/// Lifetime classification of an allocation. Default is
/// [`Lifetime::Rc`] — the safe choice that matches today's
/// production behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifetime {
    /// Stack-allocate. Reserved; the runtime currently routes
    /// Stack the same as Region until stack-alloc lands.
    Stack,
    /// Allocate in the surrounding region, identified by the
    /// per-function `RegionTag`. Layer-3 fast path.
    Region(RegionTag),
    /// Allocate in the global Rc heap. Default / safe choice.
    /// Most allocations today land here.
    Rc,
    /// Allocate in the tracing-GC heap (gated on the
    /// `tracing-revival` spec landing). The inferencer flags
    /// `may_cycle = true` allocations here so the tracing
    /// collector handles their cycles instead of the
    /// synchronous detector.
    Traced,
}

impl Default for Lifetime {
    fn default() -> Self {
        Lifetime::Rc
    }
}

impl Lifetime {
    /// `true` if this lifetime requires a region-scope context
    /// at the allocation site. Used by lowering to assert the
    /// region-scope stack is non-empty before emitting a
    /// `Gc::new_in` call.
    pub fn needs_region(self) -> bool {
        matches!(self, Lifetime::Region(_) | Lifetime::Stack)
    }

    /// Returns the [`RegionTag`] for `Lifetime::Region`, else
    /// `None`. `Lifetime::Stack` returns `None` because stack
    /// allocation isn't region-scoped.
    pub fn region_tag(self) -> Option<RegionTag> {
        match self {
            Lifetime::Region(tag) => Some(tag),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_rc() {
        assert_eq!(Lifetime::default(), Lifetime::Rc);
    }

    #[test]
    fn needs_region_only_for_region_and_stack() {
        assert!(!Lifetime::Rc.needs_region());
        assert!(!Lifetime::Traced.needs_region());
        assert!(Lifetime::Stack.needs_region());
        assert!(Lifetime::Region(RegionTag(0)).needs_region());
    }

    #[test]
    fn region_tag_only_for_region_variant() {
        assert_eq!(Lifetime::Rc.region_tag(), None);
        assert_eq!(Lifetime::Stack.region_tag(), None);
        assert_eq!(Lifetime::Traced.region_tag(), None);
        assert_eq!(
            Lifetime::Region(RegionTag(42)).region_tag(),
            Some(RegionTag(42))
        );
    }

    #[test]
    fn region_tags_distinguish() {
        assert_ne!(
            Lifetime::Region(RegionTag(1)),
            Lifetime::Region(RegionTag(2))
        );
    }
}
