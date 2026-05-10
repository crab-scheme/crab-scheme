//! Bytecode VM for CrabScheme — M4 milestone, foundation tier.
//!
//! The VM is a stack machine that consumes a [`Bytecode`] (compiled from
//! [`cs_ir::CoreExpr`]) and produces a [`Value`]. It exists alongside the
//! tree-walker in `cs-runtime` so we can differentially test the two and
//! eventually feed both an upcoming JIT.
//!
//! Foundation scope:
//! - Compile a meaningful subset of CoreExpr (Const, Ref, If, App, Set,
//!   Lambda, Begin, Letrec) to bytecode.
//! - Interpret with a value stack + a frame stack.
//! - No tail-call elimination optimization yet (only basic correctness).
//! - No closures-with-environments machinery shared with cs-runtime — VM
//!   builds its own environment chain.

pub mod compiler;
pub mod jit_translate;
pub mod opcode;
pub mod vm;

pub use compiler::{compile, compile_with_globals, compile_with_globals_and_primops, CompileError};
pub use opcode::{Bytecode, Inst};
pub use vm::{run, VmError};
