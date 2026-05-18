# Tracing Revival — Requirements

> Status: **CLOSED** (2026-05-17, with Phase 2/3 trial-
> deletion + background-sweep deferred — see
> `docs/milestones/tracing-revival-exit.md` and
> `docs/adr/0018-tracing-cycle-collector.md`).
> Spec slug: `tracing-revival`
> Roadmap slot: Layer 4 of the unified memory management
> architecture (ADR 0015).
> Predecessor: `countable-memory` (Layer 2, ADR 0014).
> Companions: `region-memory` (Layer 3); `escape-analysis`
> (Layer 5).

This spec **reverses the countable-memory iter-12b plan**: rather
than deleting the M5 tracing infrastructure (`crates/cs-gc/src/
tracing.rs`), promote it from "cfg-gated rollback fallback" to
"opt-in residual cycle collector". Wire it as Layer 4 of the
unified architecture — the rare-event collector for cycles that
Layer 3 (regions) can't bulk-free and Layer 5 (escape
analysis) can't statically rule out.

## Why revive tracing

The countable-memory work (ADR 0014) established that:
- RC handles the common case (acyclic + simple cycles via
  iter 7.1.x.y's caller-supplied baseline).
- Cycles via mutation site (set-car! / set-cdr!) are detected
  via the synchronous counter (iter 7).
- A full Bacon-Rajan trial-deletion attempt (iter 7.1.x.z)
  showed that subgraph reconstruction + safe edge selection
  is genuinely complex; runtime-only solutions struggle with
  mid-recursion observability.

Layer 3 + Layer 5 (regions + escape analysis) together prevent
most cycles from ever forming — values that don't escape their
region get bulk-freed regardless of internal references.

The **residual** — cycles that span regions AND aren't
runtime-breakable safely AND have no external anchor — is
exactly what tracing GC is good at: a periodic walk identifies
and reclaims the orphans.

The M5 tracing infrastructure already exists (`crates/cs-gc/
src/tracing.rs`, ~623 LOC, fully tested). This spec just:
1. Renames the feature flag (`countable-memory` → kept;
   add `tracing-cycle-collector` as additional opt-in).
2. Wires the trigger policy (when does a sweep happen?).
3. Limits the sweep's candidate set to cycle-suspect
   allocations (per the iter-7 detector's counter).
4. Integrates with regions (don't sweep region-allocated
   values; they'll free at region drop).

## Goals

- Make tracing GC **available** to embedders without forcing
  it. Off by default, on by explicit feature.
- Make tracing GC **automatic** when on — trigger policy
  decides when sweeps run, not the embedder.
- Make tracing **cheap** when on — only sweep cycle-suspect
  allocations, not the whole heap.
- Preserve Layer 2 correctness — tracing doesn't change
  Layer 2 semantics; it just adds a periodic cleanup pass.

---

## Functional requirements

### FR-1. Add `tracing-cycle-collector` feature

Re-expose the M5 tracing infrastructure as a new feature
flag on cs-gc, cs-core, cs-runtime, cs-vm:

```toml
[features]
default = ["countable-memory"]
countable-memory = []
# Layer 4: opt-in residual cycle collector. Operates on the
# cycle-suspect candidate set populated by countable-memory's
# detector. Default off; embedders that have observed cycle
# growth in their workload enable this for periodic cleanup.
tracing-cycle-collector = ["countable-memory"]
```

The existing `crates/cs-gc/src/tracing.rs` module gets
renamed conceptually — it's no longer the "alternate path"
to countable-memory but the "Layer 4 add-on".

**Acceptance**: `cargo build --features tracing-cycle-collector`
succeeds and includes the tracing module. The module's
existing 13 unit tests (in tracing.rs's #[cfg(test)] block)
still pass.

### FR-2. Repurpose the `Heap` / `Trace` / `Marker` machinery

The M5 `Heap` + `Trace` + `Marker` + `collect` types stay
available under `tracing-cycle-collector`. They're scoped
differently:

- **Old role**: the heap for ALL allocations.
- **New role**: a thread-local "cycle candidate registry"
  that the countable-memory detector populates when it
  observes a cycle. The `collect()` operation walks the
  registry, marks reachable, sweeps unmarked.

The registry has bounded size — only allocations that
participated in a detected cycle live there.

**Acceptance**: a new `cs_gc::cycle_registry` module exposes:
```rust
pub fn register_cycle_candidate(addr: usize, weak: Box<dyn AnyWeak>);
pub fn unregister_cycle_candidate(addr: usize);
pub fn run_sweep();
pub fn candidate_count() -> usize;
```

### FR-3. Trigger policy

The tracing sweep can be triggered three ways:

1. **Memory pressure threshold**: when `candidate_count()`
   exceeds a configurable limit (default 10⁴), invoke
   `run_sweep` automatically on the next allocation.
2. **Explicit Scheme primitive**: `(collect)` (already
   present per countable-memory, becomes a no-op when
   tracing-cycle-collector is off; runs a sweep when on).
3. **Background tick**: an embedder can spawn a thread that
   periodically calls `run_sweep` (CrabScheme single-thread
   default; embedders can opt into this).

**Acceptance**: a test that allocates 10⁵ cycle-suspect
candidates and verifies the auto-trigger fires before
allocation count grows unbounded.

### FR-4. Candidate set populated by Layer 2 detector

When the countable-memory cycle detector
(`cs_gc::cycle::check_and_break`) reports a cycle (the
counter increments), the detected root is registered as a
cycle candidate. The `cs_runtime::countable_memory_cycle`
module gains a `register_candidate(pair_addr)` call invoked
from `b_set_car` / `b_set_cdr` on positive detection.

**Acceptance**: a test verifies that after `(set-cdr! x x)`,
`candidate_count()` is at least 1. After a sweep, candidates
whose strong-count is 0 are reclaimed.

### FR-5. Region-allocated values excluded from candidate set

Region-allocated (Layer 3) `Gc<T>` handles are NEVER
registered as cycle candidates — their reclamation is
guaranteed via region drop. The candidate-registration logic
checks `Gc::is_region(p)` before registering.

**Acceptance**: a test creates a cyclic structure in a region,
performs `set-car!` mutations, drops the region;
`candidate_count()` stays at 0 throughout (and after).

### FR-6. Sweep operation

`run_sweep()` performs a mark-sweep over the candidate
registry:

1. **Mark phase**: for each candidate, check if its `Weak`
   handle still upgrades to a `Gc<T>`. If yes, mark live;
   if no, mark for reclaim.
2. **Sweep phase**: drop the `Weak`s pointing to reclaimed
   allocations from the registry.
3. **Cycle reclaim**: for entries marked live, trace via the
   existing `Trace` machinery to find cycles among
   candidates; for cycles where no node has an external
   anchor (all internal-cycle-only), drop one cycle edge
   to break the cycle.

The trace + cycle-break uses the M5 infrastructure: `Marker`
sweeps the candidate set, identifies dead ones, frees them.

**Acceptance**: a test creates a cyclic structure outside any
region (Layer 2 Rc-allocated), runs `(collect)`, observes
the cycle members reclaimed (Drop sentinels fire).

### FR-7. Sweep latency bounds

A sweep over N candidates runs in O(N log N) or better. For
N = 10⁴ (the auto-trigger threshold), the sweep completes
in < 10ms (release build, modest hardware).

**Acceptance**: a microbenchmark `bench/tracing_sweep.rs`
asserts the latency bound.

### FR-8. Compatibility with regions + escape analysis

The integration with Layer 3 (regions):
- Region-allocated values never enter the candidate set
  (FR-5).
- A region's drop doesn't trigger a sweep; the candidate
  set just naturally shrinks as the Weak handles fail to
  upgrade for region-drop allocations.

The integration with Layer 5 (escape analysis):
- Lifetime::Traced allocations explicitly request tracing.
  When `tracing-cycle-collector` is off, they fall back to
  `Lifetime::Rc`.

**Acceptance**: an integration test confirms: build with
`--features regions,tracing-cycle-collector`, allocate
mixed (some region, some traced); observe correct
reclamation per allocation's lifetime.

---

## Non-functional requirements

### NFR-1. Off-by-default — no impact when disabled

When `tracing-cycle-collector` is off (the default), there
must be no runtime overhead, no extra code in hot paths,
no extra allocations. Measured via a microbenchmark
comparing default vs. `--features tracing-cycle-collector`
on a non-cycle-creating workload.

### NFR-2. Bounded candidate-registry memory

The candidate registry's memory cost is bounded:
`sizeof(Weak<dyn AnyWeak>) * candidate_count()`. For the
default 10⁴ limit, this is ~160 KB max. Acceptable for
embedded targets.

### NFR-3. Embedder control over policy

The trigger thresholds (auto-trigger candidate count,
background tick interval) are configurable via
`Runtime::set_tracing_policy(TracingPolicy)`. Embedders that
need different policies (high-throughput servers vs.
constrained embedded devices) can tune.

### NFR-4. WASM compatibility

The tracing-cycle-collector feature works on WASM. The
background-tick triggering is per-embedder (WASM has no
threads), but memory-pressure and explicit-`(collect)`
triggers work.

### NFR-5. ADR

A new ADR (`docs/adr/0018-tracing-cycle-collector.md`)
ratifies:
- The opt-in / off-by-default policy.
- The candidate-registry approach (vs. whole-heap tracing).
- The integration with Layers 2, 3, 5.
- The deferral of generational copying (still out of
  scope; this spec only covers cycle reclamation).

---

## Out of scope

| Item | Why excluded |
|---|---|
| Generational copying | Pure perf optimization; not part of correctness story. Out of scope per ADR 0006's original deferral. |
| Concurrent / incremental tracing | Single-thread CrabScheme today; concurrent tracing is a post-multithreading concern. |
| Stack scanning | Precise rooting per the M5 design; conservative stack scan is rejected per ADR 0006. |
| Trace-everything baseline | The whole point is to avoid this; tracing is **only** for cycle candidates. |

---

## Risks

1. **Candidate registry leaks** if `register_candidate` is
   called more than `unregister_candidate`. The registry
   grows unbounded → memory leak.
   *Mitigation*: registry uses `Weak<T>` handles, not `Strong`.
   Weak handles auto-cleanup when the target drops.

2. **Auto-trigger too aggressive** → frequent sweeps cause
   pause spikes.
   *Mitigation*: NFR-3 — embedder-configurable threshold;
   adaptive threshold (raise on miss-rate, lower on hit-rate)
   is a future iter.

3. **Sweep races with user code** in a future multithreaded
   scenario.
   *Mitigation*: out-of-scope for v1; this spec is
   single-thread.

4. **Cycle reclamation behaviour change** — tracing might
   reclaim a cycle the user holds via a weakly-tracked path.
   *Mitigation*: tracing operates only on candidates the
   detector observed; it doesn't second-guess user-visible
   structure.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `tracing-cycle-collector` feature added | `Cargo.toml`s |
| `cs_gc::cycle_registry` module | `crates/cs-gc/src/cycle_registry.rs` (new) |
| Auto-trigger on threshold | FR-3 |
| Candidate populated by Layer 2 detector | FR-4 |
| Region-allocated values excluded | FR-5 |
| `(collect)` runs sweep when feature on | FR-3 |
| Sweep reclaims pure-internal cycles | FR-6 |
| Sweep ≤ 10ms for 10⁴ candidates | FR-7 |
| Off-by-default zero overhead | NFR-1 |
| Embedder-tunable policy | NFR-3 |
| WASM build green | NFR-4 |
| ADR 0018 written | NFR-5 |
