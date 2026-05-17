//! Bottom-up type inference (`infer(expr) -> Type`).
//!
//! Phase 2 iter 2.3: minimal `infer` that handles the small,
//! "obvious-bottom-up" cases — `Const`, `Ref`, `App`, `Begin`,
//! `If`. The other forms (`Lambda`, `Letrec`, `Set`) get
//! placeholder treatment that returns `Any`; iters 2.5 and 2.6
//! replace them with real implementations.
//!
//! `infer` is **infallible** in the gradual-typing sense: when
//! it cannot deduce a more-precise type than `Any`, it returns
//! `Any` rather than producing a diagnostic. The fail-mode
//! enters with `check(expr, expected)` in iter 2.4 — that's
//! where annotations turn "I don't know" into "this is wrong".

use cs_core::Value;
use cs_ir::CoreExpr;

use crate::env::TypeEnv;
use crate::types::Type;

/// Bottom-up infer a type for `expr` in `env`.
///
/// Returns the most precise atomic type the current rules can
/// derive, falling back to `Any` for anything ambiguous,
/// unbound, or not yet supported (lambdas, letrecs).
pub fn infer(expr: &CoreExpr, env: &TypeEnv) -> Type {
    match expr {
        CoreExpr::Const { value, .. } => infer_const(value),
        CoreExpr::Ref { name, .. } => env.lookup(*name).cloned().unwrap_or(Type::Any),
        CoreExpr::App { func, args, .. } => infer_app(func, args, env),
        CoreExpr::Begin { exprs, .. } => infer_begin(exprs, env),
        CoreExpr::If { then, alt, .. } => infer_if(then, alt, env),
        // Iters 2.5/2.6 replace these; for 2.3 they return `Any`
        // so a `Begin` that ends in a `Lambda` still types as
        // `Any` (best-effort) instead of crashing.
        CoreExpr::Lambda { .. } => Type::Procedure,
        CoreExpr::Letrec { body, .. } => infer(body, env),
        CoreExpr::Set { .. } => Type::Any,
    }
}

/// Map a literal `Value` to its atomic type. Multi-shape variants
/// (`Number`, `Procedure`, `Port`, `Hashtable`, etc. — anything
/// without a Phase-2 atom) fall through to `Any`.
fn infer_const(v: &Value) -> Type {
    use cs_core::Number;
    match v {
        Value::Null => Type::Null,
        Value::Boolean(_) => Type::Boolean,
        Value::Character(_) => Type::Character,
        Value::Symbol(_) => Type::Symbol,
        Value::String(_) => Type::String,
        Value::Vector(_) => Type::Vector,
        Value::ByteVector(_) => Type::ByteVector,
        Value::Pair(_) => Type::Pair,
        Value::Number(n) => match n {
            Number::Fixnum(_) | Number::Big(_) => Type::Fixnum,
            Number::Flonum(_) => Type::Flonum,
            // Rationals don't have a Phase-2 atom — Phase 3 may
            // add one. For now they're `Any` rather than wrong.
            Number::Rat(_) => Type::Any,
        },
        // No Phase-2 atom: procedures, ports, hashtables, promises,
        // the unspecified value, eof — all fall through.
        Value::Procedure(_)
        | Value::Port(_)
        | Value::Hashtable(_)
        | Value::Promise(_)
        | Value::Unspecified
        | Value::Eof => Type::Any,
    }
}

/// App: if the operator infers to a `Procedure_` with a matching
/// arity, return its declared `return_type`; otherwise `Any`.
///
/// Arity check is permissive at this iter — a mismatch falls
/// through to `Any` rather than failing, because iter 2.3 has no
/// diagnostic surface yet. Iter 2.5 tightens this (in `check`)
/// once `Lambda` annotations are wired up.
fn infer_app(func: &CoreExpr, args: &[CoreExpr], env: &TypeEnv) -> Type {
    let f_ty = infer(func, env);
    if let Type::Procedure_(pt) = f_ty {
        if pt.params.len() == args.len() {
            return pt.return_type.clone();
        }
    }
    Type::Any
}

/// Begin: type of the last expression. Earlier exprs are
/// inferred too (in `check`-mode they'll be checked against
/// `Any`), but we don't currently use those types for anything.
fn infer_begin(exprs: &[CoreExpr], env: &TypeEnv) -> Type {
    exprs.last().map(|e| infer(e, env)).unwrap_or(Type::Any)
}

/// If: least-upper-bound (LUB) of the two branches.
///
/// Phase 3: mixed branches produce a real `Union` (e.g.,
/// `(if #t 1 "hi") → (U Fixnum String)`). `Type::union`
/// normalizes — Any absorbs, Never drops, duplicates dedupe.
///
/// The condition itself is implicitly typed as anything (every
/// non-`#f` value is truthy in Scheme), so we don't inspect it.
fn infer_if(then: &CoreExpr, alt: &CoreExpr, env: &TypeEnv) -> Type {
    let t = infer(then, env);
    let a = infer(alt, env);
    Type::union(vec![t, a])
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
    use crate::types::{ProcType, Type};

    /// End-to-end: parse → expand → infer.
    fn parse_and_expand(src: &str) -> (CoreExpr, SymbolTable) {
        let mut sm = SourceMap::new();
        let f = sm.add("<infer-test>", src);
        let mut syms = SymbolTable::new();
        let data = read_all(f, src, &mut syms).expect("parse");
        let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
        let mut exp = Expander::new(&mut syms, &mut macros);
        let core = exp.expand_program(&data).expect("expand");
        drop(exp);
        (core, syms)
    }

    fn infer_program(src: &str) -> (Type, SymbolTable) {
        let (core, mut syms) = parse_and_expand(src);
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        let ty = infer(&core, &env);
        (ty, syms)
    }

    #[test]
    fn const_fixnum_infers_fixnum() {
        let (ty, _) = infer_program("42");
        assert_eq!(ty, Type::Fixnum);
    }

    #[test]
    fn const_flonum_infers_flonum() {
        let (ty, _) = infer_program("3.14");
        assert_eq!(ty, Type::Flonum);
    }

    #[test]
    fn const_string_infers_string() {
        let (ty, _) = infer_program("\"hi\"");
        assert_eq!(ty, Type::String);
    }

    #[test]
    fn const_boolean_infers_boolean() {
        let (ty, _) = infer_program("#t");
        assert_eq!(ty, Type::Boolean);
    }

    #[test]
    fn ref_via_env_returns_seeded_type() {
        let mut env = TypeEnv::new();
        let mut syms = SymbolTable::new();
        let x = syms.intern("x");
        env.define(x, Type::Fixnum);
        let expr = CoreExpr::Ref {
            name: x,
            span: cs_diag::Span::new(cs_diag::FileId(0), 0, 0),
        };
        assert_eq!(infer(&expr, &env), Type::Fixnum);
    }

    #[test]
    fn ref_to_unbound_returns_any() {
        let env = TypeEnv::new();
        let mut syms = SymbolTable::new();
        let unk = syms.intern("unknown");
        let expr = CoreExpr::Ref {
            name: unk,
            span: cs_diag::Span::new(cs_diag::FileId(0), 0, 0),
        };
        assert_eq!(infer(&expr, &env), Type::Any);
    }

    #[test]
    fn app_string_length_returns_fixnum() {
        // `string-length` is in the primop table as `String → Fixnum`.
        let (ty, _) = infer_program("(string-length \"hi\")");
        assert_eq!(ty, Type::Fixnum);
    }

    #[test]
    fn app_add_two_fixnums_returns_number_union() {
        // Phase 3: `+` widened to accept Fx|Fl and return the
        // union. Code that needs Fx-precision should use `fx+`.
        let (ty, _) = infer_program("(+ 1 2)");
        assert_eq!(ty, Type::union(vec![Type::Fixnum, Type::Flonum]));
    }

    #[test]
    fn app_fx_add_returns_fixnum() {
        // The narrow path: `fx+` is genuinely Fixnum-only.
        let (ty, _) = infer_program("(fx+ 1 2)");
        assert_eq!(ty, Type::Fixnum);
    }

    #[test]
    fn app_lt_returns_boolean() {
        let (ty, _) = infer_program("(< 1 2)");
        assert_eq!(ty, Type::Boolean);
    }

    #[test]
    fn app_with_wrong_arity_falls_back_to_any() {
        // `+` is table-typed as 2-ary. `(+ 1 2 3)` doesn't match,
        // so infer returns Any (not a hard error at this iter).
        let (ty, _) = infer_program("(+ 1 2 3)");
        assert_eq!(ty, Type::Any);
    }

    #[test]
    fn app_on_unknown_proc_returns_any() {
        // `foo` isn't in the env; ref returns Any; App on Any
        // can't deduce a return type → Any.
        let (ty, _) = infer_program("(foo 1 2)");
        assert_eq!(ty, Type::Any);
    }

    #[test]
    fn begin_returns_last_expr_type() {
        // `(begin 1 "hi")` evaluates to "hi" → String.
        let (ty, _) = infer_program("(begin 1 \"hi\")");
        assert_eq!(ty, Type::String);
    }

    #[test]
    fn if_with_same_branches_keeps_atom() {
        let (ty, _) = infer_program("(if #t 1 2)");
        assert_eq!(ty, Type::Fixnum);
    }

    #[test]
    fn if_with_mixed_branches_produces_union() {
        // Phase 3: mixed branches LUB to a real Union, not Any.
        let (ty, _) = infer_program("(if #t 1 \"hi\")");
        assert_eq!(ty, Type::union(vec![Type::Fixnum, Type::String]));
    }

    #[test]
    fn if_with_one_branch_any_widens_to_any() {
        // An unbound ref returns Any; union-with-Any collapses.
        let (ty, _) = infer_program("(if #t 1 unbound)");
        assert_eq!(ty, Type::Any);
    }

    #[test]
    fn lambda_returns_procedure() {
        // Iter 2.5 replaces this with a real ProcType, but for
        // iter 2.3 we treat lambdas as the opaque Procedure atom.
        let (ty, _) = infer_program("(lambda (x) x)");
        assert_eq!(ty, Type::Procedure);
    }

    #[test]
    fn ref_to_primop_returns_proc_type() {
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        let plus = syms.intern("+");
        let expr = CoreExpr::Ref {
            name: plus,
            span: cs_diag::Span::new(cs_diag::FileId(0), 0, 0),
        };
        let num = Type::union(vec![Type::Fixnum, Type::Flonum]);
        let want = Type::Procedure_(Box::new(ProcType {
            params: vec![num.clone(), num.clone()],
            return_type: num,
            rest: None,
            filter: None,
        }));
        assert_eq!(infer(&expr, &env), want);
    }
}
