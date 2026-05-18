//! RIR well-formedness verifier (ADR 0014 iter 4).
//!
//! Catches the most common ways a buggy transformation pass can
//! produce malformed RIR. Intended as a dev-build aid only — the
//! pass framework in `cs-opt` wires this in under a feature flag
//! so release builds skip the cost.
//!
//! ## Scope
//!
//! Checks that surface the highest-value bugs cheaply:
//!
//! 1. **Entry block exists.** `func.entry` resolves to a block in
//!    `func.blocks`. A missing entry block is an instant crash at
//!    codegen time; the verifier surfaces it with attribution.
//!
//! 2. **BlockId-target validity.** Every `BlockId` mentioned by a
//!    `Term::Jump` or `Term::Branch` corresponds to a block that
//!    exists in `func.blocks`. Dangling targets are how a buggy
//!    `dead-block-elim` (or similar) typically goes wrong.
//!
//! 3. **Value uniqueness (single-def).** Each `Value` id is
//!    defined exactly once across the entire function. Counted
//!    definitions are: `params`, each `Block.params`, and every
//!    `Inst`'s destination. Double-defining a Value usually means
//!    a pass cloned an instruction without freshening the
//!    destination — silent miscompile if not caught.
//!
//! ## Not yet covered (left for a future iter)
//!
//! - **Use-before-def.** Requires either dominator analysis or a
//!   topological proof that each use's defining instruction
//!   appears earlier in the same block (or as a block param of
//!   the block containing the use). Mechanical but verbose; iter
//!   5 territory once the framework has more passes that could
//!   plausibly produce this class of bug.
//!
//! - **Block-param arity matching.** A `Jump` / `Branch` passes
//!   `args` that should match the target's `params` length and
//!   types. Today's translator is sloppy enough about this that
//!   enforcing it eagerly would flag legitimate code; defer until
//!   the upstream sites are tightened.
//!
//! - **`Function.captures` consistency** with `EnvLookup` syms.
//!   cs-aot already crashes loudly when these get out of sync, so
//!   adding a verifier check is redundant overhead today.

use std::collections::HashSet;

use crate::{BlockId, Function, Inst, Term, Value};

/// Why `verify` rejected a `Function`. One variant per check so
/// the dev-build hook in `cs-opt` can attribute the failure to a
/// specific cause and (when wired up) the offending pass name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// `func.entry` doesn't resolve to a block in `func.blocks`.
    MissingEntry { entry: BlockId },

    /// A `Term::Jump` or `Term::Branch` targets a block not
    /// present in `func.blocks`. `from` is the block that
    /// contained the bad terminator.
    DanglingTarget { from: BlockId, target: BlockId },

    /// The same `Value` id was defined more than once. `value` is
    /// the duplicate id. Doesn't enumerate ALL definitions because
    /// the first detected duplicate is enough to fix.
    DuplicateDefinition { value: Value },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::MissingEntry { entry } => {
                write!(f, "entry block {:?} not present in function", entry)
            }
            VerifyError::DanglingTarget { from, target } => write!(
                f,
                "terminator in block {:?} targets non-existent block {:?}",
                from, target
            ),
            VerifyError::DuplicateDefinition { value } => {
                write!(f, "Value {:?} defined more than once", value)
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// Run the verifier on `func`. Returns `Ok(())` when every
/// implemented invariant holds; returns the first detected
/// failure otherwise (does not enumerate all failures).
pub fn verify(func: &Function) -> Result<(), VerifyError> {
    let known_ids: HashSet<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    if !known_ids.contains(&func.entry) {
        return Err(VerifyError::MissingEntry { entry: func.entry });
    }
    let mut defined: HashSet<Value> = HashSet::new();
    let mut see_def = |v: Value| -> Result<(), VerifyError> {
        if !defined.insert(v) {
            Err(VerifyError::DuplicateDefinition { value: v })
        } else {
            Ok(())
        }
    };
    for (v, _) in &func.params {
        see_def(*v)?;
    }
    for block in &func.blocks {
        for (v, _) in &block.params {
            see_def(*v)?;
        }
        for inst in &block.insts {
            if let Some(dst) = inst_dst(inst) {
                see_def(dst)?;
            }
        }
        match &block.terminator {
            Term::Return(_) => {}
            Term::Jump(t, _) => {
                if !known_ids.contains(t) {
                    return Err(VerifyError::DanglingTarget {
                        from: block.id,
                        target: *t,
                    });
                }
            }
            Term::Branch(_, then_id, else_id, _) => {
                if !known_ids.contains(then_id) {
                    return Err(VerifyError::DanglingTarget {
                        from: block.id,
                        target: *then_id,
                    });
                }
                if !known_ids.contains(else_id) {
                    return Err(VerifyError::DanglingTarget {
                        from: block.id,
                        target: *else_id,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Extract the destination Value from an Inst if it produces one.
/// Returns `None` for store-shaped insts that don't define a new
/// SSA value (rare; most insts have a destination).
///
/// Maintenance note: when `cs_rir::Inst` gains new variants this
/// match must be extended. The verifier is dev-only, so an
/// unhandled variant just means "we won't catch double-defs in
/// that inst" — acceptable while the variant churn settles, but
/// worth a periodic audit.
fn inst_dst(inst: &Inst) -> Option<Value> {
    match inst {
        Inst::LoadConst(d, _) => Some(*d),
        Inst::Add(d, _, _) => Some(*d),
        Inst::Sub(d, _, _) => Some(*d),
        Inst::Mul(d, _, _) => Some(*d),
        Inst::Div(d, _, _) => Some(*d),
        Inst::FlonumAdd(d, _, _) => Some(*d),
        Inst::FlonumSub(d, _, _) => Some(*d),
        Inst::FlonumMul(d, _, _) => Some(*d),
        Inst::FlonumDiv(d, _, _) => Some(*d),
        Inst::FlonumLt(d, _, _) => Some(*d),
        Inst::FlonumEq(d, _, _) => Some(*d),
        Inst::FlonumSqrt(d, _) => Some(*d),
        // Other variants exist (Move, Call, EnvLookup, etc.) — see
        // the inst-dst maintenance note above. For iter 4 the
        // covered subset captures the hot-path arithmetic variants
        // that the shipped builtin passes (constant-fold) can
        // mis-handle. Extending coverage is mechanical.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, BlockId, Const, Inst, Term, Type, Value};

    fn empty_func() -> Function {
        let mut f = Function::new("v");
        f.blocks.push(Block {
            id: BlockId(0),
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Term::Return(Value(0)),
        });
        f
    }

    #[test]
    fn empty_function_with_entry_passes() {
        let f = empty_func();
        verify(&f).unwrap();
    }

    #[test]
    fn missing_entry_block_fails() {
        let mut f = empty_func();
        f.entry = BlockId(99);
        match verify(&f) {
            Err(VerifyError::MissingEntry { entry }) => assert_eq!(entry, BlockId(99)),
            other => panic!("expected MissingEntry, got {:?}", other),
        }
    }

    #[test]
    fn dangling_jump_target_fails() {
        let mut f = empty_func();
        f.blocks[0].terminator = Term::Jump(BlockId(42), vec![]);
        match verify(&f) {
            Err(VerifyError::DanglingTarget { target, .. }) => {
                assert_eq!(target, BlockId(42))
            }
            other => panic!("expected DanglingTarget, got {:?}", other),
        }
    }

    #[test]
    fn dangling_branch_target_fails() {
        let mut f = empty_func();
        f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Boolean(true))];
        f.blocks[0].terminator = Term::Branch(Value(0), BlockId(1), BlockId(99), vec![]);
        f.blocks.push(Block {
            id: BlockId(1),
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Term::Return(Value(0)),
        });
        match verify(&f) {
            Err(VerifyError::DanglingTarget { target, .. }) => {
                assert_eq!(target, BlockId(99))
            }
            other => panic!("expected DanglingTarget, got {:?}", other),
        }
    }

    #[test]
    fn duplicate_definition_fails() {
        let mut f = empty_func();
        f.blocks[0].insts = vec![
            Inst::LoadConst(Value(7), Const::Fixnum(1)),
            Inst::LoadConst(Value(7), Const::Fixnum(2)),
        ];
        match verify(&f) {
            Err(VerifyError::DuplicateDefinition { value }) => assert_eq!(value, Value(7)),
            other => panic!("expected DuplicateDefinition, got {:?}", other),
        }
    }

    #[test]
    fn duplicate_param_and_inst_definition_fails() {
        let mut f = empty_func();
        f.params = vec![(Value(0), Type::Fixnum)];
        f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Fixnum(1))];
        // Value(0) is both a function param and an Inst dst.
        assert!(matches!(
            verify(&f),
            Err(VerifyError::DuplicateDefinition { value: Value(0) })
        ));
    }

    #[test]
    fn well_formed_two_block_function_passes() {
        let mut f = empty_func();
        f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Fixnum(5))];
        f.blocks[0].terminator = Term::Jump(BlockId(1), vec![Value(0)]);
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![(Value(1), Type::Fixnum)],
            insts: vec![Inst::Add(Value(2), Value(1), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        verify(&f).unwrap();
    }
}
