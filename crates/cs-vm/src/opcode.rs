//! Bytecode instructions and the [`Bytecode`] container.

use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_diag::Span;

#[derive(Clone, Debug)]
pub enum Inst {
    /// Push a constant onto the value stack.
    Const(Value),
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
    JumpIfFalse(usize),
    /// Unconditional jump.
    Jump(usize),
    /// Apply a procedure with N args. Args are on the stack with proc just under them.
    Call(usize),
    /// Tail call: replaces the current frame instead of pushing a new one.
    /// Same stack layout as Call.
    TailCall(usize),
    /// Construct a closure from a CompiledLambda index in the bytecode's lambdas table.
    MakeClosure(usize),
    /// Return from current call frame; top of stack is the return value.
    Return,
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
}

#[derive(Clone, Debug)]
pub struct CompiledLambda {
    pub params: Vec<Symbol>,
    pub rest: Option<Symbol>,
    pub body: Rc<Vec<Inst>>,
    /// Parallel to `body`. See `Bytecode::spans`.
    pub spans: Rc<Vec<Span>>,
}
