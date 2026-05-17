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

/// A type mismatch surfaced by `check`. Each variant carries the
/// `Span` of the offending CoreExpr so iter 2.8 can render a
/// per-source-location diagnostic.
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

/// Phase-2 subtyping over atomic types.
///
/// Rules:
/// 1. **Reflexive** — `T <: T` for every `T`.
/// 2. **Top** — `T <: Any` and `Any <: T` are both true. The
///    second is gradual typing's escape hatch: an `Any`-typed
///    value is allowed to flow into any expected position. This
///    enables untyped code to call into typed functions without
///    extra ceremony; the runtime is responsible for the
///    failure-mode (no runtime contract yet — Phase 5 adds those).
/// 3. **Bottom** — `Never <: T` for every `T`.
/// 4. **Procedure_** — structural: `(-> P… R) <: (-> Q… S)` iff
///    arities match, each `Qi <: Pi` (contravariant params), and
///    `R <: S` (covariant return). Iter 2.4 only needs the
///    same-shape case; the general rule is here for forward
///    compatibility.
///
/// Phase 3 adds: `Union` (left distributes — every member must
/// be a subtype), `Listof`, `Vectorof`.
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
            subtype(&a.return_type, &b.return_type)
        }
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

    #[test]
    fn unknown_operator_typed_as_any_is_permissive() {
        // `foo` isn't in the env; ref returns Any; calls through
        // Any are unchecked at iter 2.4 (full untyped-fallback
        // story is iter 2.7).
        assert_eq!(check_program("(foo 1 2)", &Type::Any), Ok(()));
        assert_eq!(check_program("(foo 1 2)", &Type::Fixnum), Ok(()));
    }
}
