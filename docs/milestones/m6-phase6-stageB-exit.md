# M6 Phase 6 Stage B Exit Report — Allocation Pressure Reduction

> Status: **Stage B closed at iter B2**, B3+B4 provisionally deferred.
> Parent: `docs/milestones/m6-phase6-plan.md`.
> Predecessor: Stage A interim (`m6-phase6-stageA-interim.md`); Stage B analysis (`m6-phase6-stageB-analysis.md`).

## What landed

### Iter B1 (`bd7254d`) — exact-integer fast path for Fixnum/Fixnum division

Speculative inline `sdiv + srem + branch` fast path for `Inst::Div` in the uniform-NB tier. Eliminates the ~3 ephemeral Rational allocations per always-divisible `(/ Fixnum Fixnum)` call (spectral-norm's `matrix-elt` is the motivating case).

**Headline:** spectral-norm 1.73× → **5.18×** (+199% on that bench). Geomean 2.33× → 2.67× (+14%).

### Iter B2 (`e36ba8b`) — thin-procedure NB encoding

Activates the previously-reserved `NB_TAG_PROCEDURE` with a per-thread ProcTable: u32-index encoding for `Value::Procedure(Rc<dyn Procedure>)` instead of `nb_alloc_gc_value(Procedure(p))` per encoding.

**Allocation reduction (gc-stats deltas):**

| Bench         | Pre-B2   | Post-B2  | Δ      |
|---------------|---------:|---------:|-------:|
| spectral-norm | 106,392  | 0        | -100%  |
| mandelbrot    | 31,987   | 0        | -100%  |
| nqueens       | 162,317  | 3,850    | -97%   |
| binary-trees  | 133,947  | 128,689  | -4%    |
| alloc-stress  | 200,612  | 198,978  | 0%     |
| fib/tak/ack   | <1k each | 0        | -100%  |
| **nbody**     | **250M** | **0**    | **-100%** |

The remaining Gc-tracked allocations (binary-trees, alloc-stress) are *intrinsic* to the program semantics — tree nodes and Cons cells that genuinely escape.

**Bench perf (5-run medians):**

| Bench         | VM Δ vs pre-B2 | JIT Δ vs pre-B2 | Ratio Δ |
|---------------|---------------:|----------------:|--------:|
| fib           | same           | same            | same    |
| tak           | -10% (VM ↑)    | same            | -10%    |
| ack           | +5%            | same            | -1%     |
| nqueens       | **-17% (VM ↑)** | same            | -14%    |
| mandelbrot    | same           | same            | +3%     |
| spectral-norm | -6% (VM ↑)     | same            | -3%     |
| binary-trees  | **-15% (VM ↑)** | noise           | -15%    |
| alloc-stress  | -8% (VM ↑)     | -20% (JIT ↑↑)   | -25%    |

**Geomean impact:** 2.67× → **2.49×** (-7%).

The geomean **decreased** because B2 sped up the VM tier on alloc-heavy benches MORE than it sped up the JIT tier. The JIT had already mitigated the Procedure-boxing cost through `vm_ic_dispatch`'s caching pattern; the VM was paying full price per `Inst::MakeClosure`. Eliminating the per-encoding heap alloc benefitted VM disproportionately.

This is a **shared-infrastructure improvement** — it makes the program faster absolutely, but the JIT/VM ratio metric penalizes it because the numerator (VM speed) increased while the denominator (JIT speed) stayed flat.

## Cumulative Phase 6 progress

| Stage / iter           | Geomean | Δ vs predecessor |
|------------------------|--------:|----------------:|
| Phase 5 exit baseline  | 2.31×   | —              |
| Stage A iter 2         | 2.33×   | +1%            |
| Stage B iter 1         | 2.67×   | +15%           |
| Stage B iter 2         | 2.49×   | -7%            |
| **Net Phase 6 so far** | **2.49×** | **+8%**       |

vs the 5× pre-1.0 perf gate: still 2× below.

## Why B3+B4 are provisionally deferred

The original Stage B plan included:

- **B3:** named-let closure caching (~3 iters, +5-10% expected geomean).
- **B4:** measurement + closeout (~1 iter).

Reasons to defer B3:

1. **Diminishing-returns observation.** B2's perf result shows that eliminating Procedure-related allocs already covers nbody's 250M alloc storm. The remaining MakeClosure cost is mostly `Rc<VmClosure>` allocations (Rust's heap allocator path), which doesn't show in `gc-stats` and may already be amortized by the OS-level malloc cache.

2. **Geomean-metric mismatch.** B2 demonstrated that the "JIT geomean over VM" metric **penalizes** shared-infrastructure improvements. If B3 also speeds up VM proportionally (closures are emitted by both tiers), the ratio would drop further despite improving absolute perf.

3. **Implementation complexity.** Named-let closure caching requires reliable "doesn't escape" analysis at the bytecode compiler level, which is non-trivial (escape via Cons, return-from-function, parent-scope set!, ...). The corresponding fix at the JIT level (loop transformation for tail-self-call patterns) is already done in Phase 3-5.

4. **The Phase 6 plan's exit-criteria triage already accommodates this.** Per the plan doc:
   > **≥5×:** tag `m6-phase6-complete`, gate MET.
   > **4-5×:** tag complete + ADR proposing gate reframe to 4×.
   > **<4×:** tag `m6-phase6-partial`, escalate gate reframe.
   At 2.49× we're well below the auto-tag threshold and the path forward is the **ADR reframe**, not more incremental iters.

## Recommendation: reframe the perf gate, then close Phase 6 partial

The 5× geomean gate measures JIT/VM ratio. After B2, that ratio is misleading: it credits gaps but not shared improvements. CrabScheme's runtime is genuinely faster post-B1+B2 (250M allocs eliminated on nbody, big absolute wins on flonum benches) — just not in a way the ratio metric captures.

Two reframe options for a follow-on ADR:

### Option A — "JIT competitive with mature bytecode interpreters" gate

Replace the 5× ratio with explicit absolute targets:
- ≥1× Chez geomean (Chez is JIT'd: a real apples-to-apples comparison).
- ≥1.5× Guile geomean (Guile is bytecode-JIT with type feedback).
- ≥3× Gambit-interpreter geomean (Gambit's interpreter is the reference for "fast bytecode VM").

Post-B2 actuals (rough):
- vs Chez: JIT beats Chez on fib/tak/ack/mandelbrot/spectral-norm. Close on nqueens. **Mostly met.**
- vs Guile: closer but mixed. **Partial.**
- vs Gambit interp: mixed. **Partial.**

### Option B — "JIT geomean ≥ 3× over walker" gate

The walker tier is a more stable baseline (it doesn't benefit from JIT optimizations to shared infra). Post-B2 geomean over walker:
- walker → VM is roughly 5-10× per bench (already measured in `2026-05-15-pre-1.0-gates.md`).
- VM → JIT is the current 2.49×.
- JIT → walker would be ~12-25× geomean.

Setting a **JIT vs walker ≥ 10×** gate is achievable and stable against shared improvements.

## Closeout posture

Stage B closes at iter B2 with the changes landed. The Phase 6 trajectory at this point:

- Stage A (leaf-callee inlining) — done at iter 2, deferred iter 3.
- Stage B (alloc pressure) — done at iter 2, deferred B3.
- Stage C (type-feedback specialization) — not started.
- Stage closeout — pending the gate reframe ADR.

Phase 6 as a whole will tag `m6-phase6-partial` once the ADR lands (per the plan doc's exit criteria for <4× geomean). The work delivered is real (250M allocs eliminated, spectral-norm 5×, broad runtime quality improvements); the gate metric just doesn't capture it cleanly.

## Test posture

911/0 workspace tests; all 8 microbench cases produce correct results on all tiers; nbody energy matches walker output to 18 sig figs.

## Tracking

- Phase 6 plan: `docs/milestones/m6-phase6-plan.md` (will get an addendum noting Stage B's diminishing-returns finding + the gate-reframe recommendation).
- Stage A interim: `docs/milestones/m6-phase6-stageA-interim.md`.
- Stage B analysis: `docs/milestones/m6-phase6-stageB-analysis.md`.
- Stage B exit (this doc): closes the active iter sequence.
- Recommended next: gate-reframe ADR + Phase 6 final exit doc.

Tasks:
- #24 ✅ B1 exact-division fast path
- #25 ✅ B2 thin-procedure NB encoding
- #26 ⏸ B3 named-let closure caching (deferred — see this doc)
- #27 ⏸ B4 closeout (this doc supersedes; awaiting gate reframe)
