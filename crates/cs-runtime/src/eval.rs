//! Tree-walking evaluator with a manual loop for tail-call elimination.

use std::rc::Rc;

use cs_core::{SymbolTable, Value};
use cs_diag::Span;
use cs_ir::CoreExpr;

use crate::env::Frame;
use crate::proc::{make_closure, Builtin, BuiltinFn, Closure, Parameter};

#[derive(Debug, Clone)]
pub enum EvalErrorKind {
    Message(String),
    Raised(Value),
    /// Escape from a `call/cc`: the embedded `id` matches the call/cc that
    /// installed this continuation, and the captured value becomes the
    /// result of that call/cc. Caught only by the matching call/cc; any
    /// other handler must rethrow.
    Escape(u64, Value),
}

#[derive(Debug, Clone)]
pub struct EvalError {
    pub kind: EvalErrorKind,
    pub span: Span,
    /// Call-site spans captured from `EvalCtx.call_stack` at the moment
    /// the error was raised. Innermost-last (the deepest still-pending
    /// App is at the end). Populated by the runtime when constructing a
    /// Diagnostic from the error.
    pub backtrace: Vec<Span>,
}

impl EvalError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            kind: EvalErrorKind::Message(message.into()),
            span,
            backtrace: Vec::new(),
        }
    }

    pub fn raised(condition: Value, span: Span) -> Self {
        Self {
            kind: EvalErrorKind::Raised(condition),
            span,
            backtrace: Vec::new(),
        }
    }

    pub fn message(&self) -> String {
        match &self.kind {
            EvalErrorKind::Message(m) => m.clone(),
            EvalErrorKind::Raised(v) => format!("uncaught: {}", v),
            EvalErrorKind::Escape(id, v) => {
                format!("escape continuation #{} invoked: {}", id, v)
            }
        }
    }

    pub fn escape(id: u64, v: Value, span: Span) -> Self {
        Self {
            kind: EvalErrorKind::Escape(id, v),
            span,
            backtrace: Vec::new(),
        }
    }
}

pub struct EvalCtx<'a> {
    pub top: Rc<Frame>,
    pub syms: &'a mut SymbolTable,
    pub macros: &'a mut std::collections::HashMap<cs_core::Symbol, cs_expand::Macro>,
    pub depth: u32,
    pub max_depth: u32,
    /// Side channel: when a builtin (`raise`, `error`) wants to raise a
    /// condition, it stashes the value here and returns `Err`. eval picks
    /// it up and converts to `EvalErrorKind::Raised`.
    pub pending_raise: Option<Value>,
    /// Side channel: when a higher-order builtin (e.g. call/cc) needs to
    /// rethrow an Escape continuation that isn't its own match, it stashes
    /// (id, value) here and returns `Err("__escape__")`. The wrapper
    /// `builtin_err_to_eval` rebuilds the EvalErrorKind::Escape.
    pub pending_escape: Option<(u64, Value)>,
    /// Side channel for multi-value returns from `values`.
    pub pending_values: Option<Vec<Value>>,
    /// Current input port (per-dynamic-extent).
    pub current_input_port: Option<Value>,
    /// Current output port (per-dynamic-extent).
    pub current_output_port: Option<Value>,
    /// Current error port (per-dynamic-extent). R7RS: returned by
    /// (current-error-port). Foundation: lazily created as a string
    /// output port the first time it's queried.
    pub current_error_port: Option<Value>,
    /// User-level call sites pushed as we enter App forms and truncated on
    /// successful eval() return. On error the residue is the call chain
    /// that led to the failing site, used for backtraces.
    pub call_stack: Vec<Span>,
    /// L1 sandbox import-set policy (ADR 0015 issue #15 fix).
    /// When `Some`, `(environment ...)` rejects any import-spec not in this
    /// list with an explicit error naming the disallowed library. `None`
    /// means unrestricted (normal non-sandbox eval).
    pub sandbox_allowed_imports: Option<Vec<String>>,
    /// Tail-safe continuation marks (issue #36). Each entry is
    /// `(frame_depth, key, val)`. `with-continuation-mark` upserts at
    /// the current `depth`; because tail calls reuse the same
    /// `eval_inner` activation (the loop `continue`s without bumping
    /// `depth`), a wcm reached through tail calls replaces the mark for
    /// its key at that depth instead of accumulating — so a tail loop
    /// runs in constant mark-space. A non-tail call bumps `depth` via
    /// `eval`, which clears marks at the abandoned depth on return.
    /// Empty in the overwhelmingly common case (no marks in use), so
    /// the per-`eval` bookkeeping is gated to near-zero cost.
    pub cont_marks: Vec<(u32, Value, Value)>,
}

impl<'a> EvalCtx<'a> {
    pub fn new(
        top: Rc<Frame>,
        syms: &'a mut SymbolTable,
        macros: &'a mut std::collections::HashMap<cs_core::Symbol, cs_expand::Macro>,
    ) -> Self {
        Self {
            top,
            syms,
            macros,
            depth: 0,
            max_depth: 1_000_000,
            pending_raise: None,
            pending_escape: None,
            pending_values: None,
            call_stack: Vec::new(),
            current_input_port: None,
            current_output_port: None,
            current_error_port: None,
            sandbox_allowed_imports: None,
            cont_marks: Vec::new(),
        }
    }
}

fn call_parameter(param: &Parameter, args: &[Value]) -> Value {
    if args.is_empty() {
        param.cell.borrow().clone()
    } else {
        *param.cell.borrow_mut() = args[0].clone();
        Value::Unspecified
    }
}

fn builtin_err_to_eval(ctx: &mut EvalCtx, msg: String, span: Span) -> EvalError {
    // Drain unconditionally so a stale value from a prior failure can't
    // attach to an unrelated path (e.g. an Err that bypasses type_err).
    let irritants = cs_core::take_builtin_err_irritant();
    if let Some(cond) = ctx.pending_raise.take() {
        return EvalError::raised(cond, span);
    }
    if let Some((id, v)) = ctx.pending_escape.take() {
        return EvalError::escape(id, v, span);
    }
    // Internal sentinels — these aren't real builtin failures, they're
    // protocol markers between builtins and the dispatcher.
    if matches!(
        msg.as_str(),
        "__raised__" | "__escape__" | "__stack-overflow__"
    ) {
        return EvalError::new(msg, span);
    }
    // Build a proper R6RS condition so user code can catch builtin
    // failures with `with-exception-handler` / `guard`. Most builtins
    // format their errors as "<who>: <message>" — split on the first
    // colon and surface the prefix as &who. The offending value (when
    // a `type_err` was the source) is attached as &irritants.
    let (who, message) = match msg.find(": ") {
        Some(idx) => {
            let who_str = &msg[..idx];
            let rest = &msg[idx + 2..];
            (
                Some(Value::Symbol(ctx.syms.intern(who_str))),
                rest.to_string(),
            )
        }
        None => (None, msg.clone()),
    };
    let extra_tag = cs_core::take_builtin_err_extra_tag();
    let cond = crate::builtins::make_error_condition(who, message, irritants);
    let cond = match extra_tag {
        Some(tag) => crate::builtins::add_simple_to_compound(cond, tag),
        None => cond,
    };
    EvalError::raised(cond, span)
}

/// Apply `proc_val` to `args`. Used by higher-order builtins (`apply`, `map`,
/// `for-each`, `with-exception-handler`) to re-enter the evaluator.
pub fn apply_procedure(
    proc_val: &Value,
    args: &[Value],
    ctx: &mut EvalCtx,
) -> Result<Value, EvalError> {
    match proc_val {
        Value::Procedure(p) => {
            let any = p.as_any();
            if let Some(b) = any.downcast_ref::<Builtin>() {
                return match b.f {
                    BuiltinFn::Pure(f) => {
                        f(args).map_err(|m| builtin_err_to_eval(ctx, m, Span::DUMMY))
                    }
                    BuiltinFn::Higher(f) => {
                        f(args, ctx).map_err(|m| builtin_err_to_eval(ctx, m, Span::DUMMY))
                    }
                    BuiltinFn::Syms(f) => {
                        f(args, ctx.syms).map_err(|m| builtin_err_to_eval(ctx, m, Span::DUMMY))
                    }
                };
            }
            if let Some(h) = any.downcast_ref::<crate::proc::HostBuiltin>() {
                return (h.f)(args).map_err(|m| builtin_err_to_eval(ctx, m, Span::DUMMY));
            }
            // Closure-bearing builtins constructed via make_host_builtin
            // ride cs-vm's VmHostBuiltin so they dispatch on both tiers.
            // Walker downcast added in M9 iter 2.
            if let Some(h) = any.downcast_ref::<cs_vm::vm::VmHostBuiltin>() {
                return (h.f)(args).map_err(|m| builtin_err_to_eval(ctx, m, Span::DUMMY));
            }
            if let Some(c) = any.downcast_ref::<Closure>() {
                if !c.params.accepts_arity(args.len()) {
                    return Err(EvalError::new(
                        format!(
                            "{}: arity mismatch (expected {}{}, got {})",
                            c.display_name.as_deref().unwrap_or("procedure"),
                            c.params.fixed.len(),
                            if c.params.rest.is_some() { "+" } else { "" },
                            args.len(),
                        ),
                        Span::DUMMY,
                    ));
                }
                let new_env = Frame::child(c.env.clone());
                for (name, val) in c.params.fixed.iter().zip(args.iter()) {
                    new_env.define(*name, val.clone());
                }
                if let Some(rest_name) = c.params.rest {
                    let rest_args = &args[c.params.fixed.len()..];
                    new_env.define(rest_name, Value::list(rest_args.iter().cloned()));
                }
                return eval(&c.body, new_env, ctx);
            }
            if let Some(param) = any.downcast_ref::<Parameter>() {
                return Ok(call_parameter(param, args));
            }
            if let Some(k) = any.downcast_ref::<crate::proc::Continuation>() {
                let v = if args.is_empty() {
                    Value::Unspecified
                } else {
                    args[0].clone()
                };
                // Stash via side-channel so higher-order builtins that
                // collapse `EvalError -> String` (via .message()) still let
                // call/cc reconstruct the Escape via builtin_err_to_eval.
                ctx.pending_escape = Some((k.id, v));
                return Err(EvalError::new("__escape__", Span::DUMMY));
            }
            // A `Value::Procedure` the walker doesn't recognize is a
            // VM/JIT-tier procedure (VmClosure / VmBuiltin / …). Delegate
            // to the VM's universal caller so higher-order *walker*
            // builtins (take-while, …) bridged onto the VM tier can invoke
            // a VM-closure predicate. On a pure walker run this arm is
            // unreachable (every proc is a walker type handled above).
            cs_vm::vm::vm_call_sync(proc_val, args, ctx.syms)
                .map_err(|e| EvalError::new(e.message, Span::DUMMY))
        }
        v => Err(EvalError::new(
            format!("not a procedure: {}", v.type_name()),
            Span::DUMMY,
        )),
    }
}

pub fn eval(expr: &CoreExpr, env: Rc<Frame>, ctx: &mut EvalCtx) -> Result<Value, EvalError> {
    let stack_snapshot = ctx.call_stack.len();
    // `depth` is the count of live nested `eval` invocations —
    // i.e. the host-stack recursion depth of non-tail
    // subexpressions. Tail calls stay inside `eval_inner`'s loop
    // and never re-enter `eval`, so a tail-recursive program
    // holds depth constant no matter how many iterations it runs.
    // (It used to be bumped once per closure tail-call and never
    // decremented, which made it a monotonic total-call counter
    // that spuriously tripped `max_depth` on any long run.)
    ctx.depth += 1;
    if ctx.depth > ctx.max_depth {
        ctx.depth -= 1;
        return Err(EvalError::new("stack overflow", expr.span()));
    }
    let r = eval_inner(expr, env, ctx);
    ctx.depth -= 1;
    if r.is_ok() {
        ctx.call_stack.truncate(stack_snapshot);
    }
    // Tail-safe continuation marks (issue #36): marks installed at a
    // depth deeper than the one we've returned to belong to the
    // now-completed dynamic extent — drop them. Gated on non-empty so
    // mark-free code (the overwhelming majority) pays nothing here.
    if !ctx.cont_marks.is_empty() {
        let d = ctx.depth;
        ctx.cont_marks.retain(|(md, _, _)| *md <= d);
    }
    r
}

fn eval_inner(expr: &CoreExpr, env: Rc<Frame>, ctx: &mut EvalCtx) -> Result<Value, EvalError> {
    let mut cur_expr = expr.clone();
    let mut cur_env = env;
    // Backtrace-span high-water for THIS eval_inner activation. The App
    // arm pushes a call-site span per application, but a TAIL call
    // `continue`s this loop without returning through `eval`'s
    // truncate — so before the fix a walker-tier tail loop leaked one
    // Span per iteration, forever (a long-lived server's idle
    // `(let park () (yield) (park))` ground RSS up at ~16MB/s; the
    // crab-watchstore WAN melt). Truncating at the back-edge keeps the
    // CURRENT iteration's chain for error backtraces — prior
    // iterations' spans are the same call sites repeated.
    let tail_snapshot = ctx.call_stack.len();
    // ponytail: per-activation tail-chain span cap (see back-edge below).
    const TAIL_SPAN_CAP: usize = 256;

    loop {
        // No depth check here: `depth` is bumped + checked in
        // `eval` on entry, and the tail-call `continue`s below
        // don't deepen the host stack, so depth is invariant
        // across this loop.
        match cur_expr {
            CoreExpr::Const { value, .. } => return Ok(value),
            CoreExpr::Ref { name, span } => match cur_env.get(name) {
                Some(v) => return Ok(v),
                None => {
                    return Err(EvalError::new(
                        format!("undefined variable: {}", ctx.syms.name(name)),
                        span,
                    ));
                }
            },
            CoreExpr::Set { name, value, span } => {
                let v = eval(&value, cur_env.clone(), ctx)?;
                // ADR 0015 L1.1: an `(environment ...)` snapshot
                // builds an immutable root Frame. set! against a
                // name defined in that frame raises &assertion
                // rather than silently mutating or shadowing.
                if cur_env.is_immutable_definition(name) {
                    use crate::builtins::{
                        make_compound, make_simple, TAG_ASSERTION, TAG_MESSAGE, TAG_WHO,
                    };
                    let cond = make_compound(vec![
                        make_simple(TAG_ASSERTION, vec![]),
                        make_simple(TAG_WHO, vec![Value::Symbol(ctx.syms.intern("set!"))]),
                        make_simple(
                            TAG_MESSAGE,
                            vec![Value::string(
                                "attempt to mutate immutable environment binding",
                            )],
                        ),
                    ]);
                    return Err(EvalError::raised(cond, span));
                }
                if !cur_env.set_existing(name, v.clone()) {
                    ctx.top.define(name, v);
                }
                let _ = span;
                return Ok(Value::Unspecified);
            }
            CoreExpr::Lambda {
                params, body, span, ..
            } => {
                let _ = span;
                return Ok(make_closure(params, body, cur_env.clone(), None, ctx.syms));
            }
            CoreExpr::If {
                cond, then, alt, ..
            } => {
                let c = eval(&cond, cur_env.clone(), ctx)?;
                cur_expr = if c.is_truthy() {
                    (*then).clone()
                } else {
                    (*alt).clone()
                };
                continue;
            }
            CoreExpr::Begin { exprs, .. } => {
                if exprs.is_empty() {
                    return Ok(Value::Unspecified);
                }
                let last = exprs.len() - 1;
                for e in &exprs[..last] {
                    eval(e, cur_env.clone(), ctx)?;
                }
                cur_expr = exprs[last].clone();
                continue;
            }
            CoreExpr::Letrec { bindings, body, .. } => {
                let new_env = Frame::child(cur_env.clone());
                for (name, _) in &bindings {
                    new_env.define(*name, Value::Unspecified);
                }
                for (name, expr) in &bindings {
                    let v = eval(expr, new_env.clone(), ctx)?;
                    new_env.define(*name, v);
                }
                cur_env = new_env;
                cur_expr = (*body).clone();
                continue;
            }
            CoreExpr::WithContinuationMark { key, val, body, .. } => {
                // Tail-safe continuation marks (issue #36). `body` is in
                // tail position (we `continue` into it at the same
                // `depth`), so a wcm reached through tail calls lands at
                // the same depth and replaces — constant mark-space in a
                // tail loop. Non-tail nesting bumps `depth` via `eval`
                // and accumulates (then clears on return). key/val are
                // evaluated non-tail.
                let k = eval(&key, cur_env.clone(), ctx)?;
                let v = eval(&val, cur_env.clone(), ctx)?;
                let d = ctx.depth;
                if let Some(slot) = ctx
                    .cont_marks
                    .iter_mut()
                    .find(|(md, mk, _)| *md == d && cs_core::eq::equal(mk, &k))
                {
                    slot.2 = v;
                } else {
                    ctx.cont_marks.push((d, k, v));
                }
                cur_expr = (*body).clone();
                continue;
            }
            CoreExpr::App { func, args, span } => {
                ctx.call_stack.push(span);
                let func_val = eval(&func, cur_env.clone(), ctx)?;
                let mut arg_vals = Vec::with_capacity(args.len());
                for a in &args {
                    arg_vals.push(eval(a, cur_env.clone(), ctx)?);
                }
                match &func_val {
                    Value::Procedure(p) => {
                        let any = p.as_any();
                        if let Some(b) = any.downcast_ref::<Builtin>() {
                            let res = match b.f {
                                BuiltinFn::Pure(f) => f(&arg_vals),
                                BuiltinFn::Higher(f) => f(&arg_vals, ctx),
                                BuiltinFn::Syms(f) => f(&arg_vals, ctx.syms),
                            };
                            return res.map_err(|m| builtin_err_to_eval(ctx, m, span));
                        }
                        if let Some(h) = any.downcast_ref::<crate::proc::HostBuiltin>() {
                            let res = (h.f)(&arg_vals);
                            return res.map_err(|m| builtin_err_to_eval(ctx, m, span));
                        }
                        if let Some(h) = any.downcast_ref::<cs_vm::vm::VmHostBuiltin>() {
                            let res = (h.f)(&arg_vals);
                            return res.map_err(|m| builtin_err_to_eval(ctx, m, span));
                        }
                        if let Some(c) = any.downcast_ref::<Closure>() {
                            if !c.params.accepts_arity(arg_vals.len()) {
                                return Err(EvalError::new(
                                    format!(
                                        "{}: arity mismatch (expected {}{}, got {})",
                                        c.display_name.as_deref().unwrap_or("procedure"),
                                        c.params.fixed.len(),
                                        if c.params.rest.is_some() { "+" } else { "" },
                                        arg_vals.len(),
                                    ),
                                    span,
                                ));
                            }
                            let new_env = Frame::child(c.env.clone());
                            for (name, val) in c.params.fixed.iter().zip(arg_vals.iter()) {
                                new_env.define(*name, val.clone());
                            }
                            if let Some(rest_name) = c.params.rest {
                                let rest_args = &arg_vals[c.params.fixed.len()..];
                                new_env.define(rest_name, Value::list(rest_args.iter().cloned()));
                            }
                            cur_env = new_env;
                            cur_expr = (*c.body).clone();
                            // Tail-call back-edge: a hot tail loop pushes the same
                            // call-site span every iteration and would leak forever
                            // (cw-6m8). But resetting unconditionally also wipes a
                            // genuine tail-call chain (outer->middle->...) the error
                            // backtrace needs. ponytail: only reset once the chain
                            // exceeds the cap — bounds the leak, keeps short chains
                            // intact for backtraces. Ceiling: a legit tail chain
                            // deeper than the cap loses its oldest frames on reset.
                            if ctx.call_stack.len() - tail_snapshot > TAIL_SPAN_CAP {
                                ctx.call_stack.truncate(tail_snapshot);
                            }
                            continue;
                        }
                        if let Some(param) = any.downcast_ref::<Parameter>() {
                            return Ok(call_parameter(param, &arg_vals));
                        }
                        if let Some(k) = any.downcast_ref::<crate::proc::Continuation>() {
                            let v = arg_vals.first().cloned().unwrap_or(Value::Unspecified);
                            ctx.pending_escape = Some((k.id, v));
                            return Err(EvalError::new("__escape__", span));
                        }
                        return Err(EvalError::new("unknown procedure type", span));
                    }
                    other => {
                        return Err(EvalError::new(
                            format!("call to non-procedure ({})", other.type_name()),
                            span,
                        ));
                    }
                }
            }
        }
    }
}
