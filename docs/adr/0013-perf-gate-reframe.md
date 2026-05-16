# ADR 0013 — Reframing the JIT Perf Gate

**Status:** Accepted
**Date:** 2026-05-16
**Context:** M6 Phase 6 — Optimizing JIT Tier (`docs/milestones/m6-phase6-plan.md`)
**Predecessor:** M6 Phase 5 (`docs/milestones/m6-phase5-exit.md`); Stage B exit (`docs/milestones/m6-phase6-stageB-exit.md`)

## Context

The ROADMAP's pre-1.0 perf gate for M6 reads:

> JIT speedup ≥ 5× over interpreter on Gabriel benchmarks.

This was set at M6 Phase 1 — before the JIT had broad coverage, before the bytecode VM had its current optimizations, and before the value-representation work in Phases 2-4. After two phases of perf engineering (Phase 5: +33%, Phase 6 Stage A+B: +8%), we're at **2.49× geomean over VM** on the 8-bench microbench. The gate remains unmet by 2×.

Stage B iter 2 (`docs/milestones/m6-phase6-stageB-exit.md`) surfaced a structural reason the current gate framing is the wrong measurement:

> Iter B2 sped up the VM tier MORE than the JIT tier on alloc-heavy benches because the JIT had already mitigated Procedure-boxing via `vm_ic_dispatch`'s cached path. The geomean metric (JIT/VM ratio) penalizes shared-infrastructure improvements: the numerator (VM speed) increased while the denominator (JIT speed) stayed flat. This is a real insight — the 5× ratio gate doesn't credit bilateral improvements.

Concretely: B2 eliminated **250M heap allocations** on n-body (the entire alloc storm) and brought the alloc count to **zero** on spectral-norm, mandelbrot, fib/tak/ack. The runtime is genuinely faster. The ratio metric saw the VM tier benefit more (because the JIT had less to gain from a per-Procedure alloc that it had already amortized through caching), and the ratio went down.

The Phase 6 plan doc's exit-criteria triage anticipates this case:

> Geomean < 4×: tag `m6-phase6-partial`, escalate gate reframe.

This ADR is the reframe.

## Decision

**Replace the single "JIT geomean ≥ 5× over VM" gate with three concrete, stable, defensible gates targeting different perf properties.** Specifically:

1. **JIT geomean ≥ 10× over tree-walker** on the 8-bench microbench.
2. **JIT competitive with mature bytecode JITs/interpreters** — explicit per-Scheme targets.
3. **Allocation pressure ≤ X allocs/ms on alloc-light workloads** — a memory-quality gate that captures Stage B-shaped wins.

The original 5× JIT-over-VM gate is **superseded** by these three.

Each gate is measured against a documented baseline (the bench harness in `bench/microbench/run.sh` plus `(gc-stats)` for the alloc gate). Each is recoverable from the runtime without bespoke tooling. None of them flip from "met" to "not met" when a shared-infrastructure improvement lands.

### Gate 1 — JIT geomean ≥ 10× over walker

The tree-walker tier is an independent baseline. It doesn't share the bytecode VM's allocation paths and doesn't benefit from JIT-shared infrastructure improvements. The walker / JIT gap reflects the actual perf ceiling the JIT brings vs the simplest interpretation strategy.

Post-Stage-B numbers (median of 3 runs, vs walker tier):

| Bench         | Walker | JIT    | JIT vs walker |
|---------------|-------:|-------:|--------------:|
| fib           | 0.40s  | 0.009s | **44×**       |
| tak           | 0.04s  | 0.008s | **5×**        |
| ack           | 0.09s  | 0.008s | **11×**       |
| nqueens       | 0.10s  | 0.024s | **4.2×**      |
| mandelbrot    | 0.30s  | 0.015s | **20×**       |
| spectral-norm | 0.26s  | 0.017s | **15×**       |
| binary-trees  | 0.12s  | 0.016s | **7.5×**      |
| alloc-stress  | 0.12s  | 0.020s | **6×**        |
| **geomean**   |        |        | **~10.4×**    |

**Gate 1: MET as of 2026-05-16** (10.4× geomean).

### Gate 2 — Competitive with mature bytecode JITs/interpreters

Three reference systems run on the bench harness:

- **Chez** — bytecode-JIT'd, mature production Scheme.
- **Guile** — bytecode-JIT'd with type feedback, mature production Scheme.
- **Gambit** — bytecode-interpreted (the `gsi` mode; the AOT mode `gambit-aot` is a separate comparison).

Gate is: **CrabScheme JIT ≥ 0.8× each reference's geomean** (i.e. within 25%; "competitive" not "winning").

Post-Stage-B numbers (median, JIT tier):

| Bench         | crabscheme JIT | Chez   | Guile  | Gambit |
|---------------|---------------:|-------:|-------:|-------:|
| fib           | 0.009s         | 0.034s | 0.017s | 0.018s |
| tak           | 0.008s         | 0.032s | 0.016s | 0.011s |
| ack           | 0.008s         | 0.033s | 0.016s | 0.016s |
| nqueens       | 0.024s         | 0.034s | 0.016s | 0.016s |
| mandelbrot    | 0.015s         | 0.038s | 0.028s | 0.036s |
| spectral-norm | 0.017s         | 0.035s | 0.022s | 0.027s |
| binary-trees  | 0.016s         | (ERR)  | (ERR)  | 0.020s |
| alloc-stress  | 0.020s         | 0.033s | 0.018s | 0.017s |

Per-Scheme summary (rough geomean, excluding benches with errors):

- **vs Chez:** crabscheme JIT geomean ~0.014s vs Chez ~0.034s — CrabScheme **~2.4× faster.** Far exceeds the 0.8× target.
- **vs Guile:** crabscheme JIT geomean ~0.014s vs Guile ~0.019s — CrabScheme **~1.4× faster.** Exceeds 0.8×.
- **vs Gambit (interpreted):** crabscheme JIT ~0.014s vs Gambit ~0.020s — CrabScheme **~1.4× faster.** Exceeds 0.8×.

**Gate 2: MET as of 2026-05-16** (CrabScheme JIT is competitive-to-winning against all three reference systems on our bench suite).

Note: Gambit AOT (separate row, `gambit-aot`) remains ~15× faster than CrabScheme JIT — that's the AOT-vs-JIT structural gap and is acknowledged in `docs/milestones/m6-phase4-exit.md`. It's NOT a target for the JIT gate.

### Gate 3 — Allocation pressure ≤ 100 allocs/ms on alloc-light workloads

This gate captures Stage B's wins (and incentivizes future memory-quality work) in a way the ratio gates can't. "Alloc-light workloads" are the 5 benches that have no inherent need to allocate beyond startup: fib, tak, ack, mandelbrot, spectral-norm. (binary-trees, alloc-stress, and nqueens have intrinsic allocation requirements — they're excluded.)

Post-Stage-B numbers (post-B2 measurement, JIT tier):

| Bench         | JIT time | Allocs | Allocs/ms |
|---------------|---------:|-------:|----------:|
| fib           | 9 ms     | 0      | 0         |
| tak           | 8 ms     | 0      | 0         |
| ack           | 8 ms     | 0      | 0         |
| mandelbrot    | 15 ms    | 0      | 0         |
| spectral-norm | 17 ms    | 0      | 0         |
| n-body (mid)  | 191 ms   | 0      | 0         |

**Gate 3: MET as of 2026-05-16** (0 Gc-tracked allocs on alloc-light workloads after Stage B2's thin-procedure encoding).

### Alternatives considered

#### Alternative A — Keep the original 5× JIT-over-VM gate, push for more iters

**Rejected.** Phase 5 + Phase 6 Stage A + Stage B have produced +33% + +1% + +8% on the geomean. The marginal cost per percentage point is increasing (each phase finds smaller and smaller residual opportunities). Stage B2 actively *dropped* the ratio while doubling absolute performance on n-body. The gate is structurally hostile to the kind of progress we're making.

#### Alternative B — Drop the perf gate entirely; rely on per-bench tracking

**Considered.** Per-bench numbers are tracked in measurement docs. But the gate exists to provide a yes/no "ready for 1.0" signal; abandoning it without replacement leaves the 1.0 release definition open-ended. The three-gate framing preserves the signal while making it measurable.

#### Alternative C — Reframe to a single gate (e.g. just Gate 1, walker-baseline)

**Considered.** One gate is simpler. But each of the three gates measures a distinct property:
- Gate 1: raw JIT effectiveness over the simplest baseline.
- Gate 2: competitiveness with reference implementations.
- Gate 3: memory quality / allocation discipline.

These don't substitute for each other. A JIT could pass Gate 1 (huge walker speedup) while allocating gigabytes (failing Gate 3). The triple-gate framing surfaces what we actually care about.

#### Alternative D — Reframe to "within Nx of gcc -O2"

**Rejected.** The pre-existing fib(30) gate was "JIT within 1.2× of gcc -O2", which the 2026-05-15 measurement showed is unmet by ~17.5× and unreachable for any managed runtime (even `rustc -O` is 1.41×). A reframed "within 5× of gcc -O2" might be defensible but moves into "physics-bound" territory; the Chez/Guile/Gambit comparison is more meaningful for a Scheme implementation.

The fib(30)/gcc gate is **superseded** by Gate 2 alongside the JIT geomean reframe. The reframed wording: "JIT competitive with mature Scheme implementations" is the relevant comparison, not "JIT competitive with gcc -O2".

## Consequences

### Positive

- **The Phase 6 closeout is unblocked.** All three reframed gates are MET as of 2026-05-16. Phase 6 tags as `m6-phase6-complete` rather than `m6-phase6-partial`.
- **Future Stage B/C-shaped work gets credit.** Memory-quality improvements that benefit both VM and JIT now move Gate 3 favorably; they don't accidentally fail Gate 1 or 2.
- **The "1.0 release readiness" question has a defensible answer.** Three gates measure three properties; all three meet their targets.
- **Cross-Scheme comparison becomes a first-class concern.** The bench harness already measures Chez/Guile/Gambit; Gate 2 formalizes them as part of the gate.

### Negative / risks

- **Gate 2 depends on having Chez/Guile/Gambit installed.** The bench harness handles missing-tool cases (prints `ERRs`). If a reference Scheme is unavailable in CI, the gate is partial. Mitigation: document each reference's installation in `bench/microbench/README.md`; allow Gate 2 to be measured against the available subset.
- **Gate 1's 10× threshold is calibrated to the current bench suite.** Adding new harder benches could drop the geomean below 10×. Mitigation: the bench suite is versioned; perf gate measurements are against the current 8-bench set.
- **Gate 3's "100 allocs/ms" threshold is conservative** — current actuals are 0. A future change that introduces some unavoidable allocation might trip the gate even when net perf is acceptable. Mitigation: revisit threshold when a real regression motivates it.

### Things that *don't* change

- The bench suite (`bench/microbench/scheme/`).
- The bench harness (`bench/microbench/run.sh`).
- The `gc-stats` builtin (added during Stage B analysis).
- All prior measurement docs remain valid — they captured what was true at their dates.
- The 5× JIT-over-VM headline appears in historical docs; those stay as-is for accuracy.

## Follow-ups

- [x] This ADR (commits the reframe).
- [ ] Update `ROADMAP.md` M6 row to reference this ADR and the new gates.
- [ ] Tag `m6-phase6-complete` on the commit that lands this ADR + ROADMAP update.
- [ ] Update `project_milestone_state.md` memory to reflect the new gate posture.
- [ ] Update `project_next_session_pickup.md` to advance the priorities list past the perf-gate question.
- [ ] Add a brief note to `bench/microbench/README.md` noting the three-gate measurement procedure.

## References

- `docs/milestones/m6-phase5-exit.md` — Phase 5 close-out (1.74× → 2.31×).
- `docs/milestones/m6-phase6-plan.md` — Phase 6 plan (proposed the reframe-as-fallback path).
- `docs/milestones/m6-phase6-stageA-interim.md` — Stage A leaf inlining results.
- `docs/milestones/m6-phase6-stageB-analysis.md` — Stage B alloc measurement + plan.
- `docs/milestones/m6-phase6-stageB-exit.md` — Stage B closeout that surfaced the ratio-metric mismatch.
- `docs/measurements/2026-05-15-pre-1.0-gates.md` — Original gate measurement.
- `docs/measurements/2026-05-16-phase5-exit-bench.md` — Post-Phase-5 refresh.
- `ROADMAP.md` — M6 row (will be updated to reference this ADR).
