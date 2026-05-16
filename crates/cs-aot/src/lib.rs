//! Ahead-of-time compiler for CrabScheme.
//!
//! Consumes a `cs-rir::Function` and emits Rust source. The Rust
//! source compiles to native code via the standard cargo toolchain;
//! the same `cs-rir` IR feeds both the JIT (Cranelift → native
//! bytes) and the AOT (cs-aot → Rust source → rustc → native bytes).
//!
//! ## Status: M10 Track A iter 1 (skeleton)
//!
//! - Single-block functions only (multi-block lands in A2 via
//!   loop+match block dispatch).
//! - Supported Inst variants this iter: `LoadConst(Fixnum)`,
//!   `Add` / `Sub` / `Mul` on i64. Other variants → `AotError::Unsupported`.
//! - All-i64 ABI: each SSA value maps to a Rust `i64`. NanboxValue
//!   carriers + non-Fixnum types arrive in A2.
//! - The emitter does not invoke `rustc`; it returns the source as a
//!   `String`. Callers (tests, the future `cs-aot-cli`) feed that to
//!   their build system.
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

    /// Iter-1 scope is single-block only; the supplied function has
    /// more than one block. A2 lifts this restriction.
    MultipleBlocks { count: usize },

    /// Encountered an `Inst` variant the iter-1 emitter doesn't yet
    /// handle. The string is the variant name for diagnostics.
    UnsupportedInst(&'static str),

    /// Encountered a `Term` variant the iter-1 emitter doesn't yet
    /// handle. Iter 1 only handles `Return`; iter 2 adds Jump/Branch.
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
            AotError::MultipleBlocks { count } => write!(
                f,
                "cs-aot: iter-1 supports single-block functions only (got {count})"
            ),
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
/// Iter-1 scope per the module doc. See [`AotError`] for the exact
/// rejection set.
pub fn emit(func: &Function) -> Result<String, AotError> {
    // ---- Function-level validation -------------------------------
    if func.blocks.is_empty() {
        return Err(AotError::EmptyFunction);
    }
    if func.blocks.len() > 1 {
        return Err(AotError::MultipleBlocks {
            count: func.blocks.len(),
        });
    }
    if func.return_type != Type::Fixnum {
        return Err(AotError::UnsupportedReturnType);
    }
    for (_, ty) in &func.params {
        if *ty != Type::Fixnum {
            return Err(AotError::UnsupportedParamType);
        }
    }

    let block = &func.blocks[0];

    // ---- Build the source --------------------------------------
    let mut out = String::new();
    let fn_name = sanitize_ident(&func.name);

    // Doc comment so the AOT-emitted source carries provenance
    // when read by humans (the `cargo doc` of an AOT'd crate
    // surfaces the original Scheme procedure names).
    writeln!(out, "/// AOT-emitted from cs-rir Function `{}`.", func.name).unwrap();
    writeln!(out, "///").unwrap();
    writeln!(
        out,
        "/// {} param(s) · {} inst(s) · single-block · all-i64 ABI",
        func.params.len(),
        block.insts.len()
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

    // Track defined SSA values so we can reject use-before-def.
    let mut defined: HashSet<Value> = func.params.iter().map(|(v, _)| *v).collect();
    // Track block params too (iter-1 has 1 block whose params are
    // always empty for a Return-terminated entry, but defensive).
    for (v, _) in &block.params {
        defined.insert(*v);
    }

    // ---- Emit each instruction ---------------------------------
    for inst in &block.insts {
        match inst {
            Inst::LoadConst(dst, c) => {
                let lit = const_to_rust_i64(c)?;
                writeln!(out, "    let v{}: i64 = {};", dst.0, lit).unwrap();
                defined.insert(*dst);
            }
            Inst::Add(dst, lhs, rhs) => {
                require_defined(&defined, *lhs)?;
                require_defined(&defined, *rhs)?;
                writeln!(
                    out,
                    "    let v{}: i64 = v{}.wrapping_add(v{});",
                    dst.0, lhs.0, rhs.0
                )
                .unwrap();
                defined.insert(*dst);
            }
            Inst::Sub(dst, lhs, rhs) => {
                require_defined(&defined, *lhs)?;
                require_defined(&defined, *rhs)?;
                writeln!(
                    out,
                    "    let v{}: i64 = v{}.wrapping_sub(v{});",
                    dst.0, lhs.0, rhs.0
                )
                .unwrap();
                defined.insert(*dst);
            }
            Inst::Mul(dst, lhs, rhs) => {
                require_defined(&defined, *lhs)?;
                require_defined(&defined, *rhs)?;
                writeln!(
                    out,
                    "    let v{}: i64 = v{}.wrapping_mul(v{});",
                    dst.0, lhs.0, rhs.0
                )
                .unwrap();
                defined.insert(*dst);
            }
            other => return Err(AotError::UnsupportedInst(inst_variant_name(other))),
        }
    }

    // ---- Terminator -------------------------------------------
    match &block.terminator {
        Term::Return(v) => {
            require_defined(&defined, *v)?;
            writeln!(out, "    v{}", v.0).unwrap();
        }
        Term::Jump(_, _) => return Err(AotError::UnsupportedTerm("Jump")),
        Term::Branch(_, _, _) => return Err(AotError::UnsupportedTerm("Branch")),
    }

    out.push_str("}\n");
    Ok(out)
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
    fn rejects_multiple_blocks() {
        let mut f = Function::new("multi");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(BlockId(1), vec![]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(0))],
            terminator: Term::Return(Value(0)),
        });
        match emit(&f) {
            Err(AotError::MultipleBlocks { count }) => assert_eq!(count, 2),
            other => panic!("expected MultipleBlocks, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_inst() {
        // Inst::Lt isn't handled in iter 1.
        let mut f = Function::new("less");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Lt(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        // Use Fixnum return type so we pass the return-type check.
        f.return_type = Type::Fixnum;
        assert_eq!(emit(&f), Err(AotError::UnsupportedInst("Lt")));
    }

    #[test]
    fn rejects_unsupported_terminator() {
        let mut f = Function::new("br");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(BlockId(0), vec![]),
        });
        assert_eq!(emit(&f), Err(AotError::UnsupportedTerm("Jump")));
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
