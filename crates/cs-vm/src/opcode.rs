//! Bytecode instructions and the [`Bytecode`] container.

use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_diag::Span;

use crate::vm::NanboxValue;

#[derive(Clone, Debug)]
pub enum Inst {
    /// Push a constant onto the value stack. The `u32` is an index into
    /// the owning [`Bytecode`] (or [`CompiledLambda`])'s `consts` pool —
    /// see [`Bytecode::consts`]. Not an inline `Value`: keeping `Inst`
    /// small (no 24-byte `Value` payload) improves icache density and
    /// cuts per-actor cached-bytecode RSS (cs-grt).
    Const(u32),
    /// Read a variable by name from the lexical environment chain. Pushes its value.
    LoadVar(Symbol),
    /// `set!` semantics: pops a value, walks the env chain looking for an
    /// existing binding to update. Falls back to root.define if none found
    /// (matches the tree-walker's behavior for top-level set!).
    SetVar(Symbol),
    /// Top-level define: pops a value and installs it in the root frame.
    DefineGlobal(Symbol),
    /// Local define: pops a value and installs it in the current frame's env.
    /// Used by letrec/letrec* to create local bindings that shadow outer ones.
    DefineLocal(Symbol),
    /// Pop and discard one value (used after non-tail expressions in `begin`).
    Pop,
    /// Conditional branch: pop a value; if falsy, jump to the offset; else fall through.
    JumpIfFalse(u32),
    /// Unconditional jump.
    Jump(u32),
    /// Apply a procedure with N args. Args are on the stack with proc just under them.
    Call(usize),
    /// Tail call: replaces the current frame instead of pushing a new one.
    /// Same stack layout as Call.
    TailCall(usize),
    /// Construct a closure from a CompiledLambda index in the bytecode's lambdas table.
    MakeClosure(usize),
    /// Return from current call frame; top of stack is the return value.
    Return,
    /// Tail-safe continuation marks (issue #36). Pops `val` then
    /// `key` and upserts `(key → val)` on the CURRENT call frame's
    /// mark slot (replacing an existing mark for `key` on this frame).
    /// Because `TailCall` reuses the current frame in place, a wcm
    /// reached through a tail call lands on the same frame and
    /// replaces — so a tail loop runs in constant mark-space. `Call`
    /// pushes a fresh frame whose marks start empty, so non-tail
    /// nesting accumulates. The mark is read back by
    /// `current-continuation-marks`, which walks the frame stack.
    PushMark,
    /// Push a new lexical sub-scope onto the current call frame's env
    /// (an `Env::child` layer parented to the current env). Locals
    /// `DefineLocal`'d after this land in the new layer and shadow
    /// outer bindings; `LeaveScope` pops the layer.
    ///
    /// Used by `letrec` / named-`let` so they don't need a wrapper
    /// closure just to scope their bindings — the bindings live in
    /// a stack-discipline env layer instead. (Post-M8 contification
    /// pass.)
    EnterScope,
    /// Pop the lexical sub-scope pushed by the most recent
    /// `EnterScope`. Restores the parent env on the current frame.
    /// Does not touch the value stack — the body's result on top of
    /// stack passes through unchanged.
    LeaveScope,

    // ---- 2-arg fixnum primops (specialized fast paths for common
    // arithmetic). The compiler emits these when the App's function is
    // an unshadowed reference to a standard primitive in the runtime's
    // globals snapshot. The VM checks both operands at runtime; on a
    // type / overflow miss, falls back to the generic Number arithmetic.
    /// `(+  a b)` — pop b, pop a, push result.
    AddFx2,
    /// `(-  a b)`
    SubFx2,
    /// `(*  a b)`
    MulFx2,
    /// `(<  a b)` — pushes #t / #f.
    LtFx2,
    /// `(<= a b)`
    LeFx2,
    /// `(>  a b)`
    GtFx2,
    /// `(>= a b)`
    GeFx2,
    /// `(=  a b)` — pushes #t / #f.
    EqFx2,

    // ---- Fused compare+branch (one tick instead of two). The compiler
    // emits these for `(if (PRIMOP a b) then alt)` so the boolean result
    // is never materialized on the stack. Each pops 2 args; if the
    // condition holds, jumps to the target (which the compiler patches to
    // alt-start). Falls back to a generic compare when args aren't
    // (Fixnum, Fixnum) — the boolean is then materialized and we behave
    // like the unfused LtFx2/JumpIfFalse pair.
    /// Fused `(if (<  a b) ...)` — branch when a >= b.
    BranchOnGeFx2(u32),
    /// Fused `(if (<= a b) ...)` — branch when a > b.
    BranchOnGtFx2(u32),
    /// Fused `(if (>  a b) ...)` — branch when a <= b.
    BranchOnLeFx2(u32),
    /// Fused `(if (>= a b) ...)` — branch when a < b.
    BranchOnLtFx2(u32),
    /// Fused `(if (=  a b) ...)` — branch when a != b.
    BranchOnNeFx2(u32),
}

/// A compiled program: instructions plus a table of nested compiled lambdas.
/// Bodies (top-level and per-lambda) are wrapped in `Rc` so frame creation
/// during Call/TailCall is a refcount bump rather than a Vec clone.
/// `lambdas` is also Rc-shared so HO bridge calls (vm_call_sync) avoid a
/// Vec<CompiledLambda> deep-clone per invocation.
///
/// `spans` is parallel to `insts` and lets the runtime report
/// source-pinned errors (undefined variables, arity mismatches, etc.) by
/// indexing `spans[ip - 1]` when raising a VmError.
#[derive(Clone, Debug, Default)]
pub struct Bytecode {
    pub insts: Rc<Vec<Inst>>,
    pub spans: Rc<Vec<Span>>,
    /// Compiled lambdas referenced by `MakeClosure` instructions.
    pub lambdas: Rc<Vec<CompiledLambda>>,
    /// Const pool for this compiled program, pre-encoded to `NanboxValue`
    /// at compile time. `Inst::Const(idx)` (both here and in every
    /// `CompiledLambda` in `lambdas`, which share this same `Rc`) indexes
    /// into it — execution is one `vm_value_clone_gc` incref + push,
    /// instead of a `Value` clone + `NanboxValue::from_value` re-encode
    /// on every hit (cs-grt).
    pub consts: Rc<Vec<NanboxValue>>,
}

#[derive(Clone, Debug)]
pub struct CompiledLambda {
    pub params: Vec<Symbol>,
    pub rest: Option<Symbol>,
    pub body: Rc<Vec<Inst>>,
    /// Parallel to `body`. See `Bytecode::spans`.
    pub spans: Rc<Vec<Span>>,
    /// When set, the body is structurally `[<arg0>, <arg1>, <op>, Return]`
    /// where each arg is a single LoadVar(param) or Const, and the op is
    /// one of the 2-arg fixnum primop opcodes. This lets the call sites
    /// (`Call`/`TailCall` dispatch and `vm_call_sync`) skip Env+Frame
    /// allocation and run the primop directly on the args. The body field
    /// is still populated (kept for the no-fast-path fallback in apply,
    /// arity errors, error spans, and future tooling).
    pub fast: Option<FastPrimopBody>,
    /// Set for the single-binding `letrec` lambda (the named-`let`
    /// shape). The closure is built BEFORE the letrec scope layer, so
    /// it does NOT capture the env layer holding its own binding —
    /// that self-capture is a closure↔env Rc cycle the Rc heap can
    /// never reclaim, leaked once per *execution* of the form (the
    /// crab-watchstore ~150KB/request server leak). Self-references
    /// in the body instead resolve through this name, which every
    /// bytecode call path binds to the closure itself in the callee
    /// frame env (the JIT resolves them earlier, via `SelfRef`).
    pub self_bind: Option<Symbol>,
    /// Shared per-lambda JIT profile: tier-up counter, compiled
    /// native pointer, type signature, stack maps. Exactly one
    /// `LambdaProfile` per `CompiledLambda`, shared (via `Rc`) by
    /// every `VmClosure` instance of this lambda — so a lambda
    /// constructed fresh on every call still accumulates one
    /// aggregate hotness counter and tiers up. See
    /// [`crate::vm::LambdaProfile`]. (Post-M8 JIT plan, Stage 0.)
    pub profile: Rc<crate::vm::LambdaProfile>,
    /// Same const pool as the owning [`Bytecode::consts`] (shared `Rc`,
    /// set once by the compiler after the whole program's pool is
    /// finalized). `Inst::Const(idx)` in `body` indexes into this.
    pub consts: Rc<Vec<NanboxValue>>,
}

/// Either a positional reference to one of the lambda's params, or an
/// inlined constant. Represents one operand of a fast-primop body.
#[derive(Clone, Debug)]
pub enum FastArg {
    Param(u8),
    Const(Value),
}

/// Compact description of a "leaf primop" lambda body — see
/// [`CompiledLambda::fast`].
#[derive(Clone, Debug)]
pub struct FastPrimopBody {
    /// One of: `Inst::AddFx2`, `SubFx2`, `MulFx2`, `LtFx2`, `LeFx2`,
    /// `GtFx2`, `GeFx2`, `EqFx2`. Other variants are never produced by
    /// the detector. Stored as `Inst` so the VM can dispatch with the
    /// same arms it already uses for non-fast bodies.
    pub op: Inst,
    pub args: [FastArg; 2],
    /// Span of the primop call site, used for error messages on
    /// type-mismatch / overflow falling out of the fast path.
    pub span: Span,
}

#[cfg(test)]
mod size_tests {
    use super::Inst;

    /// cs-grt: `Inst::Const(Value)` (24-byte `Value` payload) made `Inst`
    /// ~32 bytes. Pooling consts to a `u32` index and shrinking jump/
    /// branch targets to `u32` should land `Inst` at 16 bytes — the
    /// largest remaining payload is the 8-byte `usize` arg count carried
    /// by `Call`/`TailCall`/`MakeClosure`.
    #[test]
    fn inst_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Inst>(), 16);
    }
}
