//! CoreExpr → Bytecode compiler.
//!
//! Foundation scope: lowers Const, Ref, Set, If, Begin, App, Lambda, Letrec.
//! Lambdas are compiled into a separate body that gets `Return` appended.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cs_core::{Number, Symbol, Value};
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
    // cs-grt: shared const pool for the whole compiled program (top-level
    // body + every nested lambda body). `compile_expr` interns each
    // literal/folded-global `Value` here and emits `Inst::Const(idx)`;
    // finalized to `Rc<Vec<NanboxValue>>` below and shared by `Bytecode`
    // and every `CompiledLambda`.
    let mut consts: Vec<Value> = Vec::new();
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
        &mut consts,
    )?;
    // Every compiled body — top-level and per-lambda (see the Lambda arm
    // of `compile_expr`) — ends with an explicit `Return` so the VM's
    // dispatch loop can treat falling off the end of a frame's insts as
    // a defensive fallback rather than the primary termination check.
    buf.push(Inst::Return, expr.span());
    let (insts, spans) = buf.finish();
    let (insts, spans) = peephole(insts, spans, &mut consts);
    let const_pool: Rc<Vec<crate::vm::NanboxValue>> = Rc::new(
        consts
            .into_iter()
            .map(crate::vm::NanboxValue::from_value)
            .collect(),
    );
    for lambda in lambdas.iter_mut() {
        lambda.consts = Rc::clone(&const_pool);
    }
    Ok(Bytecode {
        insts: Rc::new(insts),
        spans: Rc::new(spans),
        lambdas: Rc::new(lambdas),
        consts: const_pool,
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
        CoreExpr::WithContinuationMark { key, val, body, .. } => {
            collect_assigned_names(key, out);
            collect_assigned_names(val, out);
            collect_assigned_names(body, out);
        }
    }
}

/// Walk `expr` and add every `Ref { name, .. }` to `out`. Conservative:
/// shadowed names are included too — for the letrec dependency analysis
/// below a false edge only affects ordering or forces the (safe)
/// original compile path, never correctness.
fn collect_ref_names(expr: &CoreExpr, out: &mut HashSet<Symbol>) {
    match expr {
        CoreExpr::Const { .. } => {}
        CoreExpr::Ref { name, .. } => {
            out.insert(*name);
        }
        CoreExpr::Set { name, value, .. } => {
            out.insert(*name);
            collect_ref_names(value, out);
        }
        CoreExpr::Lambda { body, .. } => collect_ref_names(body, out),
        CoreExpr::App { func, args, .. } => {
            collect_ref_names(func, out);
            for a in args {
                collect_ref_names(a, out);
            }
        }
        CoreExpr::If {
            cond, then, alt, ..
        } => {
            collect_ref_names(cond, out);
            collect_ref_names(then, out);
            collect_ref_names(alt, out);
        }
        CoreExpr::Begin { exprs, .. } => {
            for e in exprs {
                collect_ref_names(e, out);
            }
        }
        CoreExpr::Letrec { bindings, body, .. } => {
            for (_, v) in bindings {
                collect_ref_names(v, out);
            }
            collect_ref_names(body, out);
        }
        CoreExpr::WithContinuationMark { key, val, body, .. } => {
            collect_ref_names(key, out);
            collect_ref_names(val, out);
            collect_ref_names(body, out);
        }
    }
}

/// Topologically order letrec bindings by their sibling references
/// (self-edges ignored — self-recursion resolves via `self_bind`).
/// Returns `None` when the sibling graph has a cycle (true mutual
/// recursion), in which case the caller keeps the classic compile.
fn letrec_topo_order(bindings: &[(Symbol, CoreExpr)], names: &[Symbol]) -> Option<Vec<usize>> {
    let index_of: HashMap<Symbol, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    // deps[i] = set of sibling indices binding i references (minus self)
    let deps: Vec<HashSet<usize>> = bindings
        .iter()
        .enumerate()
        .map(|(i, (_, v))| {
            let mut refs = HashSet::new();
            collect_ref_names(v, &mut refs);
            refs.iter()
                .filter_map(|r| index_of.get(r).copied())
                .filter(|&j| j != i)
                .collect()
        })
        .collect();
    // Kahn's algorithm.
    let n = bindings.len();
    let mut remaining: Vec<HashSet<usize>> = deps;
    let mut placed = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let next = (0..n).find(|&i| !placed[i] && remaining[i].is_empty())?;
        placed[next] = true;
        order.push(next);
        for r in remaining.iter_mut() {
            r.remove(&next);
        }
    }
    Some(order)
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
fn branch_on_not(op: PrimOp, target: u32) -> Inst {
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
fn detect_fast_primop(
    body: &[Inst],
    spans: &[Span],
    params: &[Symbol],
    consts: &[Value],
) -> Option<FastPrimopBody> {
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
            Inst::Const(idx) => Some(FastArg::Const(consts[*idx as usize].clone())),
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

/// Post-codegen peephole pass, run once on every compiled body (top-level
/// and per-lambda) right after codegen finishes, before `detect_fast_primop`
/// sees the shape — folding can shrink a body into the 4-inst fast-primop
/// pattern that wouldn't have matched pre-fold. Three sub-passes, applied in
/// order:
///  1. Constant folding: `Const(a); Const(b); <Fx2 op>` -> `Const(folded)`
///     when both operands are compile-time fixnums and the checked op
///     doesn't overflow (an overflow falls through to the runtime's
///     bignum-promoting generic path, which this pass doesn't replicate).
///  2. Dead-store cancellation: `SetVar(s); Const(Unspecified); Pop` (the
///     exact shape `CoreExpr::Set` emits in non-tail `begin` position) ->
///     `SetVar(s)`.
///  3. Jump threading: a `Jump`/conditional-branch target that lands on an
///     unconditional `Jump` is redirected straight to that `Jump`'s own
///     target, chased transitively.
///
/// Sub-passes 1 and 2 delete instructions, which requires remapping every
/// jump target that crosses the deleted span; `compact` builds that remap.
/// Both patterns are only ever emitted as one atomic, contiguous unit by a
/// single `compile_expr` call (a primop `App`'s arg0/arg1/op, or a `Set`'s
/// SetVar/Const/Pop) — jump targets are always `out.len()` snapshots taken
/// at `compile_expr` call boundaries, so no jump can land mid-pattern.
fn peephole(insts: Vec<Inst>, spans: Vec<Span>, consts: &mut Vec<Value>) -> (Vec<Inst>, Vec<Span>) {
    let (insts, spans) = peephole_fold_consts(insts, spans, consts);
    let (insts, spans) = peephole_cancel_dead_set(insts, spans, consts);
    let mut insts = insts;
    peephole_thread_jumps(&mut insts);
    (insts, spans)
}

/// Jump/branch target as a plain `usize` instruction index — `Inst`
/// stores it as `u32` (cs-grt); the peephole passes below stay in
/// `usize` internally (matching `Vec<Inst>::len()`) and cast at this
/// boundary.
fn branch_target(inst: &Inst) -> Option<usize> {
    match inst {
        Inst::Jump(t)
        | Inst::JumpIfFalse(t)
        | Inst::BranchOnGeFx2(t)
        | Inst::BranchOnGtFx2(t)
        | Inst::BranchOnLeFx2(t)
        | Inst::BranchOnLtFx2(t)
        | Inst::BranchOnNeFx2(t) => Some(*t as usize),
        _ => None,
    }
}

fn set_branch_target(inst: &mut Inst, new_target: usize) {
    let new_target = new_target as u32;
    match inst {
        Inst::Jump(t)
        | Inst::JumpIfFalse(t)
        | Inst::BranchOnGeFx2(t)
        | Inst::BranchOnGtFx2(t)
        | Inst::BranchOnLeFx2(t)
        | Inst::BranchOnLtFx2(t)
        | Inst::BranchOnNeFx2(t) => *t = new_target,
        _ => {}
    }
}

/// Rebuild `insts`/`spans`, dropping every index where `deleted[i]` is set
/// and substituting `replace[i]` for the instruction at a kept index `i`
/// when present. Returns the new vectors plus an `old_to_new` map (length
/// `insts.len() + 1`) where `old_to_new[i]` is the new index that a jump
/// target of `i` should be rewritten to — the next surviving instruction at
/// or after logical position `i`, so a target that pointed into a deleted
/// span resumes exactly where execution would have continued.
/// `old_to_new[insts.len()]` is the new one-past-the-end index.
fn compact(
    insts: Vec<Inst>,
    spans: Vec<Span>,
    deleted: &[bool],
    mut replace: HashMap<usize, Inst>,
) -> (Vec<Inst>, Vec<Span>, Vec<usize>) {
    let len = insts.len();
    let mut new_insts = Vec::with_capacity(len);
    let mut new_spans = Vec::with_capacity(len);
    let mut old_to_new = vec![0usize; len + 1];
    for (i, (inst, span)) in insts.into_iter().zip(spans).enumerate() {
        old_to_new[i] = new_insts.len();
        if deleted[i] {
            continue;
        }
        let inst = replace.remove(&i).unwrap_or(inst);
        new_insts.push(inst);
        new_spans.push(span);
    }
    old_to_new[len] = new_insts.len();
    (new_insts, new_spans, old_to_new)
}

fn remap_targets(insts: &mut [Inst], old_to_new: &[usize]) {
    for inst in insts.iter_mut() {
        if let Some(t) = branch_target(inst) {
            set_branch_target(inst, old_to_new[t]);
        }
    }
}

fn as_fixnum_const(inst: &Inst, consts: &[Value]) -> Option<i64> {
    match inst {
        Inst::Const(idx) => match &consts[*idx as usize] {
            Value::Number(Number::Fixnum(v)) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// Fold a 2-arg fixnum primop over compile-time-known operands, matching
/// the runtime's `fixnum_binop2_nb`/`fixnum_cmp2_nb` fast-path semantics
/// exactly (checked arithmetic; `None` on overflow leaves folding to the
/// runtime's generic bignum-promoting path).
fn fold_fixnum_op(op: &Inst, a: i64, b: i64) -> Option<Value> {
    match op {
        Inst::AddFx2 => a.checked_add(b).map(|v| Value::Number(Number::Fixnum(v))),
        Inst::SubFx2 => a.checked_sub(b).map(|v| Value::Number(Number::Fixnum(v))),
        Inst::MulFx2 => a.checked_mul(b).map(|v| Value::Number(Number::Fixnum(v))),
        Inst::LtFx2 => Some(Value::Boolean(a < b)),
        Inst::LeFx2 => Some(Value::Boolean(a <= b)),
        Inst::GtFx2 => Some(Value::Boolean(a > b)),
        Inst::GeFx2 => Some(Value::Boolean(a >= b)),
        Inst::EqFx2 => Some(Value::Boolean(a == b)),
        _ => None,
    }
}

fn peephole_fold_consts(
    insts: Vec<Inst>,
    spans: Vec<Span>,
    consts: &mut Vec<Value>,
) -> (Vec<Inst>, Vec<Span>) {
    let len = insts.len();
    if len < 3 {
        return (insts, spans);
    }
    let mut deleted = vec![false; len];
    let mut replace = HashMap::new();
    let mut i = 0;
    while i + 2 < len {
        if let (Some(a), Some(b)) = (
            as_fixnum_const(&insts[i], consts),
            as_fixnum_const(&insts[i + 1], consts),
        ) {
            if let Some(folded) = fold_fixnum_op(&insts[i + 2], a, b) {
                let idx = intern_const(consts, folded);
                replace.insert(i, Inst::Const(idx));
                deleted[i + 1] = true;
                deleted[i + 2] = true;
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    if replace.is_empty() {
        return (insts, spans);
    }
    let (mut insts, spans, old_to_new) = compact(insts, spans, &deleted, replace);
    remap_targets(&mut insts, &old_to_new);
    (insts, spans)
}

fn peephole_cancel_dead_set(
    insts: Vec<Inst>,
    spans: Vec<Span>,
    consts: &[Value],
) -> (Vec<Inst>, Vec<Span>) {
    let len = insts.len();
    if len < 3 {
        return (insts, spans);
    }
    let mut deleted = vec![false; len];
    let mut any = false;
    for i in 0..len - 2 {
        let const_unspecified = matches!(
            &insts[i + 1],
            Inst::Const(idx) if matches!(consts.get(*idx as usize), Some(Value::Unspecified))
        );
        if matches!(insts[i], Inst::SetVar(_))
            && const_unspecified
            && matches!(insts[i + 2], Inst::Pop)
        {
            deleted[i + 1] = true;
            deleted[i + 2] = true;
            any = true;
        }
    }
    if !any {
        return (insts, spans);
    }
    let (mut insts, spans, old_to_new) = compact(insts, spans, &deleted, HashMap::new());
    remap_targets(&mut insts, &old_to_new);
    (insts, spans)
}

fn peephole_thread_jumps(insts: &mut [Inst]) {
    let len = insts.len();
    for i in 0..len {
        let Some(mut t) = branch_target(&insts[i]) else {
            continue;
        };
        let mut hops = 0;
        while t < len && hops <= len {
            let Inst::Jump(next) = &insts[t] else {
                break;
            };
            let next = *next as usize;
            if next == t {
                break;
            }
            t = next;
            hops += 1;
        }
        set_branch_target(&mut insts[i], t);
    }
}

/// Intern `v` into the shared const pool, returning its index. No
/// dedup — each call site gets its own pool slot; the pool only needs to
/// be pre-encoded, not minimal.
fn intern_const(consts: &mut Vec<Value>, v: Value) -> u32 {
    let idx = consts.len() as u32;
    consts.push(v);
    idx
}

#[allow(clippy::too_many_arguments)]
fn compile_expr(
    expr: &CoreExpr,
    out: &mut InstBuf,
    lambdas: &mut Vec<CompiledLambda>,
    is_tail: bool,
    globals: &HashMap<Symbol, Value>,
    primops: &HashMap<Symbol, PrimOp>,
    scope: &mut Vec<HashSet<Symbol>>,
    consts: &mut Vec<Value>,
) -> Result<(), CompileError> {
    let span = expr.span();
    match expr {
        CoreExpr::Const { value, .. } => {
            let idx = intern_const(consts, value.clone());
            out.push(Inst::Const(idx), span);
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
                        let idx = intern_const(consts, v.clone());
                        out.push(Inst::Const(idx), span);
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
            compile_expr(value, out, lambdas, false, globals, primops, scope, consts)?;
            out.push(Inst::SetVar(*name), *s);
            let idx = intern_const(consts, Value::Unspecified);
            out.push(Inst::Const(idx), *s);
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
                compile_expr(a, out, lambdas, false, globals, primops, scope, consts)?;
                compile_expr(b, out, lambdas, false, globals, primops, scope, consts)?;
                let jif_idx = out.len();
                out.push(branch_on_not(op, u32::MAX), *s);
                compile_expr(then, out, lambdas, is_tail, globals, primops, scope, consts)?;
                let jmp_idx = out.len();
                out.push(Inst::Jump(u32::MAX), *s);
                let alt_start = out.len() as u32;
                out.replace(jif_idx, branch_on_not(op, alt_start));
                compile_expr(alt, out, lambdas, is_tail, globals, primops, scope, consts)?;
                let after = out.len() as u32;
                out.replace(jmp_idx, Inst::Jump(after));
                return Ok(());
            }
            compile_expr(cond, out, lambdas, false, globals, primops, scope, consts)?;
            let jif_idx = out.len();
            out.push(Inst::JumpIfFalse(u32::MAX), *s);
            compile_expr(then, out, lambdas, is_tail, globals, primops, scope, consts)?;
            let jmp_idx = out.len();
            out.push(Inst::Jump(u32::MAX), *s);
            let alt_start = out.len() as u32;
            out.replace(jif_idx, Inst::JumpIfFalse(alt_start));
            compile_expr(alt, out, lambdas, is_tail, globals, primops, scope, consts)?;
            let after = out.len() as u32;
            out.replace(jmp_idx, Inst::Jump(after));
            Ok(())
        }
        CoreExpr::Begin { exprs, span: s } => {
            if exprs.is_empty() {
                let idx = intern_const(consts, Value::Unspecified);
                out.push(Inst::Const(idx), *s);
                return Ok(());
            }
            for (i, e) in exprs.iter().enumerate() {
                let last = i + 1 == exprs.len();
                compile_expr(
                    e,
                    out,
                    lambdas,
                    is_tail && last,
                    globals,
                    primops,
                    scope,
                    consts,
                )?;
                if !last {
                    out.push(Inst::Pop, e.span());
                }
            }
            Ok(())
        }
        CoreExpr::WithContinuationMark {
            key,
            val,
            body,
            span: s,
        } => {
            // Tail-safe continuation marks (issue #36). Evaluate key
            // then val (non-tail), install the mark on the current
            // frame via `PushMark`, then compile `body` in the caller's
            // tail position. When `body` ends in a `TailCall`, the
            // frame (and its mark) is reused, so a wcm reached through
            // the tail call replaces rather than accumulates.
            compile_expr(key, out, lambdas, false, globals, primops, scope, consts)?;
            compile_expr(val, out, lambdas, false, globals, primops, scope, consts)?;
            out.push(Inst::PushMark, *s);
            compile_expr(body, out, lambdas, is_tail, globals, primops, scope, consts)?;
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
                            compile_expr(
                                &args[0], out, lambdas, false, globals, primops, scope, consts,
                            )?;
                            compile_expr(
                                &args[1], out, lambdas, false, globals, primops, scope, consts,
                            )?;
                            out.push(primop_to_inst(op), *s);
                            return Ok(());
                        }
                    }
                }
            }
            // RC3 iter 2.10 — N-arg (N≥3) variadic primops where the
            // op is left-fold safe: Add/Sub/Mul/Div. Lower as a chain
            // of binary primops:
            //   (* a b c d) → ((a * b) * c) * d
            // This keeps AOT clean (no EnvLookup-of-primop → capture
            // chain) without changing JIT behavior — both paths see
            // the same specialized opcodes in the same order. The
            // comparison primops (Lt/Le/Gt/Ge/Eq) are NOT lowered
            // here because their N-arg semantics is "all adjacent
            // pairs satisfy", not left-fold.
            if args.len() >= 3 {
                if let CoreExpr::Ref { name, .. } = &**func {
                    if !is_locally_bound(scope, *name) {
                        if let Some(op) = primops.get(name).copied() {
                            let foldable = matches!(op, PrimOp::Add | PrimOp::Sub | PrimOp::Mul);
                            if foldable {
                                // Compile a, then for each subsequent
                                // arg compile + emit the primop.
                                compile_expr(
                                    &args[0], out, lambdas, false, globals, primops, scope, consts,
                                )?;
                                for rest in &args[1..] {
                                    compile_expr(
                                        rest, out, lambdas, false, globals, primops, scope, consts,
                                    )?;
                                    out.push(primop_to_inst(op), *s);
                                }
                                return Ok(());
                            }
                        }
                    }
                }
            }
            // Phase 5b iter9 — let* inlining at compile time.
            // Pattern: `((lambda (x) body) arg)` — produced by the
            // expander for `let*` (which rewrites to a nested chain
            // of single-binding lambda-apps).
            //
            // The default path emits MakeClosure + Call which spawns
            // a fresh closure and dispatches through CallGeneral.
            // For non-escaping single-binding lets (the common case),
            // we compile as EnterScope + DefineLocal + body + LeaveScope.
            //
            // Gate: ONLY single-binding (1 arg). Multi-binding lets
            // (e.g. `(let ((r ...) (c ...)) body)` from safe?-style
            // helpers) trade one Gc-alloc + IC-cached dispatch for
            // multiple per-binding EnvDefineLocal helper calls per
            // iteration — net regression in tight loops where the IC
            // already caches the closure call. Single-binding wins
            // because the body still has one EnvDefineLocal but
            // saves the closure allocation entirely.
            if let CoreExpr::Lambda {
                params,
                body,
                span: lam_s,
                ..
            } = &**func
            {
                if params.rest.is_none() && params.fixed.len() == 1 && args.len() == 1 {
                    let frame: std::collections::HashSet<Symbol> =
                        params.fixed.iter().copied().collect();
                    scope.push(frame);
                    out.push(Inst::EnterScope, *lam_s);
                    let name = params.fixed[0];
                    compile_expr(
                        &args[0], out, lambdas, false, globals, primops, scope, consts,
                    )?;
                    out.push(Inst::DefineLocal(name), args[0].span());
                    compile_expr(body, out, lambdas, is_tail, globals, primops, scope, consts)?;
                    out.push(Inst::LeaveScope, body.span());
                    scope.pop();
                    return Ok(());
                }
            }
            compile_expr(func, out, lambdas, false, globals, primops, scope, consts)?;
            for a in args {
                compile_expr(a, out, lambdas, false, globals, primops, scope, consts)?;
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
            compile_expr(
                body,
                &mut body_buf,
                lambdas,
                true,
                globals,
                primops,
                scope,
                consts,
            )?;
            body_buf.push(Inst::Return, body.span());
            scope.pop();
            let (fixed, rest) = match params {
                Params { fixed, rest } => (fixed.clone(), *rest),
            };
            let (body_insts, body_spans) = body_buf.finish();
            let (body_insts, body_spans) = peephole(body_insts, body_spans, consts);
            let fast = detect_fast_primop(&body_insts, &body_spans, &fixed, consts);
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: fixed,
                rest,
                body: Rc::new(body_insts),
                spans: Rc::new(body_spans),
                fast,
                self_bind: None,
                profile: Default::default(),
                // cs-grt: filled in by `compile_with_globals_and_primops`
                // once the whole program's const pool is finalized (this
                // lambda's own body may still be mid-compile relative to
                // siblings that add more consts).
                consts: Rc::new(Vec::new()),
            });
            out.push(Inst::MakeClosure(lambda_idx), *s);
            Ok(())
        }
        CoreExpr::Letrec {
            bindings,
            body,
            span: s,
        } => {
            // Inline `letrec` compilation (post-M8 contification): the
            // bindings live in an `Env::child` layer pushed onto the
            // *current* call frame's env via `Inst::EnterScope` rather
            // than a wrapper closure. Previously every `letrec` /
            // named-`let` compiled to a 0-arg `CompiledLambda` +
            // `MakeClosure` + `Call(0)`, allocating a fresh `VmClosure`
            // (and its `Gc<Value::Procedure>` heap wrapper) every time
            // control entered the form — a hot allocation site on
            // closure-heavy code (n-queens' `count-from-1-to-n` does it
            // once per call, ~2057 times for `(nqueens 8)`). The
            // wrapper closure carries no information the surrounding
            // frame doesn't already have; the only thing it provided
            // was scope isolation, which an env-layer push/pop
            // captures more cheaply.
            //
            // Tail-position handling: if the surrounding context is
            // tail, the body's tail expression can `TailCall` directly,
            // discarding the current frame (and the env layer with
            // it). Control never reaches the trailing `LeaveScope`.
            // For non-tail context, the body leaves a value on the
            // stack and control flows to `LeaveScope`, which pops the
            // env layer while the value-stack top passes through.
            // Letrec-of-lambdas cycle avoidance (cw-6m8). The classic
            // compile order — push the scope layer, then build closures
            // that capture it — makes every closure capture the env layer
            // holding its OWN binding: a closure↔layer Rc cycle that no
            // refcount can reclaim, leaked once per *execution* of the
            // form (~1KB). Named lets and internal defines are letrecs,
            // so long-lived server actors ground through GiBs of these
            // (the crab-watchstore ~150KB/request leak).
            //
            // When every binding is a lambda, no bound name is `set!`,
            // and the sibling-reference graph is ACYCLIC (self-recursion
            // allowed — it resolves via `self_bind` at call time), the
            // closures can be built in dependency order, each BEFORE the
            // scope layer that holds its own binding: references to
            // earlier siblings resolve through captured earlier layers,
            // self-references through `self_bind`, and no closure ever
            // captures its own layer — no cycle. All-lambda initializers
            // make the reorder unobservable (R6RS letrec restriction).
            // True mutual recursion (a cyclic SCC) keeps the original
            // path — that cycle is semantically real.
            {
                let names: Vec<Symbol> = bindings.iter().map(|(n, _)| *n).collect();
                let all_lambdas = bindings
                    .iter()
                    .all(|(_, v)| matches!(v, CoreExpr::Lambda { .. }));
                let mut assigned = HashSet::new();
                for (_, v) in bindings {
                    collect_assigned_names(v, &mut assigned);
                }
                collect_assigned_names(body, &mut assigned);
                let no_sets = names.iter().all(|n| !assigned.contains(n));
                if all_lambdas && no_sets {
                    if let Some(order) = letrec_topo_order(bindings, &names) {
                        let frame: HashSet<Symbol> = names.iter().copied().collect();
                        scope.push(frame);
                        for &i in &order {
                            let (name, lam_expr) = (&bindings[i].0, &bindings[i].1);
                            compile_expr(
                                lam_expr, out, lambdas, false, globals, primops, scope, consts,
                            )?;
                            // compile_expr(Lambda) pushes its CompiledLambda
                            // LAST (inner lambdas are pushed during the body
                            // compile), so the MakeClosure just emitted refers
                            // to lambdas.last().
                            lambdas.last_mut().expect("lambda just compiled").self_bind =
                                Some(*name);
                            out.push(Inst::EnterScope, *s);
                            out.push(Inst::DefineLocal(*name), lam_expr.span());
                        }
                        compile_expr(body, out, lambdas, is_tail, globals, primops, scope, consts)?;
                        for _ in &order {
                            out.push(Inst::LeaveScope, body.span());
                        }
                        scope.pop();
                        return Ok(());
                    }
                }
            }
            let frame: HashSet<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
            scope.push(frame);
            out.push(Inst::EnterScope, *s);
            for (name, _) in bindings {
                let idx = intern_const(consts, Value::Unspecified);
                out.push(Inst::Const(idx), *s);
                out.push(Inst::DefineLocal(*name), *s);
            }
            for (name, expr) in bindings {
                compile_expr(expr, out, lambdas, false, globals, primops, scope, consts)?;
                out.push(Inst::DefineLocal(*name), expr.span());
            }
            compile_expr(body, out, lambdas, is_tail, globals, primops, scope, consts)?;
            out.push(Inst::LeaveScope, body.span());
            scope.pop();
            Ok(())
        }
    }
}
