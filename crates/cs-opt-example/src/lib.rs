//! cs-opt-example — reference third-party optimizer plugin.
//!
//! Implements ADR 0014 iter 6: a worked example of how a
//! downstream Rust crate plugs into cs-opt's `PassRegistry`. The
//! pass itself (`no-op-counter`) does no real transformation —
//! it just increments a counter into `ctx.stats` — because the
//! purpose of this crate is to demonstrate the REGISTRATION
//! PATTERN, not to ship a useful optimization.
//!
//! ## How a real plugin uses this crate as a template
//!
//! 1. Depend on `cs-opt` + `cs-rir`.
//! 2. Implement `cs_opt::Pass` for your transformation type. The
//!    `Pass::name` must match `[a-z][a-z0-9-]*` so Scheme code
//!    can refer to it (e.g., `'my-fast-path`).
//! 3. Expose an `install()` function — see [`install`] here — that
//!    registers an `Arc<MyPass>` with `cs_opt::PassRegistry::global()`.
//! 4. The embedder (a CLI binary, a Rust application) calls
//!    `my_plugin::install()` once at startup, BEFORE `cs_runtime::Runtime::new()`
//!    runs user code. Alternatively, use a `#[ctor::ctor]` hook for
//!    automatic registration if your distribution model supports it.
//! 5. User Scheme code then calls
//!    `(install-optimizer-pass! 'my-fast-path)` to activate it for
//!    a given file or session.
//!
//! ## What this example does NOT do
//!
//! - **No perf claim.** The example pass is intentionally trivial.
//!   ADR 0014 iter 6's stretch criterion ("5% improvement on at
//!   least one workload") would require a real optimization;
//!   that's a separate piece of work once a specific motivating
//!   workload exists.
//!
//! - **No `#[ctor]` magic.** The registration is an explicit
//!   `install()` call. Constructor-driven registration is fine
//!   for some distribution models but adds a runtime dependency
//!   (`ctor` crate) for no benefit in a reference example.

use std::sync::Arc;

use cs_opt::{Bucket, Pass, PassContext, PassRegistry, RegisterError};
use cs_rir::Function;

/// The example pass. Walks every block, counts instructions,
/// records the total into `ctx.stats.mutations["no-op-counter"]`.
/// Does NOT mutate the function.
pub struct NoOpCounter;

impl Pass for NoOpCounter {
    fn name(&self) -> &str {
        "no-op-counter"
    }

    fn bucket(&self) -> Bucket {
        Bucket::Late
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let total: usize = func.blocks.iter().map(|b| b.insts.len()).sum();
        ctx.stats.record_mutations(self.name(), total);
    }
}

/// Register `NoOpCounter` into a specific `PassRegistry`.
/// Embedders that own the registry call this directly; embedders
/// using the global singleton call [`install_global`] instead.
///
/// Returns `Err` on registry rejection (name already taken,
/// invalid name) — see `cs_opt::RegisterError`.
pub fn install(registry: &mut PassRegistry) -> Result<(), RegisterError> {
    registry.register(Arc::new(NoOpCounter))
}

/// Register `NoOpCounter` into the global registry. Most
/// production plugins call this from their `init()` hook at
/// embedder startup.
///
/// `Err` on duplicate registration — safe to call repeatedly per
/// process IF the caller is OK ignoring duplicate-after-first.
pub fn install_global() -> Result<(), RegisterError> {
    let mut r = PassRegistry::global()
        .lock()
        .expect("cs-opt registry mutex poisoned");
    install(&mut r)
}
