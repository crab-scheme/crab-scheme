//! `dead-block-elim` — removes blocks unreachable from `func.entry`.
//!
//! ## Approach
//!
//! 1. BFS from `func.entry`, following each block's terminator
//!    targets (`Jump`, `Branch`) into successor blocks.
//! 2. Any block whose `id` isn't in the reach set is dropped from
//!    `func.blocks`.
//!
//! ## Block-id stability
//!
//! Downstream consumers (`cs-aot`, `cs-jit-cranelift`) reference
//! blocks by their `id: BlockId` field, NOT by their position in
//! the `Vec<Block>`. The only known raw-index access is
//! `cs-aot`'s `func.blocks[0]` look at the entry block — which
//! remains at index 0 because:
//!  - The entry block is always reachable from itself.
//!  - `Vec::retain` preserves relative order of kept elements.
//!
//! So `dead-block-elim` never moves the entry block off index 0,
//! and any code that indexes by `b.id` is unaffected by gaps in
//! the id space. We do NOT renumber ids — sparse ids are
//! semantically valid; renumbering would force a complete
//! Term-rewrite for no real benefit.
//!
//! ## Bucket
//!
//! `Default` — runs after `constant-fold` (which may turn a
//! conditional branch into a no-op when its condition is a known
//! const) but before `inst-stats` (which wants to count only
//! live instructions).

use std::collections::HashSet;

use cs_rir::{BlockId, Function, Term};

use crate::{Pass, PassContext};

pub struct DeadBlockElim;

impl Pass for DeadBlockElim {
    fn name(&self) -> &'static str {
        "dead-block-elim"
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let reach = reachable_blocks(func);
        let before = func.blocks.len();
        func.blocks.retain(|b| reach.contains(&b.id));
        let removed = before - func.blocks.len();
        ctx.stats.record_mutations(self.name(), removed);
    }
}

/// Compute the set of `BlockId`s reachable from `func.entry`
/// through Jump / Branch terminators. Pure — caller does the
/// mutation.
fn reachable_blocks(func: &Function) -> HashSet<BlockId> {
    let mut reach: HashSet<BlockId> = HashSet::new();
    let mut stack: Vec<BlockId> = vec![func.entry];
    while let Some(id) = stack.pop() {
        if !reach.insert(id) {
            continue;
        }
        let Some(block) = func.blocks.iter().find(|b| b.id == id) else {
            // Dangling reference. Don't try to follow it; let the
            // verifier (iter 4) surface this as malformed RIR.
            continue;
        };
        match &block.terminator {
            Term::Return(_) => {}
            Term::Jump(t, _) => stack.push(*t),
            Term::Branch(_, then_id, else_id, _) => {
                stack.push(*then_id);
                stack.push(*else_id);
            }
        }
    }
    reach
}
