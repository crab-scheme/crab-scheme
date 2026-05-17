//! Type representation for the cs-typer checker.
//!
//! The atomic variants mirror `cs_rir::Type` so the typer's
//! inferences feed directly into `param_type_hints` at the
//! JIT / AOT pipeline boundary. The richer variants (`Union`,
//! `Procedure`, `Listof`, `Vectorof`) extend the vocabulary for
//! source-level annotation without bloating cs-rir.
//!
//! Variant inventory mirrors `cs_rir::Type`'s 11 atomic variants
//! + a gradual `Any` top + a `Never` bottom + a few structural
//! containers. Polymorphism is Phase 7 (deferred).

/// Source-level types as parsed from annotations and as
/// inferred by the bidirectional checker.
///
/// `Ord` / `PartialOrd` are derived so `Type::union` can sort
/// its members canonically — this is what makes
/// `(U Fixnum Flonum) == (U Flonum Fixnum)`. The derived order
/// is by variant declaration order, then lexicographic on
/// contents; it is **not** a semantic ordering (no relation to
/// subtyping). Treat it as an opaque sort key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Type {
    // ---- Atomic (mirror cs_rir::Type) ----
    Fixnum,
    Flonum,
    Boolean,
    Character,
    Symbol,
    Pair,
    Vector,
    String,
    ByteVector,
    Procedure,
    Null,

    /// Gradual top. Untyped code's params/returns; the typer
    /// admits anything for `Any` operands and produces `Any`
    /// for unannotated inferred values.
    Any,

    /// Bottom — never-returning expression (error, infinite
    /// loop, `(raise ...)`). Subtype of every type.
    Never,

    /// Finite union. `(U Fixnum Flonum)`. The Vec is sorted +
    /// deduped for canonical equality. Empty union ≡ `Never`;
    /// single-element union ≡ that element (constructors
    /// normalize).
    Union(Vec<Type>),

    /// Procedure with arity, parameter types, and return type.
    Procedure_(Box<ProcType>),

    /// Homogeneous list `(Listof T)`. Covariant under our
    /// immutable-list assumption (Phase 3 nails this down).
    Listof(Box<Type>),

    /// Homogeneous vector `(Vectorof T)`. Invariant — mutable
    /// element types can't be sub/super-typed safely.
    Vectorof(Box<Type>),

    /// Universal quantification (Phase 7). `(All (T1 T2 …) body)`
    /// — `body` may contain `Type::Var(Ti)` references that bind
    /// to the quantifier. Substitution `subst(body, mapping)`
    /// replaces each `Var` with its mapping; capture-avoiding
    /// under nested `Forall`s.
    Forall(Vec<cs_core::Symbol>, Box<Type>),

    /// Type variable reference (Phase 7). Carries the name (a
    /// `Symbol`) — bound by an enclosing `Forall`. Free
    /// variables (no enclosing binder) are treated as `Any` by
    /// the checker; they may also appear transiently during
    /// substitution if a `mapping` doesn't cover them.
    Var(cs_core::Symbol),
}

/// Procedure signature with positional params, optional rest
/// parameter, and a return type. Procedure type arity is
/// `params.len()` (or `>= params.len()` when rest is set).
///
/// `filter` (Phase 4) is an optional "positive proposition" for
/// the 0-th positional arg. When set, the procedure is a
/// predicate: if it returns true, the arg's type narrows to the
/// `filter` type in the then-branch; if false, the arg narrows
/// to the difference. E.g., `number? : (-> Any Boolean) +
/// filter (U Fixnum Flonum)` means "in `(if (number? x) …)`,
/// `x` is `(U Fixnum Flonum)` in the then-branch."
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcType {
    pub params: Vec<Type>,
    pub return_type: Type,
    pub rest: Option<Type>,
    pub filter: Option<Type>,
}

impl Type {
    /// Sentinel for "no real annotation observed yet" — used
    /// while building unions incrementally.
    pub fn never() -> Self {
        Type::Never
    }

    /// Smart constructor for unions: flattens nested unions,
    /// drops `Never`, collapses single-element to that element,
    /// dedups, and sorts canonically so `(U A B) == (U B A)`.
    ///
    /// The sort uses the derived `Ord` on `Type` — by variant
    /// declaration order with lexicographic content comparison.
    /// It's an opaque canonical key, not a semantic ordering.
    /// If `Any` is present, the union collapses to `Any` (the
    /// gradual top absorbs).
    pub fn union(members: impl IntoIterator<Item = Type>) -> Type {
        let mut out: Vec<Type> = Vec::new();
        for t in members {
            match t {
                Type::Never => continue,
                Type::Any => return Type::Any,
                Type::Union(inner) => {
                    for x in inner {
                        if matches!(x, Type::Any) {
                            return Type::Any;
                        }
                        if !out.contains(&x) {
                            out.push(x);
                        }
                    }
                }
                other => {
                    if !out.contains(&other) {
                        out.push(other);
                    }
                }
            }
        }
        out.sort();
        match out.len() {
            0 => Type::Never,
            1 => out.into_iter().next().unwrap(),
            _ => Type::Union(out),
        }
    }

    /// True iff this type is `Union(...)`. Convenience for the
    /// subtyping rules and for downstream consumers (JIT
    /// param-hint widening).
    pub fn is_union(&self) -> bool {
        matches!(self, Type::Union(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_singleton_collapses() {
        assert_eq!(Type::union(vec![Type::Fixnum]), Type::Fixnum);
    }

    #[test]
    fn union_dedup() {
        let u = Type::union(vec![Type::Fixnum, Type::Fixnum, Type::Flonum]);
        assert_eq!(u, Type::Union(vec![Type::Fixnum, Type::Flonum]));
    }

    #[test]
    fn union_flatten_nested() {
        let inner = Type::Union(vec![Type::Fixnum, Type::Flonum]);
        let u = Type::union(vec![inner, Type::Boolean]);
        assert_eq!(
            u,
            Type::Union(vec![Type::Fixnum, Type::Flonum, Type::Boolean])
        );
    }

    #[test]
    fn union_drops_never() {
        let u = Type::union(vec![Type::Fixnum, Type::Never]);
        assert_eq!(u, Type::Fixnum);
    }

    #[test]
    fn empty_union_is_never() {
        let u = Type::union(std::iter::empty());
        assert_eq!(u, Type::Never);
    }

    #[test]
    fn union_sorts_canonically() {
        // Member order at the call site shouldn't matter for
        // equality.
        let a = Type::union(vec![Type::Fixnum, Type::Flonum, Type::String]);
        let b = Type::union(vec![Type::String, Type::Fixnum, Type::Flonum]);
        let c = Type::union(vec![Type::Flonum, Type::String, Type::Fixnum]);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn union_with_any_collapses_to_any() {
        // Any absorbs — `(U Any Fixnum) == Any` because Any is
        // the gradual top, and a union containing it can hold
        // arbitrary values.
        let u = Type::union(vec![Type::Fixnum, Type::Any, Type::String]);
        assert_eq!(u, Type::Any);
    }

    #[test]
    fn union_with_nested_any_collapses_to_any() {
        let inner = Type::Union(vec![Type::Fixnum, Type::Any]);
        let u = Type::union(vec![Type::String, inner]);
        assert_eq!(u, Type::Any);
    }
}
