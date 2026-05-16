# M6 Phase 6 Stage B — Analysis + Reframed Plan

> Status: **Analysis complete, plan reframed** as of 2026-05-16.
> Parent: `docs/milestones/m6-phase6-plan.md`.
> Predecessor: Stage A interim (`m6-phase6-stageA-interim.md`).

## Why this doc exists

The Phase 6 plan scoped Stage B as "Escape Analysis — eliminate non-escaping heap allocations" with a multi-week dataflow framework build. Before investing months in that framework, I measured actual allocation rates per bench and traced the dominant alloc sites. The data argues for a **reframed Stage B** that targets specific bottlenecks with leaner mechanisms rather than a general escape-analysis pass.

This doc captures the measurement, the bottleneck taxonomy, and the proposed reframe.

## Measurement methodology

Added a `(gc-stats)` builtin (commit pending) returning `(alloc-count collect-count)`. Wrapped each bench with a `start` / `end` snapshot and printed the delta. Ran under `--tier vm-jit` (post-Stage-A-iter2).

## Allocation rates per bench

| Bench         | JIT time | Allocs   | Allocs/ms | Dominant alloc shape |
|---------------|---------:|---------:|----------:|----------------------|
| fib           | 8 ms     | 8        | 1         | None (pure arith)    |
| tak           | 8 ms     | 1,029    | 129       | Minimal              |
| ack           | 8 ms     | 6        | <1        | None                 |
| nqueens       | 25 ms    | 162,317  | 6,493     | Cons (placed list)   |
| mandelbrot    | 16 ms    | 31,987   | 2,000     | Mixed (closure + box)|
| spectral-norm | 51 ms    | 106,392  | 2,086     | Rational (div)       |
| binary-trees  | 16 ms    | 133,947  | 8,372     | Cons (tree nodes)    |
| alloc-stress  | 19 ms    | 200,612  | 10,558    | Designed alloc test  |
| **nbody**     | ~3,600 ms| **250,518,443** | **69,588** | Closure + Procedure box |

**Key insight 1:** Three benches (fib, tak, ack) have effectively zero allocations. Stage B has zero payoff on them. They're already CPU-bound on pure arithmetic.

**Key insight 2:** nbody is the elephant — **250M allocations**. Two orders of magnitude more than any other bench. The 1500 rounds × 1000 steps × ~150 actions/step = ~225M actions; ~1 alloc per action.

**Key insight 3:** The remaining benches sit in a narrow 2k-10k allocs/ms band. Each has its own dominant alloc shape.

## Bottleneck taxonomy

### 1. Rational allocations in Fixnum division (spectral-norm)

`matrix-elt`'s body computes `(/ (* ij (+ ij 1)) 2)`. `ij * (ij+1)` is always even (product of consecutive integers), so the result is always exact integer. But the JIT calls `vm_value_div_nb` → `generic_arith2` → `Number::div`:

```rust
let a = to_rational(self);   // alloc BigInt + BigRational
let b = to_rational(other);  // alloc BigInt + BigRational
let r = a / b;               // alloc BigRational (result)
Ok(simplify_rational(r))     // returns Fixnum (no alloc), drops the 3 above
```

Per `matrix-elt` call: ~3 BigInts + ~3 BigRationals allocated, all ephemeral (consumed immediately by `simplify_rational`). For ~50k matrix-elt calls per run: ~300k heap allocs of ~32-40 bytes each = ~10 MB of garbage.

**Fix:** Speculative integer-division fast path. At JIT time, for `(/ Fixnum Fixnum)`:
```
quot = sdiv a, b
rem = srem a, b
brif rem == 0, fast_int, slow_rational

fast_int:
  result = NB Fixnum encode quot
  jump merge

slow_rational:
  result = call vm_value_div_nb(a, b)
  jump merge
```

Cost: 2 hardware ops (div + mod) + 1 branch ≈ 3-5 ns per call.
Savings: ~100 ns per call (Rational alloc + drop).
Net: ~95 ns × 50k calls = **~5 ms saved on spectral-norm**.

Implementation: one peephole in `Inst::Div` lowering. **~1 iter of work.**

### 2. Closure / Procedure boxing in dispatch (nbody)

nbody's 250M allocs come from somewhere in the CallGeneral dispatch path. The hot inner loop has 4 named-let closures (`outer`, `inner`, `upd`, `loop`) — each `MakeClosure` allocates a fresh `VmClosure` per invocation. Plus `nb_alloc_gc_value(Value::Procedure(p))` in `NanboxValue::from_value` wraps every Procedure-typed value in a fresh `Gc<Value>` (because `Rc<dyn Procedure>` is a 16-byte fat pointer that doesn't fit the 47-bit NB payload).

Per advance step (~16 inner iterations × ~10 ops per iter = ~160 actions): observed ~168 allocs. Roughly 1 alloc per action.

**Fixes — two independent levers:**

#### 2a. Thin-procedure NB encoding (nb_alloc_gc_value elimination)

Today: `Value::Procedure(Rc<dyn Procedure>)` → `nb_alloc_gc_value` wraps in `Gc<Value>` → 1 heap alloc per encoding. `NB_TAG_PROCEDURE = 11` is reserved but unused.

Approach: introduce a process-wide `ProcTable: Vec<Rc<dyn Procedure>>` (or `HashMap<u32, Rc<dyn Procedure>>` for de-dup). Each Procedure gets a small u32 index on first encoding. NB stores the index in the payload with `NB_TAG_PROCEDURE`. Decoding looks up via the table. Drop fires when the index's refcount drops to 0.

Cost: 1 table lookup per decode (hashmap or vec lookup). Comparable to indirect pointer deref.
Savings: 1 alloc per Procedure encoding.

For nbody: many Procedure encodings per step (closure args passed to CallGeneral, return values). If we save 1 alloc per 2 Procedure passes, that's ~80 allocs saved per step × 1.5M steps = **~120M allocs saved, ~80% of nbody's alloc storm**.

Implementation: extend NanboxValue encoding/decoding for NB_TAG_PROCEDURE. Add process-wide `ProcTable` (cleanup nicety: weak refs). **~2 iters of work.**

#### 2b. Closure-cache for non-escaping named-let

Today: `(let loop ((i 0) ...) ...)` compiles to MakeClosure + Call. Each MakeClosure allocates a fresh `VmClosure` per call to the surrounding function.

For non-escaping closures (called within the same function, no capture leaks outside), the closure can be cached as a per-function-instance singleton. Caller passes captured-env-frame as an arg instead of building a new closure.

This is essentially "closure conversion + recursive-call-as-loop" — a known compiler technique. For our case, the closure stored in a per-stack-frame slot suffices.

For nbody: 4 named-let closures × ~5 outer iters × ~1.5M advance calls = ~30M closure allocs. If all four are cached, **~30M allocs saved on nbody**.

Implementation: detect named-let / inner-only closures at compile time; emit a stack-allocated closure struct + direct call. **~3 iters of work.**

### 3. Cons-cell allocs (nqueens, binary-trees)

nqueens's `placed` list grows by one Cons per recursion level. binary-trees builds tree structures of Pairs.

These Cons cells generally **do escape** — they're passed recursively to safe?, returned through the recursion, etc. Pure escape analysis would identify them as escaping, so wouldn't help.

Possible win: **arena allocation** for the placed-list path. The list's lifetime is bounded by the recursion depth; an arena reset on backtrack would skip per-Cons alloc.

Not a clean fit for "escape analysis" framing. Skip for Stage B; defer to a future "allocation arena" track.

### 4. The escape-analysis-natural bottlenecks (~0% of measured allocs)

True non-escaping ephemeral allocs in our benches are rare. Most allocs are either:
- Long-lived heap structures (nqueens placed, binary-trees nodes).
- Already-eliminated-by-NB-encoding values (Flonum, Fixnum).
- Closure / procedure metadata (which a smarter representation eliminates without dataflow analysis).

A general escape-analysis pass would catch ~5-10% of measured allocs at most. Not worth the months of framework investment.

## Reframed Stage B plan

**Old framing:** "Escape Analysis — build dataflow framework + escape pass + rewrite pass."

**New framing:** "Allocation Pressure Reduction — targeted iters per bottleneck."

### Proposed iter sequence

| Iter | Target | Expected payoff | Effort |
|------|--------|-----------------|--------|
| **B1** | Exact-division fast path (for `(/ Fixnum Fixnum)`) | spectral-norm +10-15% | 1 iter |
| **B2** | Thin-procedure NB encoding (`NB_TAG_PROCEDURE` activation) | nbody -80% allocs, geomean +5-10% | 2 iters |
| **B3** | Named-let closure caching (non-escaping inner-only closures) | nbody/mandelbrot/nqueens -closure allocs, geomean +5-10% | 3 iters |
| **B4** | Measurement + closeout | — | 1 iter |

Total: ~7 iters. Stage exit criteria: geomean ≥3× (up from 2.33×).

### What's NOT in reframed Stage B

- **SSA def-use graph / liveness / general escape analysis dataflow.** These are foundational infrastructure that the targeted fixes don't need. If Stage C (type-feedback specialization) or a later phase wants them, build them then.
- **Arena allocation** for nqueens/binary-trees placed lists. Different track; defer.
- **GC-side improvements** (generational collector, bump allocator). Different milestone.

### Risks

| Risk | Mitigation |
|------|-----------|
| Exact-division fast path adds overhead when result IS Rational (e.g. `(/ 5 2)`) | Profile on bench suite; only worry if a real bench regresses. spectral-norm's pattern is "always exact" — the fast path always wins. |
| Thin-procedure encoding's table grows unbounded | Drop entries when last NB reference drops (weak-ref via table-side refcount). |
| Closure-cache breaks closure semantics when an inner closure escapes | Detect "definitely doesn't escape" via simple syntactic check (no return-from-function, no store-into-mutable-cell, no Cons of the closure). When uncertain, fall back to MakeClosure. |

## What lands first

Stage B1 (exact-division fast path) is the smallest, lowest-risk, fastest-payoff iter. Land it first.

If Stage B1 measurements come in roughly as predicted (spectral-norm +10-15%, no regressions), proceed to B2. If they're off, re-measure and adjust the plan before committing more.

## Tracking

- Stage A interim doc: `docs/milestones/m6-phase6-stageA-interim.md`.
- Stage B analysis (this doc).
- Stage B iter results will land as the iters do; full Stage B exit doc when complete.
- Phase 6 plan doc (`m6-phase6-plan.md`) will get an addendum noting the reframe.

The `gc-stats` builtin added for this analysis stays in — useful for future regression-analysis on alloc-pressure changes.
