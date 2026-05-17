//! The top-level `Checker` — couples `TypeEnv` with the
//! `AnnotationTable` so the bidirectional pass can find the
//! user's declared types for `Lambda` and `Letrec` forms.
//!
//! Phase 2 iter 2.5: Lambda handling.
//!
//! - The Checker is seeded with the primop table plus all
//!   top-level ascriptions from `AnnotationTable::top_level`.
//! - When `check` hits a `Lambda` whose `Span` matches a
//!   `LambdaAnnotation`, it pushes the annotated params into a
//!   new scope and checks the body against the annotated
//!   return type. Unannotated lambdas fall through to the
//!   infer-then-subtype path (same as iter 2.4).
//! - Top-level `Set` forms (which `define` lowers to) consult
//!   the seeded ascription for the bound name and check the
//!   value against it.
//!
//! Iter 2.6 extends this with `Letrec`, iter 2.7 nails down the
//! untyped-fallback rules, iter 2.8 converts `TypeError` to
//! `cs_diag::Diagnostic`.

use cs_core::SymbolTable;
use cs_diag::Span;
use cs_ir::{CoreExpr, Params};

use crate::annotate::{AnnotationTable, LambdaAnnotation};
use crate::builtins::install_primops;
use crate::check::{subtype, TypeError};
use crate::env::TypeEnv;
use crate::infer::infer;
use crate::types::{ProcType, Type};

/// The state a single typechecking run accumulates.
///
/// `'tab` is the lifetime of the borrowed `AnnotationTable` —
/// the Checker doesn't own it because the caller (driver /
/// LSP) holds onto the same table for downstream consumers
/// (JIT param hints, error rendering, hovers).
pub struct Checker<'tab> {
    pub table: &'tab AnnotationTable,
    pub env: TypeEnv,
}

impl<'tab> Checker<'tab> {
    /// Construct a Checker, seed the env with primops, and seed
    /// every top-level ascription so cross-function references
    /// resolve to the user's declared signature.
    pub fn new(table: &'tab AnnotationTable, syms: &mut SymbolTable) -> Self {
        let mut env = TypeEnv::new();
        install_primops(&mut env, syms);
        for ta in &table.top_level {
            env.define_top_level(ta.name, ta.type_ann.clone());
        }
        Self { table, env }
    }

    /// Infer the type of `expr`. Equivalent to the free
    /// `infer::infer` — kept as a method for symmetry and so
    /// later iters can specialize Lambda inference using
    /// `self.table`.
    pub fn infer(&self, expr: &CoreExpr) -> Type {
        infer(expr, &self.env)
    }

    /// Check that `expr` has type `expected`.
    ///
    /// The Lambda / Set arms consult `self.table` for declared
    /// types. Everything else dispatches to the iter 2.4 logic.
    pub fn check(&mut self, expr: &CoreExpr, expected: &Type) -> Result<(), TypeError> {
        match expr {
            CoreExpr::Lambda { params, body, span } => {
                self.check_lambda(params, body, *span, expected)
            }
            CoreExpr::App { func, args, span } => self.check_app(func, args, *span, expected),
            CoreExpr::If { then, alt, .. } => {
                self.check(then, expected)?;
                self.check(alt, expected)
            }
            CoreExpr::Begin { exprs, span } => {
                if let Some((last, init)) = exprs.split_last() {
                    for e in init {
                        self.check(e, &Type::Any)?;
                    }
                    self.check(last, expected)
                } else if subtype(&Type::Any, expected) {
                    Ok(())
                } else {
                    Err(TypeError::EmptyBegin { span: *span })
                }
            }
            CoreExpr::Set { name, value, span } => self.check_set(*name, value, *span, expected),
            CoreExpr::Letrec {
                bindings,
                body,
                span,
            } => self.check_letrec(bindings, body, *span, expected),
            _ => self.check_via_infer(expr, expected),
        }
    }

    /// Walk an entire program, accumulating every `TypeError`
    /// the check produced. The expected type for the program is
    /// `Any` (we don't constrain the program's overall result).
    ///
    /// This is the entry point a driver / REPL would call. Iter
    /// 2.8 wraps this in a `cs_diag`-aware variant that maps
    /// each `TypeError` to a `Diagnostic`.
    pub fn check_program(&mut self, program: &CoreExpr) -> Vec<TypeError> {
        let mut errors = Vec::new();
        self.check_collect(program, &Type::Any, &mut errors);
        errors
    }

    /// `check`, but collecting errors instead of bailing on the
    /// first. We push every error into `out` and keep going so
    /// the user sees the full set of issues per run.
    ///
    /// The recursive structure mirrors `check` — at each
    /// branching node we recurse into the children explicitly
    /// instead of letting `check`'s `?` short-circuit.
    fn check_collect(&mut self, expr: &CoreExpr, expected: &Type, out: &mut Vec<TypeError>) {
        match expr {
            CoreExpr::Lambda { params, body, span } => {
                if let Err(e) = self.check_lambda(params, body, *span, expected) {
                    out.push(e);
                }
            }
            CoreExpr::If {
                cond, then, alt, ..
            } => {
                self.check_collect(cond, &Type::Any, out);
                self.check_collect(then, expected, out);
                self.check_collect(alt, expected, out);
            }
            CoreExpr::Begin { exprs, .. } => {
                if let Some((last, init)) = exprs.split_last() {
                    for e in init {
                        self.check_collect(e, &Type::Any, out);
                    }
                    self.check_collect(last, expected, out);
                }
            }
            CoreExpr::Letrec {
                bindings,
                body,
                span,
            } => {
                // Push a scope, seed each binding's declared
                // type (or Any), check each value against its
                // declared type, then walk the body against
                // `expected`. Errors from each value-check land
                // in `out` so we keep going.
                let declared = letrec_binding_types(self.table, *span, bindings);
                let mark = self.env.push();
                for ((name, _), ty) in bindings.iter().zip(declared.iter()) {
                    self.env.define(*name, ty.clone());
                }
                for ((_, value), ty) in bindings.iter().zip(declared.iter()) {
                    self.check_collect(value, ty, out);
                }
                self.check_collect(body, expected, out);
                self.env.pop_to(mark);
            }
            CoreExpr::Set { name, value, span } => {
                if let Err(e) = self.check_set(*name, value, *span, expected) {
                    out.push(e);
                }
            }
            CoreExpr::App { .. } => {
                // App's arg-checking can produce multiple errors
                // in principle. For 2.5 we use the single-shot
                // `check`; 2.8 may refine this.
                if let Err(e) = self.check(expr, expected) {
                    out.push(e);
                }
            }
            CoreExpr::Const { .. } | CoreExpr::Ref { .. } => {
                if let Err(e) = self.check_via_infer(expr, expected) {
                    out.push(e);
                }
            }
        }
    }

    // -------- per-form helpers --------

    fn check_lambda(
        &mut self,
        params: &Params,
        body: &CoreExpr,
        span: Span,
        expected: &Type,
    ) -> Result<(), TypeError> {
        // Find the annotation (if any) and synthesize a ProcType.
        // No annotation, or `is_annotated() == false`, means all
        // params + return + rest default to Any — the untyped
        // gradual default. We *always* push a scope and recurse
        // into the body so calls inside an untyped lambda still
        // get typechecked.
        let ann = self.table.lambda(span);
        let proc_ty = match ann {
            Some(a) if a.is_annotated() => lambda_proc_type(params, a),
            _ => lambda_proc_type_all_any(params),
        };
        let Type::Procedure_(pt) = proc_ty.clone() else {
            // `lambda_proc_type*` always returns a Procedure_.
            return Ok(());
        };
        // Check the synthesized type against the expected slot
        // (matters when a typed binding ascribes the lambda).
        if !subtype(&proc_ty, expected) {
            return Err(TypeError::Mismatch {
                expected: expected.clone(),
                found: proc_ty,
                span,
            });
        }
        // Push the params into a fresh scope and check the body.
        let mark = self.env.push();
        for (i, pname) in params.fixed.iter().enumerate() {
            let pty = pt.params.get(i).cloned().unwrap_or(Type::Any);
            self.env.define(*pname, pty);
        }
        if let Some(rest_name) = &params.rest {
            let rest_ty = pt.rest.clone().unwrap_or(Type::Any);
            self.env.define(*rest_name, rest_ty);
        }
        let result = self.check(body, &pt.return_type);
        self.env.pop_to(mark);
        result
    }

    fn check_app(
        &mut self,
        func: &CoreExpr,
        args: &[CoreExpr],
        span: Span,
        expected: &Type,
    ) -> Result<(), TypeError> {
        // Special case: `(let ((x e) ...) body)` desugars to
        // `((lambda (x ...) body) e ...)`, so an App whose
        // operator is an immediate Lambda is the let-pattern.
        // We typecheck it directly here — checking args against
        // the lambda's (possibly-annotated) param types, then
        // walking the body against the App's *outer* expected
        // type. This is the only way the body actually gets
        // typechecked, since `infer(Lambda)` returns an opaque
        // Procedure type.
        if let CoreExpr::Lambda {
            params: lparams,
            body: lbody,
            span: lspan,
        } = func
        {
            return self.check_app_lambda(lparams, lbody, *lspan, args, expected);
        }
        let f_ty = self.infer(func);
        match &f_ty {
            Type::Procedure_(pt) => {
                if pt.params.len() != args.len() {
                    return Err(TypeError::ArityMismatch {
                        expected: pt.params.len(),
                        found: args.len(),
                        span,
                    });
                }
                let params = pt.params.clone();
                let return_ty = pt.return_type.clone();
                for (arg, param_ty) in args.iter().zip(params.iter()) {
                    self.check(arg, param_ty)?;
                }
                if !subtype(&return_ty, expected) {
                    return Err(TypeError::Mismatch {
                        expected: expected.clone(),
                        found: return_ty,
                        span,
                    });
                }
                Ok(())
            }
            Type::Procedure | Type::Any => Ok(()),
            other => Err(TypeError::NotAProcedure {
                found: other.clone(),
                span: func.span(),
            }),
        }
    }

    /// Special-case `App(Lambda, args)` — the `let` pattern.
    /// Walks the lambda's body in scope with its params bound
    /// to either their declared types (if annotated) or `Any`.
    /// The body is checked against the App's expected type, NOT
    /// the lambda's declared return type — gradual unannotated
    /// returns shouldn't drop a precise outer constraint.
    fn check_app_lambda(
        &mut self,
        params: &Params,
        body: &CoreExpr,
        lambda_span: Span,
        args: &[CoreExpr],
        expected: &Type,
    ) -> Result<(), TypeError> {
        let ann = self.table.lambda(lambda_span);
        let pt = match ann {
            Some(a) if a.is_annotated() => match lambda_proc_type(params, a) {
                Type::Procedure_(pt) => *pt,
                _ => unreachable!("lambda_proc_type returns Procedure_"),
            },
            _ => match lambda_proc_type_all_any(params) {
                Type::Procedure_(pt) => *pt,
                _ => unreachable!("lambda_proc_type_all_any returns Procedure_"),
            },
        };
        if pt.params.len() != args.len() {
            return Err(TypeError::ArityMismatch {
                expected: pt.params.len(),
                found: args.len(),
                span: lambda_span,
            });
        }
        for (arg, param_ty) in args.iter().zip(pt.params.iter()) {
            self.check(arg, param_ty)?;
        }
        // If the lambda's annotated return type is more precise
        // than `Any`, propagate the *intersection* (effectively
        // the more-precise of the two) into the body check.
        // Phase 2 has no proper meet operator, so we choose:
        // - declared == Any → use outer `expected`
        // - else → use declared (it's the user's promise about
        //   the body), and additionally require declared <:
        //   expected so the wider App context is honored.
        let body_expected = if pt.return_type == Type::Any {
            expected.clone()
        } else {
            if !subtype(&pt.return_type, expected) {
                return Err(TypeError::Mismatch {
                    expected: expected.clone(),
                    found: pt.return_type.clone(),
                    span: lambda_span,
                });
            }
            pt.return_type.clone()
        };
        let mark = self.env.push();
        for (i, pname) in params.fixed.iter().enumerate() {
            let pty = pt.params.get(i).cloned().unwrap_or(Type::Any);
            self.env.define(*pname, pty);
        }
        if let Some(rest_name) = &params.rest {
            let rest_ty = pt.rest.clone().unwrap_or(Type::Any);
            self.env.define(*rest_name, rest_ty);
        }
        let result = self.check(body, &body_expected);
        self.env.pop_to(mark);
        result
    }

    fn check_letrec(
        &mut self,
        bindings: &[(cs_core::Symbol, CoreExpr)],
        body: &CoreExpr,
        span: Span,
        expected: &Type,
    ) -> Result<(), TypeError> {
        // letrec*: every binding is in scope for every value
        // (and the body). Bring the declared types into scope
        // FIRST so recursive references see them, then check
        // each value against its declared type.
        let declared = letrec_binding_types(self.table, span, bindings);
        let mark = self.env.push();
        for ((name, _), ty) in bindings.iter().zip(declared.iter()) {
            self.env.define(*name, ty.clone());
        }
        let mut first_err: Option<TypeError> = None;
        for ((_, value), ty) in bindings.iter().zip(declared.iter()) {
            if let Err(e) = self.check(value, ty) {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        let body_result = self.check(body, expected);
        self.env.pop_to(mark);
        if let Some(e) = first_err {
            return Err(e);
        }
        body_result
    }

    fn check_set(
        &mut self,
        name: cs_core::Symbol,
        value: &CoreExpr,
        _span: Span,
        expected: &Type,
    ) -> Result<(), TypeError> {
        // ONLY user-written ascriptions constrain a Set's value.
        // We deliberately don't consult `self.env` here: that
        // would let primop types and ambient bindings inherited
        // through enclosing scopes hijack `(define foo …)` and
        // demand `foo` match an unrelated signature. The user's
        // intent is "this binding's type is exactly what the
        // matching `(: NAME T)` declared, if any".
        let target = self.table.ascription(name).cloned().unwrap_or(Type::Any);
        self.check(value, &target)?;
        // Set itself returns the unspecified value; treat as Any.
        if subtype(&Type::Any, expected) {
            Ok(())
        } else {
            Err(TypeError::Mismatch {
                expected: expected.clone(),
                found: Type::Any,
                span: value.span(),
            })
        }
    }

    fn check_via_infer(&self, expr: &CoreExpr, expected: &Type) -> Result<(), TypeError> {
        let found = self.infer(expr);
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

/// Look up declared types for each binding of a `Letrec`. If
/// the form has a recorded `LetrecAnnotation`, slot `i` uses
/// `binding_types[i]` (defaulting to `Any` when `None` or the
/// vec is shorter than `bindings`); otherwise every binding
/// defaults to `Any`.
fn letrec_binding_types(
    table: &AnnotationTable,
    span: Span,
    bindings: &[(cs_core::Symbol, CoreExpr)],
) -> Vec<Type> {
    let ann = table.letrec(span);
    bindings
        .iter()
        .enumerate()
        .map(|(i, _)| {
            ann.and_then(|a| a.binding_types.get(i).cloned().flatten())
                .unwrap_or(Type::Any)
        })
        .collect()
}

/// Build a `ProcType` from a `Params` shape with every slot
/// defaulted to `Any`. Used for unannotated lambdas — the
/// checker still walks the body so calls inside an untyped
/// function get typechecked, just with no per-param constraints.
fn lambda_proc_type_all_any(params: &Params) -> Type {
    let param_types: Vec<Type> = vec![Type::Any; params.fixed.len()];
    let rest = if params.rest.is_some() {
        Some(Type::Any)
    } else {
        None
    };
    Type::Procedure_(Box::new(ProcType {
        params: param_types,
        return_type: Type::Any,
        rest,
    }))
}

/// Build a `ProcType` from a `LambdaAnnotation` against a
/// concrete `Params` shape. Missing param annotations default
/// to `Any`; a missing return defaults to `Any` too.
fn lambda_proc_type(params: &Params, ann: &LambdaAnnotation) -> Type {
    let param_types: Vec<Type> = params
        .fixed
        .iter()
        .enumerate()
        .map(|(i, _)| {
            ann.param_types
                .get(i)
                .and_then(|p| p.clone())
                .unwrap_or(Type::Any)
        })
        .collect();
    let return_type = ann.return_type.clone().unwrap_or(Type::Any);
    let rest = if params.rest.is_some() {
        Some(ann.rest_type.clone().unwrap_or(Type::Any))
    } else {
        None
    };
    Type::Procedure_(Box::new(ProcType {
        params: param_types,
        return_type,
        rest,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cs_core::SymbolTable;
    use cs_diag::SourceMap;
    use cs_expand::Expander;
    use cs_parse::read_all;

    use super::*;
    use crate::extract::extract_annotations;

    fn parse_extract_expand(src: &str) -> (CoreExpr, AnnotationTable, SymbolTable) {
        let mut sm = SourceMap::new();
        let f = sm.add("<checker-test>", src);
        let mut syms = SymbolTable::new();
        let data = read_all(f, src, &mut syms).expect("parse");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty(), "annotation diags: {diags:?}");
        let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
        let mut exp = Expander::new(&mut syms, &mut macros);
        let core = exp.expand_program(&stripped).expect("expand");
        drop(exp);
        (core, table, syms)
    }

    #[test]
    fn typed_identity_function_typechecks() {
        let src = "\
            (: id (-> Fixnum Fixnum))
            (define (id [x : Fixnum]) : Fixnum x)
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_fib_typechecks() {
        // Phase 3: generic `+ - <` now return `(U Fixnum Flonum)`,
        // which doesn't subtype Fixnum. A Fixnum-precise fib uses
        // the fx*-family primops.
        let src = "\
            (: fib (-> Fixnum Fixnum))
            (define (fib [n : Fixnum]) : Fixnum
              (if (fx<? n 2) n (fx+ (fib (fx- n 1)) (fib (fx- n 2)))))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn fib_body_returning_wrong_type_is_caught() {
        // Body calls `string-length` on a Fixnum — should fail
        // with arg-type mismatch (`string-length` expects String).
        let src = "\
            (: fib (-> Fixnum Fixnum))
            (define (fib [n : Fixnum]) : Fixnum (string-length n))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(!errors.is_empty(), "expected at least one TypeError");
        let found_mismatch = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::String,
                    found: Type::Fixnum,
                    ..
                }
            )
        });
        assert!(
            found_mismatch,
            "expected a String/Fixnum mismatch on string-length arg, got: {errors:?}"
        );
    }

    #[test]
    fn return_type_mismatch_is_caught() {
        // Returns a String from a Fixnum-returning function.
        let src = "\
            (: oops (-> Fixnum Fixnum))
            (define (oops [n : Fixnum]) : Fixnum \"not-a-fixnum\")
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(!errors.is_empty(), "expected return-type error");
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::Fixnum,
                    found: Type::String,
                    ..
                }
            )
        });
        assert!(
            found,
            "expected Fixnum/String return mismatch, got: {errors:?}"
        );
    }

    #[test]
    fn untyped_define_typechecks_against_anything() {
        let src = "(define (square x) (* x x))";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_function_callable_from_untyped() {
        // The untyped `helper` calls the typed `inc`. Since
        // `helper`'s args are Any (no annotation), Any flows
        // into inc's Fixnum-typed param via the gradual rule.
        // `inc`'s body uses `fx+` to stay Fixnum-precise under
        // Phase 3's union-widened generic arithmetic.
        let src = "\
            (: inc (-> Fixnum Fixnum))
            (define (inc [n : Fixnum]) : Fixnum (fx+ n 1))
            (define (helper x) (inc x))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_call_with_wrong_arg_type_is_caught() {
        // `inc` expects Fixnum but is called with a String literal.
        let src = "\
            (: inc (-> Fixnum Fixnum))
            (define (inc [n : Fixnum]) : Fixnum (fx+ n 1))
            (define (broken) (inc \"hi\"))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(!errors.is_empty(), "expected mismatch on inc arg");
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::Fixnum,
                    found: Type::String,
                    ..
                }
            )
        });
        assert!(found, "expected Fixnum/String mismatch, got: {errors:?}");
    }

    #[test]
    fn lambda_proc_type_seeds_correctly() {
        // Just unit-test the helper directly.
        use cs_core::Symbol;
        use cs_ir::Params;
        let params = Params::fixed(vec![Symbol(1), Symbol(2)]);
        let ann = LambdaAnnotation {
            param_types: vec![Some(Type::Fixnum), Some(Type::String)],
            return_type: Some(Type::Boolean),
            rest_type: None,
        };
        let ty = lambda_proc_type(&params, &ann);
        match ty {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![Type::Fixnum, Type::String]);
                assert_eq!(pt.return_type, Type::Boolean);
                assert!(pt.rest.is_none());
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    // -------- Letrec (iter 2.6) --------

    #[test]
    fn letrec_with_mutual_recursion_typechecks() {
        // `let` desugars to `letrec` via the expander; both
        // bindings see each other in scope.
        let src = "\
            (let ((x 1) (y 2)) (+ x y))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn letrec_body_mismatch_is_caught() {
        // Body returns a String but the surrounding define
        // ascribes it to Fixnum.
        let src = "\
            (: bad (-> Fixnum))
            (define (bad) : Fixnum (let ((x 1)) \"oops\"))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(!errors.is_empty(), "expected mismatch");
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::Fixnum,
                    found: Type::String,
                    ..
                }
            )
        });
        assert!(
            found,
            "expected Fixnum/String body mismatch in let, got: {errors:?}"
        );
    }

    #[test]
    fn letrec_recursive_reference_resolves_via_binding_type() {
        // Use a typed top-level so the inner reference to
        // itself sees the right type. (Letrec sugar via `let`
        // doesn't expose typed-binding syntax yet — the
        // surface for that is a Phase 2/3 follow-up — but the
        // structural test confirms the env scoping is right.)
        // Body uses `fx+` for Fixnum-precise arithmetic.
        let src = "\
            (: f (-> Fixnum Fixnum))
            (define (f [n : Fixnum]) : Fixnum
              (let ((r n)) (fx+ r 1)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    // -------- Phase 3 iter 3.3: function-type checking --------

    #[test]
    fn function_typed_param_typechecks() {
        // `g` takes a procedure-typed param and calls it.
        let src = "\
            (: g (-> (-> Fixnum Fixnum) Fixnum))
            (define (g [f : (-> Fixnum Fixnum)]) : Fixnum (f 5))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn calling_function_typed_param_with_non_proc_fails() {
        // `g` expects a (-> Fixnum Fixnum), caller passes 42
        // — should surface a Mismatch with `found: Fixnum`.
        let src = "\
            (: g (-> (-> Fixnum Fixnum) Fixnum))
            (define (g [f : (-> Fixnum Fixnum)]) : Fixnum (f 5))
            (define (caller) (g 42))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    found: Type::Fixnum,
                    ..
                }
            )
        });
        assert!(
            found,
            "expected Fixnum/procedure mismatch on g's arg, got: {errors:?}"
        );
    }

    #[test]
    fn function_typed_param_callsite_wrong_arg_fails() {
        // Inside g, calling f with a String fails — f expects
        // Fixnum.
        let src = "\
            (: g (-> (-> Fixnum Fixnum) Fixnum))
            (define (g [f : (-> Fixnum Fixnum)]) : Fixnum (f \"hi\"))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::Fixnum,
                    found: Type::String,
                    ..
                }
            )
        });
        assert!(
            found,
            "expected Fixnum/String mismatch on f's arg, got: {errors:?}"
        );
    }

    #[test]
    fn function_typed_param_arity_mismatch_at_call_fails() {
        // g calls f with 2 args; f is declared 1-ary. Should
        // surface ArityMismatch.
        let src = "\
            (: g (-> (-> Fixnum Fixnum) Fixnum))
            (define (g [f : (-> Fixnum Fixnum)]) : Fixnum (f 1 2))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::ArityMismatch {
                    expected: 1,
                    found: 2,
                    ..
                }
            )
        });
        assert!(found, "expected ArityMismatch 1/2, got: {errors:?}");
    }

    #[test]
    fn higher_order_application_chains() {
        // h returns a (-> Fixnum Fixnum), and the result is
        // immediately applied. Tests that App-on-App correctly
        // typechecks through the function-valued return.
        let src = "\
            (: f (-> Fixnum Fixnum))
            (define (f [n : Fixnum]) : Fixnum (fx+ n 1))
            (: h (-> (-> Fixnum Fixnum)))
            (define (h) : (-> Fixnum Fixnum) f)
            (: top (-> Fixnum))
            (define (top) : Fixnum ((h) 10))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn let_pattern_walks_body_into_outer_expected_type() {
        // `(let ((x 1)) "oops")` lives inside a Fixnum-returning
        // function. The let-pattern path walks the body against
        // the OUTER expected type (Fixnum), so the String body
        // surfaces a Fixnum/String mismatch. Without the
        // App-on-Lambda special case, this would silently pass
        // because `infer(Lambda) = Procedure` and check_app falls
        // through to the permissive branch.
        let src = "\
            (: bad (-> Fixnum))
            (define (bad) : Fixnum (let ((x 1)) \"oops\"))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::Mismatch {
                    expected: Type::Fixnum,
                    found: Type::String,
                    ..
                }
            )
        });
        assert!(
            found,
            "expected Fixnum/String mismatch from let body, got: {errors:?}"
        );
    }
}
