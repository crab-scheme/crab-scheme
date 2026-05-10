//! Bytecode → RIR translator (M6 iter 5).
//!
//! Translates a [`crate::opcode::CompiledLambda`] body into a
//! [`cs_rir::Function`] that the JIT backend can lower. Supports a
//! narrow subset for now — enough for fib / fact / ack / nqueens-
//! style lambdas (pure-fixnum, control flow, single-recursion).
//!
//! Out of scope this iter:
//! - Closures / non-param `LoadVar` (env access)
//! - General `Call` (only self-recursive `Call N` is allowed; the
//!   self-name is supplied by the caller and matched against
//!   `LoadVar(self_name) ... Call N` patterns)
//! - `set!`, `define-local`, multiple values, raise, etc.
//! - Joins beyond two return arms (we only translate "branch then
//!   each arm Returns" shapes; explicit Jump-to-join lands later)
//!
//! The translator simulates the VM stack as a `Vec<RirValue>` and
//! emits SSA per push. Block boundaries are: bytecode offset 0,
//! every JumpIfFalse/Jump target, and every offset just after a
//! Jump or Return.

use std::collections::{BTreeSet, HashMap};

use cs_core::Symbol;
use cs_rir::{Block, BlockId, Const, Function, Inst as RirInst, Term, Type, Value as RirValue};

use crate::opcode::{CompiledLambda, Inst};

/// Errors the translator can surface. `Unsupported` is the dominant
/// signal — when the runtime asks the JIT to compile a closure
/// whose bytecode contains an opcode we don't yet handle, the
/// runtime stays on the VM.
#[derive(Debug)]
pub enum TranslateError {
    /// An unsupported opcode appeared in the bytecode.
    Unsupported(String),
    /// Internal invariant violated (stack underflow, dangling
    /// branch target, etc.). These indicate a bug in either the
    /// translator or the bytecode source.
    Invalid(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::Unsupported(s) => write!(f, "unsupported: {s}"),
            TranslateError::Invalid(s) => write!(f, "invalid: {s}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// Translate `lambda` into a `cs_rir::Function`.
///
/// `name` is the resulting function's name (used by the JIT module
/// for declare_function). `self_name`, if `Some`, identifies the
/// symbol that resolves to the function being translated; the
/// translator emits `Inst::CallSelf` when it sees
/// `LoadVar(self_name) ... Call N` patterns.
pub fn bytecode_to_rir(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
) -> Result<Function, TranslateError> {
    if lambda.rest.is_some() {
        return Err(TranslateError::Unsupported(
            "rest parameters not yet supported".into(),
        ));
    }
    let body = &lambda.body[..];

    // Identify block-start offsets.
    let mut starts: BTreeSet<usize> = BTreeSet::new();
    starts.insert(0);
    for (i, inst) in body.iter().enumerate() {
        match inst {
            Inst::JumpIfFalse(t) | Inst::Jump(t) => {
                starts.insert(*t);
                if i + 1 < body.len() {
                    starts.insert(i + 1);
                }
            }
            Inst::Return => {
                if i + 1 < body.len() {
                    starts.insert(i + 1);
                }
            }
            _ => {}
        }
    }

    // Assign BlockIds in offset order.
    let mut offset_to_block: HashMap<usize, BlockId> = HashMap::new();
    let mut block_offsets: Vec<usize> = Vec::new();
    for (i, off) in starts.iter().enumerate() {
        offset_to_block.insert(*off, BlockId(i as u32));
        block_offsets.push(*off);
    }

    // Build the Function shell. Params are RIR Values 0..N-1.
    let mut func = Function::new(name);
    for (i, _sym) in lambda.params.iter().enumerate() {
        func.params.push((RirValue(i as u32), Type::Fixnum));
    }
    func.entry = BlockId(0);

    // SSA value allocator. Param values reserved 0..params.len()-1.
    let mut next_value_id: u32 = lambda.params.len() as u32;
    let mut alloc = || -> RirValue {
        let v = RirValue(next_value_id);
        next_value_id += 1;
        v
    };

    // Map param symbol -> RirValue.
    let mut param_map: HashMap<Symbol, RirValue> = HashMap::new();
    for (i, sym) in lambda.params.iter().enumerate() {
        param_map.insert(*sym, RirValue(i as u32));
    }

    // Translate each block.
    for (i, &start) in block_offsets.iter().enumerate() {
        let block_id = BlockId(i as u32);
        let end = if i + 1 < block_offsets.len() {
            block_offsets[i + 1]
        } else {
            body.len()
        };

        let mut sim_stack: Vec<StackEntry> = Vec::new();
        let mut insts: Vec<RirInst> = Vec::new();
        let mut term: Option<Term> = None;

        let mut ip = start;
        while ip < end {
            let op = &body[ip];
            ip += 1;
            match op {
                Inst::Const(v) => {
                    let c = value_to_const(v)?;
                    let dst = alloc();
                    insts.push(RirInst::LoadConst(dst, c));
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::LoadVar(sym) => {
                    if let Some(v) = param_map.get(sym).copied() {
                        sim_stack.push(StackEntry::Value(v));
                    } else if Some(*sym) == self_name {
                        sim_stack.push(StackEntry::SelfRef);
                    } else {
                        return Err(TranslateError::Unsupported(format!(
                            "LoadVar of non-param {:?} (env access not yet supported)",
                            sym
                        )));
                    }
                }
                Inst::AddFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Add)?,
                Inst::SubFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Sub)?,
                Inst::MulFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Mul)?,
                Inst::LtFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Lt)?,
                Inst::EqFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Eq)?,
                Inst::JumpIfFalse(target) => {
                    let cond = pop_value(&mut sim_stack)?;
                    let target_block = *offset_to_block.get(target).ok_or_else(|| {
                        TranslateError::Invalid(format!(
                            "JumpIfFalse target {} not a block start",
                            target
                        ))
                    })?;
                    let fall_block = *offset_to_block.get(&ip).ok_or_else(|| {
                        TranslateError::Invalid(format!(
                            "JumpIfFalse fall-through {} not a block start",
                            ip
                        ))
                    })?;
                    // JumpIfFalse jumps when cond is falsy. brif: cond truthy -> first, else second.
                    term = Some(Term::Branch(cond, fall_block, target_block));
                    break;
                }
                Inst::Jump(target) => {
                    let target_block = *offset_to_block.get(target).ok_or_else(|| {
                        TranslateError::Invalid(format!("Jump target {} not a block start", target))
                    })?;
                    if !sim_stack.is_empty() {
                        return Err(TranslateError::Unsupported(
                            "Jump with non-empty stack — join blocks not yet supported".into(),
                        ));
                    }
                    term = Some(Term::Jump(target_block, vec![]));
                    break;
                }
                Inst::Return => {
                    let v = pop_value(&mut sim_stack)?;
                    term = Some(Term::Return(v));
                    break;
                }
                Inst::Call(n) | Inst::TailCall(n) => {
                    if sim_stack.len() < n + 1 {
                        return Err(TranslateError::Invalid(format!(
                            "Call({n}): stack has only {} entries",
                            sim_stack.len()
                        )));
                    }
                    let split = sim_stack.len() - n;
                    let args_entries: Vec<StackEntry> = sim_stack.split_off(split);
                    let callee = sim_stack
                        .pop()
                        .ok_or_else(|| TranslateError::Invalid("Call: missing callee".into()))?;
                    match callee {
                        StackEntry::SelfRef => {
                            let mut args: Vec<RirValue> = Vec::with_capacity(*n);
                            for e in args_entries {
                                match e {
                                    StackEntry::Value(v) => args.push(v),
                                    StackEntry::SelfRef => {
                                        return Err(TranslateError::Unsupported(
                                            "self ref as arg".into(),
                                        ));
                                    }
                                }
                            }
                            let dst = alloc();
                            insts.push(RirInst::CallSelf(dst, args));
                            sim_stack.push(StackEntry::Value(dst));
                        }
                        StackEntry::Value(_) => {
                            return Err(TranslateError::Unsupported(
                                "Call with non-self callee not yet supported".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(TranslateError::Unsupported(format!(
                        "opcode {:?} not in iter-5 scope",
                        other
                    )));
                }
            }
        }

        let terminator = match term {
            Some(t) => t,
            None => {
                // Block fell through without a terminator. Either
                // the body is malformed or the last instruction
                // before block end was a non-terminator. For our
                // subset the latter shouldn't happen because all
                // block boundaries are placed AFTER terminators.
                return Err(TranslateError::Invalid(format!(
                    "block at offset {} has no terminator",
                    start
                )));
            }
        };

        func.blocks.push(Block {
            id: block_id,
            params: vec![],
            insts,
            terminator,
        });
    }

    Ok(func)
}

/// One simulated stack slot. Either an already-bound RIR Value, or
/// the special `SelfRef` sentinel that `LoadVar(self_name)` pushes;
/// the sentinel is consumed by the matching `Call N`.
enum StackEntry {
    Value(RirValue),
    SelfRef,
}

fn pop_value(stack: &mut Vec<StackEntry>) -> Result<RirValue, TranslateError> {
    match stack.pop() {
        Some(StackEntry::Value(v)) => Ok(v),
        Some(StackEntry::SelfRef) => Err(TranslateError::Unsupported(
            "self-ref appears where a Value is required".into(),
        )),
        None => Err(TranslateError::Invalid("stack underflow".into())),
    }
}

fn emit_binop<F>(
    insts: &mut Vec<RirInst>,
    stack: &mut Vec<StackEntry>,
    alloc: &mut impl FnMut() -> RirValue,
    ctor: F,
) -> Result<(), TranslateError>
where
    F: FnOnce(RirValue, RirValue, RirValue) -> RirInst,
{
    let rhs = pop_value(stack)?;
    let lhs = pop_value(stack)?;
    let dst = alloc();
    insts.push(ctor(dst, lhs, rhs));
    stack.push(StackEntry::Value(dst));
    Ok(())
}

fn value_to_const(v: &cs_core::Value) -> Result<Const, TranslateError> {
    use cs_core::Value;
    match v {
        Value::Number(cs_core::Number::Fixnum(n)) => Ok(Const::Fixnum(*n)),
        Value::Boolean(b) => Ok(Const::Boolean(*b)),
        Value::Null => Ok(Const::Null),
        Value::Unspecified => Ok(Const::Unspecified),
        other => Err(TranslateError::Unsupported(format!(
            "Const value {:?} not in iter-5 scope",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::{CompiledLambda, FastPrimopBody, Inst};
    use cs_core::{Number, SymbolTable, Value};
    use std::rc::Rc;

    fn make_fib_lambda(syms: &mut SymbolTable) -> (CompiledLambda, Symbol) {
        let n = syms.intern("n");
        let fib = syms.intern("fib");
        // body:
        //   0: LoadVar n
        //   1: Const 2
        //   2: LtFx2
        //   3: JumpIfFalse 6
        //   4: LoadVar n
        //   5: Return
        //   6: LoadVar fib   ; self
        //   7: LoadVar n
        //   8: Const 1
        //   9: SubFx2
        //  10: Call 1
        //  11: LoadVar fib   ; self
        //  12: LoadVar n
        //  13: Const 2
        //  14: SubFx2
        //  15: Call 1
        //  16: AddFx2
        //  17: Return
        let body = vec![
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(2))),
            Inst::LtFx2,
            Inst::JumpIfFalse(6),
            Inst::LoadVar(n),
            Inst::Return,
            Inst::LoadVar(fib),
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::SubFx2,
            Inst::Call(1),
            Inst::LoadVar(fib),
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(2))),
            Inst::SubFx2,
            Inst::Call(1),
            Inst::AddFx2,
            Inst::Return,
        ];
        let len = body.len();
        let l = CompiledLambda {
            params: vec![n],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None as Option<FastPrimopBody>,
        };
        (l, fib)
    }

    #[test]
    fn translate_fib_to_rir() {
        let mut syms = SymbolTable::new();
        let (lam, fib_sym) = make_fib_lambda(&mut syms);
        let f = bytecode_to_rir(&lam, "fib", Some(fib_sym)).unwrap();
        assert_eq!(f.name, "fib");
        assert_eq!(f.params.len(), 1);
        // Three blocks: entry, then-arm (returns n), else-arm (returns fib(n-1)+fib(n-2)).
        assert_eq!(f.blocks.len(), 3);
        // Entry ends in Branch.
        match &f.blocks[0].terminator {
            Term::Branch(_, _, _) => {}
            other => panic!("entry terminator: {:?}", other),
        }
        // The else arm body should contain at least 2 CallSelf insts.
        let else_arm = &f.blocks[2];
        let call_self_count = else_arm
            .insts
            .iter()
            .filter(|i| matches!(i, RirInst::CallSelf(_, _)))
            .count();
        assert_eq!(call_self_count, 2, "fib should produce 2 CallSelf insts");
    }

    #[test]
    fn translate_const_load_var_arith() {
        // f(x) = x + 1
        let mut syms = SymbolTable::new();
        let x = syms.intern("x");
        let body = vec![
            Inst::LoadVar(x),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::AddFx2,
            Inst::Return,
        ];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![x],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
        };
        let f = bytecode_to_rir(&lam, "addone", None).unwrap();
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].insts.len(), 2);
        match &f.blocks[0].insts[1] {
            RirInst::Add(_, _, _) => {}
            other => panic!("expected Add, got {:?}", other),
        }
        match &f.blocks[0].terminator {
            Term::Return(_) => {}
            other => panic!("expected Return, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_loadvar_rejected() {
        let mut syms = SymbolTable::new();
        let foo = syms.intern("foo");
        let body = vec![Inst::LoadVar(foo), Inst::Return];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
        };
        match bytecode_to_rir(&lam, "f", None) {
            Err(TranslateError::Unsupported(msg)) => assert!(msg.contains("non-param")),
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_general_call_rejected() {
        let mut syms = SymbolTable::new();
        let g = syms.intern("g");
        // (g 1) — calls non-self.
        let body = vec![
            Inst::LoadVar(g),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::Call(1),
            Inst::Return,
        ];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
        };
        match bytecode_to_rir(&lam, "f", None) {
            Err(TranslateError::Unsupported(msg)) => assert!(
                msg.contains("LoadVar") || msg.contains("non-param") || msg.contains("non-self"),
                "msg = {msg}"
            ),
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_rest_param_rejected() {
        let mut syms = SymbolTable::new();
        let rest = syms.intern("xs");
        let body = vec![Inst::Return];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: Some(rest),
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
        };
        match bytecode_to_rir(&lam, "f", None) {
            Err(TranslateError::Unsupported(msg)) => assert!(msg.contains("rest")),
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }
}
