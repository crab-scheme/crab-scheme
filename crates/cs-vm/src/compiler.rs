//! CoreExpr → Bytecode compiler.
//!
//! Foundation scope: lowers Const, Ref, Set, If, Begin, App, Lambda, Letrec.
//! Lambdas are compiled into a separate body that gets `Return` appended.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_diag::Span;
use cs_ir::{CoreExpr, Params};

use crate::opcode::{Bytecode, CompiledLambda, Inst};

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

/// Compile with a snapshot of immutable global bindings. Refs that resolve to
/// names in `globals` AND aren't shadowed by any enclosing lambda/letrec
/// binding are folded to `Inst::Const(value)` — saving an env-chain HashMap
/// walk per execution. Used by Runtime::eval_str_via_vm to fold builtins.
pub fn compile_with_globals(
    expr: &CoreExpr,
    globals: &HashMap<Symbol, Value>,
) -> Result<Bytecode, CompileError> {
    let mut top_insts: Vec<Inst> = Vec::new();
    let mut lambdas: Vec<CompiledLambda> = Vec::new();
    let mut scope: Vec<HashSet<Symbol>> = Vec::new();
    compile_expr(
        expr,
        &mut top_insts,
        &mut lambdas,
        true,
        globals,
        &mut scope,
    )?;
    Ok(Bytecode {
        insts: Rc::new(top_insts),
        lambdas: Rc::new(lambdas),
    })
}

fn is_locally_bound(scope: &[HashSet<Symbol>], name: Symbol) -> bool {
    scope.iter().any(|s| s.contains(&name))
}

fn compile_expr(
    expr: &CoreExpr,
    out: &mut Vec<Inst>,
    lambdas: &mut Vec<CompiledLambda>,
    is_tail: bool,
    globals: &HashMap<Symbol, Value>,
    scope: &mut Vec<HashSet<Symbol>>,
) -> Result<(), CompileError> {
    match expr {
        CoreExpr::Const { value, .. } => {
            out.push(Inst::Const(value.clone()));
            Ok(())
        }
        CoreExpr::Ref { name, .. } => {
            // Fold to Const if the name is a known immutable global AND not
            // shadowed in any enclosing scope.
            if !is_locally_bound(scope, *name) {
                if let Some(v) = globals.get(name) {
                    out.push(Inst::Const(v.clone()));
                    return Ok(());
                }
            }
            out.push(Inst::LoadVar(*name));
            Ok(())
        }
        CoreExpr::Set { name, value, span } => {
            compile_expr(value, out, lambdas, false, globals, scope)?;
            out.push(Inst::SetVar(*name));
            out.push(Inst::Const(Value::Unspecified));
            let _ = span;
            Ok(())
        }
        CoreExpr::If {
            cond, then, alt, ..
        } => {
            compile_expr(cond, out, lambdas, false, globals, scope)?;
            let jif_idx = out.len();
            out.push(Inst::JumpIfFalse(usize::MAX));
            compile_expr(then, out, lambdas, is_tail, globals, scope)?;
            let jmp_idx = out.len();
            out.push(Inst::Jump(usize::MAX));
            let alt_start = out.len();
            out[jif_idx] = Inst::JumpIfFalse(alt_start);
            compile_expr(alt, out, lambdas, is_tail, globals, scope)?;
            let after = out.len();
            out[jmp_idx] = Inst::Jump(after);
            Ok(())
        }
        CoreExpr::Begin { exprs, span } => {
            if exprs.is_empty() {
                out.push(Inst::Const(Value::Unspecified));
                return Ok(());
            }
            for (i, e) in exprs.iter().enumerate() {
                let last = i + 1 == exprs.len();
                compile_expr(e, out, lambdas, is_tail && last, globals, scope)?;
                if !last {
                    out.push(Inst::Pop);
                }
            }
            let _ = span;
            Ok(())
        }
        CoreExpr::App { func, args, span } => {
            compile_expr(func, out, lambdas, false, globals, scope)?;
            for a in args {
                compile_expr(a, out, lambdas, false, globals, scope)?;
            }
            if is_tail {
                out.push(Inst::TailCall(args.len()));
            } else {
                out.push(Inst::Call(args.len()));
            }
            let _ = span;
            Ok(())
        }
        CoreExpr::Lambda {
            params, body, span, ..
        } => {
            // Track new lexical scope for locality checks during folding.
            let mut frame: HashSet<Symbol> = params.fixed.iter().copied().collect();
            if let Some(rest) = params.rest {
                frame.insert(rest);
            }
            scope.push(frame);
            let mut body_insts = Vec::new();
            compile_expr(body, &mut body_insts, lambdas, true, globals, scope)?;
            body_insts.push(Inst::Return);
            scope.pop();
            let (fixed, rest) = match params {
                Params { fixed, rest } => (fixed.clone(), *rest),
            };
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: fixed,
                rest,
                body: Rc::new(body_insts),
            });
            out.push(Inst::MakeClosure(lambda_idx));
            let _ = span;
            Ok(())
        }
        CoreExpr::Letrec {
            bindings,
            body,
            span,
        } => {
            // letrec: all binding names are visible in their own scope.
            let frame: HashSet<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
            scope.push(frame);
            let mut body_insts = Vec::new();
            for (name, _) in bindings {
                body_insts.push(Inst::Const(Value::Unspecified));
                body_insts.push(Inst::DefineLocal(*name));
            }
            for (name, expr) in bindings {
                compile_expr(expr, &mut body_insts, lambdas, false, globals, scope)?;
                body_insts.push(Inst::DefineLocal(*name));
            }
            compile_expr(body, &mut body_insts, lambdas, true, globals, scope)?;
            body_insts.push(Inst::Return);
            scope.pop();
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: Vec::new(),
                rest: None,
                body: Rc::new(body_insts),
            });
            out.push(Inst::MakeClosure(lambda_idx));
            if is_tail {
                out.push(Inst::TailCall(0));
            } else {
                out.push(Inst::Call(0));
            }
            let _ = span;
            Ok(())
        }
    }
}
