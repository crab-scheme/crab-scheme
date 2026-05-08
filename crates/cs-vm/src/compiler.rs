//! CoreExpr → Bytecode compiler.
//!
//! Foundation scope: lowers Const, Ref, Set, If, Begin, App, Lambda, Letrec.
//! Lambdas are compiled into a separate body that gets `Return` appended.

use std::rc::Rc;

use cs_core::Value;
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
    let mut top_insts: Vec<Inst> = Vec::new();
    let mut lambdas: Vec<CompiledLambda> = Vec::new();
    compile_expr(expr, &mut top_insts, &mut lambdas, true)?;
    Ok(Bytecode {
        insts: Rc::new(top_insts),
        lambdas,
    })
}

fn compile_expr(
    expr: &CoreExpr,
    out: &mut Vec<Inst>,
    lambdas: &mut Vec<CompiledLambda>,
    is_tail: bool,
) -> Result<(), CompileError> {
    match expr {
        CoreExpr::Const { value, .. } => {
            out.push(Inst::Const(value.clone()));
            Ok(())
        }
        CoreExpr::Ref { name, .. } => {
            out.push(Inst::LoadVar(*name));
            Ok(())
        }
        CoreExpr::Set { name, value, span } => {
            compile_expr(value, out, lambdas, false)?;
            // SetVar walks the env chain; if no existing binding, falls back
            // to root.define (matching tree-walker top-level behavior).
            out.push(Inst::SetVar(*name));
            // The set! form yields Unspecified.
            out.push(Inst::Const(Value::Unspecified));
            let _ = span;
            Ok(())
        }
        CoreExpr::If {
            cond, then, alt, ..
        } => {
            compile_expr(cond, out, lambdas, false)?;
            let jif_idx = out.len();
            out.push(Inst::JumpIfFalse(usize::MAX));
            // Both branches inherit tail position from the if.
            compile_expr(then, out, lambdas, is_tail)?;
            let jmp_idx = out.len();
            out.push(Inst::Jump(usize::MAX));
            let alt_start = out.len();
            out[jif_idx] = Inst::JumpIfFalse(alt_start);
            compile_expr(alt, out, lambdas, is_tail)?;
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
                compile_expr(e, out, lambdas, is_tail && last)?;
                if !last {
                    out.push(Inst::Pop);
                }
            }
            let _ = span;
            Ok(())
        }
        CoreExpr::App { func, args, span } => {
            compile_expr(func, out, lambdas, false)?;
            for a in args {
                compile_expr(a, out, lambdas, false)?;
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
            // Compile body into a separate inst list; append Return.
            let mut body_insts = Vec::new();
            compile_expr(body, &mut body_insts, lambdas, true)?;
            body_insts.push(Inst::Return);
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
            // Compile letrec to a thunk-call so the body executes in a fresh
            // local Env: ((lambda () local-defines... body))
            // This gives proper local scoping.
            let mut body_insts = Vec::new();
            // Pre-bind all names to Unspecified (letrec* semantics).
            for (name, _) in bindings {
                body_insts.push(Inst::Const(Value::Unspecified));
                body_insts.push(Inst::DefineLocal(*name));
            }
            // Then evaluate each value and define it.
            for (name, expr) in bindings {
                compile_expr(expr, &mut body_insts, lambdas, false)?;
                body_insts.push(Inst::DefineLocal(*name));
            }
            // Body
            compile_expr(body, &mut body_insts, lambdas, true)?;
            body_insts.push(Inst::Return);
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
