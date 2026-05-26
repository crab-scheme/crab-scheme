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

use std::collections::HashSet;
use std::fmt;

use cs_core::Symbol;
use cs_ir::CoreExpr;

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

    /// Top of the effect lattice — assume the worst on every
    /// axis. Used for unresolved callees (Symbol refs without
    /// SymbolTable access, computed/higher-order callees).
    pub const UNKNOWN: AllocEffect = AllocEffect {
        allocates: true,
        escapes: EscapeKind::Unknown,
        may_cycle: true,
    };
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

/// Infer an [`AllocEffect`] for `expr`. Bottom-up walk; each
/// shape's effect is derived from sub-expression effects and
/// the shape's own semantics.
///
/// # Iter-3 scope
///
/// - **No inter-procedural analysis.** App-of-Ref where the
///   callee is a known primitive looks up
///   [`primitive_effect`]; every other call site is treated
///   conservatively as the join of its argument effects plus
///   `escapes = Unknown`. Inter-procedural sharpening is
///   deferred (would require a fixpoint over the call graph;
///   the spec leaves this to a future iter).
/// - **Lambda escape conservatism.** A `Lambda` expression
///   always reports `allocates = true, escapes = Heap` —
///   closures typically escape. A later iter could detect
///   non-escaping lambdas (immediately-invoked, only used in
///   tail position, etc.).
/// - **may_cycle detection.** Set/Letrec bindings whose RHS
///   contains a Ref to the LHS name (or a mutually-recursive
///   sibling) flag `may_cycle = true`. Direct
///   `(set-car! x v)` / `(set-cdr! x v)` calls where `v` is
///   `x` (same Symbol) likewise flag `may_cycle`.
///
/// The `env` parameter is currently unused but reserved —
/// future iters will consult it for known-function-effect
/// lookups (so user-defined functions can have their effects
/// inferred and cached).
pub fn infer_effect(expr: &CoreExpr, env: &crate::env::TypeEnv) -> AllocEffect {
    let _ = env;
    infer_effect_with_scope(expr, &HashSet::new())
}

/// Helper that threads a "self-bound names" set for cycle
/// detection. Each Letrec / Set expression extends this set
/// with its LHS so its RHS can be flagged may_cycle if it
/// captures.
fn infer_effect_with_scope(expr: &CoreExpr, self_bound: &HashSet<Symbol>) -> AllocEffect {
    match expr {
        CoreExpr::Const { .. } | CoreExpr::Ref { .. } => AllocEffect::PURE,

        CoreExpr::Set { name, value, .. } => {
            // Build a scope extended with `name` for cycle
            // detection on the RHS.
            let mut inner = self_bound.clone();
            inner.insert(*name);
            let rhs_effect = infer_effect_with_scope(value, &inner);
            // A Set itself doesn't allocate; it just propagates
            // the RHS effect. But if the RHS captures `name`
            // (free-var check), flag may_cycle.
            let captures_self = ref_captures_any(value, &inner);
            AllocEffect {
                allocates: rhs_effect.allocates,
                escapes: rhs_effect.escapes.join(EscapeKind::Heap),
                may_cycle: rhs_effect.may_cycle || captures_self,
            }
        }

        CoreExpr::Lambda { body, .. } => {
            // The Lambda itself allocates a closure that
            // typically escapes (stored, returned, called by an
            // unknown caller). Body's effect is folded in for
            // its may_cycle bit only — the body doesn't
            // execute until the closure is called, so its
            // alloc/escape bits don't accumulate at this site.
            let body_effect = infer_effect_with_scope(body, self_bound);
            AllocEffect {
                allocates: true,
                escapes: EscapeKind::Heap,
                may_cycle: body_effect.may_cycle,
            }
        }

        CoreExpr::App { func, args, .. } => {
            // Try to resolve `func` to a primitive name.
            let prim_effect = match &**func {
                CoreExpr::Ref { .. } => {
                    // Symbol(u32) is the interned form;
                    // resolving it to a primitive effect needs
                    // SymbolTable access we don't have at this
                    // layer. Conservative top-of-lattice until a
                    // later iter threads the table down here.
                    AllocEffect::UNKNOWN
                }
                _ => {
                    // Unknown callee (computed or higher-order
                    // closure). Conservative — assume Unknown
                    // escape, may_cycle.
                    AllocEffect {
                        allocates: true,
                        escapes: EscapeKind::Unknown,
                        may_cycle: true,
                    }
                }
            };
            // Join the function's effect with each arg's.
            let mut acc = prim_effect;
            for arg in args {
                acc = acc.join(infer_effect_with_scope(arg, self_bound));
            }
            acc
        }

        CoreExpr::If {
            cond, then, alt, ..
        } => infer_effect_with_scope(cond, self_bound)
            .join(infer_effect_with_scope(then, self_bound))
            .join(infer_effect_with_scope(alt, self_bound)),

        CoreExpr::Begin { exprs, .. } => exprs.iter().fold(AllocEffect::PURE, |acc, e| {
            acc.join(infer_effect_with_scope(e, self_bound))
        }),

        CoreExpr::Letrec { bindings, body, .. } => {
            // Extend the scope with every LHS upfront so each
            // RHS sees all sibling names (the may_cycle
            // detector flags captures of any of them).
            let mut inner = self_bound.clone();
            for (name, _) in bindings {
                inner.insert(*name);
            }
            let mut acc = AllocEffect::PURE;
            for (_, rhs) in bindings {
                let rhs_effect = infer_effect_with_scope(rhs, &inner);
                let captures = ref_captures_any(rhs, &inner);
                acc = acc.join(AllocEffect {
                    allocates: rhs_effect.allocates,
                    escapes: rhs_effect.escapes,
                    may_cycle: rhs_effect.may_cycle || captures,
                });
            }
            acc.join(infer_effect_with_scope(body, &inner))
        }

        CoreExpr::WithContinuationMark { key, val, body, .. } => {
            infer_effect_with_scope(key, self_bound)
                .join(infer_effect_with_scope(val, self_bound))
                .join(infer_effect_with_scope(body, self_bound))
        }
    }
}

/// `true` if `expr` contains a free reference to any name in
/// `names`. Used by Set/Letrec to flag `may_cycle` when a
/// binding's RHS captures its own LHS or a mutually-recursive
/// sibling.
fn ref_captures_any(expr: &CoreExpr, names: &HashSet<Symbol>) -> bool {
    match expr {
        CoreExpr::Const { .. } => false,
        CoreExpr::Ref { name, .. } => names.contains(name),
        CoreExpr::Set { name: _, value, .. } => ref_captures_any(value, names),
        CoreExpr::Lambda { params, body, .. } => {
            // Lambda parameters shadow outer names — remove
            // any shadowed ones from the lookup set before
            // recursing into the body.
            let mut inner = names.clone();
            for p in &params.fixed {
                inner.remove(p);
            }
            if let Some(r) = params.rest {
                inner.remove(&r);
            }
            ref_captures_any(body, &inner)
        }
        CoreExpr::App { func, args, .. } => {
            ref_captures_any(func, names) || args.iter().any(|a| ref_captures_any(a, names))
        }
        CoreExpr::If {
            cond, then, alt, ..
        } => {
            ref_captures_any(cond, names)
                || ref_captures_any(then, names)
                || ref_captures_any(alt, names)
        }
        CoreExpr::Begin { exprs, .. } => exprs.iter().any(|e| ref_captures_any(e, names)),
        CoreExpr::Letrec { bindings, body, .. } => {
            // Letrec shadows: its LHS names shadow outer
            // names of the same id.
            let mut inner = names.clone();
            for (n, _) in bindings {
                inner.remove(n);
            }
            bindings.iter().any(|(_, e)| ref_captures_any(e, &inner))
                || ref_captures_any(body, &inner)
        }
        CoreExpr::WithContinuationMark { key, val, body, .. } => {
            ref_captures_any(key, names)
                || ref_captures_any(val, names)
                || ref_captures_any(body, names)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    use cs_core::Value;
    use cs_diag::Span;
    use cs_ir::Params;

    fn s(id: u32) -> Symbol {
        Symbol(id)
    }
    fn cnst(v: i64) -> CoreExpr {
        CoreExpr::Const {
            value: Value::fixnum(v),
            span: Span::DUMMY,
        }
    }
    fn rf(sym: Symbol) -> CoreExpr {
        CoreExpr::Ref {
            name: sym,
            span: Span::DUMMY,
        }
    }
    fn app(func: CoreExpr, args: Vec<CoreExpr>) -> CoreExpr {
        CoreExpr::App {
            func: Rc::new(func),
            args,
            span: Span::DUMMY,
        }
    }
    fn lam(params: Vec<Symbol>, body: CoreExpr) -> CoreExpr {
        CoreExpr::Lambda {
            params: Params::fixed(params),
            body: Rc::new(body),
            span: Span::DUMMY,
        }
    }
    fn set(name: Symbol, value: CoreExpr) -> CoreExpr {
        CoreExpr::Set {
            name,
            value: Rc::new(value),
            span: Span::DUMMY,
        }
    }
    fn letrec(bindings: Vec<(Symbol, CoreExpr)>, body: CoreExpr) -> CoreExpr {
        CoreExpr::Letrec {
            bindings,
            body: Rc::new(body),
            span: Span::DUMMY,
        }
    }
    fn env() -> crate::env::TypeEnv {
        crate::env::TypeEnv::new()
    }

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

    // === infer_effect tests ===

    #[test]
    fn const_is_pure() {
        assert_eq!(infer_effect(&cnst(42), &env()), AllocEffect::PURE);
    }

    #[test]
    fn ref_is_pure() {
        assert_eq!(infer_effect(&rf(s(1)), &env()), AllocEffect::PURE);
    }

    #[test]
    fn if_joins_all_three_branches() {
        // (if #t 1 2) — all pure, joined → still pure
        let e = CoreExpr::If {
            cond: Rc::new(cnst(1)),
            then: Rc::new(cnst(2)),
            alt: Rc::new(cnst(3)),
            span: Span::DUMMY,
        };
        assert_eq!(infer_effect(&e, &env()), AllocEffect::PURE);
    }

    #[test]
    fn begin_joins_subexprs() {
        let e = CoreExpr::Begin {
            exprs: vec![cnst(1), cnst(2), cnst(3)],
            span: Span::DUMMY,
        };
        assert_eq!(infer_effect(&e, &env()), AllocEffect::PURE);
    }

    #[test]
    fn lambda_allocates_and_escapes_heap() {
        // (lambda (x) x) — closure allocation, escapes Heap
        let e = lam(vec![s(1)], rf(s(1)));
        let eff = infer_effect(&e, &env());
        assert!(eff.allocates);
        assert_eq!(eff.escapes, EscapeKind::Heap);
        assert!(!eff.may_cycle);
    }

    #[test]
    fn app_of_unknown_function_is_conservative_unknown() {
        // ((lambda (x) x) 5) — App of non-Ref callee →
        // conservative Unknown
        let e = app(lam(vec![s(1)], rf(s(1))), vec![cnst(5)]);
        let eff = infer_effect(&e, &env());
        assert_eq!(eff.escapes, EscapeKind::Unknown);
    }

    #[test]
    fn set_propagates_rhs_alloc_and_heap_escape() {
        // (set! x (some-pure-thing)) — escape Heap (mutation
        // target), no may_cycle.
        let e = set(s(1), cnst(99));
        let eff = infer_effect(&e, &env());
        assert_eq!(eff.escapes, EscapeKind::Heap);
        assert!(!eff.may_cycle);
    }

    #[test]
    fn set_x_x_flags_may_cycle() {
        // (set! x x) — RHS captures LHS → may_cycle
        let e = set(s(1), rf(s(1)));
        let eff = infer_effect(&e, &env());
        assert!(eff.may_cycle, "set! x x must flag may_cycle");
    }

    #[test]
    fn letrec_self_recursive_lambda_flags_may_cycle() {
        // (letrec ((f (lambda () f))) f) — f closes over
        // itself
        let body_f = lam(vec![], rf(s(1)));
        let e = letrec(vec![(s(1), body_f)], rf(s(1)));
        let eff = infer_effect(&e, &env());
        assert!(eff.may_cycle, "self-recursive letrec must flag may_cycle");
    }

    #[test]
    fn letrec_acyclic_does_not_flag_may_cycle() {
        // (letrec ((x 1) (y 2)) (+ x y)) — no captures
        let e = letrec(vec![(s(1), cnst(1)), (s(2), cnst(2))], cnst(0));
        let eff = infer_effect(&e, &env());
        assert!(!eff.may_cycle);
    }

    #[test]
    fn letrec_mutual_recursion_flags_may_cycle() {
        // (letrec ((f (lambda () (g))) (g (lambda () (f)))) f)
        let body_f = lam(vec![], app(rf(s(2)), vec![]));
        let body_g = lam(vec![], app(rf(s(1)), vec![]));
        let e = letrec(vec![(s(1), body_f), (s(2), body_g)], rf(s(1)));
        let eff = infer_effect(&e, &env());
        assert!(
            eff.may_cycle,
            "mutually recursive letrec must flag may_cycle"
        );
    }

    #[test]
    fn lambda_shadows_outer_name_no_cycle_flag() {
        // (set! x (lambda (x) x)) — the param `x` shadows the
        // outer; body refs the param, NOT the outer. So no
        // may_cycle.
        let inner_lam = lam(vec![s(1)], rf(s(1)));
        let e = set(s(1), inner_lam);
        let eff = infer_effect(&e, &env());
        assert!(
            !eff.may_cycle,
            "param shadowing must suppress may_cycle flag"
        );
    }
}
