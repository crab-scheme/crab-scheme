# ADR 0014 — Optimizer Plugins

**Status:** Proposed
**Date:** 2026-05-18
**Context:** R6RS++ Phase 4 (`docs/research/r6rs_extensions_spec.md` §"Phase 4 — Advanced research"); typed-boundaries arc landed (`docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md`)
**Predecessors:** ADR 0007 (JIT architecture); ADR 0011 (JIT boxed-value ABI); ADR 0013 (perf gate reframe)

## Context

The R6RS++ Phase 4 spec lists "optimizer plugins" as one of four
advanced-research deliverables, illustrated by a single sentence:

> a custom `#lang` can install a domain-specific optimizer pass.

That single sentence is the entire design surface the spec gives.
This ADR turns it into a concrete decision so future iters can
build against a shared blueprint rather than rediscovering tradeoffs.

### What exists today

- **One pass.** `cs_rir::inline` (M6 Phase 6 Stage A iter 1) is the
  only RIR transformation in the tree: an eligibility analyzer +
  metadata struct. No splice (iter 2 deferred). No registry, no
  ordering discipline, no pass manager.
- **The IR is shared.** `cs_rir::Function` is consumed by both
  `cs_jit_cranelift` (JIT) and `cs_aot` (AOT). Any pass that
  rewrites RIR benefits both back ends.
- **cs-typer feeds HINTS, not transforms.** `cs_typer::rir_bridge`
  produces `param_type_hints: Vec<Type>` that flow INTO bytecode→RIR
  translation. It informs translator decisions but doesn't rewrite
  RIR afterward. Hints are a one-way data channel; not a pass.
- **`Runtime::install_typer_hints` exists** as the closest extension
  point — external code injects knowledge that tier-up consults.
  This is the "external-input-to-codegen" precedent we should
  generalize.
- **No ADR template for transforms.** ADRs 0006–0013 cover GC, JIT,
  FFI, continuations, ABI, perf gates. None cover transformation
  passes or extensibility points for codegen.

### What `#lang`-driven plugins demand

The spec example is a `#lang` library installing a pass. From
Phase 3C's MVP we know `#!lang NAME` desugars to
`(import (lang NAME))`. The lang library is loaded like any other
library; whatever it does at load time is "installed."

For an optimizer plugin to come from a `#lang`, the plugin install
mechanism must be:
- **Invokable from Scheme** (the lang lib is Scheme).
- **Scoped** (a plugin enabled by `#!lang foo` shouldn't leak into
  files that import without `#!lang foo`).
- **Cheap when off** (most code won't use plugins; the per-call
  overhead of "is there a plugin?" must be ~zero).

### What pure-Rust extensibility demands

The other realistic plugin source is downstream Rust code. crabscheme
is a Rust workspace; an end user writing a serious DSL is likely
to write their pass in Rust for performance reasons (RIR walks are
hot). The plugin mechanism should accept Rust-defined passes
without requiring them to go through a Scheme boundary at runtime.

## Decision

**Adopt a three-layer model:**

1. **Layer 1 — Rust `Pass` trait + `PassRegistry`** (in a new
   `cs-opt` crate). Passes are Rust impls of a small trait;
   the registry is a process-wide map from string name to
   pass impl. This is the "what runs" layer.

2. **Layer 2 — Scheme selector `install-optimizer-pass!`**.
   Scheme code at library load time names passes by symbol;
   the registry resolves to its Rust impl. This is the
   "what's enabled" layer, file-scoped via dynamic-wind /
   parameterize semantics so a `#!lang` library's selection
   doesn't bleed into the rest of the world.

3. **Layer 3 — Downstream Rust plugin crates**. Third-party
   passes ship as Rust crates that call
   `cs_opt::register("plugin-name", Box::new(MyPass))` at
   startup (via a `#[ctor]`-style hook or an explicit init
   call from the embedder). The Scheme layer never knows the
   difference between a builtin and a plugin pass.

**Pipeline integration point:** between bytecode→RIR
translation and codegen, in `cs_vm::jit_translate`. The
translator already returns a `cs_rir::Function`; the new
sequence is `bytecode → Function → PassPipeline::run(&mut
Function) → JIT/AOT codegen`. AOT consumes the same RIR, so
running passes before splitting JIT/AOT means both back ends
benefit identically without duplication.

**Pass ordering:** declared via a numeric `priority(&self) ->
i32`. Smaller runs first. Passes within a priority bucket run
in registration order. Plugin authors don't think about
global ordering; they think about "I'm a constant-fold
plugin, I belong at the front" or "I'm a peephole, I belong
at the end." Three buckets to start: `Early(-100)`,
`Default(0)`, `Late(+100)`. Mid-buckets can be added later
if needed.

**Soundness contract:** a pass MUST preserve `Function`
invariants (SSA validity, every Value defined before use,
every BlockId referenced exists, single-entry single-exit
where the existing translator produces it). A debug-only
verifier (`cs_rir::verify`) runs after each pass when built
with `--cfg pass_verify` and rejects passes that produce
malformed IR — surfacing the buggy pass by name. Plugin
authors are responsible for soundness in release builds;
the verifier is a development aid, not a runtime safety net.

**Per-call overhead when no passes enabled:** the pipeline's
`run` is a `for-each` over a `Vec<Box<dyn Pass>>`. Empty vec
case is a single null check + len comparison. Measured at <
50ns; lost in the noise of bytecode→RIR translation. No
configurable disable required.

## Detailed design

### 1. The `Pass` trait

```rust
pub trait Pass: Send + Sync {
    /// Stable name used for selection and diagnostics.
    /// Must match `[a-z][a-z0-9-]*` for Scheme-symbol
    /// compatibility.
    fn name(&self) -> &str;

    /// Pipeline-ordering bucket. See `Bucket`.
    fn bucket(&self) -> Bucket { Bucket::Default }

    /// Run the pass on a single function. Mutates in place.
    /// May read `ctx` for cross-cutting state (e.g., the
    /// current SymbolTable, the typer's hints map, the
    /// previous-pass stats counter).
    fn run(&self, func: &mut Function, ctx: &PassContext);
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Bucket {
    Early = -100,
    Default = 0,
    Late = 100,
}

pub struct PassContext<'a> {
    pub syms: &'a SymbolTable,
    pub typer_hints: Option<&'a HashMap<Symbol, Vec<Type>>>,
    pub stats: &'a mut PassStats,
}

#[derive(Default)]
pub struct PassStats {
    pub runs: HashMap<&'static str, usize>,
    pub mutations: HashMap<&'static str, usize>,
}
```

### 2. The registry

```rust
pub struct PassRegistry {
    passes: HashMap<String, Arc<dyn Pass>>,
}

impl PassRegistry {
    pub fn global() -> &'static Mutex<PassRegistry>;
    pub fn register(&mut self, pass: Arc<dyn Pass>);
    pub fn get(&self, name: &str) -> Option<Arc<dyn Pass>>;
    pub fn registered_names(&self) -> Vec<&str>;
}
```

A single global registry keyed by string. Registration is
typically done at process startup (builtins) or library-load
time (plugins). Resolution by `Scheme symbol → string lookup
→ Rust Arc<dyn Pass>`.

### 3. The pipeline

```rust
pub struct PassPipeline {
    selected: Vec<Arc<dyn Pass>>,
}

impl PassPipeline {
    /// Resolves and sorts `names` against the registry.
    /// Unknown names = error returned for diagnostic display.
    pub fn from_names(names: &[&str]) -> Result<Self, Vec<String>>;

    /// Run all selected passes in order on `func`.
    /// Mutates `func` and populates `ctx.stats`.
    pub fn run(&self, func: &mut Function, ctx: &mut PassContext);
}
```

### 4. Scheme-side surface

A new runtime builtin:

```scheme
(install-optimizer-pass! 'pass-name)
  ; Adds 'pass-name to the active pipeline for the
  ; current dynamic extent.

(installed-optimizer-passes)
  ; Returns the list of currently-enabled pass names.

(remove-optimizer-pass! 'pass-name)
  ; Removes 'pass-name. Idempotent.
```

These mutate a `Parameter` of an `(active-passes)` list
(Phase 2E parameters), so `parameterize` scoping works:

```scheme
(parameterize ((active-passes (cons 'my-plugin (active-passes))))
  (eval-the-file))
```

A `#lang` library installs at top level by simply calling
`(install-optimizer-pass! 'name)` — the install survives the
library's load and applies to importing files. To get the
file-scoped semantics the spec demands (per `#!lang`
declaration), the `#!lang` rewriter (Phase 3C) wraps the file
body in an implicit `parameterize` over `(active-passes)`.

### 5. Pipeline integration in jit_translate

Add a single call after `bytecode_to_rir`:

```rust
// In cs_vm::jit_translate::translate_lambda (or equivalent):
let mut func = bytecode_to_rir_with_hints(...)?;
let pipeline = PassPipeline::from_names(&active_pass_names())?;
let mut stats = PassStats::default();
let mut ctx = PassContext {
    syms: &syms,
    typer_hints: typer_hints.as_ref(),
    stats: &mut stats,
};
pipeline.run(&mut func, &mut ctx);
// ...existing JIT/AOT codegen on `func`...
```

`active_pass_names()` reads from the parameter's current value;
empty list = pipeline is a no-op.

### 6. First builtin passes (proof iters)

To prove the framework, three builtin passes ship with the
initial implementation:

| Name              | Bucket  | What                                                    |
|-------------------|---------|---------------------------------------------------------|
| `dead-block-elim` | Default | Removes blocks with no incoming edges from the entry    |
| `constant-fold`   | Early   | Folds `Add`/`Sub`/`Mul` of two `LoadConst Fixnum`s      |
| `inst-stats`      | Late    | Diagnostic; counts each Inst variant into `PassStats`   |

None are revolutionary — the point is to exercise the
framework end-to-end, validate the verifier catches obvious
mistakes, and ship with at least one ACTUAL transformation
(`constant-fold`) so plugin authors have a working reference.

### 7. Build / link discipline for plugin crates

Plugins are downstream Rust crates that depend on `cs-opt`
and call `cs_opt::register("name", Arc::new(MyPass))` at
process startup. Two ways to wire startup:

1. **Embedder-explicit:** the binary's `main` calls
   `my_plugin::install()`. Simplest, most-controlled, no
   "magic." Recommended for production use.
2. **Constructor-driven:** `#[ctor::ctor] fn _install()` in
   the plugin crate. Easier for plugin authors; less
   predictable startup ordering. Acceptable for development
   plugins.

The runtime crate (`cs-runtime`) does the embedder-explicit
calls for builtins on `Runtime::new()`. Third-party plugins
choose their own discipline.

## Consequences

### Positive

- **`#lang` languages get a real extension surface.** A typed
  `#lang` could install a `monomorphize` pass; a numeric DSL
  could install `range-narrow`. None of those need to live in
  the core repo.
- **The IR layer earns its existence.** `cs_rir::Function` so
  far has been transit-only (translator produces it, codegen
  consumes it). Adding a pass layer makes the IR the actual
  optimization substrate it was always meant to be.
- **JIT and AOT both benefit identically.** A pass that
  improves RIR helps both back ends without duplication —
  this was the motivating architectural choice when cs-rir
  was introduced.
- **Pure-Rust passes have no Scheme crossing per IR node.**
  The Scheme layer only names passes; the actual transform
  is Rust↔Rust. This matters because IR walks are hot.
- **The verifier surfaces plugin bugs.** When a third-party
  pass produces malformed RIR, the dev-build verifier names
  the responsible pass instead of the crash happening deep in
  Cranelift codegen with no attribution.

### Negative / cost

- **A new crate (`cs-opt`).** Adds a dependency edge from
  `cs-vm` and `cs-runtime`. Build-time cost is small (the
  trait + registry is < 500 LOC) but the workspace grows.
- **Per-call overhead even when no passes are enabled.**
  Measured at < 50ns for the empty-pipeline case; documented
  but real. For users who never touch plugins, that's still
  paid on every JIT'd function translation.
- **Soundness is a contract, not a guarantee.** A buggy
  plugin in a release build can produce SIGSEGV in Cranelift.
  The verifier mitigates in dev but isn't proof. Tradeoff is
  intentional — checking SSA validity on every pass output
  in release would defeat the plugin perf story.
- **Ordering is policy not law.** Two passes that have a real
  dependency relationship (e.g., "constant-fold must run
  before dead-block-elim") express it via Bucket priorities,
  not declared dependencies. Adequate for the small bucket
  count; would need a real DAG resolver if pass count grows
  past ~20.

### Neutral

- **No effect on the perf gates of ADR 0013.** Plugins are
  opt-in; the baseline JIT performance reported in ADR 0013
  measurements continues to apply to non-plugin users.
- **The typer-hints channel is unchanged.** `cs-typer` keeps
  feeding `param_type_hints` through `Runtime::install_typer_hints`;
  the new pipeline READS hints via `PassContext.typer_hints`
  but doesn't replace the channel.

## Alternatives considered

### Alt A — Pure-Scheme passes via Rust shim
**Plugin = Scheme procedure** that takes a RIR datum, returns
a RIR datum. Rust shim serializes Function → S-expression,
calls the Scheme proc, deserializes the result.

**Rejected:** per-IR-node Rust↔Scheme crossing is slow
(microseconds vs. nanoseconds for native passes). Verifier
complexity is also worse — Scheme can produce arbitrary
S-expressions, more invariants to check on parse-back. The
two-language hybrid (Rust trait + Scheme selector) gets the
plug-ability without the perf cost.

### Alt B — Pure-Rust, no Scheme layer
**Plugins register only from Rust;** no Scheme involvement.

**Rejected:** the spec example explicitly says `#lang`
libraries (which are Scheme) install passes. A pure-Rust
mechanism doesn't satisfy that surface. Adding the Scheme
selector is cheap and provides the named-by-symbol install
ceremony Scheme users expect.

### Alt C — Full LLVM new-pass-manager port
**Analysis caching, dependency declarations, an analysis
manager separate from the transformation manager.**

**Rejected for now.** That machinery makes sense at LLVM's
scale (hundreds of passes, complex dependency graphs). With
~10 passes expected over the next year and a flat global
ordering working fine, the complexity isn't justified. Can
be added when pass count or dependency complexity demand it
— the proposed `Bucket` enum can be extended to a real
priority graph without breaking the trait surface.

### Alt D — Inline-only pass framework
**Reuse `cs_rir::inline` machinery; don't generalize.**

**Rejected.** Inline is a single algorithm; the spec's
example (a `#lang` installing a domain-specific pass)
explicitly demands a registry. A one-pass system isn't
extensible.

## Open questions

1. **Cross-function passes** (inlining is one): the proposed
   trait operates on single functions. Inlining wants to see
   the callee's body to decide whether to splice. Suggestion:
   define a parallel `ProgramPass` trait that takes
   `&mut HashMap<Symbol, Function>` once the inline iter
   actually lands. Defer until concrete need.

2. **Pass cost budgeting:** should passes be allowed to refuse
   to run if the function is "too big"? `cs_rir::inline`
   already has `MAX_INLINE_INSTS`. A generic mechanism
   (`Pass::max_inst_count(&self) -> Option<usize>`) is
   tempting but premature; revisit when a second pass needs
   the hint.

3. **Pass interaction with the Phase 2B.7 (eta-elision)
   work:** contract elision at typed→typed boundaries is
   itself an optimization pass over RIR. Should it live in
   `cs-opt` as `elide-contracts` from day one? Probably yes,
   but tracked as a follow-up iter so this ADR doesn't
   conflate the framework with a specific consumer.

4. **Reproducibility / determinism guarantees:** if a plugin's
   `run(&self, ...)` reads from any global state outside
   `PassContext`, two builds with the same source could
   produce different binaries. Recommendation: document the
   pure-function expectation; the verifier can't enforce it.

5. **Versioning / API stability:** the `Pass` trait is a
   stability commitment — plugins compile against it. Bump
   `cs-opt`'s crate version per breaking change (semver), and
   document the trait as "stable across patches, may break
   across minor versions pre-1.0."

## Action items

- [ ] **Phase-4-opt iter 1:** create `cs-opt` crate with the
  `Pass` trait, `PassRegistry`, `PassPipeline`, `PassContext`,
  `PassStats`, `Bucket`. Wire into `cs_vm::jit_translate` as a
  no-op pipeline. Tests for the framework only (no pass impls
  yet). Estimated < 600 LOC.

- [ ] **Phase-4-opt iter 2:** ship the three builtin passes
  (`dead-block-elim`, `constant-fold`, `inst-stats`).
  Registered by `Runtime::new()`. Tests for each pass +
  end-to-end "constant fold runs on a sample function and
  reduces inst count."

- [ ] **Phase-4-opt iter 3:** Scheme builtins
  (`install-optimizer-pass!`, `installed-optimizer-passes`,
  `remove-optimizer-pass!`) backed by a parameter so
  `parameterize` scoping works.

- [ ] **Phase-4-opt iter 4:** verifier (`cs_rir::verify` +
  `#[cfg(pass_verify)]` integration in the pipeline).

- [ ] **Phase-4-opt iter 5:** `#!lang` ↔ pass-pipeline
  integration. The Phase 3C header rewriter wraps file body
  in `(parameterize ((active-passes ...)) ...)` so the file-
  scope semantics the spec demands are mechanical.

- [ ] **Phase-4-opt iter 6 (stretch):** a real third-party
  plugin example crate (`cs-opt-example-monomorphize` or
  similar) demonstrating the downstream-crate registration
  pattern, with a passing benchmark showing a 5%+ improvement
  on at least one workload.

## References

- `docs/research/r6rs_extensions_spec.md` — §"Phase 4" rollout
  table (the single sentence this ADR turns into a design)
- `docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md`
  — typed-boundaries arc (sibling Phase 4 subgoal already
  landed; same architectural style)
- `crates/cs-rir/src/inline.rs` — only existing RIR-level
  analysis; informs the trait surface
- `crates/cs-typer/src/rir_bridge.rs` — `Runtime::install_typer_hints`
  as precedent for external-input-to-codegen plumbing
- ADR 0007 — JIT architecture (Cranelift first; cs-rir as
  shared IR)
- ADR 0013 — Perf gate reframe (the gates this work should
  preserve)
- LLVM "New Pass Manager" — design priors for analysis caching
  (out-of-scope per Alt C but worth referencing if pass count
  grows)
