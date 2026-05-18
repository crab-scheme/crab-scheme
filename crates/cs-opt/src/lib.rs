//! cs-opt — pluggable optimizer-pass framework for CrabScheme.
//!
//! Implements ADR 0014 iter 1: the [`Pass`] trait,
//! [`PassRegistry`], [`PassPipeline`], [`PassContext`], and
//! [`PassStats`]. No actual pass implementations ship in iter 1
//! (those land in iter 2 — `dead-block-elim`, `constant-fold`,
//! `inst-stats`). This iter is the substrate other iters and
//! plugin authors build against.
//!
//! ## Architectural position
//!
//! The pipeline runs between bytecode→RIR translation and codegen.
//! Both `cs-jit-cranelift` and `cs-aot` consume the post-pass RIR,
//! so a pass that rewrites RIR benefits both back ends.
//!
//! ```text
//!     bytecode
//!         │
//!         ▼
//!     cs-vm::jit_translate ──► cs_rir::Function
//!                                    │
//!                                    ▼
//!                          cs_opt::PassPipeline::run  ◀──  cs-opt
//!                                    │
//!                          ┌─────────┴────────┐
//!                          ▼                  ▼
//!                  cs-jit-cranelift        cs-aot
//! ```
//!
//! ## Plugin authoring (Rust)
//!
//! ```ignore
//! struct MyPass;
//! impl cs_opt::Pass for MyPass {
//!     fn name(&self) -> &str { "my-pass" }
//!     fn bucket(&self) -> cs_opt::Bucket { cs_opt::Bucket::Default }
//!     fn run(&self, func: &mut cs_rir::Function, _ctx: &mut cs_opt::PassContext) {
//!         // ... mutate func ...
//!     }
//! }
//!
//! // At startup:
//! cs_opt::PassRegistry::global()
//!     .lock()
//!     .unwrap()
//!     .register(std::sync::Arc::new(MyPass));
//! ```
//!
//! ## Plugin selection (Scheme — landed in iter 3)
//!
//! ```scheme
//! (install-optimizer-pass! 'my-pass)
//! ```
//!
//! ## Soundness contract
//!
//! Passes MUST preserve `Function` invariants (SSA validity, every
//! `Value` defined before use, every `BlockId` reachable). The
//! dev-build verifier (lands in iter 4) catches violations and
//! attributes them by pass name. Release builds skip verification
//! — plugin authors own correctness.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Symbol, SymbolTable};
use cs_rir::Function;

pub mod passes;
pub use passes::{register_builtins, BUILTIN_NAMES};

// ---- Bucket: pass-ordering priority ----

/// Pipeline-ordering bucket. Passes within the same bucket run in
/// registration order; buckets run in numeric order (smallest
/// first).
///
/// Three buckets are enough at current pass counts (~3 builtins).
/// If the registered-pass count grows past ~20, ADR 0014 specifies
/// promotion to a real DAG-resolver; the trait surface stays the
/// same — only the pipeline construction changes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    /// Runs first. Use for normalization / constant folding —
    /// anything that produces a smaller / simpler IR that later
    /// passes benefit from.
    Early,
    /// Default. Use unless a specific reason argues otherwise.
    Default,
    /// Runs last. Use for diagnostics, peephole tweaks, or
    /// cleanups that need to see all prior passes' output.
    Late,
}

impl Bucket {
    /// Numeric priority for sorting. Smaller runs first.
    pub fn priority(self) -> i32 {
        match self {
            Bucket::Early => -100,
            Bucket::Default => 0,
            Bucket::Late => 100,
        }
    }
}

impl Default for Bucket {
    fn default() -> Self {
        Bucket::Default
    }
}

// ---- Pass trait ----

/// A pluggable optimizer pass over `cs_rir::Function`.
///
/// Implementors:
/// - return a stable [`name`](Pass::name) matching
///   `[a-z][a-z0-9-]*` so Scheme symbols can refer to it
/// - declare a [`bucket`](Pass::bucket) for pipeline ordering
/// - implement [`run`](Pass::run) — the in-place transformation
///
/// Trait objects are stored in `Arc<dyn Pass>`; impls must be
/// `Send + Sync` since the pipeline may run on any worker thread.
pub trait Pass: Send + Sync {
    /// Stable name. MUST be unique within a `PassRegistry`.
    /// MUST match `[a-z][a-z0-9-]*` (lowercase, ASCII letters,
    /// digits, hyphens; starts with a letter). The registry
    /// rejects names violating this.
    fn name(&self) -> &str;

    /// Pipeline-ordering bucket. Defaults to [`Bucket::Default`].
    fn bucket(&self) -> Bucket {
        Bucket::Default
    }

    /// Run the pass on `func`. Mutates in place. Mutations
    /// should preserve `Function` invariants (the dev verifier
    /// in iter 4 will catch violations).
    ///
    /// `ctx` provides read-only access to the symbol table and
    /// optional typer hints, plus a mutable [`PassStats`] for
    /// the pass to record what it did.
    fn run(&self, func: &mut Function, ctx: &mut PassContext);
}

// ---- PassContext: cross-cutting state during a pipeline run ----

/// Cross-cutting state that every pass in a pipeline run can
/// read or update. Lives only for the duration of one
/// `PassPipeline::run` call — passes that need persistent state
/// across runs must own their own storage.
pub struct PassContext<'a> {
    /// Read-only access to the symbol table. Use for resolving
    /// `Symbol` IDs in `Inst` operands to printable names (for
    /// diagnostics) or for sym→sym comparisons.
    pub syms: &'a SymbolTable,
    /// Optional typer-derived hints. When present, maps from
    /// procedure name (`Symbol`) to per-parameter type
    /// information. Passes that want to specialize on typed
    /// procedures consult this; passes that ignore types skip
    /// it. `None` means no typer is wired in for this run.
    pub typer_hints: Option<&'a HashMap<Symbol, Vec<cs_rir::Type>>>,
    /// Mutable scratch for passes to record what they did.
    /// Surface to the embedder (and to bench harnesses) after
    /// the run via the returned `PassStats` reference.
    pub stats: &'a mut PassStats,
}

// ---- PassStats: what each pass did ----

/// Per-run statistics. Each pass records:
/// - that it ran (`runs[name] += 1`) — a single pipeline-run
///   call increments `runs` once per executed pass
/// - how many mutations it made (`mutations[name]`) — semantics
///   are pass-defined ("blocks deleted," "constants folded,"
///   etc.); the pipeline doesn't enforce a unit
///
/// Two `HashMap`s rather than a single `HashMap<name, struct>`
/// because passes commonly want only one or the other; the split
/// keeps the per-pass accessor cheap.
#[derive(Debug, Default, Clone)]
pub struct PassStats {
    pub runs: HashMap<String, usize>,
    pub mutations: HashMap<String, usize>,
}

impl PassStats {
    /// Record a single execution of `pass_name`. Passes don't
    /// normally call this directly; the pipeline does it.
    pub fn record_run(&mut self, pass_name: &str) {
        *self.runs.entry(pass_name.to_string()).or_default() += 1;
    }

    /// Record `n` mutations made by `pass_name`. Called by the
    /// pass itself at the end of its `run` (the pipeline can't
    /// know what counts as a "mutation" generically).
    pub fn record_mutations(&mut self, pass_name: &str, n: usize) {
        *self.mutations.entry(pass_name.to_string()).or_default() += n;
    }

    /// How many times `pass_name` ran in this PassStats.
    pub fn runs(&self, pass_name: &str) -> usize {
        self.runs.get(pass_name).copied().unwrap_or(0)
    }

    /// How many mutations `pass_name` recorded in this PassStats.
    pub fn mutations(&self, pass_name: &str) -> usize {
        self.mutations.get(pass_name).copied().unwrap_or(0)
    }
}

// ---- PassRegistry: process-wide name → pass mapping ----

/// Global registry of all known passes. Builtins are registered
/// at process startup by `cs_runtime::Runtime::new` (will happen
/// in iter 2 when the builtin passes ship). Third-party plugins
/// register at their own startup (embedder-explicit or
/// `#[ctor]`-driven, per ADR 0014).
///
/// Single global registry, locked behind a `Mutex`. Registration
/// is rare (startup + plugin-load); read-only access during
/// pipeline construction is cheap (one lock-acquire per
/// `PassPipeline::from_names`).
pub struct PassRegistry {
    passes: HashMap<String, Arc<dyn Pass>>,
}

impl PassRegistry {
    /// Construct an empty registry. Normally accessed via
    /// [`PassRegistry::global`]; this constructor exists so
    /// tests can build an isolated registry without poisoning
    /// the global one.
    pub fn new() -> Self {
        Self {
            passes: HashMap::new(),
        }
    }

    /// Process-wide singleton. Builtins register here once at
    /// startup; plugins register here once at their own startup.
    pub fn global() -> &'static Mutex<PassRegistry> {
        static REGISTRY: OnceLock<Mutex<PassRegistry>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(PassRegistry::new()))
    }

    /// Register `pass`. Returns `Err` if the name violates the
    /// `[a-z][a-z0-9-]*` rule or duplicates an already-registered
    /// pass (re-registration is rejected rather than silently
    /// shadowing — duplicates are almost always a bug).
    pub fn register(&mut self, pass: Arc<dyn Pass>) -> Result<(), RegisterError> {
        let name = pass.name().to_string();
        if !is_valid_pass_name(&name) {
            return Err(RegisterError::InvalidName(name));
        }
        if self.passes.contains_key(&name) {
            return Err(RegisterError::Duplicate(name));
        }
        self.passes.insert(name, pass);
        Ok(())
    }

    /// Look up `name`. Returns `None` if unknown.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Pass>> {
        self.passes.get(name).cloned()
    }

    /// Names of all registered passes, in arbitrary order. Used
    /// by Scheme's `(installed-optimizer-passes)` (iter 3) and by
    /// diagnostic output when an unknown pass is requested.
    pub fn names(&self) -> Vec<String> {
        self.passes.keys().cloned().collect()
    }

    /// Count of registered passes.
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }
}

impl Default for PassRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a `PassRegistry::register` call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The pass's name doesn't match `[a-z][a-z0-9-]*`.
    InvalidName(String),
    /// A pass with this name is already registered.
    Duplicate(String),
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::InvalidName(n) => {
                write!(
                    f,
                    "pass name {:?} is invalid (must match [a-z][a-z0-9-]*)",
                    n
                )
            }
            RegisterError::Duplicate(n) => {
                write!(f, "pass name {:?} is already registered", n)
            }
        }
    }
}

impl std::error::Error for RegisterError {}

/// Pass-name validity check. Public so plugin authors can validate
/// names before attempting registration.
pub fn is_valid_pass_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ---- PassPipeline: an ordered selection ----

/// An ordered list of passes to run on a single `Function`. Built
/// from a list of pass names (resolved against a `PassRegistry`)
/// plus the pass-ordering rules from `Bucket::priority`.
///
/// `from_names` resolves and sorts once; subsequent `run` calls
/// reuse the sorted vec — cheap when applied to many functions
/// (e.g., a whole-module compile).
pub struct PassPipeline {
    /// Sorted by `(bucket.priority(), registration_order)`. The
    /// registration_order tie-break is implicit: equal buckets
    /// preserve the input order from `from_names`.
    selected: Vec<Arc<dyn Pass>>,
}

impl std::fmt::Debug for PassPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassPipeline")
            .field("passes", &self.names())
            .finish()
    }
}

impl PassPipeline {
    /// Build a pipeline from pass names resolved against the
    /// given registry. Returns `Err` listing every unknown name
    /// — collected so the diagnostic message can show all
    /// missing passes at once rather than failing fast on the
    /// first.
    pub fn from_names(registry: &PassRegistry, names: &[&str]) -> Result<Self, PipelineError> {
        let mut resolved = Vec::with_capacity(names.len());
        let mut unknown = Vec::new();
        for n in names {
            match registry.get(n) {
                Some(p) => resolved.push(p),
                None => unknown.push((*n).to_string()),
            }
        }
        if !unknown.is_empty() {
            return Err(PipelineError::UnknownPasses(unknown));
        }
        // Stable sort by bucket priority: equal buckets keep
        // their registration order.
        resolved.sort_by_key(|p| p.bucket().priority());
        Ok(Self { selected: resolved })
    }

    /// Convenience: empty pipeline (a no-op). Used by the
    /// pipeline integration point in `cs-vm::jit_translate` when
    /// no passes are selected — the typical case.
    pub fn empty() -> Self {
        Self {
            selected: Vec::new(),
        }
    }

    /// Whether this pipeline has any passes.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Number of passes in the pipeline (post-resolution).
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// Names of the selected passes in execution order.
    pub fn names(&self) -> Vec<&str> {
        self.selected.iter().map(|p| p.name()).collect()
    }

    /// Run every selected pass on `func`. Each pass's run is
    /// counted in `ctx.stats`. Passes are responsible for
    /// recording their own mutation counts via
    /// `ctx.stats.record_mutations`.
    pub fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        for pass in &self.selected {
            ctx.stats.record_run(pass.name());
            pass.run(func, ctx);
        }
    }
}

impl Default for PassPipeline {
    fn default() -> Self {
        Self::empty()
    }
}

/// Why a `PassPipeline::from_names` call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// One or more requested pass names aren't in the registry.
    /// The vec lists every unknown name so the diagnostic can
    /// surface them all at once.
    UnknownPasses(Vec<String>),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::UnknownPasses(names) => {
                write!(f, "unknown optimizer pass(es): {}", names.join(", "))
            }
        }
    }
}

impl std::error::Error for PipelineError {}

// ---- Integration shim for cs-vm::jit_translate ----

/// Pipeline-integration entry point called by
/// `cs-vm::jit_translate::bytecode_to_rir_full` just before the
/// translated `Function` flows on to codegen.
///
/// **Iter 1 implementation: no-op.** A future iter (3 per ADR
/// 0014) replaces the body to read the active pipeline from a
/// thread-local set by Scheme's `parameterize` over
/// `(active-passes)`. Until then, this gives the cs-vm side a
/// stable call-site whose semantics can change without further
/// cs-vm edits.
///
/// The signature deliberately takes only `&mut Function` (no
/// `SymbolTable`, no hints) so the cs-vm integration doesn't
/// have to thread state it doesn't currently carry. When iter 3
/// wires the Scheme side, that side will pull context from a
/// thread-local rather than rely on the caller threading it.
pub fn run_active_pipeline(_func: &mut Function) {
    // Intentional no-op. See iter-3 plan in ADR 0014.
}
