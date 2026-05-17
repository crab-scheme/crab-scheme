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

/// Look up the [`AllocEffect`] of a primitive procedure by
/// its R6RS name. Returns `AllocEffect::PURE` for unknown
/// names — safe default since PURE means "the inferencer
/// can't reason about this call site; treat the result as if
/// nothing was allocated and let the caller's context tighten
/// or loosen as needed."
///
/// Categories (see iter 2 of the escape-analysis spec):
///
/// - **PURE**: arithmetic, comparison, type predicates,
///   equality predicates, leaf accessors (`car`, `cdr`,
///   `vector-ref`, `hashtable-ref`, `string-ref`, `length`),
///   `not`, `null`, `void`, leaf reflection.
/// - **Allocates + escapes=Region**: `cons`, `list`, `vector`,
///   `make-vector`, `make-string`, `make-bytevector`,
///   `make-hashtable`, `reverse`, `append`, `list-copy`.
///   Default escape is `Region` — the inferencer downgrades
///   to `Heap` when the result is stored in a long-lived
///   binding (iter 3).
/// - **Heap + may_cycle**: mutation primitives that can
///   introduce cycles (`set-car!`, `set-cdr!`, `vector-set!`,
///   `hashtable-set!`). `set!` to a top-level binding is also
///   may_cycle when the RHS captures the LHS (detected in
///   iter 3 by free-var analysis).
/// - **Unknown**: opaque higher-order or non-local-control
///   primitives — `apply`, `call/cc`, `call-with-values`,
///   `dynamic-wind`, `eval`, `with-exception-handler`. The
///   inferencer can't trace where the value escapes; lowering
///   conservatively treats Unknown as Heap.
pub fn primitive_effect(name: &str) -> AllocEffect {
    let alloc_region = AllocEffect {
        allocates: true,
        escapes: EscapeKind::Region,
        may_cycle: false,
    };
    let mut_heap = AllocEffect {
        allocates: false,
        escapes: EscapeKind::Heap,
        may_cycle: true,
    };
    let unknown = AllocEffect {
        allocates: true,
        escapes: EscapeKind::Unknown,
        may_cycle: true,
    };

    match name {
        // Allocators — fresh heap value, Region by default.
        "cons" | "list" | "vector" | "make-vector" | "make-string" | "make-bytevector"
        | "make-hashtable" | "make-eq-hashtable" | "make-eqv-hashtable" | "string"
        | "string-copy" | "list-copy" | "reverse" | "append" | "list->vector" | "vector->list"
        | "string->list" | "list->string" | "bytevector-copy" | "vector-map"
        | "vector-for-each" | "map" | "for-each" | "values" | "call-with-values"
        | "make-promise" | "delay" | "make-parameter" => alloc_region,

        // Mutation — propagates Heap escape and flags may_cycle.
        "set-car!" | "set-cdr!" | "vector-set!" | "vector-fill!" | "bytevector-u8-set!"
        | "bytevector-set!" | "string-set!" | "hashtable-set!" | "hashtable-delete!"
        | "hashtable-update!" | "hashtable-clear!" | "set!" => mut_heap,

        // Opaque control / higher-order — Unknown escape.
        "apply"
        | "call-with-current-continuation"
        | "call/cc"
        | "dynamic-wind"
        | "with-exception-handler"
        | "raise"
        | "raise-continuable"
        | "guard"
        | "eval"
        | "load"
        | "compile"
        | "expand"
        | "expand-once" => unknown,

        // I/O — allocates (port handle) and escapes to global
        // resources. Treat as Heap-escape so port objects don't
        // get region-allocated (they're long-lived).
        "open-input-string"
        | "open-output-string"
        | "open-input-bytevector"
        | "open-output-bytevector"
        | "open-input-file"
        | "open-output-file"
        | "current-input-port"
        | "current-output-port"
        | "current-error-port"
        | "standard-input-port"
        | "standard-output-port"
        | "standard-error-port" => AllocEffect {
            allocates: true,
            escapes: EscapeKind::Heap,
            may_cycle: false,
        },

        // Everything else (arithmetic, comparisons, predicates,
        // pure accessors, …) is PURE.
        _ => AllocEffect::PURE,
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

    // === primitive_effect table tests ===

    #[test]
    fn primitive_arithmetic_is_pure() {
        for op in &["+", "-", "*", "/", "<", ">", "=", "<=", ">=", "fx+", "fl*"] {
            assert_eq!(
                primitive_effect(op),
                AllocEffect::PURE,
                "{op} should be PURE"
            );
        }
    }

    #[test]
    fn primitive_predicates_are_pure() {
        for op in &[
            "pair?", "null?", "fixnum?", "boolean?", "string?", "symbol?", "vector?",
        ] {
            assert_eq!(
                primitive_effect(op),
                AllocEffect::PURE,
                "{op} should be PURE"
            );
        }
    }

    #[test]
    fn primitive_accessors_are_pure() {
        for op in &[
            "car",
            "cdr",
            "vector-ref",
            "string-ref",
            "length",
            "list-ref",
        ] {
            assert_eq!(
                primitive_effect(op),
                AllocEffect::PURE,
                "{op} should be PURE"
            );
        }
    }

    #[test]
    fn primitive_allocators_are_region_alloc() {
        for op in &[
            "cons",
            "list",
            "vector",
            "make-vector",
            "make-string",
            "make-bytevector",
            "make-hashtable",
            "reverse",
            "append",
        ] {
            let e = primitive_effect(op);
            assert!(e.allocates, "{op} should allocate");
            assert_eq!(e.escapes, EscapeKind::Region, "{op} should escape Region");
            assert!(!e.may_cycle, "{op} should not flag may_cycle");
        }
    }

    #[test]
    fn primitive_mutators_are_heap_may_cycle() {
        for op in &[
            "set-car!",
            "set-cdr!",
            "vector-set!",
            "hashtable-set!",
            "hashtable-delete!",
            "set!",
        ] {
            let e = primitive_effect(op);
            assert_eq!(e.escapes, EscapeKind::Heap, "{op} should escape Heap");
            assert!(e.may_cycle, "{op} should flag may_cycle");
        }
    }

    #[test]
    fn primitive_higher_order_is_unknown() {
        for op in &[
            "apply",
            "call-with-current-continuation",
            "call/cc",
            "dynamic-wind",
            "with-exception-handler",
            "eval",
        ] {
            let e = primitive_effect(op);
            assert_eq!(e.escapes, EscapeKind::Unknown, "{op} should be Unknown");
        }
    }

    #[test]
    fn primitive_ports_are_heap_escape() {
        for op in &[
            "open-input-string",
            "open-output-string",
            "open-input-file",
            "current-input-port",
        ] {
            let e = primitive_effect(op);
            assert!(e.allocates, "{op} should allocate");
            assert_eq!(
                e.escapes,
                EscapeKind::Heap,
                "{op} (port) should escape Heap"
            );
        }
    }

    #[test]
    fn primitive_unknown_name_defaults_pure() {
        assert_eq!(
            primitive_effect("totally-fake-name"),
            AllocEffect::PURE,
            "unknown names default to PURE (safe — inferencer's context tightens later)"
        );
    }
}
