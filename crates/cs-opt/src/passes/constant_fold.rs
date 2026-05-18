//! `constant-fold` — fixnum arithmetic over two `LoadConst`-defined
//! operands collapsed to a single `LoadConst`.
//!
//! ## Scope (intentionally narrow)
//!
//! Only folds:
//! - `Add` / `Sub` / `Mul` where both operands are tagged Fixnum
//!   constants from a `LoadConst(_, Const::Fixnum(_))` earlier in
//!   the SAME block
//! - Uses checked arithmetic to skip overflow (matches the
//!   runtime semantics, which raise on overflow rather than
//!   wrapping silently)
//!
//! Does NOT fold:
//! - `Div` — Fixnum/Fixnum can produce a `Rational` per R6RS
//!   exact-division semantics; foldable but the result wouldn't
//!   fit in `Const::Fixnum`. Skipped.
//! - Flonum arithmetic — NaN / inf / -0 corners require careful
//!   parity with the runtime's IEEE-754 implementation.
//! - Cross-block constants — block params can take different
//!   values from different predecessors; conservative single-block
//!   tracking is enough for the bytecode we generate today.
//! - Sequences of folds — a single pass produces a new const
//!   that a hypothetical re-run could fold further. Iteration to
//!   fixpoint is iter-4 territory.
//!
//! ## Rewrite shape
//!
//! Each foldable instruction is REPLACED in place by
//! `LoadConst(dst, Const::Fixnum(result))` — the destination
//! `Value` SSA id is preserved, only its definition changes.
//! That means downstream uses of `dst` automatically pick up the
//! constant without any remapping. Other passes that walk insts
//! see a cleaner `LoadConst` chain.
//!
//! ## Bucket
//!
//! `Early` — folding produces smaller IR that later passes
//! (dead-block-elim, inlining) benefit from.

use std::collections::HashMap;

use cs_rir::{Const, Function, Inst, Value};

use crate::{Bucket, Pass, PassContext};

pub struct ConstantFold;

impl Pass for ConstantFold {
    fn name(&self) -> &str {
        "constant-fold"
    }

    fn bucket(&self) -> Bucket {
        Bucket::Early
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let mut folded: usize = 0;
        for block in func.blocks.iter_mut() {
            // Per-block map from Value → known Fixnum const.
            // Cleared at each block boundary because block-params /
            // cross-block flow can rebind values we don't track.
            let mut fixnums: HashMap<Value, i64> = HashMap::new();
            for inst in block.insts.iter_mut() {
                if let Some(replacement) = fold_inst(inst, &fixnums) {
                    // Stash the result for chain folding (later
                    // insts in the same block can fold against the
                    // newly-produced const).
                    if let Inst::LoadConst(dst, Const::Fixnum(n)) = &replacement {
                        fixnums.insert(*dst, *n);
                    }
                    *inst = replacement;
                    folded += 1;
                } else if let Inst::LoadConst(dst, Const::Fixnum(n)) = inst {
                    // Not folded but still a fixnum-const definition;
                    // record so chain folding sees the original consts.
                    fixnums.insert(*dst, *n);
                }
            }
        }
        ctx.stats.record_mutations(self.name(), folded);
    }
}

/// Try to fold `inst` against the known fixnum-const map. Returns
/// the replacement instruction when foldable, `None` otherwise.
/// Pure — no mutation of `fixnums`.
fn fold_inst(inst: &Inst, fixnums: &HashMap<Value, i64>) -> Option<Inst> {
    match inst {
        Inst::Add(dst, lhs, rhs) => {
            let a = *fixnums.get(lhs)?;
            let b = *fixnums.get(rhs)?;
            let r = a.checked_add(b)?;
            Some(Inst::LoadConst(*dst, Const::Fixnum(r)))
        }
        Inst::Sub(dst, lhs, rhs) => {
            let a = *fixnums.get(lhs)?;
            let b = *fixnums.get(rhs)?;
            let r = a.checked_sub(b)?;
            Some(Inst::LoadConst(*dst, Const::Fixnum(r)))
        }
        Inst::Mul(dst, lhs, rhs) => {
            let a = *fixnums.get(lhs)?;
            let b = *fixnums.get(rhs)?;
            let r = a.checked_mul(b)?;
            Some(Inst::LoadConst(*dst, Const::Fixnum(r)))
        }
        _ => None,
    }
}
