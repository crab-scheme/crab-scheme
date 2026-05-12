//! CoreExpr → Bytecode compiler.
//!
//! Foundation scope: lowers Const, Ref, Set, If, Begin, App, Lambda, Letrec.
//! Lambdas are compiled into a separate body that gets `Return` appended.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_diag::Span;
use cs_ir::{CoreExpr, Params};

use crate::opcode::{Bytecode, CompiledLambda, FastArg, FastPrimopBody, Inst};

#[derive(Clone, Debug)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl CompileError {
    pub fn new(msg: impl Into<String>, span: Span) -> Self {
        Self {
            message: msg.into(),
            span,
        }
    }
}

pub fn compile(expr: &CoreExpr) -> Result<Bytecode, CompileError> {
    compile_with_globals(expr, &HashMap::new())
}

/// Standard 2-arg primops that the compiler can specialize when the App's
/// function references the matching name AND that name is in `globals`
/// AND it's not shadowed by any enclosing scope. The runtime supplies the
/// Symbol-keyed map; we never compare value identity so the runtime can
/// regenerate the map cheaply each compile.
#[derive(Clone, Copy, Debug)]
pub enum PrimOp {
    Add,
    Sub,
    Mul,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

/// Compile with a snapshot of immutable global bindings. Refs that resolve to
/// names in `globals` AND aren't shadowed by any enclosing lambda/letrec
/// binding are folded to `Inst::Const(value)` — saving an env-chain HashMap
/// walk per execution. Used by Runtime::eval_str_via_vm to fold builtins.
pub fn compile_with_globals(
    expr: &CoreExpr,
    globals: &HashMap<Symbol, Value>,
) -> Result<Bytecode, CompileError> {
    compile_with_globals_and_primops(expr, globals, &HashMap::new())
}

/// Like [`compile_with_globals`] but also takes a primop dispatch map. The
/// compiler emits specialized opcodes (AddFx2, LtFx2, ...) for 2-arg calls
/// whose function is an unshadowed Ref to a name in the map.
pub fn compile_with_globals_and_primops(
    expr: &CoreExpr,
    globals: &HashMap<Symbol, Value>,
    primops: &HashMap<Symbol, PrimOp>,
) -> Result<Bytecode, CompileError> {
    let mut buf = InstBuf::new();
    let mut lambdas: Vec<CompiledLambda> = Vec::new();
    // Names that are mutated anywhere in the program — top-level
    // `define` lowers to `CoreExpr::Set`, as does `set!`. Treat them
    // as a synthetic scope so the global-fold optimization in
    // compile_expr's `Ref` arm sees them as locally bound and emits
    // `Inst::LoadVar` instead of folding the captured-at-snapshot
    // global value. Without this guard, `(define count 0) (+ count
    // 1)` mistakenly folds `count` to the builtin procedure of the
    // same name.
    let mut scope: Vec<HashSet<Symbol>> = Vec::new();
    let mut assigned: HashSet<Symbol> = HashSet::new();
    collect_assigned_names(expr, &mut assigned);
    if !assigned.is_empty() {
        scope.push(assigned);
    }
    compile_expr(
        expr,
        &mut buf,
        &mut lambdas,
        true,
        globals,
        primops,
        &mut scope,
    )?;
    let (insts, spans) = buf.finish();
    Ok(Bytecode {
        insts: Rc::new(insts),
        spans: Rc::new(spans),
        lambdas: Rc::new(lambdas),
    })
}

/// Walk `expr` and add every `Set { name, .. }` target to `out`.
/// Used to suppress the global-fold optimization for any name the
/// program rebinds (top-level `define` lowers to Set in expand).
fn collect_assigned_names(expr: &CoreExpr, out: &mut HashSet<Symbol>) {
    match expr {
        CoreExpr::Const { .. } | CoreExpr::Ref { .. } => {}
        CoreExpr::Set { name, value, .. } => {
            out.insert(*name);
            collect_assigned_names(value, out);
        }
        CoreExpr::Lambda { body, .. } => collect_assigned_names(body, out),
        CoreExpr::App { func, args, .. } => {
            collect_assigned_names(func, out);
            for a in args {
                collect_assigned_names(a, out);
            }
        }
        CoreExpr::If {
            cond, then, alt, ..
        } => {
            collect_assigned_names(cond, out);
            collect_assigned_names(then, out);
            collect_assigned_names(alt, out);
        }
        CoreExpr::Begin { exprs, .. } => {
            for e in exprs {
                collect_assigned_names(e, out);
            }
        }
        CoreExpr::Letrec { bindings, body, .. } => {
            // letrec bindings ARE locally bound, not free globals,
            // so they're already excluded from the fold by the
            // letrec scope push. We still walk the binding values
            // and body to find any nested Sets.
            for (_, v) in bindings {
                collect_assigned_names(v, out);
            }
            collect_assigned_names(body, out);
        }
    }
}

/// Buffered output of compile: parallel insts + spans Vecs that grow
/// together, ensuring spans[i] is the source span of insts[i].
struct InstBuf {
    insts: Vec<Inst>,
    spans: Vec<Span>,
}

impl InstBuf {
    fn new() -> Self {
        Self {
            insts: Vec::new(),
            spans: Vec::new(),
        }
    }

    fn push(&mut self, inst: Inst, span: Span) {
        self.insts.push(inst);
        self.spans.push(span);
    }

    fn len(&self) -> usize {
        self.insts.len()
    }

    fn replace(&mut self, idx: usize, inst: Inst) {
        self.insts[idx] = inst;
    }

    fn finish(self) -> (Vec<Inst>, Vec<Span>) {
        (self.insts, self.spans)
    }
}

fn is_locally_bound(scope: &[HashSet<Symbol>], name: Symbol) -> bool {
    scope.iter().any(|s| s.contains(&name))
}

/// If `expr` is `(<op> a b)` where `<op>` is an unshadowed PrimOp Ref,
/// return `(op, a, b)` so an enclosing `if` can fuse the compare+branch.
/// Otherwise return None.
fn match_primop_2arg<'a>(
    expr: &'a CoreExpr,
    scope: &[HashSet<Symbol>],
    primops: &HashMap<Symbol, PrimOp>,
) -> Option<(PrimOp, &'a CoreExpr, &'a CoreExpr)> {
    if let CoreExpr::App { func, args, .. } = expr {
        if args.len() != 2 {
            return None;
        }
        if let CoreExpr::Ref { name, .. } = &**func {
            if is_locally_bound(scope, *name) {
                return None;
            }
            if let Some(op) = primops.get(name).copied() {
                // Only the comparison primops are useful as branch
                // conditions; arithmetic ones don't produce booleans.
                match op {
                    PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge | PrimOp::Eq => {
                        return Some((op, &args[0], &args[1]));
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

/// Map a primop comparison to the fused "branch on negation" instruction.
/// The branch fires when the original comparison is false (i.e., we should
/// take the alt branch of the surrounding `if`).
fn branch_on_not(op: PrimOp, target: usize) -> Inst {
    match op {
        PrimOp::Lt => Inst::BranchOnGeFx2(target),
        PrimOp::Le => Inst::BranchOnGtFx2(target),
        PrimOp::Gt => Inst::BranchOnLeFx2(target),
        PrimOp::Ge => Inst::BranchOnLtFx2(target),
        PrimOp::Eq => Inst::BranchOnNeFx2(target),
        _ => unreachable!("branch_on_not called with non-comparison primop"),
    }
}

/// If `body` is structurally `[<arg0>, <arg1>, <Fx2 op>, Return]` where
/// each arg is a single LoadVar(param) or Const, return a FastPrimopBody
/// describing it. The VM's call sites use this to skip Env+Frame setup for
/// trivially small bodies — by far the common case for lambdas passed to
/// map/fold (`(lambda (x) (* x x))`, `(lambda (a b) (+ a b))`, etc).
fn detect_fast_primop(body: &[Inst], spans: &[Span], params: &[Symbol]) -> Option<FastPrimopBody> {
    if body.len() != 4 {
        return None;
    }
    if !matches!(body[3], Inst::Return) {
        return None;
    }
    let arg = |slot: &Inst| -> Option<FastArg> {
        match slot {
            Inst::LoadVar(s) => params
                .iter()
                .position(|p| p == s)
                .map(|i| FastArg::Param(i as u8)),
            Inst::Const(v) => Some(FastArg::Const(v.clone())),
            _ => None,
        }
    };
    let arg0 = arg(&body[0])?;
    let arg1 = arg(&body[1])?;
    let op = match &body[2] {
        Inst::AddFx2
        | Inst::SubFx2
        | Inst::MulFx2
        | Inst::LtFx2
        | Inst::LeFx2
        | Inst::GtFx2
        | Inst::GeFx2
        | Inst::EqFx2 => body[2].clone(),
        _ => return None,
    };
    let span = spans.get(2).copied().unwrap_or(Span::DUMMY);
    Some(FastPrimopBody {
        op,
        args: [arg0, arg1],
        span,
    })
}

fn primop_to_inst(op: PrimOp) -> Inst {
    match op {
        PrimOp::Add => Inst::AddFx2,
        PrimOp::Sub => Inst::SubFx2,
        PrimOp::Mul => Inst::MulFx2,
        PrimOp::Lt => Inst::LtFx2,
        PrimOp::Le => Inst::LeFx2,
        PrimOp::Gt => Inst::GtFx2,
        PrimOp::Ge => Inst::GeFx2,
        PrimOp::Eq => Inst::EqFx2,
    }
}

fn compile_expr(
    expr: &CoreExpr,
    out: &mut InstBuf,
    lambdas: &mut Vec<CompiledLambda>,
    is_tail: bool,
    globals: &HashMap<Symbol, Value>,
    primops: &HashMap<Symbol, PrimOp>,
    scope: &mut Vec<HashSet<Symbol>>,
) -> Result<(), CompileError> {
    let span = expr.span();
    match expr {
        CoreExpr::Const { value, .. } => {
            out.push(Inst::Const(value.clone()), span);
            Ok(())
        }
        CoreExpr::Ref { name, .. } => {
            // Fold to Const if the name is a known immutable global AND not
            // shadowed in any enclosing scope. Restricted to Procedure
            // values: builtins like `+`, `<`, `car` rarely get redefined
            // and benefit hugely from the fold. User-bound non-procedure
            // globals (numbers, strings, etc.) can be redefined / set!'d
            // across compile units, so we keep them as LoadVar so the
            // runtime resolves them live. (M6 Phase 2 iter B: this also
            // preserves correctness for the JIT's env-lookup path —
            // free-var refs to user globals stay as LoadVar in the
            // bytecode and translate to Inst::EnvLookup at JIT time.)
            //
            // ADR 0012 D-1 iter JG note: this fold blocks JIT
            // compilation of caller→callee patterns where callee is a
            // user-defined VmClosure (the folded Const(Procedure(VmClosure))
            // pushes a BuiltinRef("vm-closure") sentinel in the JIT
            // translator, which can't dispatch). Un-folding for
            // VmClosure procedures enables JIT but exposes a latent
            // bug in self-recursive JIT compilation (fact-12 stack
            // overflow) that needs separate investigation.
            if !is_locally_bound(scope, *name) {
                if let Some(v) = globals.get(name) {
                    if matches!(v, Value::Procedure(_)) {
                        out.push(Inst::Const(v.clone()), span);
                        return Ok(());
                    }
                }
            }
            out.push(Inst::LoadVar(*name), span);
            Ok(())
        }
        CoreExpr::Set {
            name,
            value,
            span: s,
        } => {
            compile_expr(value, out, lambdas, false, globals, primops, scope)?;
            out.push(Inst::SetVar(*name), *s);
            out.push(Inst::Const(Value::Unspecified), *s);
            Ok(())
        }
        CoreExpr::If {
            cond,
            then,
            alt,
            span: s,
        } => {
            // Try the fused compare+branch pattern: when cond is a 2-arg
            // primop App, emit `<a> <b> BranchOn<NotOp>(alt_start)` and
            // skip materializing the boolean.
            if let Some((op, a, b)) = match_primop_2arg(cond, scope, primops) {
                compile_expr(a, out, lambdas, false, globals, primops, scope)?;
                compile_expr(b, out, lambdas, false, globals, primops, scope)?;
                let jif_idx = out.len();
                out.push(branch_on_not(op, usize::MAX), *s);
                compile_expr(then, out, lambdas, is_tail, globals, primops, scope)?;
                let jmp_idx = out.len();
                out.push(Inst::Jump(usize::MAX), *s);
                let alt_start = out.len();
                out.replace(jif_idx, branch_on_not(op, alt_start));
                compile_expr(alt, out, lambdas, is_tail, globals, primops, scope)?;
                let after = out.len();
                out.replace(jmp_idx, Inst::Jump(after));
                return Ok(());
            }
            compile_expr(cond, out, lambdas, false, globals, primops, scope)?;
            let jif_idx = out.len();
            out.push(Inst::JumpIfFalse(usize::MAX), *s);
            compile_expr(then, out, lambdas, is_tail, globals, primops, scope)?;
            let jmp_idx = out.len();
            out.push(Inst::Jump(usize::MAX), *s);
            let alt_start = out.len();
            out.replace(jif_idx, Inst::JumpIfFalse(alt_start));
            compile_expr(alt, out, lambdas, is_tail, globals, primops, scope)?;
            let after = out.len();
            out.replace(jmp_idx, Inst::Jump(after));
            Ok(())
        }
        CoreExpr::Begin { exprs, span: s } => {
            if exprs.is_empty() {
                out.push(Inst::Const(Value::Unspecified), *s);
                return Ok(());
            }
            for (i, e) in exprs.iter().enumerate() {
                let last = i + 1 == exprs.len();
                compile_expr(e, out, lambdas, is_tail && last, globals, primops, scope)?;
                if !last {
                    out.push(Inst::Pop, e.span());
                }
            }
            Ok(())
        }
        CoreExpr::App {
            func,
            args,
            span: s,
        } => {
            // Specialize 2-arg calls whose function is an unshadowed Ref
            // to a known primop name (e.g. (+ a b) -> AddFx2).
            if args.len() == 2 {
                if let CoreExpr::Ref { name, .. } = &**func {
                    if !is_locally_bound(scope, *name) {
                        if let Some(op) = primops.get(name).copied() {
                            compile_expr(&args[0], out, lambdas, false, globals, primops, scope)?;
                            compile_expr(&args[1], out, lambdas, false, globals, primops, scope)?;
                            out.push(primop_to_inst(op), *s);
                            return Ok(());
                        }
                    }
                }
            }
            compile_expr(func, out, lambdas, false, globals, primops, scope)?;
            for a in args {
                compile_expr(a, out, lambdas, false, globals, primops, scope)?;
            }
            if is_tail {
                out.push(Inst::TailCall(args.len()), *s);
            } else {
                out.push(Inst::Call(args.len()), *s);
            }
            Ok(())
        }
        CoreExpr::Lambda {
            params,
            body,
            span: s,
            ..
        } => {
            let mut frame: HashSet<Symbol> = params.fixed.iter().copied().collect();
            if let Some(rest) = params.rest {
                frame.insert(rest);
            }
            scope.push(frame);
            let mut body_buf = InstBuf::new();
            compile_expr(body, &mut body_buf, lambdas, true, globals, primops, scope)?;
            body_buf.push(Inst::Return, body.span());
            scope.pop();
            let (fixed, rest) = match params {
                Params { fixed, rest } => (fixed.clone(), *rest),
            };
            let (body_insts, body_spans) = body_buf.finish();
            let fast = detect_fast_primop(&body_insts, &body_spans, &fixed);
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: fixed,
                rest,
                body: Rc::new(body_insts),
                spans: Rc::new(body_spans),
                fast,
            });
            out.push(Inst::MakeClosure(lambda_idx), *s);
            Ok(())
        }
        CoreExpr::Letrec {
            bindings,
            body,
            span: s,
        } => {
            let frame: HashSet<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
            scope.push(frame);
            let mut body_buf = InstBuf::new();
            for (name, _) in bindings {
                body_buf.push(Inst::Const(Value::Unspecified), *s);
                body_buf.push(Inst::DefineLocal(*name), *s);
            }
            for (name, expr) in bindings {
                compile_expr(expr, &mut body_buf, lambdas, false, globals, primops, scope)?;
                body_buf.push(Inst::DefineLocal(*name), expr.span());
            }
            compile_expr(body, &mut body_buf, lambdas, true, globals, primops, scope)?;
            body_buf.push(Inst::Return, body.span());
            scope.pop();
            let (body_insts, body_spans) = body_buf.finish();
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: Vec::new(),
                rest: None,
                body: Rc::new(body_insts),
                spans: Rc::new(body_spans),
                fast: None,
            });
            out.push(Inst::MakeClosure(lambda_idx), *s);
            if is_tail {
                out.push(Inst::TailCall(0), *s);
            } else {
                out.push(Inst::Call(0), *s);
            }
            Ok(())
        }
    }
}
