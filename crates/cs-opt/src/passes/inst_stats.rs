//! `inst-stats` — diagnostic pass that records IR size into
//! `ctx.stats`.
//!
//! Doesn't mutate the IR. Useful as:
//! - a sanity check in tests (verify that earlier passes
//!   actually shrunk the function)
//! - a benchmark hook (measure per-pass code-size impact)
//! - a worked reference for "what does a non-transforming pass
//!   look like?"
//!
//! Recorded into the stats:
//! - `mutations["inst-stats"]` = total inst count across all
//!   blocks at the time this pass ran
//! - `mutations["inst-stats:blocks"]` = block count
//!
//! Using `mutations` for two different metrics is intentional —
//! the field is a generic counter; namespacing the key with `:`
//! gives passes a place to record richer multi-dim data without
//! widening PassStats.
//!
//! ## Bucket
//!
//! `Late` — runs after every transforming pass so the counts
//! reflect post-pipeline IR, not pre-pipeline.

use cs_rir::Function;

use crate::{Bucket, Pass, PassContext};

pub struct InstStats;

impl Pass for InstStats {
    fn name(&self) -> &str {
        "inst-stats"
    }

    fn bucket(&self) -> Bucket {
        Bucket::Late
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let inst_total: usize = func.blocks.iter().map(|b| b.insts.len()).sum();
        let block_count = func.blocks.len();
        ctx.stats.record_mutations(self.name(), inst_total);
        ctx.stats.record_mutations("inst-stats:blocks", block_count);
    }
}
