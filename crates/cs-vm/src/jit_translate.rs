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
            Inst::JumpIfFalse(t)
            | Inst::Jump(t)
            | Inst::BranchOnGeFx2(t)
            | Inst::BranchOnGtFx2(t)
            | Inst::BranchOnLeFx2(t)
            | Inst::BranchOnLtFx2(t)
            | Inst::BranchOnNeFx2(t) => {
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

    // Per-block entry stack: the SSA values that should be on the
    // simulated stack when the block starts executing. Set by the
    // predecessor's Jump emission (the predecessor allocates fresh
    // RIR Values to serve as block params + names them in this map
    // for the target). The entry block starts empty (function args
    // are bound separately via param_map).
    let mut block_entry_stack: HashMap<BlockId, Vec<RirValue>> = HashMap::new();
    block_entry_stack.insert(BlockId(0), Vec::new());

    // Per-block declared params. Populated alongside
    // block_entry_stack so we can emit Block { params: ... } at the
    // end. Each entry's RirValues are the same SSA ids that the
    // entry-stack contains.
    let mut block_params: HashMap<BlockId, Vec<(RirValue, Type)>> = HashMap::new();
    block_params.insert(BlockId(0), Vec::new());

    // Translate each block.
    for (i, &start) in block_offsets.iter().enumerate() {
        let block_id = BlockId(i as u32);
        let end = if i + 1 < block_offsets.len() {
            block_offsets[i + 1]
        } else {
            body.len()
        };

        // Initialize the simulated stack from the block-entry stack
        // table. If a block was never targeted by a predecessor
        // (unreachable in offset order), we default to empty — the
        // body will catch any underflow.
        let mut sim_stack: Vec<StackEntry> = block_entry_stack
            .get(&block_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(StackEntry::Value)
            .collect();
        let mut insts: Vec<RirInst> = Vec::new();
        let mut term: Option<Term> = None;

        let mut ip = start;
        while ip < end {
            let op = &body[ip];
            ip += 1;
            match op {
                Inst::Const(v) => {
                    // Procedure constants (compiler-folded builtins
                    // like `quotient`, `bitwise-and`) get pushed as
                    // BuiltinRef sentinels. The matching Call N
                    // consumes them and emits a specialized RIR op.
                    if let cs_core::Value::Procedure(p) = v {
                        if let Some(name) = p.name() {
                            // Leak the name into 'static. Builtins
                            // we lower have stable static names; the
                            // leak is one-per-distinct-builtin per
                            // process and bounded.
                            let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
                            sim_stack.push(StackEntry::BuiltinRef(leaked));
                            continue;
                        }
                    }
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
                        // Free variable: emit RIR EnvLookup, which
                        // the lowerer turns into a call to the
                        // runtime helper `vm_env_lookup_fixnum`.
                        // The helper reads the closure's env from
                        // the thread-local set by `try_dispatch_jit`.
                        // Currently only Fixnum-bound free vars are
                        // supported; non-Fixnum bindings panic.
                        let dst = alloc();
                        insts.push(RirInst::EnvLookup(dst, sym.0));
                        sim_stack.push(StackEntry::Value(dst));
                    }
                }
                Inst::Pop => {
                    pop_value(&mut sim_stack)?;
                }
                Inst::SetVar(sym) => {
                    // SetVar pops one value and stores it into the
                    // binding for `sym`. After SetVar, the cs-vm
                    // compiler emits Const(Unspecified) so the
                    // result of `(set! x v)` is well-defined; we
                    // honor that by also pushing a placeholder
                    // value here. (Const(Unspecified) appears in
                    // the bytecode as the next instruction, which
                    // we'll see and emit as LoadConst.)
                    //
                    // For free-var SetVar, lower to Inst::EnvSet.
                    // Local-var SetVar isn't yet supported; we
                    // never bind locals via DefineLocal in the
                    // current translator scope, so any SetVar is a
                    // free-var update.
                    let val = pop_value(&mut sim_stack)?;
                    if param_map.contains_key(sym) {
                        return Err(TranslateError::Unsupported(
                            "set! of a parameter (mutable params not yet supported)".into(),
                        ));
                    }
                    insts.push(RirInst::EnvSet(sym.0, val));
                }
                Inst::AddFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Add)?,
                Inst::SubFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Sub)?,
                Inst::MulFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Mul)?,
                Inst::LtFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Lt)?,
                Inst::EqFx2 => emit_binop(&mut insts, &mut sim_stack, &mut alloc, RirInst::Eq)?,
                Inst::GtFx2 => {
                    // a > b  →  b < a (swap operands).
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let dst = alloc();
                    insts.push(RirInst::Lt(dst, b, a));
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::LeFx2 => {
                    // a <= b  →  NOT (b < a)  →  Eq(_, Lt(_, b, a), 0).
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let lt = alloc();
                    insts.push(RirInst::Lt(lt, b, a));
                    let zero = alloc();
                    insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                    let dst = alloc();
                    insts.push(RirInst::Eq(dst, lt, zero));
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::GeFx2 => {
                    // a >= b  →  NOT (a < b)  →  Eq(_, Lt(_, a, b), 0).
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let lt = alloc();
                    insts.push(RirInst::Lt(lt, a, b));
                    let zero = alloc();
                    insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                    let dst = alloc();
                    insts.push(RirInst::Eq(dst, lt, zero));
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::JumpIfFalse(target) => {
                    let cond = pop_value(&mut sim_stack)?;
                    let target_block = lookup_block(&offset_to_block, *target, "JumpIfFalse")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "JumpIfFalse fall")?;
                    let stack_height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        stack_height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        stack_height,
                    )?;
                    // JumpIfFalse jumps when cond is falsy. brif: cond truthy -> first, else second.
                    term = Some(Term::Branch(cond, fall_block, target_block));
                    break;
                }
                Inst::BranchOnGeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = alloc();
                    insts.push(RirInst::Lt(cond, a, b));
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnGeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnGeFx2 fall")?;
                    let height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        height,
                    )?;
                    term = Some(Term::Branch(cond, fall_block, target_block));
                    break;
                }
                Inst::BranchOnGtFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = alloc();
                    insts.push(RirInst::Lt(cond, b, a));
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnGtFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnGtFx2 fall")?;
                    let height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        height,
                    )?;
                    term = Some(Term::Branch(cond, target_block, fall_block));
                    break;
                }
                Inst::BranchOnLeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = alloc();
                    insts.push(RirInst::Lt(cond, b, a));
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnLeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnLeFx2 fall")?;
                    let height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        height,
                    )?;
                    term = Some(Term::Branch(cond, fall_block, target_block));
                    break;
                }
                Inst::BranchOnLtFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = alloc();
                    insts.push(RirInst::Lt(cond, a, b));
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnLtFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnLtFx2 fall")?;
                    let height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        height,
                    )?;
                    term = Some(Term::Branch(cond, target_block, fall_block));
                    break;
                }
                Inst::BranchOnNeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = alloc();
                    insts.push(RirInst::Eq(cond, a, b));
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnNeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnNeFx2 fall")?;
                    let height = sim_stack.len();
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        height,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        fall_block,
                        height,
                    )?;
                    term = Some(Term::Branch(cond, fall_block, target_block));
                    break;
                }
                Inst::Jump(target) => {
                    let target_block = lookup_block(&offset_to_block, *target, "Jump")?;
                    // Pull the current stack out as the jump-args.
                    // The first jump to a target seeds its block
                    // params; subsequent jumps must match the count
                    // (well-formed bytecode invariant).
                    let stack_vals: Vec<RirValue> = sim_stack
                        .iter()
                        .map(|e| match e {
                            StackEntry::Value(v) => Ok(*v),
                            StackEntry::SelfRef => Err(TranslateError::Unsupported(
                                "self-ref in Jump-arg position".into(),
                            )),
                            StackEntry::BuiltinRef(_) => Err(TranslateError::Unsupported(
                                "builtin-ref in Jump-arg position".into(),
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut alloc,
                        target_block,
                        stack_vals.len(),
                    )?;
                    term = Some(Term::Jump(target_block, stack_vals));
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
                                    StackEntry::SelfRef | StackEntry::BuiltinRef(_) => {
                                        return Err(TranslateError::Unsupported(
                                            "non-Value entry as Call arg".into(),
                                        ));
                                    }
                                }
                            }
                            let dst = alloc();
                            insts.push(RirInst::CallSelf(dst, args));
                            sim_stack.push(StackEntry::Value(dst));
                        }
                        StackEntry::BuiltinRef(name) => {
                            // Specialized lowering for known fixnum-only
                            // builtins. Unknown names fall through to
                            // Unsupported.
                            let mut args: Vec<RirValue> = Vec::with_capacity(*n);
                            for e in args_entries {
                                match e {
                                    StackEntry::Value(v) => args.push(v),
                                    _ => {
                                        return Err(TranslateError::Unsupported(
                                            "non-Value entry as builtin arg".into(),
                                        ));
                                    }
                                }
                            }
                            let dst = alloc();
                            let inst = match (name, args.len()) {
                                ("quotient", 2) => RirInst::Quotient(dst, args[0], args[1]),
                                ("remainder", 2) => RirInst::Remainder(dst, args[0], args[1]),
                                ("bitwise-and", 2) => RirInst::BitAnd(dst, args[0], args[1]),
                                ("bitwise-ior", 2) | ("bitwise-or", 2) => {
                                    RirInst::BitOr(dst, args[0], args[1])
                                }
                                ("bitwise-xor", 2) => RirInst::BitXor(dst, args[0], args[1]),
                                _ => {
                                    return Err(TranslateError::Unsupported(format!(
                                        "Call to builtin `{name}` (arity {}) not yet lowered",
                                        args.len()
                                    )));
                                }
                            };
                            insts.push(inst);
                            sim_stack.push(StackEntry::Value(dst));
                        }
                        StackEntry::Value(_) => {
                            return Err(TranslateError::Unsupported(
                                "Call with non-builtin non-self callee not yet supported".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(TranslateError::Unsupported(format!(
                        "opcode {} not yet lowered",
                        opcode_name(other)
                    )));
                }
            }
        }

        let terminator = match term {
            Some(t) => t,
            None => {
                // Implicit fall-through to the next block in offset
                // order. Pull the current stack as Jump args; seed
                // the successor's entry stack height accordingly.
                if i + 1 >= block_offsets.len() {
                    return Err(TranslateError::Invalid(format!(
                        "block at offset {} falls off function end",
                        start
                    )));
                }
                let next_id = BlockId((i + 1) as u32);
                let stack_vals: Vec<RirValue> = sim_stack
                    .iter()
                    .map(|e| match e {
                        StackEntry::Value(v) => Ok(*v),
                        StackEntry::SelfRef => Err(TranslateError::Unsupported(
                            "self-ref in fall-through stack".into(),
                        )),
                        StackEntry::BuiltinRef(_) => Err(TranslateError::Unsupported(
                            "builtin-ref in fall-through stack".into(),
                        )),
                    })
                    .collect::<Result<_, _>>()?;
                seed_block_entry(
                    &mut block_entry_stack,
                    &mut block_params,
                    &mut alloc,
                    next_id,
                    stack_vals.len(),
                )?;
                Term::Jump(next_id, stack_vals)
            }
        };

        let params = block_params.get(&block_id).cloned().unwrap_or_default();
        func.blocks.push(Block {
            id: block_id,
            params,
            insts,
            terminator,
        });
    }

    Ok(func)
}

/// One simulated stack slot. Either an already-bound RIR Value, or
/// the special `SelfRef` sentinel that `LoadVar(self_name)` pushes,
/// or a `BuiltinRef` for Const-folded builtin procedures (consumed
/// by a matching `Call N` to emit a specialized RIR op like
/// `Quotient` / `Remainder` / `BitAnd` etc.).
enum StackEntry {
    Value(RirValue),
    SelfRef,
    /// Captured at Const-of-Procedure time. The static str is the
    /// procedure's `name()`. Recognized names trigger specialized
    /// lowering at the matching Call; unrecognized names cause the
    /// translator to reject the function (Unsupported).
    BuiltinRef(&'static str),
}

fn pop_value(stack: &mut Vec<StackEntry>) -> Result<RirValue, TranslateError> {
    match stack.pop() {
        Some(StackEntry::Value(v)) => Ok(v),
        Some(StackEntry::SelfRef) => Err(TranslateError::Unsupported(
            "self-ref appears where a Value is required".into(),
        )),
        Some(StackEntry::BuiltinRef(name)) => Err(TranslateError::Unsupported(format!(
            "builtin `{name}` reference appears where a Value is required (passed to non-Call)"
        ))),
        None => Err(TranslateError::Invalid("stack underflow".into())),
    }
}

fn pop_two_values(stack: &mut Vec<StackEntry>) -> Result<(RirValue, RirValue), TranslateError> {
    let b = pop_value(stack)?;
    let a = pop_value(stack)?;
    Ok((a, b))
}

/// Short opcode name for diagnostics (Debug prints fields too,
/// which clutters error messages with closure indices etc.).
fn opcode_name(inst: &Inst) -> &'static str {
    match inst {
        Inst::Const(_) => "Const",
        Inst::LoadVar(_) => "LoadVar",
        Inst::SetVar(_) => "SetVar",
        Inst::DefineGlobal(_) => "DefineGlobal",
        Inst::DefineLocal(_) => "DefineLocal",
        Inst::Pop => "Pop",
        Inst::JumpIfFalse(_) => "JumpIfFalse",
        Inst::Jump(_) => "Jump",
        Inst::Call(_) => "Call",
        Inst::TailCall(_) => "TailCall",
        Inst::MakeClosure(_) => "MakeClosure",
        Inst::Return => "Return",
        Inst::AddFx2 => "AddFx2",
        Inst::SubFx2 => "SubFx2",
        Inst::MulFx2 => "MulFx2",
        Inst::LtFx2 => "LtFx2",
        Inst::LeFx2 => "LeFx2",
        Inst::GtFx2 => "GtFx2",
        Inst::GeFx2 => "GeFx2",
        Inst::EqFx2 => "EqFx2",
        Inst::BranchOnGeFx2(_) => "BranchOnGeFx2",
        Inst::BranchOnGtFx2(_) => "BranchOnGtFx2",
        Inst::BranchOnLeFx2(_) => "BranchOnLeFx2",
        Inst::BranchOnLtFx2(_) => "BranchOnLtFx2",
        Inst::BranchOnNeFx2(_) => "BranchOnNeFx2",
    }
}

fn lookup_block(
    map: &HashMap<usize, BlockId>,
    off: usize,
    label: &str,
) -> Result<BlockId, TranslateError> {
    map.get(&off)
        .copied()
        .ok_or_else(|| TranslateError::Invalid(format!("{label}: offset {off} not a block start")))
}

/// Seed a target block's entry stack with `count` fresh RIR Values
/// (allocated via `alloc`), or — if the target was already seeded
/// — verify that the count matches.
fn seed_block_entry(
    entry_stack: &mut HashMap<BlockId, Vec<RirValue>>,
    block_params: &mut HashMap<BlockId, Vec<(RirValue, Type)>>,
    alloc: &mut impl FnMut() -> RirValue,
    target: BlockId,
    count: usize,
) -> Result<(), TranslateError> {
    if let Some(existing) = entry_stack.get(&target) {
        if existing.len() != count {
            return Err(TranslateError::Invalid(format!(
                "block {:?} seeded with {} entries, predecessor wants {}",
                target,
                existing.len(),
                count
            )));
        }
        return Ok(());
    }
    let vals: Vec<RirValue> = (0..count).map(|_| alloc()).collect();
    let params: Vec<(RirValue, Type)> = vals.iter().map(|v| (*v, Type::Fixnum)).collect();
    entry_stack.insert(target, vals);
    block_params.insert(target, params);
    Ok(())
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
    fn loadvar_of_free_var_emits_envlookup() {
        // Free-var LoadVar now lowers to Inst::EnvLookup (M6 Phase 2
        // iter B). Translator accepts; the lowerer emits a Cranelift
        // call to vm_env_lookup_fixnum.
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
        let f = bytecode_to_rir(&lam, "f", None).expect("free-var LoadVar should translate");
        // Look for the EnvLookup in block 0's insts.
        let has_envlookup = f.blocks[0]
            .insts
            .iter()
            .any(|i| matches!(i, RirInst::EnvLookup(_, _)));
        assert!(has_envlookup, "expected EnvLookup, got {:?}", f.blocks[0]);
    }

    #[test]
    fn unsupported_general_call_rejected() {
        let mut syms = SymbolTable::new();
        let g = syms.intern("g");
        // (g 1) — calls non-self. The LoadVar(g) succeeds (becomes
        // EnvLookup); the Call non-self is what rejects.
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
                msg.contains("non-self") || msg.contains("Call"),
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
