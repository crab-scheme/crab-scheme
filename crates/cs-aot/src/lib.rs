//! Ahead-of-time compiler for CrabScheme.
//!
//! Consumes a `cs-rir::Function` and emits Rust source. The Rust
//! source compiles to native code via the standard cargo toolchain;
//! the same `cs-rir` IR feeds both the JIT (Cranelift → native
//! bytes) and the AOT (cs-aot → Rust source → rustc → native bytes).
//!
//! ## Status: M10 Track A iter 2a (multi-block + comparisons)
//!
//! - **Multi-block**: functions with branching / loops now compile.
//!   The emitter falls into one of two shapes:
//!   - **Straight-line shape** (single block, `Return` terminator):
//!     emits `let v_N = ...;` lines and a tail `v_R`. Preserves the
//!     iter-1 output exactly for the simple case.
//!   - **Loop+match shape** (anything else): pre-declares every
//!     non-param SSA value as `let mut v_N: i64 = 0;` at function
//!     top, then dispatches via a `loop { match block { ... } }`
//!     state machine. `Jump(target, args)` assigns block params and
//!     `continue`s; `Branch(cond, t, e)` reads `cond != 0` and
//!     dispatches; `Return(v)` exits the loop via `return v_N;`.
//! - **Supported Inst variants** (added in iter 2a):
//!   - `LoadConst(Fixnum)`, `Add`, `Sub`, `Mul` (iter 1)
//!   - `Lt`, `Eq` — comparisons return 0/1 as i64, matching the
//!     JIT's `emit_nb_cmp_fixnum_fast` shape.
//!   - `Move` — SSA alias copy.
//! - **All-i64 ABI** remains. NanboxValue carriers + non-Fixnum
//!   types still arrive in A2b.
//! - The emitter does not invoke `rustc`; it returns the source as a
//!   `String`. Callers feed that to their build system.
//!
//! ## Invariants & contracts
//!
//! - Output is `#![deny(unsafe_code)]` clean — no unsafe blocks in
//!   emitted code (AOT is meant to be auditable).
//! - Output parses as a valid `syn::File`; the AOT tests assert this
//!   for every emitted snippet, separately from semantic tests that
//!   actually `rustc` the source.
//! - Function naming: `pub extern "C" fn {sanitized(name)}(arg0: i64,
//!   ...) -> i64`. The `extern "C"` matches the JIT's outer-trampoline
//!   signature so the runtime's dispatch path can call AOT'd
//!   procedures the same way it calls JIT'd ones.

#![deny(unsafe_code)]

use std::collections::HashSet;
use std::fmt::Write;

use cs_rir::{Const, Function, Inst, Term, Type, Value};

/// Errors the AOT emitter can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AotError {
    /// Function has zero blocks (defensive — shouldn't happen for
    /// well-formed RIR).
    EmptyFunction,

    /// Encountered an `Inst` variant the emitter doesn't yet handle.
    /// The string is the variant name for diagnostics. See the
    /// module doc for the supported set.
    UnsupportedInst(&'static str),

    /// Encountered a `Term` variant the emitter doesn't yet handle.
    /// All current Term variants (Return / Jump / Branch) are
    /// supported in iter 2a; reserved for future Term additions.
    UnsupportedTerm(&'static str),

    /// A `Const::Fixnum` value is the only constant flavor iter 1
    /// emits; other variants reach this arm.
    UnsupportedConst(&'static str),

    /// The function's `return_type` isn't one the iter-1 ABI can
    /// represent (only `Fixnum` for now).
    UnsupportedReturnType,

    /// A param's declared type isn't Fixnum.
    UnsupportedParamType,

    /// SSA value referenced before definition. Indicates malformed
    /// input RIR.
    UndefinedValue(Value),
}

impl std::fmt::Display for AotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AotError::EmptyFunction => write!(f, "cs-aot: function has no blocks"),
            AotError::UnsupportedInst(name) => {
                write!(f, "cs-aot: Inst::{name} not yet supported (iter 1)")
            }
            AotError::UnsupportedTerm(name) => {
                write!(f, "cs-aot: Term::{name} not yet supported (iter 1)")
            }
            AotError::UnsupportedConst(name) => {
                write!(f, "cs-aot: Const::{name} not yet supported (iter 1)")
            }
            AotError::UnsupportedReturnType => {
                write!(f, "cs-aot: return type other than Fixnum not yet supported")
            }
            AotError::UnsupportedParamType => {
                write!(f, "cs-aot: param type other than Fixnum not yet supported")
            }
            AotError::UndefinedValue(v) => write!(f, "cs-aot: value v{} used before defined", v.0),
        }
    }
}

impl std::error::Error for AotError {}

/// Emit Rust source for `func`. The result is a complete
/// definition: one `pub extern "C" fn` matching the JIT's outer-
/// trampoline signature.
///
/// See the module doc for supported variants and the two output
/// shapes (straight-line vs loop+match).
pub fn emit(func: &Function) -> Result<String, AotError> {
    // ---- Function-level validation -------------------------------
    if func.blocks.is_empty() {
        return Err(AotError::EmptyFunction);
    }
    if func.return_type != Type::Fixnum {
        return Err(AotError::UnsupportedReturnType);
    }
    for (_, ty) in &func.params {
        if *ty != Type::Fixnum {
            return Err(AotError::UnsupportedParamType);
        }
    }

    // ---- Build the source --------------------------------------
    let mut out = String::new();
    let fn_name = sanitize_ident(&func.name);

    // Decide which shape to emit. Straight-line is reserved for the
    // simplest case (1 block, Return terminator) so iter-1 output is
    // preserved exactly for the snapshot tests + readability.
    let straight_line = func.blocks.len() == 1
        && matches!(func.blocks[0].terminator, Term::Return(_))
        && func.blocks[0].params.is_empty();

    // Doc comment: provenance + shape annotation so humans reading
    // the AOT'd source know which emitter path produced it.
    writeln!(out, "/// AOT-emitted from cs-rir Function `{}`.", func.name).unwrap();
    writeln!(out, "///").unwrap();
    writeln!(
        out,
        "/// {} param(s) · {} block(s) · {} inst(s) · {} · all-i64 ABI",
        func.params.len(),
        func.blocks.len(),
        func.blocks.iter().map(|b| b.insts.len()).sum::<usize>(),
        if straight_line {
            "straight-line"
        } else {
            "loop+match"
        }
    )
    .unwrap();

    // Function header. `extern "C"` matches the JIT's outer
    // trampoline so the runtime's dispatch path can `transmute` the
    // AOT'd fn pointer just like a JIT'd one.
    write!(out, "pub extern \"C\" fn {fn_name}(").unwrap();
    for (i, (v, _ty)) in func.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write!(out, "v{}: i64", v.0).unwrap();
    }
    out.push_str(") -> i64 {\n");

    if straight_line {
        emit_straight_line(&mut out, func)?;
    } else {
        emit_loop_match(&mut out, func)?;
    }

    out.push_str("}\n");
    Ok(out)
}

/// Iter-1 shape: emit each Inst as a `let v_N = expr;` then the
/// Return value as a trailing tail expression. Preserves snapshot-
/// test output for simple functions and is more readable when the
/// CFG is degenerate.
fn emit_straight_line(out: &mut String, func: &Function) -> Result<(), AotError> {
    let block = &func.blocks[0];
    let mut defined: HashSet<Value> = func.params.iter().map(|(v, _)| *v).collect();

    for inst in &block.insts {
        emit_inst_let(out, inst, &defined)?;
        if let Some(dst) = inst_dst(inst) {
            defined.insert(dst);
        }
    }

    if let Term::Return(v) = &block.terminator {
        require_defined(&defined, *v)?;
        writeln!(out, "    v{}", v.0).unwrap();
        Ok(())
    } else {
        // Should be unreachable given straight_line gate, but
        // defensive.
        Err(AotError::UnsupportedTerm(term_variant_name(
            &block.terminator,
        )))
    }
}

/// Iter-2a shape: pre-declare every non-param SSA Value as
/// `let mut v_N: i64 = 0;` at function top, then run a
/// `loop { match block { ... } }` state machine for block dispatch.
/// Verbose but always correct for arbitrary CFGs without needing
/// liveness analysis.
fn emit_loop_match(out: &mut String, func: &Function) -> Result<(), AotError> {
    // Pre-declare every Value that ISN'T already a function
    // parameter. Block params and Inst destinations all go here.
    // Using `let mut` + assignment (not `let`) means a Value defined
    // in one block can be read in another without scope issues.
    let params: HashSet<Value> = func.params.iter().map(|(v, _)| *v).collect();
    let mut all_values: Vec<Value> = Vec::new();
    for block in &func.blocks {
        for (v, _) in &block.params {
            if !params.contains(v) {
                all_values.push(*v);
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst_dst(inst) {
                if !params.contains(&dst) {
                    all_values.push(dst);
                }
            }
        }
    }
    all_values.sort_by_key(|v| v.0);
    all_values.dedup();

    for v in &all_values {
        writeln!(out, "    let mut v{}: i64 = 0;", v.0).unwrap();
    }

    // Dispatch state machine. `block` carries the current block id;
    // `loop { match block { ... } }` runs forever, with each block
    // arm either jumping (`block = ...; continue;`) or returning.
    writeln!(out, "    let mut block: u32 = {};", func.entry.0).unwrap();
    writeln!(out, "    loop {{").unwrap();
    writeln!(out, "        match block {{").unwrap();

    for block in &func.blocks {
        writeln!(out, "            {} => {{", block.id.0).unwrap();
        for inst in &block.insts {
            emit_inst_assign(out, inst)?;
        }
        emit_terminator(out, &block.terminator, func)?;
        writeln!(out, "            }}").unwrap();
    }

    // unreachable!() is the safety net: well-formed RIR never
    // dispatches to a non-existent block id, but rustc demands a
    // catch-all for non-exhaustive integer matches.
    writeln!(out, "            _ => unreachable!(),").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    Ok(())
}

/// Emit an Inst as `let v_N: i64 = expr;` (straight-line shape).
/// Used only by `emit_straight_line`; the loop+match shape uses
/// `emit_inst_assign` instead (assignment to pre-declared mut).
fn emit_inst_let(out: &mut String, inst: &Inst, defined: &HashSet<Value>) -> Result<(), AotError> {
    let (dst, expr) = inst_rhs(inst, Some(defined))?;
    writeln!(out, "    let v{}: i64 = {};", dst.0, expr).unwrap();
    Ok(())
}

/// Emit an Inst as `v_N = expr;` (loop+match shape). The Value is
/// pre-declared as `let mut` at function top.
fn emit_inst_assign(out: &mut String, inst: &Inst) -> Result<(), AotError> {
    // The loop+match shape pre-declares all values; SSA validity is
    // a property of well-formed RIR, not something we re-check here
    // (the check requires cross-block dataflow analysis we don't yet
    // do in the emitter). Pass `None` to skip.
    let (dst, expr) = inst_rhs(inst, None)?;
    writeln!(out, "                v{} = {};", dst.0, expr).unwrap();
    Ok(())
}

/// Compute the (dst, RHS-expression) pair for an Inst. The expression
/// is in Rust source form ready to be assigned. When `defined` is
/// `Some(set)`, perform use-before-def detection against it; when
/// `None`, skip (the loop+match shape uses this).
fn inst_rhs(inst: &Inst, defined: Option<&HashSet<Value>>) -> Result<(Value, String), AotError> {
    let check = |v: Value| -> Result<(), AotError> {
        match defined {
            Some(set) if !set.contains(&v) => Err(AotError::UndefinedValue(v)),
            _ => Ok(()),
        }
    };

    Ok(match inst {
        Inst::LoadConst(dst, c) => (*dst, const_to_rust_i64(c)?),
        Inst::Add(dst, lhs, rhs) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_add(v{})", lhs.0, rhs.0))
        }
        Inst::Sub(dst, lhs, rhs) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_sub(v{})", lhs.0, rhs.0))
        }
        Inst::Mul(dst, lhs, rhs) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_mul(v{})", lhs.0, rhs.0))
        }
        Inst::Lt(dst, lhs, rhs) => {
            // Match the JIT's `emit_nb_cmp_fixnum_fast` shape:
            // comparison returns 0/1 as an i64 (the bare integer,
            // pre-NB-tag — A2b adds the NB Boolean tag).
            check(*lhs)?;
            check(*rhs)?;
            (
                *dst,
                format!("if v{} < v{} {{ 1 }} else {{ 0 }}", lhs.0, rhs.0),
            )
        }
        Inst::Eq(dst, lhs, rhs) => {
            check(*lhs)?;
            check(*rhs)?;
            (
                *dst,
                format!("if v{} == v{} {{ 1 }} else {{ 0 }}", lhs.0, rhs.0),
            )
        }
        Inst::Move(dst, src) => {
            check(*src)?;
            (*dst, format!("v{}", src.0))
        }
        other => return Err(AotError::UnsupportedInst(inst_variant_name(other))),
    })
}

/// Emit the terminator for a block in the loop+match shape.
fn emit_terminator(out: &mut String, term: &Term, func: &Function) -> Result<(), AotError> {
    match term {
        Term::Return(v) => {
            writeln!(out, "                return v{};", v.0).unwrap();
        }
        Term::Jump(target, args) => {
            // Assign target block's params from `args` before
            // jumping. RIR contract: args.len() == target.params.len().
            let target_block = func
                .blocks
                .iter()
                .find(|b| b.id == *target)
                .ok_or(AotError::UnsupportedTerm("Jump to unknown block"))?;
            if args.len() != target_block.params.len() {
                // Malformed RIR: arg/param count mismatch.
                return Err(AotError::UnsupportedTerm(
                    "Jump arity mismatch with target block params",
                ));
            }
            for (arg_v, (param_v, _ty)) in args.iter().zip(target_block.params.iter()) {
                writeln!(out, "                v{} = v{};", param_v.0, arg_v.0).unwrap();
            }
            writeln!(out, "                block = {};", target.0).unwrap();
            writeln!(out, "                continue;").unwrap();
        }
        Term::Branch(cond, then_target, else_target) => {
            // The JIT (and our Lt/Eq emission) produces 0/1 as i64;
            // any non-zero value is truthy, matching cs-vm's NB
            // brif semantics (NB Boolean payload's low bit).
            writeln!(out, "                if v{} != 0 {{", cond.0).unwrap();
            writeln!(out, "                    block = {};", then_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }} else {{").unwrap();
            writeln!(out, "                    block = {};", else_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }}").unwrap();
        }
    }
    Ok(())
}

/// Return the destination Value an Inst writes, if any. Used for
/// pre-declaring all non-param Values in the loop+match shape.
fn inst_dst(inst: &Inst) -> Option<Value> {
    match inst {
        Inst::LoadConst(v, _) => Some(*v),
        Inst::Add(v, _, _) | Inst::Sub(v, _, _) | Inst::Mul(v, _, _) => Some(*v),
        Inst::Lt(v, _, _) | Inst::Eq(v, _, _) => Some(*v),
        Inst::Move(v, _) => Some(*v),
        // Any inst not in the supported set: returning None is fine
        // because the emitter rejects unsupported variants before
        // pre-declaration anyway. Keeps this helper lean.
        _ => None,
    }
}

fn term_variant_name(term: &Term) -> &'static str {
    match term {
        Term::Return(_) => "Return",
        Term::Jump(_, _) => "Jump",
        Term::Branch(_, _, _) => "Branch",
    }
}

/// Sanitize a Scheme procedure name into a valid Rust identifier.
/// Replaces non-alphanumeric characters with underscores; prepends
/// `proc_` if the result would start with a digit. Empty names
/// become `proc_anon`.
fn sanitize_ident(name: &str) -> String {
    if name.is_empty() {
        return "proc_anon".to_string();
    }
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("proc_{s}");
    }
    s
}

fn const_to_rust_i64(c: &Const) -> Result<String, AotError> {
    match c {
        Const::Fixnum(n) => {
            // i64::MIN renders as e.g. `-9223372036854775808i64`;
            // Rust parses negative literals as a unary `-` over the
            // positive literal, which doesn't work for i64::MIN.
            // Round-trip via the `i64` suffix and bit pattern when
            // MIN is involved.
            if *n == i64::MIN {
                Ok("i64::MIN".to_string())
            } else {
                Ok(format!("{n}i64"))
            }
        }
        Const::Boolean(_) => Err(AotError::UnsupportedConst("Boolean")),
        Const::Flonum(_) => Err(AotError::UnsupportedConst("Flonum")),
        Const::Character(_) => Err(AotError::UnsupportedConst("Character")),
        Const::Null => Err(AotError::UnsupportedConst("Null")),
        Const::Unspecified => Err(AotError::UnsupportedConst("Unspecified")),
        Const::Eof => Err(AotError::UnsupportedConst("Eof")),
        Const::Symbol(_) => Err(AotError::UnsupportedConst("Symbol")),
        Const::StringRef(_) => Err(AotError::UnsupportedConst("StringRef")),
    }
}

fn require_defined(defined: &HashSet<Value>, v: Value) -> Result<(), AotError> {
    if defined.contains(&v) {
        Ok(())
    } else {
        Err(AotError::UndefinedValue(v))
    }
}

fn inst_variant_name(inst: &Inst) -> &'static str {
    // Iter-1 only needs names for the unsupported variants the
    // analyzer might reach. We list the common shapes; anything
    // else falls through to "<other>" — diagnostic-only, not a
    // correctness path.
    match inst {
        Inst::LoadConst(..) => "LoadConst",
        Inst::Add(..) => "Add",
        Inst::Sub(..) => "Sub",
        Inst::Mul(..) => "Mul",
        Inst::Div(..) => "Div",
        Inst::FlonumAdd(..) => "FlonumAdd",
        Inst::FlonumSub(..) => "FlonumSub",
        Inst::FlonumMul(..) => "FlonumMul",
        Inst::FlonumDiv(..) => "FlonumDiv",
        Inst::Lt(..) => "Lt",
        Inst::Eq(..) => "Eq",
        Inst::Call(..) => "Call",
        Inst::CallSelf(..) => "CallSelf",
        Inst::CallGeneral(..) => "CallGeneral",
        Inst::EnvLookup(..) => "EnvLookup",
        Inst::EnvLookupAny(..) => "EnvLookupAny",
        Inst::EnvSet(..) => "EnvSet",
        Inst::EnvDefineLocal(..) => "EnvDefineLocal",
        Inst::MakeClosure(..) => "MakeClosure",
        Inst::VecAlloc(..) => "VecAlloc",
        Inst::VecRef(..) => "VecRef",
        Inst::VecSet(..) => "VecSet",
        Inst::BoxTyped(..) => "BoxTyped",
        _ => "<other>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_rir::{Block, BlockId, Const, Inst, Term, Type, Value};

    /// Helper: a single-block function `(define (sq x) (* x x))`.
    fn sq_function() -> Function {
        let mut f = Function::new("sq");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Mul(Value(1), Value(0), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        f
    }

    /// Helper: `(define (add3 a b c) (+ (+ a b) c))`.
    fn add3_function() -> Function {
        let mut f = Function::new("add3");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.params.push((Value(2), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Add(Value(3), Value(0), Value(1)),
                Inst::Add(Value(4), Value(3), Value(2)),
            ],
            terminator: Term::Return(Value(4)),
        });
        f
    }

    #[test]
    fn emits_sq() {
        let src = emit(&sq_function()).unwrap();
        // Smoke: contains the expected pieces.
        assert!(src.contains("pub extern \"C\" fn sq(v0: i64) -> i64"));
        assert!(src.contains("let v1: i64 = v0.wrapping_mul(v0);"));
        assert!(src.contains("    v1\n"));
    }

    #[test]
    fn emits_add3() {
        let src = emit(&add3_function()).unwrap();
        assert!(src.contains("pub extern \"C\" fn add3(v0: i64, v1: i64, v2: i64) -> i64"));
        assert!(src.contains("let v3: i64 = v0.wrapping_add(v1);"));
        assert!(src.contains("let v4: i64 = v3.wrapping_add(v2);"));
        assert!(src.contains("    v4\n"));
    }

    #[test]
    fn emits_loadconst() {
        let mut f = Function::new("answer");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(42))],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("pub extern \"C\" fn answer() -> i64"));
        assert!(src.contains("let v0: i64 = 42i64;"));
    }

    #[test]
    fn loadconst_i64_min_uses_const_to_avoid_unary_neg() {
        // `-9223372036854775808i64` is a literal Rust rejects (the
        // negation overflows i64). Emit via `i64::MIN` instead.
        let mut f = Function::new("min_val");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(i64::MIN))],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("let v0: i64 = i64::MIN;"));
    }

    #[test]
    fn emitted_source_parses_as_rust() {
        // The most important non-runtime correctness check: every
        // emitter output must be valid Rust. We parse via `syn`;
        // anything malformed gets caught here before users hit a
        // confusing rustc error inside their AOT'd binary.
        for src in [
            emit(&sq_function()).unwrap(),
            emit(&add3_function()).unwrap(),
        ] {
            let parsed = syn::parse_file(&src);
            assert!(
                parsed.is_ok(),
                "emitted source failed to parse as Rust:\n--- begin ---\n{src}--- end ---\nerror: {:?}",
                parsed.err()
            );
        }
    }

    #[test]
    fn rejects_empty_function() {
        let f = Function::new("empty");
        assert_eq!(emit(&f), Err(AotError::EmptyFunction));
    }

    #[test]
    fn multi_block_jump_emits_loop_match() {
        // Two-block trivial: entry jumps to block 1 which returns 7.
        // Exercises Jump(no args) + the loop+match shape.
        let mut f = Function::new("jump_to_return");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(7))],
            terminator: Term::Jump(BlockId(1), vec![Value(0)]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![(Value(1), Type::Fixnum)],
            insts: vec![],
            terminator: Term::Return(Value(1)),
        });
        let src = emit(&f).unwrap();
        // Multi-block always uses loop+match shape.
        assert!(src.contains("let mut v0: i64 = 0;"));
        assert!(src.contains("let mut v1: i64 = 0;"));
        assert!(src.contains("let mut block: u32 = 0;"));
        assert!(src.contains("loop {"));
        assert!(src.contains("match block {"));
        assert!(src.contains("0 => {"));
        assert!(src.contains("v0 = 7i64;"));
        // Jump assigns block param then dispatches.
        assert!(src.contains("v1 = v0;"));
        assert!(src.contains("block = 1;"));
        assert!(src.contains("continue;"));
        assert!(src.contains("1 => {"));
        assert!(src.contains("return v1;"));
        assert!(src.contains("_ => unreachable!(),"));
    }

    #[test]
    fn branch_emits_if_else_dispatch() {
        // (define (abs-ish x) (if (< x 0) (- 0 x) x))
        // Translated to RIR with Branch:
        //   block 0: v1 = 0; v2 = v0 < v1; Branch(v2, 1, 2)
        //   block 1: v3 = 0; v4 = v3 - v0; Return v4
        //   block 2: Return v0
        let mut f = Function::new("abs_ish");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(1), Const::Fixnum(0)),
                Inst::Lt(Value(2), Value(0), Value(1)),
            ],
            terminator: Term::Branch(Value(2), BlockId(1), BlockId(2)),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(3), Const::Fixnum(0)),
                Inst::Sub(Value(4), Value(3), Value(0)),
            ],
            terminator: Term::Return(Value(4)),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("v2 = if v0 < v1 { 1 } else { 0 };"));
        assert!(src.contains("if v2 != 0 {"));
        assert!(src.contains("block = 1;"));
        assert!(src.contains("block = 2;"));
    }

    #[test]
    fn lt_and_eq_emit_zero_one() {
        let mut f = Function::new("cmp");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Lt(Value(2), Value(0), Value(1)),
                Inst::Eq(Value(3), Value(0), Value(1)),
                Inst::Add(Value(4), Value(2), Value(3)),
            ],
            terminator: Term::Return(Value(4)),
        });
        let src = emit(&f).unwrap();
        // Single-block + Return uses straight-line shape.
        assert!(src.contains("let v2: i64 = if v0 < v1 { 1 } else { 0 };"));
        assert!(src.contains("let v3: i64 = if v0 == v1 { 1 } else { 0 };"));
    }

    #[test]
    fn move_emits_alias() {
        let mut f = Function::new("alias");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Move(Value(1), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("let v1: i64 = v0;"));
    }

    #[test]
    fn rejects_unsupported_inst() {
        // Inst::Div isn't yet handled (no plain integer-divide
        // emitted yet; A2b adds it via NB-aware helper).
        let mut f = Function::new("div");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Div(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        f.return_type = Type::Fixnum;
        assert_eq!(emit(&f), Err(AotError::UnsupportedInst("Div")));
    }

    #[test]
    fn rejects_use_before_def() {
        let mut f = Function::new("bad");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Add(Value(0), Value(99), Value(99))],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(emit(&f), Err(AotError::UndefinedValue(Value(99))));
    }

    #[test]
    fn rejects_non_fixnum_return_type() {
        let mut f = Function::new("flo");
        f.return_type = Type::Flonum;
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(0))],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(emit(&f), Err(AotError::UnsupportedReturnType));
    }

    #[test]
    fn sanitize_handles_scheme_names() {
        assert_eq!(sanitize_ident("foo"), "foo");
        assert_eq!(sanitize_ident("matrix-elt"), "matrix_elt");
        assert_eq!(sanitize_ident("list?"), "list_");
        assert_eq!(sanitize_ident("set!"), "set_");
        assert_eq!(sanitize_ident("string->list"), "string__list");
        assert_eq!(sanitize_ident(""), "proc_anon");
        assert_eq!(sanitize_ident("1plus"), "proc_1plus");
    }
}
