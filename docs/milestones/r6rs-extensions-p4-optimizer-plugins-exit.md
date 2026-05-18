# R6RS++ Phase 4 — optimizer-plugins arc, exit report

> Status: **All 6 iters in ADR 0014's rollout plan shipped. The
> framework + builtins + Scheme surface + verifier + #!lang
> integration + third-party example are in place; no follow-up
> iters are blocking on the substrate.**
> Branch: `r6rs-extensions`.
> Spec: `docs/research/r6rs_extensions_spec.md` (§"Phase 4"); design at
> `docs/adr/0014-optimizer-plugins.md`.
> Predecessor: Phase 4 typed-boundaries arc
> (`docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md`).

Captures what shipped across the optimizer-plugins arc of Phase 4
and what remains as follow-ups.

## What shipped

### Iter 1 — cs-opt crate skeleton
Commit: `b7effcf` (+ `e142801` lockfile).

New `crates/cs-opt`: `Pass` trait, `PassRegistry`, `PassPipeline`,
`PassContext`, `PassStats`, `Bucket`, `RegisterError`,
`PipelineError`. Integration shim wired into
`cs-vm::jit_translate::bytecode_to_rir_full` as a no-op pending
iter 3. 20 framework tests.

### Iter 2 — three builtin passes
Commit: `8e99617`.

- `constant-fold` (Early): folds Add/Sub/Mul of two Fixnum
  constants in the same block; uses checked arithmetic to skip
  overflow; chain-folds against newly-folded consts. Skips Div
  (R6RS exact-division semantics) and Flonum (IEEE-754 parity).
- `dead-block-elim` (Default): BFS reach from `func.entry`
  through Jump/Branch targets; `Vec::retain` drops unreachable
  blocks. Preserves entry-at-index-0 invariant cs-aot relies on.
- `inst-stats` (Late): non-mutating diagnostic; records total
  inst count + block count into stats.

`cs_opt::register_builtins(&mut registry)` registers all three.
`BUILTIN_NAMES` const lists them. 18 builtin-specific tests.

### Iter 3 — Scheme install-optimizer-pass!
Commit: `e0d698e`.

Thread-local active-pass list in cs-opt. Three new higher-order
builtins in cs-runtime: `install-optimizer-pass!`,
`remove-optimizer-pass!`, `installed-optimizer-passes`. install!
validates the name against the registry at install time (immediate
diagnostic, not silent skip at codegen). `Runtime::new()`
registers the shipped builtins on first call. The pipeline
integration shim from iter 1 now actually runs whatever the
thread-local says is active. Empty-list path short-circuits to a
single thread-local read. 11 Scheme-surface tests.

### Iter 4 — cs_rir::verify + pass-attribution
Commit: `ce10db6`.

New `cs_rir::verify(&Function) -> Result<(), VerifyError>` with
three invariants: MissingEntry, DanglingTarget,
DuplicateDefinition. Use-before-def + block-param-arity-match
deferred to a future iter when more passes plausibly produce
those classes of bug.

`cs-opt::PassPipeline::run` runs the verifier after each pass
when built with `--features pass_verify`. On verifier failure
the pipeline PANICS with the offending pass name + error
message — that's the attribution the ADR called for.

7 verify unit tests + 2 framework attribution tests (using
deliberately-broken `BuggyPass` / `DanglerPass` impls under
`#[cfg(feature = "pass_verify")]`).

### Iter 5 — #!lang ↔ pass-pipeline integration
Commit: `6ff9130`.

New `lib/lang/opt-fold.scm`: minimal demonstration `(lang
opt-fold)` library that installs `'constant-fold` at top level.
Files declaring `#!lang opt-fold` (Phase 3C MVP rewriter) thereby
opt into constant folding for the file's evaluation.

Scoping caveat (documented): install! persists across file
boundaries within a session; users wanting strict file-scope
pair install! with explicit remove! at end of file. The
Phase-2E-parameter-backed migration that would give
`parameterize` scoping is tracked but deferred — matches the
Scheme `use`/`unuse` convention.

6 lang-integration tests cover baseline, explicit lib load,
`#!lang` header, post-load eval continues working, explicit
cleanup, multi-lib stacking.

### Iter 6 (stretch) — third-party plugin example
Commit: `33bc12b`.

New `crates/cs-opt-example` crate: `NoOpCounter` Pass impl + two
install helpers (`install(&mut PassRegistry)`,
`install_global()`). Module docstring is a 5-step template for
how a real downstream Rust crate plugs into cs-opt without
requiring source modifications to cs-opt itself.

NO PERF CLAIM. The example pass is intentionally trivial. ADR
0014's "5% improvement on a workload" stretch criterion is left
as a follow-up tied to a specific motivating workload.

4 end-to-end tests verify the registration pattern works
identically to builtins (bucket ordering, name validation,
install round-trip).

## Test additions

| Suite                                            | New tests |
|--------------------------------------------------|-----------|
| crates/cs-opt/tests/framework.rs                 | 20 (+2 pass_verify) |
| crates/cs-opt/tests/builtins.rs                  | 18        |
| crates/cs-rir/src/verify.rs (unit tests)         |  7        |
| crates/cs-runtime/tests/phase4_opt_scheme_surface.rs | 11    |
| crates/cs-runtime/tests/phase4_opt_lang_integration.rs |  6  |
| crates/cs-opt-example/tests/integration.rs       |  4        |
| **Total**                                        | **66 (+2)** |

All green. Full workspace test sweep clean under both default and
`--features pass_verify` configurations.

## What's natural-but-not-yet-built

Six follow-ups that don't block any current consumer:

1. **Phase-2E Parameter migration for active-passes.** Today's
   thread-local gives a flat global model; migrating to a Scheme
   Parameter would give `parameterize` scoping for free, matching
   the spec example's file-scope expectation without manual
   install/remove pairing.

2. **Cross-function `ProgramPass` trait.** The current `Pass`
   sees one Function at a time. Inlining wants to read callee
   bodies; a parallel `ProgramPass` operating on
   `&mut HashMap<Symbol, Function>` would unblock that direction
   once the inline iter actually lands.

3. **PassContext.syms wiring.** cs-vm doesn't currently thread
   a SymbolTable through to the pipeline-integration call;
   `run_active_pipeline` stubs an empty table. Passes that need
   names today must check `ctx.syms.len() == 0` as a sentinel.
   Widening the integration signature is mechanical.

4. **PassContext.typer_hints wiring.** Same shape as #3 but for
   typer-derived hints. The hint channel exists
   (`Runtime::install_typer_hints`); plumbing it to the
   pipeline integration point would let passes specialize on
   typed procedures.

5. **Use-before-def verifier check.** Iter 4's verifier covers
   the cheap invariants; SSA-strict dominator analysis is the
   next-tier check that would catch a wider class of pass bugs
   (at non-trivial verify cost).

6. **A non-trivial example pass with measured improvement.**
   ADR 0014's iter-6 stretch criterion. Would tie this work to
   a concrete perf demonstration once a motivating workload
   exists.

## Cross-cutting

- ADR 0013 perf gates: not affected by this work. Passes are
  opt-in; baseline JIT performance reported in ADR 0013
  measurements continues to apply to non-plugin users.

- cs-typer Phase 5 `install_typer_hints` channel: unchanged.
  PassContext gained a `typer_hints` field for the future-iter
  wiring described in #4 above.

- Phase 3C `#!lang` MVP: now has a concrete consumer (iter 5's
  opt-fold lang library). Validates the MVP shape against a
  real use case.

- Phase 2B contracts library: unrelated; the typed-boundaries
  arc continues independently with its own iters and exit doc.

## What's next for Phase 4 as a whole

Phase 4 in the spec has four deliverables. After this arc and
the typed-boundaries arc:

| Deliverable           | Status                                                     |
|-----------------------|------------------------------------------------------------|
| Typed integration     | Substrate + define/typed shipped (typed-boundaries-status) |
| **Optimizer plugins** | **Full ADR 0014 rollout shipped (this doc)**               |
| Sandboxing            | Untouched. Needs design ADR before implementation.         |
| Custom readers        | Tracked as #156 (Phase 3C.full)                            |

Two of four Phase 4 deliverables have working substrate now.
Sandboxing remains as a future design ADR. Custom readers
follow-on is the natural #156 extension.
