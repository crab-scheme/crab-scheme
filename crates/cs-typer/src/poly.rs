//! Polymorphism support (Phase 7).
//!
//! Two operations on quantified types:
//!
//! - [`subst`] — capture-avoiding substitution: replace each
//!   `Type::Var(T)` in a body with its mapping. Under a nested
//!   `Forall`, the inner binder shadows any outer mapping for
//!   the same `T` (the bound var isn't replaced).
//!
//! - [`unify`] — match a polymorphic template against a
//!   concrete type, returning a `Symbol → Type` mapping for
//!   the template's free vars. Returns `None` on shape
//!   mismatch or on a binding conflict (same var inferred to
//!   two distinct types). The result is what
//!   `check_app`/`infer_app` substitute into the procedure's
//!   return type at a call site (iter 7.4).
//!
//! Variance treatment in `unify`: vars unify with anything
//! (the inferred binding is recorded). Concrete atoms must
//! match exactly. `Procedure_` unifies fixed params + rest +
//! return covariantly — at a call site we already know the
//! caller-supplied arg types and want to derive the
//! template's vars, not solve general subtype constraints.
//! That's "syntactic" unification, not full Hindley-Milner,
//! and it's enough for the gradual-typing use case.

use std::collections::HashMap;

use cs_core::Symbol;

use crate::types::{ProcType, Type};

/// Substitute `Type::Var`s in `ty` according to `mapping`.
///
/// Capture-avoiding: under a nested `Forall(vs, body)`, any
/// `v ∈ vs` is removed from the mapping before recursing into
/// `body`. Vars not in `mapping` are left untouched.
pub fn subst(ty: &Type, mapping: &HashMap<Symbol, Type>) -> Type {
    if mapping.is_empty() {
        return ty.clone();
    }
    match ty {
        Type::Var(s) => mapping.get(s).cloned().unwrap_or(Type::Var(*s)),
        Type::Forall(vs, body) => {
            let mut inner: HashMap<Symbol, Type> = mapping
                .iter()
                .filter(|(k, _)| !vs.contains(k))
                .map(|(k, v)| (*k, v.clone()))
                .collect();
            if inner.is_empty() {
                return ty.clone();
            }
            // Rebuilt body uses the shrunken mapping.
            let new_body = subst(body, &mut inner);
            Type::Forall(vs.clone(), Box::new(new_body))
        }
        Type::Procedure_(pt) => Type::Procedure_(Box::new(ProcType {
            params: pt.params.iter().map(|p| subst(p, mapping)).collect(),
            return_type: subst(&pt.return_type, mapping),
            rest: pt.rest.as_ref().map(|r| subst(r, mapping)),
            // Filter types are propositional — also subject to
            // substitution if they mention bound vars (rare in
            // practice; predicates aren't usually polymorphic).
            filter: pt.filter.as_ref().map(|f| subst(f, mapping)),
        })),
        Type::Union(members) => {
            // Substitute each member, then run through `union`
            // so the result re-normalizes (drop duplicates,
            // sort, collapse to Any if any member became Any).
            Type::union(members.iter().map(|m| subst(m, mapping)))
        }
        Type::Listof(elem) => Type::Listof(Box::new(subst(elem, mapping))),
        Type::Vectorof(elem) => Type::Vectorof(Box::new(subst(elem, mapping))),
        // Atoms / Any / Never / Procedure (opaque) have no
        // structure to walk.
        Type::Fixnum
        | Type::Flonum
        | Type::Boolean
        | Type::Character
        | Type::Symbol
        | Type::Pair
        | Type::Vector
        | Type::String
        | Type::ByteVector
        | Type::Procedure
        | Type::Null
        | Type::Any
        | Type::Never => ty.clone(),
    }
}

/// Instantiate a `Forall(vs, body)` by substituting each `vs[i]`
/// with `args[i]`. Returns `None` if the arities don't match.
/// Used by explicit `(inst proc T)` syntax (iter 7.3) and by
/// implicit instantiation (iter 7.4) once the args are derived.
pub fn instantiate(forall: &Type, args: &[Type]) -> Option<Type> {
    let Type::Forall(vs, body) = forall else {
        return None;
    };
    if vs.len() != args.len() {
        return None;
    }
    let mapping: HashMap<Symbol, Type> = vs.iter().copied().zip(args.iter().cloned()).collect();
    Some(subst(body, &mapping))
}

/// Try to unify a polymorphic `template` against a `concrete`
/// type, returning the inferred binding for each free var.
///
/// `tvars` is the set of variables we're solving for —
/// typically the names quantified by the enclosing `Forall`.
/// Variables NOT in `tvars` (e.g., from a different binder, or
/// truly free) unify only if both sides agree as-written.
///
/// Returns `None` on:
/// - shape mismatch (atom vs different atom, arity mismatch on
///   procedures, ...),
/// - binding conflict (same var inferred to two distinct
///   concrete types).
pub fn unify(template: &Type, concrete: &Type, tvars: &[Symbol]) -> Option<HashMap<Symbol, Type>> {
    let mut out: HashMap<Symbol, Type> = HashMap::new();
    if unify_into(template, concrete, tvars, &mut out) {
        Some(out)
    } else {
        None
    }
}

fn unify_into(
    template: &Type,
    concrete: &Type,
    tvars: &[Symbol],
    out: &mut HashMap<Symbol, Type>,
) -> bool {
    // Template var that we're solving for: record / check the
    // binding. Vars NOT in `tvars` are treated as opaque atoms.
    if let Type::Var(s) = template {
        if tvars.contains(s) {
            match out.get(s) {
                None => {
                    out.insert(*s, concrete.clone());
                    return true;
                }
                Some(existing) => {
                    // Re-occurrence — must match. We treat Any
                    // on either side as compatible (gradual
                    // escape hatch); otherwise structural eq.
                    return existing == concrete
                        || existing == &Type::Any
                        || concrete == &Type::Any;
                }
            }
        }
    }
    // Both sides are non-template-var atoms / structurals.
    if template == concrete {
        return true;
    }
    // Any unifies with anything (gradual escape).
    if matches!(template, Type::Any) || matches!(concrete, Type::Any) {
        return true;
    }
    match (template, concrete) {
        (Type::Procedure_(a), Type::Procedure_(b)) => {
            if a.params.len() != b.params.len() {
                return false;
            }
            for (ap, bp) in a.params.iter().zip(b.params.iter()) {
                if !unify_into(ap, bp, tvars, out) {
                    return false;
                }
            }
            // Rest: both absent or both present, recurse.
            match (&a.rest, &b.rest) {
                (None, None) => {}
                (Some(ar), Some(br)) => {
                    if !unify_into(ar, br, tvars, out) {
                        return false;
                    }
                }
                _ => return false,
            }
            unify_into(&a.return_type, &b.return_type, tvars, out)
        }
        (Type::Listof(a), Type::Listof(b)) => unify_into(a, b, tvars, out),
        (Type::Vectorof(a), Type::Vectorof(b)) => unify_into(a, b, tvars, out),
        // Unions: only attempt unification when both sides are
        // unions of the same arity, member-wise. Polymorphic
        // union-template unification (Pierce-Turner-style
        // constraint solving) is out of scope for iter 7.2 —
        // this conservative rule handles the common case where
        // a Forall'd primop receives a concrete union arg.
        (Type::Union(a), Type::Union(b)) => {
            if a.len() != b.len() {
                return false;
            }
            a.iter()
                .zip(b.iter())
                .all(|(am, bm)| unify_into(am, bm, tvars, out))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProcType;

    fn s(n: u32) -> Symbol {
        Symbol(n)
    }

    fn pt(params: Vec<Type>, ret: Type) -> Type {
        Type::Procedure_(Box::new(ProcType {
            params,
            return_type: ret,
            rest: None,
            filter: None,
        }))
    }

    // ---- subst ----

    #[test]
    fn subst_replaces_simple_var() {
        let mut m = HashMap::new();
        m.insert(s(1), Type::Fixnum);
        assert_eq!(subst(&Type::Var(s(1)), &m), Type::Fixnum);
    }

    #[test]
    fn subst_leaves_unmapped_var() {
        let mut m = HashMap::new();
        m.insert(s(1), Type::Fixnum);
        assert_eq!(subst(&Type::Var(s(2)), &m), Type::Var(s(2)));
    }

    #[test]
    fn subst_descends_into_procedure() {
        let mut m = HashMap::new();
        m.insert(s(1), Type::Fixnum);
        let ty = pt(vec![Type::Var(s(1))], Type::Var(s(1)));
        let got = subst(&ty, &m);
        match got {
            Type::Procedure_(p) => {
                assert_eq!(p.params, vec![Type::Fixnum]);
                assert_eq!(p.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn subst_capture_avoiding_under_inner_forall() {
        // Outer mapping T → Fixnum.
        // Inner: `(All (T) (-> T T))` — inner T shadows outer
        // and shouldn't get substituted.
        let mut m = HashMap::new();
        m.insert(s(1), Type::Fixnum);
        let inner = Type::Forall(
            vec![s(1)],
            Box::new(pt(vec![Type::Var(s(1))], Type::Var(s(1)))),
        );
        let got = subst(&inner, &m);
        // The inner Forall should be preserved as-is.
        assert_eq!(got, inner);
    }

    #[test]
    fn subst_descends_into_union_and_normalizes() {
        let mut m = HashMap::new();
        m.insert(s(1), Type::Flonum);
        let ty = Type::union(vec![Type::Fixnum, Type::Var(s(1))]);
        let got = subst(&ty, &m);
        assert_eq!(got, Type::union(vec![Type::Fixnum, Type::Flonum]));
    }

    // ---- instantiate ----

    #[test]
    fn instantiate_simple_identity() {
        // (All (T) (-> T T)) with T → Fixnum.
        let id = Type::Forall(
            vec![s(1)],
            Box::new(pt(vec![Type::Var(s(1))], Type::Var(s(1)))),
        );
        let inst = instantiate(&id, &[Type::Fixnum]).unwrap();
        match inst {
            Type::Procedure_(p) => {
                assert_eq!(p.params, vec![Type::Fixnum]);
                assert_eq!(p.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn instantiate_arity_mismatch_returns_none() {
        let id = Type::Forall(vec![s(1)], Box::new(Type::Var(s(1))));
        assert!(instantiate(&id, &[Type::Fixnum, Type::Flonum]).is_none());
    }

    #[test]
    fn instantiate_non_forall_returns_none() {
        assert!(instantiate(&Type::Fixnum, &[Type::Fixnum]).is_none());
    }

    // ---- unify ----

    #[test]
    fn unify_var_to_atom() {
        // Template: Var(T). Concrete: Fixnum. Binding: T → Fixnum.
        let m = unify(&Type::Var(s(1)), &Type::Fixnum, &[s(1)]).unwrap();
        assert_eq!(m.get(&s(1)), Some(&Type::Fixnum));
    }

    #[test]
    fn unify_atom_to_atom() {
        let m = unify(&Type::Fixnum, &Type::Fixnum, &[]).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn unify_distinct_atoms_fails() {
        assert!(unify(&Type::Fixnum, &Type::Flonum, &[]).is_none());
    }

    #[test]
    fn unify_procedure_matches_each_param_and_return() {
        // Template: (-> T T). Concrete: (-> Fixnum Fixnum).
        let t = pt(vec![Type::Var(s(1))], Type::Var(s(1)));
        let c = pt(vec![Type::Fixnum], Type::Fixnum);
        let m = unify(&t, &c, &[s(1)]).unwrap();
        assert_eq!(m.get(&s(1)), Some(&Type::Fixnum));
    }

    #[test]
    fn unify_conflicting_binding_fails() {
        // Template: (-> T T). Concrete: (-> Fixnum String).
        // T inferred Fixnum first, then conflict with String.
        let t = pt(vec![Type::Var(s(1)), Type::Var(s(1))], Type::Var(s(1)));
        let c = pt(vec![Type::Fixnum, Type::String], Type::Any);
        assert!(unify(&t, &c, &[s(1)]).is_none());
    }

    #[test]
    fn unify_with_any_passes_for_reoccurrence() {
        // T's first binding is Fixnum; second is Any →
        // accepted (gradual escape).
        let t = pt(vec![Type::Var(s(1)), Type::Var(s(1))], Type::Var(s(1)));
        let c = pt(vec![Type::Fixnum, Type::Any], Type::Any);
        let m = unify(&t, &c, &[s(1)]).unwrap();
        assert_eq!(m.get(&s(1)), Some(&Type::Fixnum));
    }

    #[test]
    fn unify_listof_recursively() {
        // (Listof T) vs (Listof Fixnum) → T = Fixnum.
        let t = Type::Listof(Box::new(Type::Var(s(1))));
        let c = Type::Listof(Box::new(Type::Fixnum));
        let m = unify(&t, &c, &[s(1)]).unwrap();
        assert_eq!(m.get(&s(1)), Some(&Type::Fixnum));
    }

    #[test]
    fn unify_arity_mismatch_on_procedure_fails() {
        let t = pt(vec![Type::Var(s(1))], Type::Var(s(1)));
        let c = pt(vec![Type::Fixnum, Type::Fixnum], Type::Fixnum);
        assert!(unify(&t, &c, &[s(1)]).is_none());
    }

    #[test]
    fn unify_two_vars_independently() {
        // (-> A B B) vs (-> Fixnum String String)
        let t = pt(vec![Type::Var(s(1)), Type::Var(s(2))], Type::Var(s(2)));
        let c = pt(vec![Type::Fixnum, Type::String], Type::String);
        let m = unify(&t, &c, &[s(1), s(2)]).unwrap();
        assert_eq!(m.get(&s(1)), Some(&Type::Fixnum));
        assert_eq!(m.get(&s(2)), Some(&Type::String));
    }

    #[test]
    fn unify_then_subst_round_trip() {
        // End-to-end: (-> T T) unifies against (-> Fixnum
        // Fixnum), then substituting the binding back into the
        // template yields the concrete.
        let template = pt(vec![Type::Var(s(1))], Type::Var(s(1)));
        let concrete = pt(vec![Type::Fixnum], Type::Fixnum);
        let m = unify(&template, &concrete, &[s(1)]).unwrap();
        assert_eq!(subst(&template, &m), concrete);
    }
}
