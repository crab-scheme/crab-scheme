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
    /// User-level call sites pushed as we enter App forms and truncated on
    /// successful eval() return. On error the residue is the call chain
    /// that led to the failing site, used for backtraces.
    pub call_stack: Vec<Span>,
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
            Err(EvalError::new("unknown procedure type", Span::DUMMY))
        }
        v => Err(EvalError::new(
            format!("not a procedure: {}", v.type_name()),
            Span::DUMMY,
        )),
    }
}

pub fn eval(expr: &CoreExpr, env: Rc<Frame>, ctx: &mut EvalCtx) -> Result<Value, EvalError> {
    let stack_snapshot = ctx.call_stack.len();
    let r = eval_inner(expr, env, ctx);
    if r.is_ok() {
        ctx.call_stack.truncate(stack_snapshot);
    }
    r
}

fn eval_inner(expr: &CoreExpr, env: Rc<Frame>, ctx: &mut EvalCtx) -> Result<Value, EvalError> {
    let mut cur_expr = expr.clone();
    let mut cur_env = env;

    loop {
        if ctx.depth > ctx.max_depth {
            return Err(EvalError::new("stack overflow", cur_expr.span()));
        }
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
                            ctx.depth += 1;
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
