//! CrabScheme JIT-backend IR (RIR — Rust IR).
//!
//! Lowered from `cs-ir` (the existing `CoreExpr` / bytecode source) and
//! consumed by every JIT backend (`cs-jit-cranelift`, future
//! `cs-jit-holy`). Backend-agnostic: SSA-shaped values, basic blocks,
//! terminator-style control flow, with each opcode documented against
//! its `cs-vm` bytecode equivalent so the differential test in the M6
//! spec FR-5 reduces to per-instruction equivalence.
//!
//! See `.spec-workflow/specs/jit-cranelift/design.md` for the design
//! and `docs/adr/0007-jit-design.md` for the architecture decisions.

#![deny(unsafe_code)]

/// SSA value identifier within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(pub u32);

/// Basic-block identifier within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// Coarse type tag carried alongside each SSA value. The JIT uses
/// these for type-specialization: a `Fixnum`-tagged value can use
/// integer ops directly; an `Any`-tagged value must dispatch
/// dynamically.
///
/// Tags don't have to be precise — the deopt machinery catches the
/// case where a value's actual type at runtime contradicts its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// A fixnum (i64). Direct register arithmetic possible.
    Fixnum,
    /// A flonum (f64). Direct FP arithmetic possible.
    Flonum,
    /// A boolean.
    Boolean,
    /// A character (u32 codepoint).
    Character,
    /// Heap-pointer to a Pair.
    Pair,
    /// Heap-pointer to a Vector.
    Vector,
    /// Heap-pointer to a String.
    String,
    /// Heap-pointer to a ByteVector.
    ByteVector,
    /// Heap-pointer to a Procedure (closure or builtin).
    Procedure,
    /// Type unknown at compile time — must do runtime dispatch.
    Any,
}

/// Compile-time literal. Materialized as `LoadConst`.
#[derive(Debug, Clone)]
pub enum Const {
    Fixnum(i64),
    Flonum(f64),
    Boolean(bool),
    Character(char),
    Null,
    Unspecified,
    Eof,
    /// Symbol id from the runtime's symbol table; emitted as an i32.
    Symbol(u32),
    /// Static-string-table index. The JIT loads via a `static`-table
    /// indirection so we don't bake string content into native code.
    StringRef(u32),
}

/// One RIR instruction. Each variant cites the equivalent `cs-vm`
/// bytecode opcode; the differential test asserts they produce
/// identical results.
#[derive(Debug, Clone)]
pub enum Inst {
    /// `dst = const`. cs-vm: `Inst::Const`.
    LoadConst(Value, Const),

    /// `dst = lhs + rhs`. cs-vm: `Inst::Add`.
    /// Type-stable variant: both operands tagged Fixnum or Flonum;
    /// guard inserted by the lowerer if not.
    Add(Value, Value, Value),

    /// `dst = lhs - rhs`. cs-vm: `Inst::Sub`.
    Sub(Value, Value, Value),

    /// `dst = lhs * rhs`. cs-vm: `Inst::Mul`.
    Mul(Value, Value, Value),

    /// `dst = (lhs < rhs)`. cs-vm: `Inst::Lt`.
    Lt(Value, Value, Value),

    /// `dst = (lhs == rhs)`. cs-vm: `Inst::Eq`.
    Eq(Value, Value, Value),

    /// `dst = call(callee, args...)`. cs-vm: `Inst::Call`.
    /// `callee` is a Value of type Procedure; the JIT specializes on
    /// the procedure identity if the type-feedback is monomorphic.
    Call(Value, Value, Vec<Value>),

    /// `dst = call_self(args...)`. Recursive call to the function
    /// being compiled. cs-vm: `Inst::Call` with a callee that the
    /// monomorphic feedback resolved to "self". This dedicated form
    /// lets iter-4b lower self-recursion (fib, fact, etc.) without
    /// the general procedure-value lookup that lands later.
    CallSelf(Value, Vec<Value>),

    /// `dst = env_lookup(sym)`. Look up a free variable by symbol id
    /// in the closure's captured environment. cs-vm: `Inst::LoadVar`
    /// of a non-parameter non-self symbol. The lowerer emits a
    /// Cranelift call to a runtime helper that reads from a
    /// thread-local env pointer set up by the dispatch site.
    /// Currently the helper assumes the bound value is a Fixnum
    /// and returns its i64; non-fixnum bindings panic. A future
    /// iter adds proper deopt for type mismatch.
    EnvLookup(Value, u32),

    /// `env_set(sym, value)`. Write a Fixnum back to a free
    /// variable's binding. cs-vm: `Inst::SetVar` of a non-local
    /// symbol (Set! to a closure-captured or top-level var). The
    /// lowerer emits a call to `vm_env_set_fixnum(sym, value)`
    /// which walks the env chain via `set_existing`. The Value is
    /// just `()` (void) — no SSA result.
    EnvSet(u32, Value),

    /// `dst = sdiv(lhs, rhs)`. R6RS `quotient` for fixnums.
    /// Cranelift native sdiv (signed integer divide). Divide-by-
    /// zero traps; the JIT body propagates the trap as a panic
    /// (matches the bytecode VM's error path).
    Quotient(Value, Value, Value),

    /// `dst = srem(lhs, rhs)`. R6RS `remainder` for fixnums.
    Remainder(Value, Value, Value),

    /// `dst = band(lhs, rhs)`. R6RS `bitwise-and` (R6RS) /
    /// `bitwise-and-bitwise` for two fixnums.
    BitAnd(Value, Value, Value),

    /// `dst = bor(lhs, rhs)`. R6RS `bitwise-ior` for two fixnums.
    BitOr(Value, Value, Value),

    /// `dst = bxor(lhs, rhs)`. R6RS `bitwise-xor` for two fixnums.
    BitXor(Value, Value, Value),

    /// `dst = abs(src)`. R6RS `abs` for fixnums. Cranelift `iabs`.
    /// Note: i64::MIN has no positive representation; the bytecode
    /// VM upgrades to bignum, while the JIT fastpath wraps. The
    /// Fixnum-only contract means this is fine for typical inputs;
    /// pathological inputs (i64::MIN) would deopt under a real
    /// trampoline.
    AbsFixnum(Value, Value),

    /// `dst = max(lhs, rhs)`. R6RS `max` for two fixnums.
    /// Cranelift `smax`.
    MaxFixnum(Value, Value, Value),

    /// `dst = min(lhs, rhs)`. R6RS `min` for two fixnums.
    /// Cranelift `smin`.
    MinFixnum(Value, Value, Value),

    /// `dst = arg<i>`. cs-vm: implicit (arguments are on the stack
    /// at the procedure entry; this names them as SSA values).
    Param(Value, u32),

    /// `dst = src` (move; lowered away in most backends but useful in
    /// IR for clarity). cs-vm: no-op equivalent.
    Move(Value, Value),

    /// `dst = src` (same bit pattern), but tags `dst` as a Character
    /// for return-type inference. Lowered identically to `Move` in
    /// the i64-only ABI — the i64 carries the codepoint, the
    /// dispatcher decodes it back into `Value::Character` based on
    /// the function's inferred return type. Used for `integer->char`.
    IntCharBitcast(Value, Value),

    /// Type guard: if the value's runtime type doesn't match the
    /// expected tag, deopt to the VM. cs-vm: implicit (interpreter
    /// always dispatches dynamically).
    DeoptCheck(Value, Type),
}

/// Block terminator. Every basic block ends in exactly one of these.
#[derive(Debug, Clone)]
pub enum Term {
    /// `return v`. cs-vm: `Inst::Ret`.
    Return(Value),

    /// Unconditional jump to `target`, passing `args` as block params.
    Jump(BlockId, Vec<Value>),

    /// Branch on `cond`. If `cond` is truthy go to `then_target`, else
    /// `else_target`. cs-vm: `Inst::JumpIf` / `JumpIfNot`.
    Branch(Value, BlockId, BlockId),
}

/// One basic block: a list of straight-line instructions plus a
/// terminator. Block parameters are SSA values that incoming jumps
/// supply (cf. Cranelift's block params).
#[derive(Debug, Clone)]
pub struct Block {
    pub id: BlockId,
    pub params: Vec<(Value, Type)>,
    pub insts: Vec<Inst>,
    pub terminator: Term,
}

/// One JIT-compilable procedure body.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<(Value, Type)>,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    /// Logical return type of the procedure. The Cranelift signature
    /// is always `i64 → i64` regardless; this annotation tells the
    /// dispatcher how to *decode* the i64 back into a `Value`. Defaults
    /// to `Type::Fixnum` for back-compat with iter-6's i64-only ABI.
    pub return_type: Type,
}

impl Function {
    /// Create an empty function with the given name and entry block.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            entry: BlockId(0),
            blocks: Vec::new(),
            return_type: Type::Fixnum,
        }
    }

    /// Number of basic blocks.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Total instruction count across all blocks. Used as a coarse
    /// "is this worth JIT-compiling" heuristic by the tier-up code.
    pub fn inst_count(&self) -> usize {
        self.blocks.iter().map(|b| b.insts.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_function_construction() {
        let f = Function::new("foo");
        assert_eq!(f.name, "foo");
        assert_eq!(f.block_count(), 0);
        assert_eq!(f.inst_count(), 0);
    }

    #[test]
    fn one_block_one_instruction() {
        let mut f = Function::new("inc");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(1), Const::Fixnum(1)),
                Inst::Add(Value(2), Value(0), Value(1)),
            ],
            terminator: Term::Return(Value(2)),
        });
        assert_eq!(f.block_count(), 1);
        assert_eq!(f.inst_count(), 2);
    }

    #[test]
    fn const_variants_round_trip_via_clone() {
        let consts = [
            Const::Fixnum(42),
            Const::Flonum(3.14),
            Const::Boolean(true),
            Const::Character('a'),
            Const::Null,
            Const::Unspecified,
            Const::Eof,
            Const::Symbol(7),
            Const::StringRef(99),
        ];
        for c in consts {
            // Clone path exists.
            let _c2 = c.clone();
        }
    }

    #[test]
    fn type_tags_distinct() {
        let tags = [
            Type::Fixnum,
            Type::Flonum,
            Type::Boolean,
            Type::Character,
            Type::Pair,
            Type::Vector,
            Type::String,
            Type::ByteVector,
            Type::Procedure,
            Type::Any,
        ];
        // Distinct under PartialEq.
        for (i, a) in tags.iter().enumerate() {
            for (j, b) in tags.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
