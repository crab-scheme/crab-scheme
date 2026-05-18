# Tracing Revival — Tasks

> Status: **CLOSED** (2026-05-17). Iters 1-5 complete; tasks
> #4 Phase 2/3 (full Bacon-Rajan trial-deletion) and the
> `Runtime::start_background_sweep` half of task #7 deferred
> — see `docs/milestones/tracing-revival-exit.md` and
> `docs/adr/0018-tracing-cycle-collector.md`.
> Companion: `requirements.md`, `design.md`.
> Format mirrors the countable-memory / region-memory specs.

Depends on the `region-memory` spec being CLOSED (for FR-5
region exclusion). Independent of the `escape-analysis` spec
(though `Lifetime::Traced` allocations from there route here).

5 iters, each a single commit per the per-iter commit policy.

---

## Iter 1 — `tracing-cycle-collector` feature

- [x] 1. Add feature flag + un-deprecate tracing.rs
  - File: `crates/cs-gc/Cargo.toml`,
    `crates/cs-core/Cargo.toml`,
    `crates/cs-runtime/Cargo.toml`,
    `crates/cs-vm/Cargo.toml`,
    workspace `Cargo.toml`
  - Add to each crate:
    ```toml
    [features]
    # ... existing ...
    tracing-cycle-collector = ["countable-memory"]
    ```
    The cs-gc forward chain is: cs-core forwards to cs-gc,
    cs-runtime forwards to cs-core, etc.
  - Verify `crates/cs-gc/src/tracing.rs` is no longer
    pinned to `#[cfg(not(feature = "countable-memory"))]`
    (its old role); instead, it stays available under the
    new feature.
  - Purpose: re-expose the M5 tracing infrastructure as an
    opt-in addon to countable-memory.
  - _Leverage: the existing `crates/cs-gc/src/tracing.rs`
    module (~623 LOC)._
  - _Requirements: FR-1_
  - _Prompt: Role: Rust workspace maintainer | Task: Add
    the `tracing-cycle-collector` feature to each
    workspace crate that needs it; ensure the existing
    `crates/cs-gc/src/tracing.rs` compiles under the new
    feature (modify its cfg gates to allow both
    `countable-memory` and `tracing-cycle-collector` to
    enable it). The feature implies countable-memory
    | Restrictions: do NOT regress countable-memory
    (default-on); the new feature is purely additive |
    Success: `cargo build -p cs-gc --features tracing-cycle-collector`
    succeeds; the existing 13 tracing.rs unit tests still
    pass under the new feature configuration._

---

## Iter 2 — `cs_gc::cycle_registry` module

- [x] 2. Implement `cycle_registry` module
  - File: `crates/cs-gc/src/cycle_registry.rs` (new),
    `crates/cs-gc/src/lib.rs`
  - Implement per design.md §"Component 1":
    - `thread_local! REGISTRY: RefCell<HashMap<usize, Box<dyn AnyWeak>>>`
    - `thread_local! AUTO_TRIGGER_THRESHOLD: Cell<usize>`
    - `pub trait AnyWeak { ... }`
    - `pub fn register_cycle_candidate<T: 'static + CycleVisit>(addr, weak)`
    - `pub fn unregister_cycle_candidate(addr)`
    - `pub fn run_sweep()`
    - `pub fn candidate_count() -> usize`
    - `pub fn set_auto_trigger_threshold(n)`
  - Gate on `#[cfg(feature = "tracing-cycle-collector")]`.
  - Add `pub mod cycle_registry;` to lib.rs.
  - The `run_sweep` for iter 2 is a stub returning early; the
    actual cycle-reclaim logic lands in iter 4.
  - Purpose: stand up the registry API.
  - _Leverage: cs_gc::Weak (from countable-memory),
    cs_gc::cycle::CycleVisit._
  - _Requirements: FR-2, FR-3, NFR-2_
  - _Prompt: Role: Rust developer | Task: Implement
    `crates/cs-gc/src/cycle_registry.rs` per design.md
    §"Component 1" with stubbed `run_sweep` (return early).
    The trait `AnyWeak` is implemented for `cs_gc::Weak<T>`
    via a blanket impl | Restrictions: gate on the new
    feature; thread_local!-based (no global mutex); the
    `register_cycle_candidate` returns immediately if
    candidate_count is already at the threshold but the
    auto-trigger flag wasn't yet set — this avoids
    double-registration | Success: 4 unit tests in
    `cycle_registry.rs` cover register/unregister/
    candidate_count/set_auto_trigger_threshold._

---

## Iter 3 — Wire Layer 2 detector to populate the registry

- [x] 3. Modify countable-memory detector to register candidates
  - File: `crates/cs-runtime/src/countable_memory_cycle.rs`,
    `crates/cs-runtime/src/builtins/mod.rs`
  - Add a new function in `countable_memory_cycle.rs`:
    ```rust
    #[cfg(all(feature = "countable-memory", feature = "tracing-cycle-collector"))]
    pub fn record_cycle_with_candidate<T: 'static + cs_gc::cycle::CycleVisit>(
        p: &cs_gc::Gc<T>,
    ) {
        CYCLE_COUNT.with(|c| c.set(c.get().saturating_add(1)));

        #[cfg(feature = "regions")]
        if cs_gc::Gc::is_region(p) {
            return;
        }

        let addr = cs_gc::Gc::as_addr(p);
        let weak = cs_gc::Gc::downgrade(p);
        cs_gc::cycle_registry::register_cycle_candidate(addr, weak);
    }
    ```
  - Modify `b_set_car` / `b_set_cdr` to call
    `record_cycle_with_candidate(p)` when the tracing feature
    is on; otherwise fall back to `record_cycle_detected()`.
  - Purpose: populate the candidate set automatically.
  - _Leverage: countable-memory's existing record_cycle_detected
    pattern._
  - _Requirements: FR-4, FR-5_
  - _Prompt: Role: Rust developer | Task: Wire the
    countable-memory cycle detector to register cycle
    candidates with the new registry. Region-allocated
    values are excluded per FR-5 (use
    `cs_gc::Gc::is_region`) | Restrictions: when the
    `tracing-cycle-collector` feature is OFF, behaviour
    must match countable-memory iter 7.1.x.y (no
    registration); when ON, registration fires on every
    detected cycle except region-allocated | Success:
    a test demonstrates that after N=20
    `(set-cdr! x_i x_i)` mutations (x_i top-bound),
    `candidate_count() == N`._

---

## Iter 4 — Sweep cycle-reclaim logic

- [x] 4. Implement `run_sweep` mark+sweep+reclaim
  - File: `crates/cs-gc/src/cycle_registry.rs`
  - Replace the stubbed `run_sweep` with the algorithm per
    design.md §"Component 3":
    - Phase 1: walk each candidate, mark reachable.
    - Phase 2: drop registry entries whose Weak no longer
      upgrades.
    - Phase 3: identify pure-internal cycles among
      reachable candidates; for each, pick a safe edge to
      break via `Pair::break_*_cycle(0)`.
  - `identify_internal_cycles` uses the iter 7.1.x.z
    Bacon-Rajan subgraph reconstruction (now in a CONTROLLED
    environment — the candidate set, not the whole heap).
  - `pick_safe_edge` follows the same logic as iter 7.1.x.z's
    `try_safe_break` (which was reverted from the runtime
    but the design is correct for THIS controlled context).
  - Purpose: actually reclaim residual cycles.
  - _Leverage: iter 7.1.x.z's design (now applied correctly
    in a controlled environment)._
  - _Requirements: FR-6, FR-7_
  - _Prompt: Role: Rust developer with GC algorithm
    background | Task: Implement run_sweep with the three
    phases per design.md §"Component 3". The cycle-reclaim
    uses Pair::break_car_cycle(0)/break_cdr_cycle(0)
    (baseline=0 because the sweep already verified the
    candidate is reachable and the edge is safe to break)
    | Restrictions: the sweep operates ONLY on the
    candidate set, never the broader heap; the mark phase
    uses Marker from tracing.rs; the cycle reclaim picks
    one edge per cycle group and lets RC drop the rest |
    Success: an integration test creates a cyclic structure
    outside any region, runs run_sweep, observes the
    Drop sentinels fire for all cycle members._

- [x] 5. Wire `(collect)` Scheme builtin
  - File: `crates/cs-runtime/src/builtins/mod.rs`
  - Modify the existing `b_collect` (which is a no-op
    under countable-memory) to call
    `cs_gc::cycle_registry::run_sweep()` when the
    `tracing-cycle-collector` feature is on.
  - Purpose: give users explicit control over sweep timing.
  - _Leverage: existing b_collect builtin._
  - _Requirements: FR-3_
  - _Prompt: Role: Rust developer | Task: Update b_collect
    in cs-runtime to call `run_sweep` when the tracing
    feature is enabled; remain a no-op otherwise | Restrictions:
    behaviour unchanged for non-tracing builds | Success:
    a Scheme-level test `(collect)` triggers a sweep when
    the feature is on; under default features the builtin
    is still a no-op._

- [x] 6. Auto-trigger on threshold
  - File: `crates/cs-gc/src/cycle_registry.rs`,
    `crates/cs-gc/src/rc_only.rs`
  - In `register_cycle_candidate`: after adding to
    REGISTRY, if `REGISTRY.len() >= AUTO_TRIGGER_THRESHOLD`,
    set a TLS `SWEEP_PENDING: Cell<bool>` to true.
  - In `Gc::new` (the next allocation): check
    `SWEEP_PENDING`; if true, clear it and call
    `run_sweep`.
  - Purpose: automatic cleanup without explicit `(collect)`.
  - _Leverage: thread_local! pattern._
  - _Requirements: FR-3_
  - _Prompt: Role: Rust developer | Task: Implement the
    auto-trigger flag per the description. The check in
    Gc::new is `#[cfg(feature = "tracing-cycle-collector")]`-gated;
    when off, zero overhead | Restrictions: the check
    must be a single TLS read in the Gc::new hot path
    (~1ns) when SWEEP_PENDING is false; when true, the
    sweep runs synchronously before the allocation
    returns (consistent with the Scheme runtime's
    single-thread model) | Success: a test sets threshold=5,
    registers 6 candidates, observes auto-trigger fires
    on the next Gc::new._

---

## Iter 5 — Embedder API + ADR 0018 + exit report

- [x] 7. `TracingPolicy` + `Runtime::set_tracing_policy`
  - File: `crates/cs-runtime/src/lib.rs`
  - Implement per design.md §"Component 5":
    - `pub struct TracingPolicy { auto_trigger_threshold: usize, background_tick: Option<Duration> }`
    - `Runtime::set_tracing_policy(&mut self, policy: TracingPolicy)`
    - `Runtime::start_background_sweep(&self, interval: Duration)` —
      spawns a thread that periodically calls run_sweep.
  - Purpose: embedder control.
  - _Leverage: existing Runtime structure._
  - _Requirements: NFR-3_
  - _Prompt: Role: Rust developer | Task: Add TracingPolicy
    struct + Runtime::set_tracing_policy +
    Runtime::start_background_sweep per design.md
    §"Component 5". The background thread is gated on
    target_arch != wasm32 (WASM has no threads, but the
    other triggers still work) | Restrictions:
    set_tracing_policy mutates the runtime; background
    sweep thread holds a Weak<Runtime> not Strong (no
    forced-alive); the thread loop checks Weak::upgrade
    and exits cleanly when the runtime drops |
    Success: 3 unit tests cover policy mutation, threshold
    application, background-sweep start (test asserts
    a sweep ran via a counter)._

- [x] 8. ADR 0018 + exit report + spec close
  - File: `docs/adr/0018-tracing-cycle-collector.md` (new),
    `docs/milestones/tracing-revival-exit.md` (new),
    spec files status update.
  - Write ADR 0018 per requirements.md NFR-5:
    - Off-by-default policy.
    - Candidate-registry approach (not whole-heap).
    - Integration with Layers 2, 3, 5.
    - Deferral of generational copying.
  - Write exit report per the M5 / countable-memory /
    region-memory style.
  - Mark spec status CLOSED.
  - Purpose: lock layer 4 of the unified architecture into
    project history.
  - _Leverage: previous ADR styles._
  - _Requirements: NFR-5_
  - _Prompt: Role: Rust + documentation author | Task:
    Write ADR 0018, exit report, mark spec CLOSED.
    ADR covers the four ratification points; exit
    report includes the FR-7 latency measurement,
    the NFR-1 off-by-default zero-overhead confirmation,
    and the deferred items | Restrictions: do not delete
    the M5 tracing infrastructure (the whole point of
    this spec is to keep it as an opt-in addon) |
    Success: ADR landed; exit report measurements
    included; spec marked CLOSED._

---

## Sequencing summary

| Iter | Title | Depends on | Default-on? |
|------|-------|------------|-------------|
| 1 | Feature flag + un-deprecate tracing | region-memory iters 1–3 (for is_region) | no (opt-in) |
| 2 | cs_gc::cycle_registry module | 1 | no (gated on feature) |
| 3 | Wire detector to registry | 2 | no |
| 4 | Sweep cycle-reclaim logic | 3 | no |
| 5 | Embedder API + ADR 0018 | 4 | no |

All iters keep the feature opt-in. Default cs-runtime build
includes only Layers 1, 2, 3 (when region-memory ships).
Layer 4 activates when an embedder opts into
`tracing-cycle-collector`.

## What this spec enables

After this spec:
- Embedders that need cycle reclamation (long-running daemons,
  notebook kernels) opt into `tracing-cycle-collector` and
  get automatic residual cleanup.
- The candidate set populated by Layer 2's detector is small
  (only cycle-suspect allocations), so sweeps are cheap.
- Region-allocated values are correctly excluded; only true
  cross-region cycles enter the registry.
- Layer 5 (escape-analysis) emits `Lifetime::Traced` for
  allocations it can't otherwise place, and those route
  here for safe reclamation.

The five-layer architecture of ADR 0015 is now complete:
- Layer 1: shipped (ownership)
- Layer 2: shipped (countable-memory)
- Layer 3: spec'd (region-memory)
- Layer 4: spec'd (this)
- Layer 5: spec'd (escape-analysis)
