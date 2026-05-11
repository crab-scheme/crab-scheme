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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// `Value::Symbol(u32)`. The i64 carries the symbol id
    /// zero-extended into the low 32 bits; the dispatcher decodes
    /// via `cs_core::Symbol(i as u32)`.
    Symbol,
    /// `Value::Null` (the `'()` singleton). Carried as a sentinel
    /// i64 (always 0); the dispatcher decodes by tag rather than
    /// inspecting the i64.
    Null,
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

    /// `dst = lhs + rhs` interpreted as flonums. Operands and dst are
    /// i64 carriers of f64 bit patterns; the lowerer bitcasts to f64,
    /// runs Cranelift's `fadd`, and bitcasts back. Used when the
    /// translator's per-Value type analysis classifies both operands
    /// as Flonum.
    FlonumAdd(Value, Value, Value),

    /// `dst = lhs - rhs` (flonum). See `FlonumAdd`.
    FlonumSub(Value, Value, Value),

    /// `dst = lhs * rhs` (flonum). See `FlonumAdd`.
    FlonumMul(Value, Value, Value),

    /// `dst = lhs / rhs` (flonum). See `FlonumAdd`. IEEE-754 division —
    /// no zero-check; division by zero produces ±inf or NaN per spec.
    FlonumDiv(Value, Value, Value),

    /// `dst = (lhs < rhs)` interpreted as flonums. Distinct from
    /// `Lt` because IEEE-754 ordering doesn't match signed-integer
    /// compare on the bit pattern (negative zero, NaN, etc.).
    /// Result is 0/1 i64 — Boolean-typed for return-decoding.
    FlonumLt(Value, Value, Value),

    /// `dst = (lhs == rhs)` interpreted as flonums. Distinct from
    /// `Eq` because IEEE-754 equality has NaN ≠ NaN semantics that
    /// integer compare on bits would mishandle.
    FlonumEq(Value, Value, Value),

    /// `dst = sqrt(src)` (flonum). Lowers to Cranelift `sqrt`.
    FlonumSqrt(Value, Value),

    /// `dst = |src|` (flonum). Lowers to Cranelift `fabs`. Strips
    /// the sign bit; NaN propagates unchanged.
    FlonumAbs(Value, Value),

    /// `dst = max(lhs, rhs)` (flonum). Lowers to Cranelift `fmax` —
    /// IEEE-754 maximum (NaN-preserving on both inputs).
    FlonumMax(Value, Value, Value),

    /// `dst = min(lhs, rhs)` (flonum). Lowers to Cranelift `fmin`.
    FlonumMin(Value, Value, Value),

    /// `dst = floor(src)` (flonum). Cranelift `floor`.
    FlonumFloor(Value, Value),

    /// `dst = ceil(src)` (flonum). Cranelift `ceil`.
    FlonumCeil(Value, Value),

    /// `dst = trunc(src)` (flonum). Cranelift `trunc`.
    FlonumTrunc(Value, Value),

    /// `dst = round-to-nearest-even(src)` (flonum). Cranelift `nearest` —
    /// IEEE-754 banker's rounding, matching R6RS `round`.
    FlonumRound(Value, Value),

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

    /// `dst = call_general(callee, args...)` — slow-path general
    /// call into a non-self, non-builtin closure. cs-vm: `Inst::Call`
    /// with a callee that the translator couldn't resolve to
    /// `SelfRef` or a `BuiltinRef`.
    ///
    /// Both `callee` and every entry of `args` are Any-tagged
    /// (`Gc<Value>` raw handles); the translator inserts `BoxTyped`
    /// before emitting if any operand is immediate-shaped. `dst` is
    /// also Any-tagged: the helper returns a fresh `Gc<Value>`
    /// handle carrying the result.
    ///
    /// Lowers to a Cranelift call against `vm_call_general(callee,
    /// args_ptr, n_args) -> i64`. The lowerer materializes `args`
    /// into a stack-allocated `[i64]` buffer (one slot per arg),
    /// passes the buffer address plus the arity, and threads the
    /// returned Gc handle through `declare_value_needs_stack_map`.
    ///
    /// Per ADR 0012 D-1, this is the IC miss path. The IC hot path
    /// (load-compare-call into a per-call-site cache) lands later;
    /// today every CallGeneral takes the slow path unconditionally.
    CallGeneral(Value, Value, Vec<Value>),

    /// `dst = env_lookup(sym)`. Look up a free variable by symbol id
    /// in the closure's captured environment. cs-vm: `Inst::LoadVar`
    /// of a non-parameter non-self symbol. The lowerer emits a
    /// Cranelift call to a runtime helper that reads from a
    /// thread-local env pointer set up by the dispatch site.
    /// Currently the helper assumes the bound value is a Fixnum
    /// and returns its i64; non-fixnum bindings panic. A future
    /// iter adds proper deopt for type mismatch.
    EnvLookup(Value, u32),

    /// `dst = env_lookup_any(sym)`. Look up a free variable's full
    /// Value and box it into an Any-tagged `Gc<Value>` handle.
    /// cs-vm: `Inst::LoadVar` of a non-parameter non-self symbol
    /// whose use site requires a polymorphic value (e.g. a closure
    /// flowing to `CallGeneral` as the callee). Lowers to
    /// `vm_env_lookup_any(sym) -> i64`. Non-fatal: an unbound symbol
    /// panics, but any bound `Value` succeeds (the helper clones
    /// the binding through `value_to_gc_i64`). `dst` is typed Any.
    EnvLookupAny(Value, u32),

    /// `env_set(sym, value)`. Write a Fixnum back to a free
    /// variable's binding. cs-vm: `Inst::SetVar` of a non-local
    /// symbol (Set! to a closure-captured or top-level var). The
    /// lowerer emits a call to `vm_env_set_fixnum(sym, value)`
    /// which walks the env chain via `set_existing`. The Value is
    /// just `()` (void) — no SSA result.
    EnvSet(u32, Value),

    /// `dst = make-vector(n, fill)`. Lowers to `vm_alloc_vector_gc`.
    /// `n` is Fixnum-shape; `fill` is Any (Gc handle, consumed).
    /// `dst` is Any (fresh Gc handle to a Vector slot).
    /// ADR 0012 D-2 (iter BV).
    VecAlloc(Value, Value, Value),

    /// `dst = vector-ref(vec, idx)`. Lowers to `vm_vector_ref_gc`.
    /// `vec` Any (consumed), `idx` Fixnum, `dst` Any.
    VecRef(Value, Value, Value),

    /// `dst = vector-set!(vec, idx, x)`. Lowers to `vm_vector_set_gc`.
    /// `vec` Any (consumed), `idx` Fixnum, `x` Any (consumed). `dst`
    /// is Any — the helper returns a Gc-wrapped Unspecified.
    VecSet(Value, Value, Value, Value),

    /// `dst = vector-length(vec)`. Lowers to `vm_vector_length_gc`.
    /// `vec` Any (consumed). `dst` is Fixnum (raw length, not boxed).
    VecLength(Value, Value),

    /// `dst = vector?(v)`. Lowers to `vm_vector_p_gc`. `v` Any
    /// (consumed). `dst` is Boolean (0/1).
    VecP(Value, Value),

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

    /// `dst = bits(f64::from(src))` — convert a Fixnum i64 to a
    /// Flonum f64, then bitcast back to i64 so the value still fits
    /// the i64-only ABI's lane. Tags dst as Flonum for the return-
    /// type inference; the dispatcher decodes the i64 via
    /// `f64::from_bits`. Used for `real->flonum` /
    /// `exact->inexact`.
    FixToFlo(Value, Value),

    /// `dst = cons(car, cdr)` — heap-allocate a Pair via the
    /// `vm_alloc_pair` runtime helper. The two `u8` fields are the
    /// per-operand JIT_RT_* tags, embedded at translate time so the
    /// lowerer can pass them through to the helper without consulting
    /// per-Value type tables. dst is tagged as `Type::Any` (the i64
    /// carries `Box::into_raw(Box<Value::Pair(_)>)`).
    Cons(Value, Value, u8, Value, u8),

    /// `dst = car(pair)` — extract the first slot of an Any-tagged
    /// pair via the `vm_pair_car` runtime helper. Operand is
    /// expected to be `Type::Any`; dst is `Type::Any` too.
    Car(Value, Value),

    /// `dst = cdr(pair)` — extract the second slot of an Any-tagged
    /// pair via the `vm_pair_cdr` runtime helper. Operand and dst
    /// are both `Type::Any`.
    Cdr(Value, Value),

    /// `dst = pair?(v)` — type predicate. Operand is `Type::Any`,
    /// dst is `Type::Boolean`. Lowers to `vm_pair_p`, which
    /// consumes the operand box.
    PairP(Value, Value),

    /// `dst = null?(v)` — type predicate for `'()`. Operand is
    /// `Type::Any`, dst is `Type::Boolean`. Lowers to `vm_null_p`
    /// which consumes the operand box.
    NullP(Value, Value),

    /// `dst = clone(src)` — produce a fresh Any-tagged box from a
    /// peek of `src`. `src` remains live; `dst` is independently
    /// owned. Lowers to the `vm_value_clone` runtime helper.
    /// Used by the translator to support multi-use of an Any
    /// operand (each non-final use pulls through a clone; the
    /// original is dropped at function exit via `AnyDrop`).
    AnyClone(Value, Value),

    /// `drop(src)` — release an Any-tagged box. Lowers to
    /// `vm_value_drop`. Inserted at every return path for
    /// Any-typed params so the dispatch-side allocation doesn't
    /// leak when the body never consumed it.
    AnyDrop(Value),

    /// `dst = box_typed(src, tag)` — box a typed i64 (Fixnum /
    /// Boolean / Character / Flonum) into an Any-tagged
    /// `Box<Value>` via the `vm_box_typed` runtime helper. The
    /// `u8` tag is the JIT_RT_* code identifying how to interpret
    /// `src`. Inserted by the translator's post-pass when a Jump's
    /// arg or a function's Return value needs to widen to Any
    /// because a sibling control-flow path produced an Any-tagged
    /// value.
    BoxTyped(Value, Value, u8),

    /// `dst = unbox_fixnum(src)` — consume an Any-tagged box and
    /// extract its inner Fixnum as a raw i64. Lowers to
    /// `vm_unbox_fixnum` which panics if the runtime value isn't
    /// a Fixnum (the JIT body's type-feedback signature filtered
    /// for that case at the dispatch layer; deopt rather than UB).
    /// Inserted by `emit_arith_binop` / `emit_typed_lt` etc. when
    /// an operand is `Type::Any` but the op needs raw Fixnum bits.
    AnyToFix(Value, Value),

    /// `dst = unbox_boolean(src)` — consume an Any-tagged box and
    /// extract its inner Boolean as 0/1 i64. Lowers to
    /// `vm_unbox_boolean`. dst is `Type::Boolean`.
    AnyToBool(Value, Value),

    /// `dst = unbox_flonum(src)` — consume an Any-tagged box and
    /// extract its inner Flonum's bit pattern. Lowers to
    /// `vm_unbox_flonum`. dst is `Type::Flonum`.
    AnyToFlo(Value, Value),

    /// `dst = eq?(lhs, rhs)` on two Any-tagged boxes. Consumes
    /// both operands and produces `Type::Boolean`. Lowers to
    /// `vm_eq_any` which does the per-variant identity comparison
    /// (Symbol id, Fixnum value, Gc::ptr_eq for heap-pointer
    /// types).
    EqAny(Value, Value, Value),

    /// `dst = truthy(src)` — consume an Any-tagged box and
    /// produce a 0/1 i64 that reflects R6RS truthiness (only
    /// `Boolean(false)` is falsy). Lowers to `vm_any_truthy`.
    /// Inserted before `Term::Branch` when the condition is
    /// `Type::Any` — otherwise the brif would compare the raw
    /// Box pointer (always nonzero) and always take the truthy
    /// branch.
    AnyTruthy(Value, Value),

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
