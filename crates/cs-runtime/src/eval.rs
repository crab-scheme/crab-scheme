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
}

#[derive(Debug, Clone)]
pub struct EvalError {
    pub kind: EvalErrorKind,
    pub span: Span,
}

impl EvalError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            kind: EvalErrorKind::Message(message.into()),
            span,
        }
    }

    pub fn raised(condition: Value, span: Span) -> Self {
        Self {
            kind: EvalErrorKind::Raised(condition),
            span,
        }
    }

    pub fn message(&self) -> String {
        match &self.kind {
            EvalErrorKind::Message(m) => m.clone(),
            EvalErrorKind::Raised(v) => format!("uncaught: {}", v),
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
    /// Side channel for multi-value returns from `values`.
    pub pending_values: Option<Vec<Value>>,
    /// Current input port (per-dynamic-extent).
    pub current_input_port: Option<Value>,
    /// Current output port (per-dynamic-extent).
    pub current_output_port: Option<Value>,
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
            pending_values: None,
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
    if let Some(cond) = ctx.pending_raise.take() {
        EvalError::raised(cond, span)
    } else {
        EvalError::new(msg, span)
    }
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
            Err(EvalError::new("unknown procedure type", Span::DUMMY))
        }
        v => Err(EvalError::new(
            format!("not a procedure: {}", v.type_name()),
            Span::DUMMY,
        )),
    }
}

pub fn eval(expr: &CoreExpr, env: Rc<Frame>, ctx: &mut EvalCtx) -> Result<Value, EvalError> {
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
