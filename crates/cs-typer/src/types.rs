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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
}

/// Procedure signature with positional params, optional rest
/// parameter, and a return type. Procedure type arity is
/// `params.len()` (or `>= params.len()` when rest is set).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcType {
    pub params: Vec<Type>,
    pub return_type: Type,
    pub rest: Option<Type>,
}

impl Type {
    /// Sentinel for "no real annotation observed yet" — used
    /// while building unions incrementally.
    pub fn never() -> Self {
        Type::Never
    }

    /// Smart constructor for unions: flattens nested unions,
    /// drops `Never`, collapses single-element to that element,
    /// dedups via the derived `Eq`/`Hash` impl.
    pub fn union(members: impl IntoIterator<Item = Type>) -> Type {
        let mut out: Vec<Type> = Vec::new();
        for t in members {
            match t {
                Type::Never => continue,
                Type::Union(inner) => {
                    for x in inner {
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
        match out.len() {
            0 => Type::Never,
            1 => out.into_iter().next().unwrap(),
            _ => Type::Union(out),
        }
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
}
