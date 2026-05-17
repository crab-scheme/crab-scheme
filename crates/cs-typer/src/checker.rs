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
use crate::check::{narrow_negative, narrow_positive, subtype, TypeError};
use crate::env::TypeEnv;
use crate::infer::infer;
use crate::poly::{subst, unify};
use crate::types::{ProcType, Type};

/// Narrowing decision produced by
/// [`Checker::detect_predicate_narrowing`].
///
/// `x` is the operand whose type will be narrowed in each `If`
/// branch. `filter` is the predicate's positive proposition.
/// `negated` is set when the cond was wrapped in `(not …)` — at
/// the call site, swap the positive/negative narrowings.
#[derive(Clone, Debug)]
struct Narrowing {
    x: cs_core::Symbol,
    filter: Type,
    negated: bool,
}

/// The state a single typechecking run accumulates.
///
/// `'tab` is the lifetime of the borrowed `AnnotationTable` —
/// the Checker doesn't own it because the caller (driver /
/// LSP) holds onto the same table for downstream consumers
/// (JIT param hints, error rendering, hovers).
pub struct Checker<'tab> {
    pub table: &'tab AnnotationTable,
    pub env: TypeEnv,
    /// Per-binding param hints inferred from `Letrec` body call
    /// sites (Phase 5+ extension). When a `Letrec` binds a
    /// lambda `NAME = (lambda (p…) …)` and the body
    /// immediately calls `(NAME arg…)`, the arg types become
    /// hints for the lambda's params. Consumers (cs-cli's
    /// `aot --multi`, the JIT) merge this with
    /// `hints_by_name(table)` to specialize inner-let inner
    /// loops without surface annotation. Empty by default.
    pub inferred_param_hints:
        std::cell::RefCell<std::collections::HashMap<cs_core::Symbol, Vec<cs_rir::Type>>>,
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
        Self {
            table,
            env,
            inferred_param_hints: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
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
            CoreExpr::If {
                cond, then, alt, ..
            } => self.check_if(cond, then, alt, expected),
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
                // Phase 4 iter 4.2: narrow operand types inside
                // each branch based on a predicate filter.
                let narrowing = self.detect_predicate_narrowing(cond);
                if let Some(n) = narrowing {
                    let x_ty = self.env.lookup(n.x).cloned().unwrap_or(Type::Any);
                    let (then_ty, alt_ty) = if n.negated {
                        (
                            narrow_negative(&x_ty, &n.filter),
                            narrow_positive(&x_ty, &n.filter),
                        )
                    } else {
                        (
                            narrow_positive(&x_ty, &n.filter),
                            narrow_negative(&x_ty, &n.filter),
                        )
                    };
                    let mark = self.env.push();
                    self.env.define(n.x, then_ty);
                    self.check_collect(then, expected, out);
                    self.env.pop_to(mark);
                    let mark = self.env.push();
                    self.env.define(n.x, alt_ty);
                    self.check_collect(alt, expected, out);
                    self.env.pop_to(mark);
                } else {
                    self.check_collect(then, expected, out);
                    self.check_collect(alt, expected, out);
                }
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
                let mut declared = letrec_binding_types(self.table, *span, bindings);
                self.refine_letrec_via_body_call(bindings, body, &mut declared);
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
            // Inside the body, the rest binding sees `(Listof T)`
            // — the user's `T` is the per-element type, not the
            // list type. Phase 3.4.
            let rest_elem = pt.rest.clone().unwrap_or(Type::Any);
            self.env
                .define(*rest_name, Type::Listof(Box::new(rest_elem)));
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
        // Phase 7.4: if the operator's type is polymorphic,
        // monomorphize it against the inferred arg types
        // first. The resulting Procedure_ replaces f_ty for
        // the remainder of this fn.
        let f_ty = self.monomorphize_for_call(&f_ty, args);
        match &f_ty {
            Type::Procedure_(pt) => {
                // Arity: with no rest, must match exactly;
                // with rest, args.len() ≥ fixed.len().
                let fixed = pt.params.len();
                let has_rest = pt.rest.is_some();
                if (has_rest && args.len() < fixed) || (!has_rest && args.len() != fixed) {
                    return Err(TypeError::ArityMismatch {
                        expected: fixed,
                        found: args.len(),
                        span,
                    });
                }
                let params = pt.params.clone();
                let rest = pt.rest.clone();
                let return_ty = pt.return_type.clone();
                for (i, arg) in args.iter().enumerate() {
                    let param_ty = if i < fixed {
                        &params[i]
                    } else {
                        rest.as_ref().expect("has_rest holds when i >= fixed")
                    };
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

    /// Phase 7.4: implicit instantiation of a polymorphic
    /// callee at a call site.
    ///
    /// When `f_ty` is `Forall(vs, Procedure_(pt))` and the
    /// fixed arity matches `args.len()`, we infer each arg's
    /// type, unify it against the corresponding `pt.params[i]`
    /// (treating `vs` as the solvable variable set), accumulate
    /// the bindings, and substitute through the procedure body
    /// to produce a monomorphic version. Conflicts or shape
    /// mismatches fall back to returning `f_ty` unchanged so
    /// downstream error handling stays consistent.
    fn monomorphize_for_call(&self, f_ty: &Type, args: &[CoreExpr]) -> Type {
        let Type::Forall(vs, body) = f_ty else {
            return f_ty.clone();
        };
        let Type::Procedure_(pt) = body.as_ref() else {
            return f_ty.clone();
        };
        let fixed = pt.params.len();
        let has_rest = pt.rest.is_some();
        if (has_rest && args.len() < fixed) || (!has_rest && args.len() != fixed) {
            return f_ty.clone();
        }
        // Accumulate var bindings across all (param, arg)
        // unifications. Reject on conflict.
        let mut combined: std::collections::HashMap<cs_core::Symbol, Type> =
            std::collections::HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let param_ty = if i < fixed {
                &pt.params[i]
            } else {
                pt.rest.as_ref().expect("has_rest holds when i >= fixed")
            };
            let arg_ty = self.infer(arg);
            let Some(m) = unify(param_ty, &arg_ty, vs) else {
                return f_ty.clone();
            };
            for (k, v) in m {
                match combined.get(&k) {
                    None => {
                        combined.insert(k, v);
                    }
                    Some(existing) => {
                        if existing != &v && existing != &Type::Any && v != Type::Any {
                            return f_ty.clone();
                        }
                        if existing == &Type::Any {
                            combined.insert(k, v);
                        }
                    }
                }
            }
        }
        // Fill any unconstrained vars with Any so substitution
        // produces a fully-monomorphic Procedure_ (no lingering
        // Var references in the return type).
        for v in vs {
            combined.entry(*v).or_insert(Type::Any);
        }
        subst(body, &combined)
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
            // Iter 4.5: per-binding refinement. When the param's
            // declared type is `Any` (unannotated lambda — the
            // typical `let` desugaring path), use the arg's
            // inferred type instead. This is what makes
            // `(let ((x (if cond 1 "hi"))) …)` see x as
            // `(U Fixnum String)` in the body rather than `Any`.
            // For explicitly-typed params (declared != Any), the
            // declared type is the user's promise — keep it.
            let declared = pt.params.get(i).cloned().unwrap_or(Type::Any);
            let bound_ty = if declared == Type::Any {
                args.get(i).map(|a| self.infer(a)).unwrap_or(Type::Any)
            } else {
                declared
            };
            self.env.define(*pname, bound_ty);
        }
        if let Some(rest_name) = &params.rest {
            let rest_elem = pt.rest.clone().unwrap_or(Type::Any);
            self.env
                .define(*rest_name, Type::Listof(Box::new(rest_elem)));
        }
        let result = self.check(body, &body_expected);
        self.env.pop_to(mark);
        result
    }

    /// `check` for `If` — narrows operand types in branches
    /// based on a predicate filter (Phase 4 iter 4.2).
    ///
    /// Also typechecks the condition expression against `Any`
    /// (every Scheme value is a valid condition — only `#f` is
    /// falsy). Without this step, malformed conditions like
    /// `(if (car not-a-pair) …)` would silently pass.
    fn check_if(
        &mut self,
        cond: &CoreExpr,
        then: &CoreExpr,
        alt: &CoreExpr,
        expected: &Type,
    ) -> Result<(), TypeError> {
        self.check(cond, &Type::Any)?;
        let narrowing = self.detect_predicate_narrowing(cond);
        if let Some(n) = narrowing {
            let x_ty = self.env.lookup(n.x).cloned().unwrap_or(Type::Any);
            // For `(not …)` the polarity flips: then-branch
            // sees the negative, else-branch sees the positive.
            let (then_ty, alt_ty) = if n.negated {
                (
                    narrow_negative(&x_ty, &n.filter),
                    narrow_positive(&x_ty, &n.filter),
                )
            } else {
                (
                    narrow_positive(&x_ty, &n.filter),
                    narrow_negative(&x_ty, &n.filter),
                )
            };
            let mark = self.env.push();
            self.env.define(n.x, then_ty);
            let then_res = self.check(then, expected);
            self.env.pop_to(mark);
            let mark = self.env.push();
            self.env.define(n.x, alt_ty);
            let alt_res = self.check(alt, expected);
            self.env.pop_to(mark);
            then_res?;
            alt_res
        } else {
            self.check(then, expected)?;
            self.check(alt, expected)
        }
    }

    /// If `cond` is `(pred x)` where `pred` is a Ref to a
    /// predicate-typed procedure (filter present) and `x` is a
    /// Ref to a name in scope, return `(x, filter_type)` so the
    /// caller can narrow `x` in the then/else branches.
    fn detect_predicate_narrowing(&self, cond: &CoreExpr) -> Option<Narrowing> {
        // (not <inner>) flips polarity and recurses.
        if let Some(inner) = self.match_not_app(cond) {
            let inner_narrow = self.detect_predicate_narrowing(inner)?;
            return Some(Narrowing {
                x: inner_narrow.x,
                filter: inner_narrow.filter,
                negated: !inner_narrow.negated,
            });
        }
        let CoreExpr::App { func, args, .. } = cond else {
            return None;
        };
        if args.len() != 1 {
            return None;
        }
        let CoreExpr::Ref {
            name: pred_name, ..
        } = &**func
        else {
            return None;
        };
        let CoreExpr::Ref { name: x, .. } = &args[0] else {
            return None;
        };
        let pred_ty = self.env.lookup(*pred_name)?;
        let Type::Procedure_(pt) = pred_ty else {
            return None;
        };
        let filter = pt.filter.as_ref()?.clone();
        Some(Narrowing {
            x: *x,
            filter,
            negated: false,
        })
    }

    /// If `expr` is `(not e)`, return `e`. We identify `not` by
    /// its shape in the env — `(-> Any Boolean)` with no
    /// `filter`. Practical heuristic since the Checker doesn't
    /// own a SymbolTable for name-based lookup. False positives
    /// (some other 1-arg `(-> Any Boolean)` without filter)
    /// only invert narrowing, which is harmless to typecheck.
    fn match_not_app<'a>(&self, expr: &'a CoreExpr) -> Option<&'a CoreExpr> {
        let CoreExpr::App { func, args, .. } = expr else {
            return None;
        };
        if args.len() != 1 {
            return None;
        }
        let CoreExpr::Ref { name, .. } = &**func else {
            return None;
        };
        let pred_ty = self.env.lookup(*name)?;
        let Type::Procedure_(pt) = pred_ty else {
            return None;
        };
        if pt.params.len() != 1
            || pt.params[0] != Type::Any
            || pt.return_type != Type::Boolean
            || pt.filter.is_some()
        {
            return None;
        }
        Some(&args[0])
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
        //
        // Phase 5+ refinement: when a binding's value is an
        // unannotated Lambda and the body's first expression
        // is a direct App to that binding, infer the lambda's
        // param types from the App's arg types. This is what
        // named-let bodies (`(let loop ((zr 0.0) …) (loop …))`)
        // produce — the first call carries the initial values,
        // which are the only call-site types AOT needs to
        // specialize against. Record the inferred hints into
        // `inferred_param_hints` and also use them as the
        // binding's type so the lambda body sees its params
        // typed instead of Any.
        let mut declared = letrec_binding_types(self.table, span, bindings);
        self.refine_letrec_via_body_call(bindings, body, &mut declared);
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

    /// Per-binding refinement for `Letrec` bindings: if the
    /// body is `(BindingName arg…)`, replace the binding's
    /// declared type with a `Procedure_` inferred from the
    /// arg types. Also stash the lowered hints in
    /// `inferred_param_hints` so downstream consumers (AOT)
    /// pick them up.
    ///
    /// Only applies when:
    /// - the binding value is a `Lambda` whose fixed-param
    ///   count matches the App's arg count;
    /// - the binding's declared type was `Any` (no user
    ///   annotation to honor);
    /// - the body's outermost expression is `App(Ref(name),
    ///   args)` where `name` matches the binding.
    ///
    /// Conservative: ignores nested or multi-step bodies.
    /// Suffices for the named-let desugaring pattern, which is
    /// the dominant case that produces unannotated inner
    /// lambdas in typed code.
    fn refine_letrec_via_body_call(
        &self,
        bindings: &[(cs_core::Symbol, CoreExpr)],
        body: &CoreExpr,
        declared: &mut [Type],
    ) {
        let CoreExpr::App { func, args, .. } = body else {
            return;
        };
        let CoreExpr::Ref {
            name: called_name, ..
        } = &**func
        else {
            return;
        };
        for (i, (binding_name, value)) in bindings.iter().enumerate() {
            if binding_name != called_name {
                continue;
            }
            if declared[i] != Type::Any {
                continue;
            }
            let CoreExpr::Lambda {
                params: lparams, ..
            } = value
            else {
                continue;
            };
            if lparams.fixed.len() != args.len() || lparams.rest.is_some() {
                continue;
            }
            // Infer each arg's type from the body's call site
            // and build a Procedure_ with those as params.
            let arg_types: Vec<Type> = args.iter().map(|a| self.infer(a)).collect();
            // Record AOT-side hints by name.
            let hints: Vec<cs_rir::Type> = arg_types.iter().map(crate::rir_bridge::lower).collect();
            self.inferred_param_hints
                .borrow_mut()
                .insert(*binding_name, hints);
            // Use the inferred ProcType as the binding's type
            // so the lambda body checks under the refined
            // env. Return type stays Any (we don't infer it
            // from the body of the call — recursive calls
            // would loop) and the JIT/AOT pipelines only care
            // about params anyway.
            declared[i] = Type::Procedure_(Box::new(ProcType {
                params: arg_types,
                return_type: Type::Any,
                rest: None,
                filter: None,
            }));
        }
    }

    /// Phase 5+ extension: name-keyed hint map populated by
    /// `Letrec` body-driven inference. Merge with
    /// `cs_typer::hints_by_name(table)` for the full AOT hint
    /// table; callers handle the union (later entries from this
    /// map override the by-name table when both have the same
    /// key, but inner-let names won't collide with top-level
    /// ascriptions in practice).
    pub fn inferred_hints_by_name(
        &self,
    ) -> std::collections::HashMap<cs_core::Symbol, Vec<cs_rir::Type>> {
        self.inferred_param_hints.borrow().clone()
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
        filter: None,
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
        filter: None,
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

    // -------- Phase 5+ inner-let inference --------

    #[test]
    fn named_let_body_call_seeds_inferred_hints() {
        // The classic named-let pattern desugars to a Letrec
        // whose body is `(loop initial-values)`. The Checker
        // infers loop's params from those initial values and
        // records hints by name.
        let src = "\
            (: top (-> Fixnum))
            (define (top) : Fixnum
              (let loop ((zr 0.0) (zi 0.0) (i 0))
                (if (fx>? i 50) 0 (loop zr zi (fx+ i 1)))))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let _ = checker.check_program(&core);
        let hints = checker.inferred_hints_by_name();
        let loop_sym = syms.intern("loop");
        let v = hints.get(&loop_sym).expect("loop's hints recorded");
        assert_eq!(
            v,
            &vec![
                cs_rir::Type::Flonum,
                cs_rir::Type::Flonum,
                cs_rir::Type::Fixnum
            ],
            "loop should get Flonum/Flonum/Fixnum from initial body call"
        );
    }

    #[test]
    fn named_let_unannotated_top_level_still_gets_hints() {
        // No outer annotation; the loop's hints come purely
        // from the body's call. Confirms the Checker walks
        // even when nothing about `top` is typed.
        let src = "\
            (define (top)
              (let loop ((x 1.5) (n 0))
                (if (fx>? n 10) x (loop x (fx+ n 1)))))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let _ = checker.check_program(&core);
        let hints = checker.inferred_hints_by_name();
        let loop_sym = syms.intern("loop");
        let v = hints.get(&loop_sym).expect("loop's hints recorded");
        assert_eq!(v, &vec![cs_rir::Type::Flonum, cs_rir::Type::Fixnum]);
    }

    #[test]
    fn letrec_with_no_body_call_yields_no_inferred_hints() {
        // Body doesn't immediately call a binding — no
        // inference should fire. The `let` here desugars to
        // App-on-Lambda, not Letrec, so nothing populates the
        // named map either.
        let src = "\
            (define (top)
              (let ((x 1.5) (n 0))
                (fx+ n 1)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let _ = checker.check_program(&core);
        assert!(checker.inferred_hints_by_name().is_empty());
    }

    // -------- Phase 7: polymorphism --------

    #[test]
    fn polymorphic_identity_call_instantiates_to_arg_type() {
        // Seed a polymorphic `id` directly into the
        // Checker's env (the typed-define surface for
        // polymorphic params is post-Phase-7 work, but the
        // call-site mechanics are what iter 7.4 delivers).
        let src = "(id 5)";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let id_sym = syms.intern("id");
        let tvar = cs_core::Symbol(0x9000_0001);
        let id_ty = Type::Forall(
            vec![tvar],
            Box::new(Type::Procedure_(Box::new(ProcType {
                params: vec![Type::Var(tvar)],
                return_type: Type::Var(tvar),
                rest: None,
                filter: None,
            }))),
        );
        checker.env.define_top_level(id_sym, id_ty);
        // The App's inferred return type should be Fixnum
        // (the arg's type) after instantiation.
        assert_eq!(checker.infer(&core), Type::Fixnum);
        // And it should typecheck against any reasonable
        // expectation.
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn polymorphic_identity_call_with_string_instantiates_to_string() {
        let src = "(id \"hi\")";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let id_sym = syms.intern("id");
        let tvar = cs_core::Symbol(0x9000_0002);
        let id_ty = Type::Forall(
            vec![tvar],
            Box::new(Type::Procedure_(Box::new(ProcType {
                params: vec![Type::Var(tvar)],
                return_type: Type::Var(tvar),
                rest: None,
                filter: None,
            }))),
        );
        checker.env.define_top_level(id_sym, id_ty);
        assert_eq!(checker.infer(&core), Type::String);
    }

    #[test]
    fn polymorphic_pair_returns_correct_member_type() {
        // pick-first : (All (A B) (-> A B A))
        let src = "(pick-first 5 \"hi\")";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let pick_sym = syms.intern("pick-first");
        let a = cs_core::Symbol(0x9000_0010);
        let b = cs_core::Symbol(0x9000_0011);
        let pick_ty = Type::Forall(
            vec![a, b],
            Box::new(Type::Procedure_(Box::new(ProcType {
                params: vec![Type::Var(a), Type::Var(b)],
                return_type: Type::Var(a),
                rest: None,
                filter: None,
            }))),
        );
        checker.env.define_top_level(pick_sym, pick_ty);
        assert_eq!(checker.infer(&core), Type::Fixnum);
    }

    #[test]
    fn unannotated_lambda_satisfies_forall_ascription() {
        // The gradual subtype rule: an Any→Any lambda
        // satisfies a polymorphic ascription. This is what
        // makes `(: id (All (T) (-> T T)))` + `(define (id x) x)`
        // typecheck cleanly even though the define has no
        // per-param annotations.
        use crate::check::subtype;
        let tvar = cs_core::Symbol(0x9000_0020);
        let mono = Type::Procedure_(Box::new(ProcType {
            params: vec![Type::Any],
            return_type: Type::Any,
            rest: None,
            filter: None,
        }));
        let poly = Type::Forall(
            vec![tvar],
            Box::new(Type::Procedure_(Box::new(ProcType {
                params: vec![Type::Var(tvar)],
                return_type: Type::Var(tvar),
                rest: None,
                filter: None,
            }))),
        );
        assert!(subtype(&mono, &poly));
        assert!(subtype(&poly, &mono));
    }

    // -------- Phase 4 iter 4.2: branch narrowing --------

    #[test]
    fn null_check_narrows_else_branch_to_pair() {
        // `lst : (U Pair Null)`; `(if (null? lst) … (car lst))`
        // — in the else-branch `lst` is narrowed to Pair so
        // `(car lst)` typechecks.
        let src = "\
            (define-type PairOrNull (U Pair Null))
            (: head (-> PairOrNull Any))
            (define (head [lst : PairOrNull]) : Any
              (if (null? lst) #f (car lst)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn without_narrowing_pair_or_null_caller_to_car_fails() {
        // Sanity: without the `null?` guard, `(car lst)` on a
        // `(U Pair Null)` would fail because Null isn't a Pair.
        let src = "\
            (define-type PairOrNull (U Pair Null))
            (: head (-> PairOrNull Any))
            (define (head [lst : PairOrNull]) : Any (car lst))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors
            .iter()
            .any(|e| matches!(e, TypeError::Mismatch { .. }));
        assert!(
            found,
            "expected Mismatch without narrowing, got: {errors:?}"
        );
    }

    #[test]
    fn string_check_narrows_then_branch() {
        // x : (U Fixnum String); in `(if (string? x) (string-length x) 0)`
        // the then-branch narrows x to String so `(string-length x)` typechecks.
        let src = "\
            (define-type FxOrStr (U Fixnum String))
            (: len (-> FxOrStr Fixnum))
            (define (len [x : FxOrStr]) : Fixnum
              (if (string? x) (string-length x) 0))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn string_check_negative_narrows_else_to_other() {
        // Conversely: `(if (string? x) ??? (string-length x))`
        // — the else-branch narrows x to NOT-String → Fixnum;
        // string-length on a Fixnum fails.
        let src = "\
            (define-type FxOrStr (U Fixnum String))
            (: bad (-> FxOrStr Fixnum))
            (define (bad [x : FxOrStr]) : Fixnum
              (if (string? x) 0 (string-length x)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors.iter().any(|e| {
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
            found,
            "expected String/Fixnum mismatch in else-branch, got: {errors:?}"
        );
    }

    #[test]
    fn fixnum_check_narrows_in_arithmetic() {
        // x : Any; `(if (fixnum? x) (fx+ x 1) 0)` — fx+ requires
        // Fixnum, and `fixnum?` narrows x to Fixnum in then.
        let src = "\
            (: inc (-> Any Fixnum))
            (define (inc [x : Any]) : Fixnum
              (if (fixnum? x) (fx+ x 1) 0))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    // -------- Phase 4 iter 4.3: not / and / or --------

    #[test]
    fn not_inverts_narrowing_polarity() {
        // `(if (not (null? lst)) (car lst) #f)` — `not` flips:
        // then-branch now sees `lst : Pair` (the negative
        // proposition of null?), so `(car lst)` typechecks.
        let src = "\
            (: head (-> (U Pair Null) Any))
            (define (head [lst : (U Pair Null)]) : Any
              (if (not (null? lst)) (car lst) #f))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn not_else_branch_sees_positive() {
        // Symmetric: `(if (not (null? lst)) #t (car lst))` —
        // else-branch sees the positive (Null), so `(car lst)`
        // should fail.
        let src = "\
            (: head (-> (U Pair Null) Any))
            (define (head [lst : (U Pair Null)]) : Any
              (if (not (null? lst)) #t (car lst)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let found = errors
            .iter()
            .any(|e| matches!(e, TypeError::Mismatch { .. }));
        assert!(found, "expected Mismatch in else, got: {errors:?}");
    }

    #[test]
    fn double_not_re_narrows_to_positive() {
        // `(if (not (not (null? lst))) #f (car lst))` — two
        // `not`s cancel; lst is Null in then, Pair in else, so
        // `(car lst)` in else typechecks.
        let src = "\
            (: head (-> (U Pair Null) Any))
            (define (head [lst : (U Pair Null)]) : Any
              (if (not (not (null? lst))) #f (car lst)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn and_intersects_narrowings_via_desugaring() {
        // `(and (pair? lst) (number? (car lst)))` desugars to
        // `(if (pair? lst) (if (number? (car lst)) #t #f) #f)`.
        // The outer If's then-branch narrows lst to Pair, so
        // `(car lst)` typechecks; then the inner number? check
        // proceeds. No standalone "and intersects" logic
        // needed — narrowing composes via nested Ifs.
        let src = "\
            (: pair-head-number (-> (U Pair Null) Boolean))
            (define (pair-head-number [lst : (U Pair Null)]) : Boolean
              (and (pair? lst) (number? (car lst))))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn or_unions_via_desugaring() {
        // `(or (null? lst) (pair? lst))` — desugars to nested
        // If. With `lst : (U Pair Null)` both branches succeed.
        let src = "\
            (: nonempty (-> (U Pair Null) Boolean))
            (define (nonempty [lst : (U Pair Null)]) : Boolean
              (or (null? lst) (pair? lst)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    // -------- Phase 4 iter 4.5: per-binding refinement --------

    #[test]
    fn let_binding_inherits_inferred_arg_type() {
        // `(let ((x (string-length "hi"))) ...)` — x should be
        // inferred as Fixnum (string-length's return), so the
        // body can use it where Fixnum is expected.
        let src = "\
            (: f (-> Fixnum))
            (define (f) : Fixnum
              (let ((x (string-length \"hi\")))
                (fx+ x 1)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn let_binding_picks_up_lub_from_if_arg() {
        // `(let ((x (if cond 1 "hi"))) ...)` — x's inferred
        // type is `(U Fixnum String)`. In the body, calling
        // `(string-length x)` should fail because x isn't
        // narrowed to String.
        let src = "\
            (: f (-> Boolean Any))
            (define (f [cond : Boolean]) : Any
              (let ((x (if cond 1 \"hi\")))
                (string-length x)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let mismatch = errors
            .iter()
            .any(|e| matches!(e, TypeError::Mismatch { .. }));
        assert!(
            mismatch,
            "expected Mismatch for string-length on (U Fixnum String), got: {errors:?}"
        );
    }

    #[test]
    fn let_binding_narrowed_via_predicate_in_body() {
        // Combines iter 4.5 (inherited binding type) with iter
        // 4.2 (predicate narrowing). The let-bound `x` gets the
        // LUB; then `(if (string? x) (string-length x) 0)`
        // narrows to String in the then-branch.
        let src = "\
            (: f (-> Boolean Fixnum))
            (define (f [cond : Boolean]) : Fixnum
              (let ((x (if cond 1 \"hi\")))
                (if (string? x) (string-length x) 0)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_let_binding_keeps_declared_type() {
        // When the lambda param has an explicit type (via
        // [x : T] sugar — though `let` doesn't currently have
        // typed-binding syntax, this exercises the underlying
        // "declared != Any → keep declared" path via
        // top-level ascription).
        //
        // `(: g (-> Fixnum Any))` declares g takes Fixnum.
        // Inside g, the param x is Fixnum (declared); calling
        // string-length on it must fail.
        let src = "\
            (: g (-> Fixnum Any))
            (define (g [x : Fixnum]) : Any (string-length x))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let mismatch = errors.iter().any(|e| {
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
            mismatch,
            "expected String/Fixnum mismatch (declared type wins), got: {errors:?}"
        );
    }

    // -------- Phase 4 iter 4.4: union refinement --------

    #[test]
    fn three_way_union_narrows_to_each_member() {
        // x : (U Fixnum String Symbol). Predicate-guarded
        // calls in three different branches each operate on
        // the narrowed type.
        let src = "\
            (define-type Three (U Fixnum String Symbol))
            (: classify (-> Three Fixnum))
            (define (classify [x : Three]) : Fixnum
              (if (string? x)
                  (string-length x)
                  (if (fixnum? x)
                      x
                      0)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn when_form_narrows_body() {
        // `(when (string? x) (string-length x))` — when desugars
        // to `(if cond body unspecified)`. The body sees the
        // positive proposition.
        let src = "\
            (define-type FxOrStr (U Fixnum String))
            (: len-if-str (-> FxOrStr Any))
            (define (len-if-str [x : FxOrStr]) : Any
              (when (string? x) (string-length x)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn unless_form_narrows_to_negative() {
        // `(unless (null? lst) (car lst))` — unless inverts;
        // body sees the negative narrowing (lst : Pair).
        let src = "\
            (: maybe-head (-> (U Pair Null) Any))
            (define (maybe-head [lst : (U Pair Null)]) : Any
              (unless (null? lst) (car lst)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn narrowing_disjoint_filter_yields_never_branch() {
        // x : Fixnum; `(if (string? x) … …)` — the filter
        // (String) is disjoint from x's type (Fixnum), so the
        // then-branch sees `Never`. The body should still
        // typecheck against expected (Never subtypes everything),
        // and ANY use of x in that branch type-passes since
        // Never <: anything.
        let src = "\
            (: f (-> Fixnum Fixnum))
            (define (f [x : Fixnum]) : Fixnum
              (if (string? x) (string-length x) x))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        // The then-branch is dead (Never), but the rule
        // `Never <: T` lets x flow through string-length's
        // String param vacuously. So this should pass.
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn narrowing_only_fires_when_arg_is_a_ref() {
        // Narrowing requires `(pred Ref(x))`. A complex
        // expression as the arg shouldn't narrow anything (and
        // shouldn't error either — gradual fallback).
        let src = "\
            (: head (-> (U Pair Null) Any))
            (define (head [lst : (U Pair Null)]) : Any
              (if (null? (car lst)) #f #t))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        // (car lst) fails because lst isn't narrowed to Pair —
        // no `null?` on a bare `lst` to trigger narrowing.
        let mismatch = errors
            .iter()
            .any(|e| matches!(e, TypeError::Mismatch { .. }));
        assert!(
            mismatch,
            "expected Mismatch (car on non-narrowed lst), got: {errors:?}"
        );
    }

    // -------- Phase 3 iter 3.4: variadic / rest args --------

    #[test]
    fn variadic_primop_accepts_any_arity() {
        // `list` is `(-> Any ... (Listof Any))` — accepts any
        // number of args.
        let (core, table, mut syms) = parse_extract_expand("(list 1 2 3 \"hi\" #t)");
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn variadic_primop_zero_args_typechecks() {
        // `(list)` — zero trailing args is valid for rest.
        let (core, table, mut syms) = parse_extract_expand("(list)");
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_variadic_function_call_typechecks() {
        // The expander's `(define (name . xs) …)` sugar doesn't
        // accept dotted-pair formals; use the explicit `lambda`
        // with a single rest binding to get variadic shape.
        let src = "\
            (: sum (-> Fixnum ... Fixnum))
            (define sum (lambda xs 0))
            (: top (-> Fixnum))
            (define top (lambda () (sum 1 2 3)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        assert!(errors.is_empty(), "errors: {errors:?}");
    }

    #[test]
    fn typed_variadic_function_wrong_arg_type_caught() {
        let src = "\
            (: sum (-> Fixnum ... Fixnum))
            (define sum (lambda xs 0))
            (define caller (lambda () (sum 1 \"oops\" 3)))
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
            "expected Fixnum/String mismatch on rest arg, got: {errors:?}"
        );
    }

    #[test]
    fn fixed_plus_rest_arity_works() {
        // `(: f (-> Fixnum Fixnum ... Boolean))` — at least 1
        // Fixnum followed by 0+ Fixnums; 0 args should fail
        // arity.
        let src = "\
            (: f (-> Fixnum Fixnum ... Boolean))
            (define f (lambda (first . rest) #t))
            (define good (lambda () (f 1 2 3)))
            (define also-good (lambda () (f 1)))
            (define bad-arity (lambda () (f)))
        ";
        let (core, table, mut syms) = parse_extract_expand(src);
        let mut checker = Checker::new(&table, &mut syms);
        let errors = checker.check_program(&core);
        let arity_err = errors.iter().any(|e| {
            matches!(
                e,
                TypeError::ArityMismatch {
                    expected: 1,
                    found: 0,
                    ..
                }
            )
        });
        assert!(
            arity_err,
            "expected ArityMismatch{{1, 0}} for empty call, got: {errors:?}"
        );
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
