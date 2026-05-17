//! Top-down type checking (`check(expr, expected) -> Result<()>`).
//!
//! Phase 2 iter 2.4: the second half of the bidirectional pair.
//! `check` takes an `expected: &Type` and tries to prove the
//! expression conforms to it; failures become [`TypeError`].
//!
//! The split between `infer` (bottom-up, infallible) and `check`
//! (top-down, fallible) follows Pierce & Turner's local type
//! inference: annotations drive top-down propagation of expected
//! types; everything else falls back to `infer` + a subtype
//! check at the boundary.
//!
//! Iter 2.4 only wires up the framework — `App` arg-checking,
//! `If` branch-checking, `Begin` last-expr-checking. Iter 2.5
//! plugs in Lambda annotations (via `AnnotationTable`), iter
//! 2.6 plugs in Letrec, iter 2.7 settles the untyped-fallback
//! rules, and iter 2.8 hooks `TypeError` to `cs_diag::Diagnostic`.

use cs_diag::Span;
use cs_ir::CoreExpr;

use crate::env::TypeEnv;
use crate::infer::infer;
use crate::types::Type;

/// Pretty-print a `Type` for diagnostic messages.
///
/// Renders close to the user's source syntax: atoms by their
/// canonical name, `(U …)` for unions, `(-> … R)` for procs
/// (with `... T` for rest), `(Listof T)`, `(Vectorof T)`.
/// Round-trips with `parse_type_ann` for the constructors the
/// parser recognizes.
pub fn render_type(t: &Type) -> String {
    match t {
        Type::Fixnum => "Fixnum".into(),
        Type::Flonum => "Flonum".into(),
        Type::Boolean => "Boolean".into(),
        Type::Character => "Character".into(),
        Type::Symbol => "Symbol".into(),
        Type::Pair => "Pair".into(),
        Type::Vector => "Vector".into(),
        Type::String => "String".into(),
        Type::ByteVector => "ByteVector".into(),
        Type::Procedure => "Procedure".into(),
        Type::Null => "Null".into(),
        Type::Any => "Any".into(),
        Type::Never => "Never".into(),
        Type::Union(members) => {
            let inner: Vec<String> = members.iter().map(render_type).collect();
            format!("(U {})", inner.join(" "))
        }
        Type::Listof(elem) => format!("(Listof {})", render_type(elem)),
        Type::Vectorof(elem) => format!("(Vectorof {})", render_type(elem)),
        Type::Procedure_(pt) => {
            let mut parts: Vec<String> = pt.params.iter().map(render_type).collect();
            if let Some(rest) = &pt.rest {
                parts.push(render_type(rest));
                parts.push("...".into());
            }
            parts.push(render_type(&pt.return_type));
            format!("(-> {})", parts.join(" "))
        }
        // Phase 7: polymorphic types render as `(All (T1 T2 …)
        // body)` — close to the surface syntax. Type variables
        // print as their Symbol's u32 id since the Checker
        // doesn't own a SymbolTable; downstream renderers
        // (LSP, future error pretty-printer) can rewrap to the
        // user's chosen name if needed.
        Type::Forall(vars, body) => {
            let var_names: Vec<String> = vars.iter().map(|v| format!("?{}", v.0)).collect();
            format!("(All ({}) {})", var_names.join(" "), render_type(body))
        }
        Type::Var(s) => format!("?{}", s.0),
    }
}

/// A type mismatch surfaced by `check`. Each variant carries the
/// `Span` of the offending CoreExpr so iter 6.1's
/// [`TypeError::to_diagnostic`] can render a per-source-location
/// diagnostic.
///
/// `TypeError` deliberately stays decoupled from `cs_diag` —
/// iter 2.8 owns the conversion. Keeping it as a plain enum here
/// makes it cheap to unit-test the checker without dragging in
/// `SourceMap` plumbing.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeError {
    /// `expected` and `found` disagreed at `span`.
    Mismatch {
        expected: Type,
        found: Type,
        span: Span,
    },
    /// Procedure application with the wrong number of args.
    ArityMismatch {
        expected: usize,
        found: usize,
        span: Span,
    },
    /// `(<non-proc> ...)` — the operator slot inferred to a type
    /// that isn't a procedure (and isn't `Any`).
    NotAProcedure { found: Type, span: Span },
    /// An empty `Begin` was checked against a non-`Any` type
    /// (in well-formed core this shouldn't happen — the expander
    /// fills empty bodies with `unspecified` — but the variant
    /// keeps the match exhaustive).
    EmptyBegin { span: Span },
}

impl TypeError {
    /// The source span this error is attached to.
    pub fn span(&self) -> Span {
        match self {
            TypeError::Mismatch { span, .. }
            | TypeError::ArityMismatch { span, .. }
            | TypeError::NotAProcedure { span, .. }
            | TypeError::EmptyBegin { span } => *span,
        }
    }

    /// Render as a `cs_diag::Diagnostic`. The `code` field is
    /// stable and groups errors by kind for tooling.
    pub fn to_diagnostic(&self) -> cs_diag::Diagnostic {
        match self {
            TypeError::Mismatch {
                expected,
                found,
                span,
            } => cs_diag::Diagnostic {
                severity: cs_diag::Severity::Error,
                code: Some("typer-mismatch"),
                message: format!(
                    "expected {}, found {}",
                    render_type(expected),
                    render_type(found)
                ),
                primary: *span,
                labels: vec![],
                notes: vec![],
            },
            TypeError::ArityMismatch {
                expected,
                found,
                span,
            } => cs_diag::Diagnostic {
                severity: cs_diag::Severity::Error,
                code: Some("typer-arity"),
                message: format!(
                    "procedure expects {} argument{}, got {}",
                    expected,
                    if *expected == 1 { "" } else { "s" },
                    found
                ),
                primary: *span,
                labels: vec![],
                notes: vec![],
            },
            TypeError::NotAProcedure { found, span } => cs_diag::Diagnostic {
                severity: cs_diag::Severity::Error,
                code: Some("typer-not-a-procedure"),
                message: format!(
                    "tried to call a value of type {}; expected a procedure",
                    render_type(found)
                ),
                primary: *span,
                labels: vec![],
                notes: vec![],
            },
            TypeError::EmptyBegin { span } => cs_diag::Diagnostic {
                severity: cs_diag::Severity::Error,
                code: Some("typer-empty-begin"),
                message: "empty `begin` cannot satisfy a non-Any expected type".into(),
                primary: *span,
                labels: vec![],
                notes: vec![],
            },
        }
    }
}

/// Subtyping over the full Phase-3 type lattice.
///
/// Rules, checked in order:
/// 1. **Reflexive** — `T <: T` for every `T`.
/// 2. **Gradual top** — `T <: Any` and `Any <: T` are both
///    true. The second is gradual typing's escape hatch: an
///    `Any`-typed value flows into any expected position so
///    untyped code can call typed code without ceremony. The
///    runtime is responsible for the failure-mode (no
///    contracts yet — Phase 5).
/// 3. **Bottom** — `Never <: T` for every `T`.
/// 4. **Union, left** — `(U A B …) <: T` iff every member is a
///    subtype of `T`. Checked BEFORE the right-rule so a union
///    on both sides recurses on each left member separately.
/// 5. **Union, right** — `T <: (U A B …)` iff `T` is a subtype
///    of some member.
/// 6. **Procedure_** — structural: `(-> P… R) <: (-> Q… S)` iff
///    arities match, each `Qi <: Pi` (contravariant params),
///    and `R <: S` (covariant return). Iter 3.5 adds rest-arg
///    handling.
/// 7. **Listof** — covariant: `(Listof A) <: (Listof B)` iff
///    `A <: B` (lists are read-mostly in idiomatic Scheme).
/// 8. **Vectorof** — invariant: `(Vectorof A) <: (Vectorof B)`
///    iff `A == B` (vectors are mutable, so subtype variance
///    would be unsound).
pub fn subtype(sub: &Type, sup: &Type) -> bool {
    if sub == sup {
        return true;
    }
    if matches!(sup, Type::Any) || matches!(sub, Type::Any) {
        return true;
    }
    if matches!(sub, Type::Never) {
        return true;
    }
    // Union, left: every member of `sub` must subtype `sup`.
    // Important: this comes before the right-rule so unions on
    // both sides decompose member-by-member rather than
    // accidentally matching `Union ≡ Union` only when both
    // members lists are equal.
    if let Type::Union(subs) = sub {
        return subs.iter().all(|s| subtype(s, sup));
    }
    // Union, right: `sub` subtypes some member of `sup`.
    if let Type::Union(sups) = sup {
        return sups.iter().any(|s| subtype(sub, s));
    }
    // Phase 7.4: gradual Forall subtyping. When either side is
    // a `Forall`, we don't attempt alpha-equivalence or full
    // higher-rank reasoning. Instead we instantiate with `Any`
    // for every quantified variable and subtype the resulting
    // bodies. This is sound under the gradual rule that `Any`
    // flows freely: an unannotated `Any → Any` lambda satisfies
    // `(All (T) (-> T T))`, and conversely a polymorphic value
    // can flow into any expected position where its Any-
    // instantiated body would.
    if let Type::Forall(vs, body) = sup {
        let mapping: std::collections::HashMap<cs_core::Symbol, Type> =
            vs.iter().map(|v| (*v, Type::Any)).collect();
        let instantiated = crate::poly::subst(body, &mapping);
        return subtype(sub, &instantiated);
    }
    if let Type::Forall(vs, body) = sub {
        let mapping: std::collections::HashMap<cs_core::Symbol, Type> =
            vs.iter().map(|v| (*v, Type::Any)).collect();
        let instantiated = crate::poly::subst(body, &mapping);
        return subtype(&instantiated, sup);
    }
    match (sub, sup) {
        (Type::Procedure_(a), Type::Procedure_(b)) => {
            if a.params.len() != b.params.len() {
                return false;
            }
            for (ap, bp) in a.params.iter().zip(b.params.iter()) {
                if !subtype(bp, ap) {
                    return false;
                }
            }
            // Rest-arg subtyping is iter 3.5; for now require
            // both absent or both present + contravariant.
            match (&a.rest, &b.rest) {
                (None, None) => {}
                (Some(ar), Some(br)) => {
                    if !subtype(br, ar) {
                        return false;
                    }
                }
                _ => return false,
            }
            subtype(&a.return_type, &b.return_type)
        }
        (Type::Listof(a), Type::Listof(b)) => subtype(a, b),
        (Type::Vectorof(a), Type::Vectorof(b)) => a == b,
        _ => false,
    }
}

/// Check that `expr` has type `expected` in `env`.
///
/// Sites that propagate the expected type into sub-positions
/// (App args, If branches, Begin's last expr) do so directly —
/// they don't synthesize an intermediate type, they call `check`
/// recursively with the position's expected type. Everything
/// else (Const, Ref, Lambda, Letrec, Set in this iter) falls
/// back to `infer` + a subtype check at the boundary.
pub fn check(expr: &CoreExpr, expected: &Type, env: &mut TypeEnv) -> Result<(), TypeError> {
    match expr {
        CoreExpr::App { func, args, span } => check_app(func, args, *span, expected, env),
        CoreExpr::If { then, alt, .. } => {
            check(then, expected, env)?;
            check(alt, expected, env)
        }
        CoreExpr::Begin { exprs, span } => {
            if let Some((last, init)) = exprs.split_last() {
                for e in init {
                    check(e, &Type::Any, env)?;
                }
                check(last, expected, env)
            } else if subtype(&Type::Any, expected) {
                Ok(())
            } else {
                Err(TypeError::EmptyBegin { span: *span })
            }
        }
        // Iters 2.5 (Lambda), 2.6 (Letrec), 2.8 (full Set) will
        // re-route these. For 2.4 they fall through to the
        // infer-then-subtype path, which is the right shape for
        // unannotated lambdas / letrecs against expected `Any`.
        _ => {
            let found = infer(expr, env);
            if subtype(&found, expected) {
                Ok(())
            } else {
                Err(TypeError::Mismatch {
                    expected: expected.clone(),
                    found,
                    span: expr.span(),
                })
            }
        }
    }
}

/// Narrow `t` to the positive proposition of `filter` — the
/// type the operand has if the predicate returned true.
///
/// Rules (Phase 4 iter 4.2):
/// - If `t <: filter`, no change (already as narrow as possible).
/// - If `t` is a `Union`, keep only members that are subtypes
///   of `filter`. Members that are non-subtypes drop because
///   the predicate ruled them out.
/// - If `filter <: t`, narrow to `filter`.
/// - Otherwise the operand's type and the filter are disjoint;
///   the then-branch is unreachable, so narrow to `Never`.
pub fn narrow_positive(t: &Type, filter: &Type) -> Type {
    // Gradual `Any`: in the predicate's then-branch, treat the
    // operand AS the filter type. Without this special case the
    // `subtype(Any, filter) == true` (gradual escape) below
    // would short-circuit and return `Any`, which is correct
    // for the typecheck but loses the refinement for downstream
    // consumers (AOT hints, JIT specialization).
    if matches!(t, Type::Any) {
        return filter.clone();
    }
    if subtype(t, filter) {
        return t.clone();
    }
    if let Type::Union(members) = t {
        let kept: Vec<Type> = members
            .iter()
            .filter(|m| subtype(m, filter))
            .cloned()
            .collect();
        return Type::union(kept);
    }
    if subtype(filter, t) {
        return filter.clone();
    }
    Type::Never
}

/// Narrow `t` to the negative proposition of `filter` — the
/// type the operand has if the predicate returned false.
///
/// Rules:
/// - If `t` is a `Union`, drop members that are subtypes of
///   `filter`.
/// - If `t <: filter`, narrow to `Never` (the else-branch is
///   unreachable).
/// - Otherwise return `t` unchanged — we can't subtract.
pub fn narrow_negative(t: &Type, filter: &Type) -> Type {
    // Gradual `Any`: "not filter" still admits everything
    // OTHER than the filter — we have no information about
    // what specifically it might be, so the right answer is
    // still `Any`, not `Never`. Without this special case,
    // `subtype(Any, filter) == true` (gradual escape) would
    // route to the `Never` branch below.
    if matches!(t, Type::Any) {
        return Type::Any;
    }
    if let Type::Union(members) = t {
        let kept: Vec<Type> = members
            .iter()
            .filter(|m| !subtype(m, filter))
            .cloned()
            .collect();
        return Type::union(kept);
    }
    if subtype(t, filter) {
        return Type::Never;
    }
    t.clone()
}

/// `check`'s helper for `App`. When the operator infers to a
/// `Procedure_(pt)`:
/// 1. Arity mismatch → `ArityMismatch`.
/// 2. Each arg is `check`ed against the corresponding `pt.params[i]`.
/// 3. The procedure's `return_type` is subtype-checked against
///    `expected`.
///
/// When the operator infers to `Any` (most common case for
/// unannotated user-defined functions and untyped primops): no
/// per-arg checks (we have nothing to check against); just
/// confirm `Any <: expected` (always true) and return Ok.
///
/// When the operator infers to a non-procedure atom (`Fixnum`,
/// `String`, …): `NotAProcedure`.
fn check_app(
    func: &CoreExpr,
    args: &[CoreExpr],
    span: Span,
    expected: &Type,
    env: &mut TypeEnv,
) -> Result<(), TypeError> {
    let f_ty = infer(func, env);
    match &f_ty {
        Type::Procedure_(pt) => {
            if pt.params.len() != args.len() {
                return Err(TypeError::ArityMismatch {
                    expected: pt.params.len(),
                    found: args.len(),
                    span,
                });
            }
            for (arg, param_ty) in args.iter().zip(pt.params.iter()) {
                check(arg, param_ty, env)?;
            }
            if !subtype(&pt.return_type, expected) {
                return Err(TypeError::Mismatch {
                    expected: expected.clone(),
                    found: pt.return_type.clone(),
                    span,
                });
            }
            Ok(())
        }
        // Both Procedure (opaque) and Any are permissive.
        Type::Procedure | Type::Any => Ok(()),
        other => Err(TypeError::NotAProcedure {
            found: other.clone(),
            span: func.span(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use cs_core::SymbolTable;
    use cs_diag::SourceMap;
    use cs_expand::Expander;
    use cs_parse::read_all;
    use std::collections::HashMap;

    use super::*;
    use crate::builtins::install_primops;
    use crate::types::Type;

    fn parse_and_expand(src: &str) -> (CoreExpr, SymbolTable) {
        let mut sm = SourceMap::new();
        let f = sm.add("<check-test>", src);
        let mut syms = SymbolTable::new();
        let data = read_all(f, src, &mut syms).expect("parse");
        let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
        let mut exp = Expander::new(&mut syms, &mut macros);
        let core = exp.expand_program(&data).expect("expand");
        drop(exp);
        (core, syms)
    }

    fn check_program(src: &str, expected: &Type) -> Result<(), TypeError> {
        let (core, mut syms) = parse_and_expand(src);
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        check(&core, expected, &mut env)
    }

    // ---- subtype tests ----

    #[test]
    fn subtype_reflexive() {
        for t in [
            Type::Fixnum,
            Type::Flonum,
            Type::String,
            Type::Pair,
            Type::Boolean,
            Type::Any,
            Type::Never,
        ] {
            assert!(subtype(&t, &t), "T <: T should hold for {t:?}");
        }
    }

    #[test]
    fn subtype_any_is_top_and_bottom_gradually() {
        // T <: Any
        assert!(subtype(&Type::Fixnum, &Type::Any));
        // Any <: T  (gradual escape hatch)
        assert!(subtype(&Type::Any, &Type::Fixnum));
    }

    #[test]
    fn subtype_never_is_bottom() {
        assert!(subtype(&Type::Never, &Type::Fixnum));
        assert!(subtype(&Type::Never, &Type::String));
    }

    #[test]
    fn subtype_distinct_atoms_fail() {
        assert!(!subtype(&Type::Fixnum, &Type::Flonum));
        assert!(!subtype(&Type::String, &Type::Fixnum));
        assert!(!subtype(&Type::Pair, &Type::Vector));
    }

    // ---- Phase 3 iter 3.2: Union subtyping ----

    #[test]
    fn subtype_member_into_union() {
        // T <: (U A B) iff T <: A or T <: B
        let u = Type::union(vec![Type::Fixnum, Type::Flonum]);
        assert!(subtype(&Type::Fixnum, &u));
        assert!(subtype(&Type::Flonum, &u));
        assert!(!subtype(&Type::String, &u));
    }

    #[test]
    fn subtype_union_distributes_left() {
        // (U A B) <: T iff A <: T and B <: T
        let u = Type::union(vec![Type::Fixnum, Type::Flonum]);
        assert!(subtype(&u, &u));
        assert!(subtype(&u, &Type::Any));
        // Neither member is a subtype of just Fixnum, so the
        // union isn't either.
        assert!(!subtype(&u, &Type::Fixnum));
    }

    #[test]
    fn subtype_union_to_wider_union() {
        let narrow = Type::union(vec![Type::Fixnum, Type::Flonum]);
        let wide = Type::union(vec![Type::Fixnum, Type::Flonum, Type::String]);
        assert!(subtype(&narrow, &wide));
        assert!(!subtype(&wide, &narrow));
    }

    #[test]
    fn subtype_never_into_union() {
        let u = Type::union(vec![Type::Fixnum, Type::Flonum]);
        assert!(subtype(&Type::Never, &u));
    }

    #[test]
    fn subtype_listof_is_covariant() {
        let any_list = Type::Listof(Box::new(Type::Any));
        let fx_list = Type::Listof(Box::new(Type::Fixnum));
        // Listof Fixnum <: Listof Any
        assert!(subtype(&fx_list, &any_list));
        // and Listof Any <: Listof Fixnum (gradual escape hatch)
        assert!(subtype(&any_list, &fx_list));
        // But Listof Fixnum is NOT <: Listof String (distinct
        // atoms; no gradual escape because element type is
        // concrete on both sides).
        let str_list = Type::Listof(Box::new(Type::String));
        assert!(!subtype(&fx_list, &str_list));
    }

    #[test]
    fn subtype_vectorof_is_invariant() {
        let fx_vec = Type::Vectorof(Box::new(Type::Fixnum));
        let any_vec = Type::Vectorof(Box::new(Type::Any));
        // Invariant: Vectorof Fixnum is NOT a subtype of
        // Vectorof Any, even though Fixnum <: Any. Mutability
        // makes width subtyping unsound for vectors.
        assert!(!subtype(&fx_vec, &any_vec));
        assert!(!subtype(&any_vec, &fx_vec));
        // Same-element vectors do subtype each other.
        let fx_vec2 = Type::Vectorof(Box::new(Type::Fixnum));
        assert!(subtype(&fx_vec, &fx_vec2));
    }

    #[test]
    fn subtype_proc_with_union_args_via_distribution() {
        // (-> (U Fixnum Flonum) Fixnum) accepts a value of
        // declared type Fixnum (Fixnum <: (U Fixnum Flonum))
        // contravariantly through Procedure_.
        use crate::types::ProcType;
        let union_arg = Type::union(vec![Type::Fixnum, Type::Flonum]);
        let proc_union = Type::Procedure_(Box::new(ProcType {
            params: vec![union_arg.clone()],
            return_type: Type::Fixnum,
            rest: None,
            filter: None,
        }));
        let proc_fx = Type::Procedure_(Box::new(ProcType {
            params: vec![Type::Fixnum],
            return_type: Type::Fixnum,
            rest: None,
            filter: None,
        }));
        // (-> (U Fx Fl) Fx) is a subtype of (-> Fx Fx):
        // contravariantly, Fx <: (U Fx Fl).
        assert!(subtype(&proc_union, &proc_fx));
        // The reverse fails: (-> Fx Fx) accepts only Fx, but
        // its supertype (-> (U Fx Fl) Fx) would need to accept
        // both, and the contravariant Fx <: Fl doesn't hold.
        assert!(!subtype(&proc_fx, &proc_union));
    }

    // ---- check tests ----

    #[test]
    fn const_fixnum_checks_against_fixnum() {
        assert_eq!(check_program("42", &Type::Fixnum), Ok(()));
    }

    #[test]
    fn const_fixnum_against_string_mismatch() {
        let result = check_program("42", &Type::String);
        assert!(matches!(result, Err(TypeError::Mismatch { .. })));
    }

    #[test]
    fn const_anything_against_any_passes() {
        assert_eq!(check_program("42", &Type::Any), Ok(()));
        assert_eq!(check_program("\"hi\"", &Type::Any), Ok(()));
        assert_eq!(check_program("#t", &Type::Any), Ok(()));
    }

    #[test]
    fn app_string_length_checks_against_fixnum() {
        assert_eq!(
            check_program("(string-length \"hi\")", &Type::Fixnum),
            Ok(())
        );
    }

    #[test]
    fn app_string_length_against_string_mismatch() {
        let result = check_program("(string-length \"hi\")", &Type::String);
        assert!(matches!(result, Err(TypeError::Mismatch { .. })));
    }

    #[test]
    fn app_arg_type_mismatch_fails() {
        // string-length expects String; passing a Fixnum should fail.
        let result = check_program("(string-length 42)", &Type::Fixnum);
        match result {
            Err(TypeError::Mismatch {
                expected, found, ..
            }) => {
                assert_eq!(expected, Type::String);
                assert_eq!(found, Type::Fixnum);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn app_arity_mismatch_fails() {
        // string-length is 1-ary; passing two args should err.
        let result = check_program("(string-length \"a\" \"b\")", &Type::Fixnum);
        match result {
            Err(TypeError::ArityMismatch {
                expected, found, ..
            }) => {
                assert_eq!(expected, 1);
                assert_eq!(found, 2);
            }
            other => panic!("expected ArityMismatch, got {other:?}"),
        }
    }

    #[test]
    fn app_on_non_procedure_value_errors() {
        // Calling a string literal as a function should be NotAProcedure.
        // First we have to seed a typed binding for the operator slot;
        // the simplest way is to use a (begin) with a let-like fake —
        // here we just inline-check a synthetic Ref.
        use cs_diag::{FileId, Span};
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        let x = syms.intern("x");
        env.define(x, Type::String);
        let dummy = Span::new(FileId(0), 0, 0);
        let expr = CoreExpr::App {
            func: std::rc::Rc::new(CoreExpr::Ref {
                name: x,
                span: dummy,
            }),
            args: vec![],
            span: dummy,
        };
        let result = check(&expr, &Type::Any, &mut env);
        match result {
            Err(TypeError::NotAProcedure { found, .. }) => {
                assert_eq!(found, Type::String);
            }
            other => panic!("expected NotAProcedure, got {other:?}"),
        }
    }

    #[test]
    fn if_both_branches_must_check() {
        // Both branches need to satisfy the expected type.
        assert_eq!(check_program("(if #t 1 2)", &Type::Fixnum), Ok(()));
        let result = check_program("(if #t 1 \"hi\")", &Type::Fixnum);
        assert!(matches!(result, Err(TypeError::Mismatch { .. })));
    }

    #[test]
    fn begin_only_last_expr_matters_for_expected() {
        assert_eq!(check_program("(begin 1 2 \"x\")", &Type::String), Ok(()));
        // Initial expressions check against Any (they're discarded
        // values), so even a type-mismatch in them doesn't fail.
        assert_eq!(check_program("(begin \"x\" 1)", &Type::Fixnum), Ok(()));
    }

    #[test]
    fn begin_fails_when_last_expr_mismatches() {
        let result = check_program("(begin 1 \"x\")", &Type::Fixnum);
        assert!(matches!(result, Err(TypeError::Mismatch { .. })));
    }

    // ---- render_type + to_diagnostic ----

    #[test]
    fn render_type_atoms() {
        assert_eq!(render_type(&Type::Fixnum), "Fixnum");
        assert_eq!(render_type(&Type::String), "String");
        assert_eq!(render_type(&Type::Any), "Any");
        assert_eq!(render_type(&Type::Never), "Never");
    }

    #[test]
    fn render_type_union_and_containers() {
        let u = Type::union(vec![Type::Fixnum, Type::Flonum]);
        assert_eq!(render_type(&u), "(U Fixnum Flonum)");
        assert_eq!(
            render_type(&Type::Listof(Box::new(Type::Fixnum))),
            "(Listof Fixnum)"
        );
    }

    #[test]
    fn render_type_procedure_with_rest() {
        use crate::types::ProcType;
        let pt = Type::Procedure_(Box::new(ProcType {
            params: vec![Type::Fixnum],
            return_type: Type::Boolean,
            rest: Some(Type::String),
            filter: None,
        }));
        assert_eq!(render_type(&pt), "(-> Fixnum String ... Boolean)");
    }

    #[test]
    fn diagnostic_mismatch_renders_human_readable() {
        use cs_diag::{FileId, Span};
        let e = TypeError::Mismatch {
            expected: Type::Fixnum,
            found: Type::String,
            span: Span::new(FileId(0), 0, 5),
        };
        let d = e.to_diagnostic();
        assert_eq!(d.severity, cs_diag::Severity::Error);
        assert_eq!(d.code, Some("typer-mismatch"));
        assert_eq!(d.message, "expected Fixnum, found String");
    }

    #[test]
    fn diagnostic_arity_renders_with_plural() {
        use cs_diag::{FileId, Span};
        let e = TypeError::ArityMismatch {
            expected: 1,
            found: 3,
            span: Span::new(FileId(0), 0, 5),
        };
        assert_eq!(
            e.to_diagnostic().message,
            "procedure expects 1 argument, got 3"
        );
        let e2 = TypeError::ArityMismatch {
            expected: 2,
            found: 0,
            span: Span::new(FileId(0), 0, 5),
        };
        assert_eq!(
            e2.to_diagnostic().message,
            "procedure expects 2 arguments, got 0"
        );
    }

    #[test]
    fn unknown_operator_typed_as_any_is_permissive() {
        // `foo` isn't in the env; ref returns Any; calls through
        // Any are unchecked at iter 2.4 (full untyped-fallback
        // story is iter 2.7).
        assert_eq!(check_program("(foo 1 2)", &Type::Any), Ok(()));
        assert_eq!(check_program("(foo 1 2)", &Type::Fixnum), Ok(()));
    }
}
