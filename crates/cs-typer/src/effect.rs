//! Allocation-effect inference (layer 5 of the unified memory
//! architecture — see ADR 0015 and the
//! `.spec-workflow/specs/escape-analysis/` spec).
//!
//! Every expression in CrabScheme has an [`AllocEffect`]:
//! whether it allocates, where the allocation escapes to, and
//! whether the allocation could be involved in a cycle. The
//! cs-typer effect inferencer derives these tags bottom-up
//! from the cs-ir AST; downstream lowering (cs-rir →
//! cs-runtime/cs-vm/cs-aot) consumes them to dispatch each
//! allocation site to the right tier:
//!
//! - `escapes = Local` → could be stack-allocated (future
//!   work; today routes to Region for safety).
//! - `escapes = Region` → `Gc::new_in(current_region, …)` —
//!   the bump-arena fast path from the region-memory spec.
//! - `escapes = Heap` → `Gc::new(…)` — the global Rc heap
//!   (today's default).
//! - `escapes = Unknown` → conservative; treat as Heap.
//! - `may_cycle = true` → cycle detector wired in (Rc path)
//!   or future tracing path (`tracing-revival` spec).
//!
//! # Status — iter 1
//!
//! This module ships only the pure data types and the lattice
//! algebra. The actual inferencer that consumes a CoreExpr
//! lands in iter 3. The per-primitive effect table lands in
//! iter 2.
//!
//! # Lattice
//!
//! `EscapeKind` is a 4-point lattice ordered
//! `Local ⊑ Region ⊑ Unknown ⊑ Heap`. `Heap` is the most
//! pessimistic (the value escapes to the global heap, must
//! be Rc-allocated); `Local` is the most optimistic (the
//! value never leaves its surrounding scope, could in theory
//! live on the stack).
//!
//! `AllocEffect::join` is the pointwise lub: OR for the bool
//! fields, [`EscapeKind::join`] for the escape kind.

use std::fmt;

/// Allocation effect of an expression — what the runtime
/// allocator has to do when evaluating it.
///
/// `PURE` is the bottom element: allocates nothing, doesn't
/// escape, doesn't cycle. Compose effects across sub-
/// expressions with [`AllocEffect::join`] (pointwise lub).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AllocEffect {
    /// Whether evaluating the expression produces a fresh heap
    /// allocation. A leaf constant or a primitive ref is
    /// `false`; a `(cons a b)` or `(make-vector n)` is `true`.
    pub allocates: bool,
    /// Where the (possibly-allocated) value's lifetime can
    /// reach. Drives allocation-tier selection downstream.
    pub escapes: EscapeKind,
    /// Whether the expression could participate in a reference
    /// cycle (either by self-mutation like `(set-cdr! x x)` or
    /// by closing over its own binding in a letrec). When
    /// `true`, the runtime keeps cycle-detection wired in
    /// (today) or — once the tracing-revival spec lands —
    /// triggers a tracing pass on the surrounding scope.
    pub may_cycle: bool,
}

/// Where an allocation's lifetime can reach. Lattice ordering:
/// `Local ⊑ Region ⊑ Unknown ⊑ Heap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EscapeKind {
    /// Value is provably confined to the enclosing dynamic
    /// scope and could be stack-allocated. Future work; today
    /// the runtime treats Local the same as Region for safety.
    Local,
    /// Value's lifetime is bounded by some surrounding region
    /// (`let` body, function call). Eligible for `Gc::new_in`.
    Region,
    /// Value escapes to the global heap (returned from a
    /// function, stored in a long-lived binding, etc.). Must
    /// be Rc-allocated.
    Heap,
    /// Inferencer couldn't determine the escape kind (e.g.,
    /// passed to an opaque higher-order function like `apply`).
    /// Conservative — treated as `Heap` by lowering.
    Unknown,
}

impl AllocEffect {
    /// The bottom of the effect lattice — no allocation, no
    /// escape, no cycle. Use as the seed when folding over a
    /// list of sub-expression effects with [`Self::join`].
    pub const PURE: AllocEffect = AllocEffect {
        allocates: false,
        escapes: EscapeKind::Local,
        may_cycle: false,
    };

    /// Pointwise least-upper-bound of two effects. OR for the
    /// bool fields, lattice-join for `escapes`.
    pub const fn join(self, other: AllocEffect) -> AllocEffect {
        AllocEffect {
            allocates: self.allocates || other.allocates,
            escapes: self.escapes.join(other.escapes),
            may_cycle: self.may_cycle || other.may_cycle,
        }
    }
}

impl EscapeKind {
    /// Lattice join: `Local ⊑ Region ⊑ Unknown ⊑ Heap`.
    /// `Heap` absorbs everything; `Local` is the identity.
    pub const fn join(self, other: EscapeKind) -> EscapeKind {
        use EscapeKind::*;
        match (self, other) {
            (Heap, _) | (_, Heap) => Heap,
            (Unknown, _) | (_, Unknown) => Unknown,
            (Region, _) | (_, Region) => Region,
            (Local, Local) => Local,
        }
    }
}

impl Default for AllocEffect {
    /// The default effect is `PURE`. Convenient for
    /// initializing accumulators in fold-like patterns.
    fn default() -> Self {
        Self::PURE
    }
}

impl Default for EscapeKind {
    fn default() -> Self {
        EscapeKind::Local
    }
}

impl fmt::Display for EscapeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            EscapeKind::Local => "local",
            EscapeKind::Region => "region",
            EscapeKind::Heap => "heap",
            EscapeKind::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

impl fmt::Display for AllocEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[alloc={} escapes={} cycle={}]",
            self.allocates, self.escapes, self.may_cycle
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_is_identity_for_join() {
        let e = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Region,
            may_cycle: false,
        };
        assert_eq!(AllocEffect::PURE.join(e), e);
        assert_eq!(e.join(AllocEffect::PURE), e);
    }

    #[test]
    fn join_idempotent() {
        let e = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Heap,
            may_cycle: true,
        };
        assert_eq!(e.join(e), e);
    }

    #[test]
    fn join_commutative() {
        let a = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Region,
            may_cycle: false,
        };
        let b = AllocEffect {
            allocates: false,
            escapes: EscapeKind::Heap,
            may_cycle: true,
        };
        assert_eq!(a.join(b), b.join(a));
    }

    #[test]
    fn join_associative() {
        let a = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Local,
            may_cycle: false,
        };
        let b = AllocEffect {
            allocates: false,
            escapes: EscapeKind::Region,
            may_cycle: false,
        };
        let c = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Heap,
            may_cycle: true,
        };
        assert_eq!(a.join(b).join(c), a.join(b.join(c)));
    }

    #[test]
    fn escape_kind_lattice_ordering() {
        use EscapeKind::*;
        // Heap absorbs everything.
        assert_eq!(Heap.join(Local), Heap);
        assert_eq!(Heap.join(Region), Heap);
        assert_eq!(Heap.join(Unknown), Heap);
        assert_eq!(Heap.join(Heap), Heap);
        // Unknown absorbs Local, Region.
        assert_eq!(Unknown.join(Local), Unknown);
        assert_eq!(Unknown.join(Region), Unknown);
        // Region absorbs Local.
        assert_eq!(Region.join(Local), Region);
        // Local is the identity for itself.
        assert_eq!(Local.join(Local), Local);
    }

    #[test]
    fn escape_kind_join_commutative() {
        use EscapeKind::*;
        for &a in &[Local, Region, Heap, Unknown] {
            for &b in &[Local, Region, Heap, Unknown] {
                assert_eq!(a.join(b), b.join(a), "{a:?} ⊔ {b:?}");
            }
        }
    }

    #[test]
    fn pure_constant_is_bottom() {
        assert!(!AllocEffect::PURE.allocates);
        assert_eq!(AllocEffect::PURE.escapes, EscapeKind::Local);
        assert!(!AllocEffect::PURE.may_cycle);
    }

    #[test]
    fn region_alloc_does_not_promote_to_heap() {
        let region_alloc = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Region,
            may_cycle: false,
        };
        let pure = AllocEffect::PURE;
        let joined = region_alloc.join(pure);
        assert_eq!(joined.escapes, EscapeKind::Region);
        assert!(joined.allocates);
        assert!(!joined.may_cycle);
    }

    #[test]
    fn heap_dominates_region_in_join() {
        let region = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Region,
            may_cycle: false,
        };
        let heap = AllocEffect {
            allocates: true,
            escapes: EscapeKind::Heap,
            may_cycle: false,
        };
        assert_eq!(region.join(heap).escapes, EscapeKind::Heap);
    }

    #[test]
    fn default_is_pure() {
        assert_eq!(AllocEffect::default(), AllocEffect::PURE);
        assert_eq!(EscapeKind::default(), EscapeKind::Local);
    }

    #[test]
    fn display_format_is_stable() {
        assert_eq!(format!("{}", EscapeKind::Region), "region");
        assert_eq!(
            format!("{}", AllocEffect::PURE),
            "[alloc=false escapes=local cycle=false]"
        );
    }
}
