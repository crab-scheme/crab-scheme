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
    let mut buf = InstBuf::new();
    let mut lambdas: Vec<CompiledLambda> = Vec::new();
    let mut scope: Vec<HashSet<Symbol>> = Vec::new();
    compile_expr(expr, &mut buf, &mut lambdas, true, globals, &mut scope)?;
    let (insts, spans) = buf.finish();
    Ok(Bytecode {
        insts: Rc::new(insts),
        spans: Rc::new(spans),
        lambdas: Rc::new(lambdas),
    })
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

fn compile_expr(
    expr: &CoreExpr,
    out: &mut InstBuf,
    lambdas: &mut Vec<CompiledLambda>,
    is_tail: bool,
    globals: &HashMap<Symbol, Value>,
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
            // shadowed in any enclosing scope.
            if !is_locally_bound(scope, *name) {
                if let Some(v) = globals.get(name) {
                    out.push(Inst::Const(v.clone()), span);
                    return Ok(());
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
            compile_expr(value, out, lambdas, false, globals, scope)?;
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
            compile_expr(cond, out, lambdas, false, globals, scope)?;
            let jif_idx = out.len();
            out.push(Inst::JumpIfFalse(usize::MAX), *s);
            compile_expr(then, out, lambdas, is_tail, globals, scope)?;
            let jmp_idx = out.len();
            out.push(Inst::Jump(usize::MAX), *s);
            let alt_start = out.len();
            out.replace(jif_idx, Inst::JumpIfFalse(alt_start));
            compile_expr(alt, out, lambdas, is_tail, globals, scope)?;
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
                compile_expr(e, out, lambdas, is_tail && last, globals, scope)?;
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
            compile_expr(func, out, lambdas, false, globals, scope)?;
            for a in args {
                compile_expr(a, out, lambdas, false, globals, scope)?;
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
            compile_expr(body, &mut body_buf, lambdas, true, globals, scope)?;
            body_buf.push(Inst::Return, body.span());
            scope.pop();
            let (fixed, rest) = match params {
                Params { fixed, rest } => (fixed.clone(), *rest),
            };
            let (body_insts, body_spans) = body_buf.finish();
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: fixed,
                rest,
                body: Rc::new(body_insts),
                spans: Rc::new(body_spans),
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
                compile_expr(expr, &mut body_buf, lambdas, false, globals, scope)?;
                body_buf.push(Inst::DefineLocal(*name), expr.span());
            }
            compile_expr(body, &mut body_buf, lambdas, true, globals, scope)?;
            body_buf.push(Inst::Return, body.span());
            scope.pop();
            let (body_insts, body_spans) = body_buf.finish();
            let lambda_idx = lambdas.len();
            lambdas.push(CompiledLambda {
                params: Vec::new(),
                rest: None,
                body: Rc::new(body_insts),
                spans: Rc::new(body_spans),
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
