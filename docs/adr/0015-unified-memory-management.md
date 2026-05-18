# ADR 0015: Unified Memory Management Architecture

> Status: Proposed
> Date: 2026-05-17
> Builds on: ADR 0006 (M5 tracing GC, superseded), ADR 0014
> (countable-memory)
> Related: cs-typer (gradual typing), cs-rir / cs-aot (typed IR)

## Context

The countable-memory work (iters 7.1 → 7.1.x.z, ADR 0014)
collapsed CrabScheme's reclamation story from "tracing GC with
Rc-backed Phase 1 + targeted cycle detection" to "Rc-only +
synchronous mutation-site cycle detection". That migration
delivered:

- Zero stop-the-world pauses
- Deterministic port finalization
- Detection of mutation-induced cycles (with the counter API)
- A partial structural break via `Pair::break_*_cycle` +
  caller-supplied baseline (iter 7.1.x.y)
- API primitives for a full Bacon-Rajan trial-deletion algorithm
  (iter 7.1.x.z)

The full Bacon-Rajan attempt revealed a deeper limitation: even
with subgraph reconstruction and external-anchor analysis, the
right edge to weaken in a closure-over-env cycle isn't
mechanically derivable without semantic understanding of which
edges are "structural" (forward in a list spine) vs.
"back-references" (closures over an enclosing env). The naive
edge picker either orphans freshly-allocated values or breaks
critical traversal paths.

This ADR steps back from the narrow "pick the right RC break"
problem and proposes a **layered memory-management architecture**
that combines five complementary mechanisms, each chosen for the
allocation shapes it handles best:

| Layer | Mechanism | Handles |
|---|---|---|
| 1 | **Ownership** (Rust borrows) | Stack-local, single-owner values; the host runtime's own data structures |
| 2 | **Reference counting** (`Rc<T>` / `cs_gc::Gc<T>`) | Shared heap values with acyclic or quickly-reclaimed cycles |
| 3 | **Regions** (arena allocation) | Per-call-frame allocations with known scope; pattern-matched literal data; compile-time-bounded closures |
| 4 | **Tracing** (mark/sweep, cycle collector) | Long-lived cyclic structures (the residual after RC handles the common case) |
| 5 | **Compiler analysis** (escape analysis, type-driven specialization, region inference) | Whole-program optimization: decide which mechanism to use per allocation site |

CrabScheme has the pieces of each layer either in tree
(`cs_gc::Gc<T>` for RC; `cs-typer` for type analysis; the
old M5 tracing infra still cfg-gated) or queued
(regions, escape analysis). The proposal is to **integrate
them into a single allocation strategy** rather than picking
one and ignoring the others.

## Decision

Adopt a **five-layer memory management architecture** where the
compiler (cs-typer + a new region inferencer) decides per-
allocation which mechanism to use, and the runtime supports
all five mechanisms behind a uniform `Gc<T>` smart-pointer
facade.

### Layer 1 — Ownership (Rust borrows)

**What**: The host runtime's own data structures (e.g.,
`Frame.bindings`, `Vec<NanboxValue>` on the VM stack,
`Cargo.toml`-style configuration) use Rust ownership directly.
No `Gc<T>` wrapper.

**When**: For values whose lifetime is bounded by a single Rust
function or call stack — and that don't escape into Scheme
user code. Already in use throughout cs-runtime / cs-vm.

**Why first**: Zero overhead. The compiler enforces
correctness. No reclamation cost.

**Status**: Shipped. No change.

### Layer 2 — Reference Counting (`Rc<T>` / `Gc<T>`)

**What**: The default heap representation for Scheme values
that escape Rust's stack discipline. `cs_gc::Gc<T>` wraps
`std::rc::Rc<T>`; clone is a strong-count bump, drop is
deterministic reclamation.

**When**: For Scheme values that flow through the user program:
pairs, vectors, strings, hashtables, ports, promises,
procedures. The bulk of every heap allocation.

**Why second**: Deterministic, cheap on monomorphic workloads,
no global stop. The countable-memory work (ADR 0014) ratified
this as CrabScheme's primary path.

**Gap**: Cycles via user mutation (`set-cdr!`, closure-over-env).
Layers 3, 4, 5 close the gap.

**Status**: Shipped. The structural break iter 7.1.x.z proved
that a runtime-only cycle breaker can't reliably pick safe
cycle edges; that work transitions to layers 3/4/5.

### Layer 3 — Regions (arena allocation)

**What**: A per-scope arena where allocations live as long as
the enclosing region. When the region ends, all allocations in
it free at once. No per-allocation refcount; no per-object
tracing.

**When**: For allocation sites the compiler proves are scoped to
a single dynamic extent — e.g., the args list passed to a
function call that doesn't escape, lexical closures that don't
outlive their defining `let`, intermediate `cons` chains in
`map` / `filter` pipelines.

**Why third**: Bulk reclamation (one `free` per region) beats
per-object refcount when the allocation count is high and the
lifetime is short. Cycle-free by construction (region ends
release everything; cycles inside the region don't matter).

**Inspiration**: ML Kit's region inference, Cyclone's region
types, Tofte-Talpin "Region-Based Memory Management".

**Implementation sketch**:
- A new `cs_gc::Region` type — a bump allocator with an
  attached "extent" tag.
- `Gc<T>` gains a region-aware constructor: `Gc::new_in(region,
  v)` allocates `v` in the region rather than via global `Rc`.
- The constructor returns a `Gc<T>` that, for the duration of
  the region, behaves like a normal `Gc<T>` (clone bumps a
  region-local refcount; drop decrements). When the region
  ends, the region-local arena drops in one shot, releasing
  every allocation regardless of refcount.
- For values that "promote out" of the region (escape into
  longer-lived storage), the region's destructor flips them to
  global `Rc<T>` before tearing down (copy-on-promote).

**Integration with the typer**: cs-typer infers an `effect`
annotation per expression — whether allocations in that
expression escape the current dynamic extent. Non-escaping
allocations get region tags; escaping allocations get global
`Rc`.

**Status**: Not started. Spec slug `region-memory` to be
written.

### Layer 4 — Tracing (mark-sweep cycle collector)

**What**: A precise tracing collector that runs **only** on the
residual cyclic allocations that escaped layers 1–3. Reuses
the M5 Phase 1 infrastructure (currently cfg-gated under
`#[cfg(not(feature = "countable-memory"))]`).

**When**: Triggered lazily — on memory-pressure thresholds,
explicit `(collect)`, or periodically on a background tick
when the user opts in. Operates only on values that:
- Are `Rc`-managed (layer 2) AND
- Have been observed to participate in a cycle (the
  iter-7.1.x.x detector logs candidates) AND
- Couldn't be region-handled (layer 3 confirms the cycle
  spans dynamic-extent boundaries)

**Why fourth**: For the few cycles the previous layers can't
resolve. Bacon-Rajan's full algorithm becomes feasible here
because the candidate set is tiny (cycles confirmed cross-
region) and the cost is amortized.

**Implementation**: The M5 `Heap` / `Trace` / `Marker` /
`collect()` machinery is preserved in `crates/cs-gc/src/
tracing.rs`. Iter 12b of the countable-memory spec is
unwound — the tracing path stays in tree, gated behind a
new feature `tracing-cycle-collector` that's off by default
but available for embedders that need it.

**Status**: Already in tree (cfg-gated). Re-enabling is
mechanical; the heavy lift is layer 5's analysis that decides
when to trigger it.

### Layer 5 — Compiler analysis (escape analysis + type-driven specialization)

**What**: A whole-program analysis pass that runs over the
expanded core IR (cs-ir) and annotates each allocation site
with:
- **Lifetime category**: `stack`, `region(R)`, `rc`,
  `traced`
- **Escape**: does the value flow out of the current call
  frame / region?
- **Cyclicity hint**: from the typer's effect system —
  could this allocation participate in a cycle?
- **Region**: which region (if any) owns this allocation?

The runtime uses these annotations to pick the right
constructor (`Stack::alloc`, `Region::alloc_in(R)`,
`Gc::new`, `Gc::new_traced`) without changing the call
site's source.

**Inputs**:
- **cs-typer's type system**: already infers types for
  expressions; can be extended to track "may-cycle" (a
  value of type T that holds a `cs_runtime::Closure` whose
  env may close over a binding to itself). Static
  analysis prunes most allocations from needing cycle
  detection at all.
- **cs-rir's typed IR**: already carries type info from
  cs-typer (via `rir_bridge`). Extend RIR types with
  lifetime annotations.
- **Existing constructor sites**: `Pair::new`,
  `Vector::new`, etc. — instrument each to dispatch on
  the annotation.

**Outputs** (what the analysis pass produces):
- Per-allocation lifetime annotation embedded in the IR.
- A small bytecode opcode set extension: `AllocPair`
  becomes `AllocPair[region|rc|traced]`.

**Why fifth**: This is the integration layer. Without it,
layers 3–4 are unused (the user has to manually opt into
regions; the tracing GC has to be triggered manually).
With it, the runtime automatically uses the cheapest
mechanism for each allocation.

**Status**: Not started. Depends on cs-typer's effect
inference (also not started). Spec slug `escape-analysis`
to be written.

## Architecture diagram

```
┌─────────────────────────────────────────────────────────────────┐
│  Scheme program (R6RS)                                         │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  cs-expand → cs-ir (core IR)                                    │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  cs-typer (gradual types) → cs-rir (typed IR)                   │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Layer 5: Escape analysis + region inference                    │
│  ───────────────────────────────────────────                    │
│  Annotates each `cons` / `make-vector` / `lambda` / etc. with   │
│  one of: stack / region(R) / rc / traced                        │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Allocation dispatch (per annotation)                          │
├──────────────┬───────────────┬───────────────┬──────────────────┤
│   stack      │   region(R)   │      rc       │     traced       │
│   Layer 1    │    Layer 3    │    Layer 2    │     Layer 4      │
│              │               │               │                  │
│  Ownership   │   Bump arena  │  Rc<T> /      │  Mark-sweep GC   │
│  (Rust)      │   (Region R)  │  Gc<T>        │  (cs-gc          │
│              │               │  (countable-  │  tracing,        │
│              │               │   memory)     │  cfg-gated)      │
└──────────────┴───────────────┴───────────────┴──────────────────┘
```

## Why this architecture

### Each layer handles what it's best at

- **Ownership** (Rust borrows): zero overhead, compile-time
  safety, no reclamation. Best for values that never escape
  Rust scope.
- **RC**: deterministic, pay-as-you-go, no global pause. Best
  for the long tail of Scheme heap values that escape Rust
  scope but don't form cycles.
- **Regions**: bulk free, cycle-free by construction. Best
  for bounded-extent allocations in tight loops (`map`,
  `filter`, `let`-locals).
- **Tracing**: handles the residual cyclic garbage that the
  above three can't. Best for rare, lived-long-enough cycles
  where the amortized cost of a sweep is justified.
- **Compiler analysis**: the conductor. Without it, the user
  picks; with it, the system picks.

Crucially, **no single layer carries the whole load**. RC's
weakness (cycles) is covered by regions (no cycles inside a
region) and tracing (residual). Tracing's weakness (pause
time, fragmentation) is reduced because regions handle the
bulk and RC handles the rest. Regions' weakness (manual scope
prediction) is removed by escape analysis. Ownership's
limitation (no shared state) is the reason layers 2–4 exist.

### CrabScheme has the foundations

- **Layer 1**: shipped (Rust ownership throughout `cs-runtime`
  / `cs-vm`).
- **Layer 2**: shipped (countable-memory, ADR 0014).
- **Layer 3**: not started, but `cs_gc::Gc<T>` is the right
  abstraction wall — adding `Region::alloc_in` is internal.
- **Layer 4**: in tree, cfg-gated (`crates/cs-gc/src/
  tracing.rs`). Just needs the trigger policy.
- **Layer 5**: cs-typer exists and reaches into cs-rir. Effect
  inference / escape analysis is additive on top.

### Comparable systems

- **Cyclone** (C with region types): regions inferred via type
  system, no GC needed.
- **ML Kit** (Standard ML compiler): region inference primary,
  GC fallback for cases the region inferencer punts on.
- **Lobster** (Wouter van Oortmerssen): static lifetime analysis +
  ownership + RC fallback, no GC.
- **Verona** (Microsoft Research): region-based with capability
  types, RC inside regions.
- **Inko**: ownership + RC + capabilities, no tracing GC.
- **Roc** (functional, immutable): RC + opportunistic mutation
  via uniqueness, no GC.
- **Lean 4**: RC + Perceus (compile-time reuse) + escape
  analysis.

None combine ALL of (ownership + RC + regions + tracing +
analysis). CrabScheme could.

## Consequences

### Positive

- **Best-in-class memory safety**: every value uses the most
  efficient mechanism the analysis can prove safe.
- **Better than any single-layer competitor**: RC-only systems
  (Swift, Inko) leak cycles; tracing-only systems (V8, JVM)
  have pause issues; region-only systems (ML Kit) punt to GC
  for hard cases.
- **Migration path that doesn't break existing work**: each
  layer can be developed and rolled out incrementally. Layer
  2 (countable-memory) is already done; layer 3 (regions) is
  next; layer 4 (tracing) is preservation; layer 5 (analysis)
  is whole-system.
- **Embeddability**: the same Rust crate (`cs-runtime`) can be
  configured for different deployment targets (WASM with no
  regions; embedded with no tracing; server with all five).

### Negative / risks

- **Complexity**: five layers is a lot of surface area. Mitigate
  via the `Gc<T>` uniform facade — most call sites never see
  the layer.
- **Analysis quality is load-bearing**: layer 5 has to be smart
  enough to keep layers 3–4 well-fed. Bad escape analysis
  forces too much into RC and the cycle problem comes back.
  Mitigate via gradual rollout: start with conservative
  analysis (everything is RC) and tighten over time.
- **Correctness risk in the region copy-on-promote path**:
  values escaping a region need to be deep-copied to global
  storage. Cyclic values that span regions are the killer
  case. Mitigate via the typer: cycles are detectable at the
  type level via the effect system, and the analyzer can
  refuse to region-allocate values whose type indicates
  potential cycling.
- **Scope mismatch with R6RS semantics**: continuations
  (`call/cc`) make region scoping non-trivial — a captured
  continuation can re-enter a region long after its
  "official" extent ended. Mitigate via the
  one-shot-continuation policy that CrabScheme's `cs-vm`
  already prefers; multi-shot continuations promote their
  region to RC at capture time.

### Things that change for users

- **Performance**: programs with tight `map` / `filter` loops
  should see significant improvements as their intermediate
  cells move from RC to regions.
- **GC pauses**: still zero by default. Tracing only runs when
  the runtime opts in, and only on the residual.
- **Memory usage**: lower steady-state (regions bulk-free)
  but with brief allocation spikes (a region holds its peak
  until release).

### Things that don't change

- The `Value` enum's variant set, the JIT raw-handle ABI
  (ADR 0012 D-2), the WASM target's surface — all preserved.
- The countable-memory work (ADR 0014) — fully preserved as
  layer 2.

## Implementation roadmap

The work is sequenced from "lowest risk + highest leverage"
first:

| Step | Scope | Effort | Gates on |
|---|---|---|---|
| **5a** — Effect inference in cs-typer | Track per-expression "may allocate" / "may cycle" / "escapes" | 2-3 weeks | cs-typer (in tree) |
| **3a** — `cs_gc::Region` arena type | Bump allocator + extent guard | 1-2 weeks | Layer 2 stable |
| **5b** — Annotate cs-rir with lifetime tags | Extend RIR Type with lifetime info; rir_bridge propagates | 1-2 weeks | 5a |
| **3b** — `Gc::new_in(region, v)` constructor + dispatch | Per-allocation site, pick region-or-rc based on RIR tag | 2-3 weeks | 3a, 5b |
| **5c** — Conservative analysis pass | Mark all allocations RC unless region-safe | 1-2 weeks | 3b |
| **4a** — Re-enable tracing as opt-in feature | Un-deprecate `tracing-cycle-collector` feature; document | 1 week | none |
| **5d** — Tracing-trigger policy | When does cs-runtime opt into a tracing pass? | 2-3 weeks | 4a |
| **5e** — Tighten analysis | Move more allocations from RC to regions as analysis improves | ongoing | continuous |

Total: ~3-5 months for v1 of all five layers; ongoing tightening
indefinitely.

## Open questions

1. **Region polymorphism**: can a function be polymorphic over
   the region of its argument? Cyclone says yes (region kinds);
   ML Kit says yes (region polymorphism). CrabScheme should
   look at both.
2. **Continuations and regions**: how do escape continuations
   interact with region scoping? Probably one-shot continuations
   are OK (they unwind in extent); multi-shot needs RC
   promotion.
3. **JIT lowering of region allocation**: can Cranelift emit
   bump-allocator inline-fastpath code (as fast as native `new`
   on a generational nursery)?
4. **AOT region lowering**: same question for cs-aot.
5. **Per-thread regions?** Single-threaded today; if we ever go
   multi-threaded, regions need thread affinity.

These resolve as part of step 3a / 5d / future specs.

## Follow-ups

- Spec `region-memory` (regions)
- Spec `escape-analysis` (layer 5 analysis pass)
- ADR 0016 ratifying region type rules (after step 3a + 3b)
- Update ADR 0014 to mark layer 2 as "one of five" rather than
  the sole mechanism

## References

- Tofte & Talpin, "Region-Based Memory Management" (1997)
- Grossman et al., "Region-Based Memory Management in Cyclone"
  (PLDI 2002)
- Bacon & Rajan, "Concurrent Cycle Collection in
  Reference Counted Systems" (ECOOP 2001)
- Reinking et al., "Perceus: Garbage Free Reference Counting
  with Reuse" (PLDI 2021)
- Verona memory model:
  https://github.com/microsoft/verona/tree/main/docs/explore
- Lean 4 RC + Perceus: Leonardo de Moura et al.
- ML Kit project: https://elsman.com/mlkit/
