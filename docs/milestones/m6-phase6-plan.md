# M6 Phase 6 Plan — Optimizing JIT Tier (multi-quarter)

> Status: **Open** as of 2026-05-16. Predecessor: M6 Phase 5 (`m6-phase5-complete` tag, geomean 2.31×).
> Estimated duration: 4-6 months across three stages + a closeout.
> Target: close the JIT geomean gap from 2.31× to ≥5× on the 8-bench microbench (the pre-1.0 perf gate).
> Spec slug: `jit-optimizing-tier` (new — kept separate from `jit-cranelift` which already covers Phase 1-4 as historical scaffolding).

## Why Phase 6 exists

M6 Phase 5 closed at 2.31× geomean — the architectural ceiling for incremental coverage-expansion work against the existing JIT pipeline. The 5× pre-1.0 perf gate remains unmet by ~117%. Phase 5's exit doc identifies four structural bottlenecks blocking further incremental progress; Phase 6 attacks three of them directly via dedicated stages.

The fourth bottleneck (the SystemV ↔ Tail calling-convention crossing) was confirmed unfixable within Cranelift's current verifier and is excluded from Phase 6 scope. If the three stages below land and the geomean still falls short, the gate gets reframed via ADR (see Phase 5 exit doc, "Next architectural moves" item E).

## Scope: three stages

Phase 6 ships as three sequenced stages, each with its own iter plan and exit criteria. The order is deliberate:

```
Stage A — Leaf-callee inlining       (4-6 weeks)
        ↓
Stage B — Escape analysis            (8-12 weeks)
        ↓
Stage C — Type-feedback specialization (6-10 weeks)
        ↓
Phase 6 closeout                     (1-2 weeks)
```

Each stage builds on the previous: inlined code is easier to analyze for escape; escape-eliminated code is easier to type-specialize; type-specialized inlined bodies are where the bulk of the geomean win actually materializes.

### Stage A — Leaf-callee inlining (correct iter6)

**Target bottleneck:** `vm_ic_dispatch` overhead per IC hit. spectral-norm has 100k+ IC hits per run, each carrying SystemV↔Tail-conv crossing + per-arg type check + per-arg refcount + TLS guard install + (optional) frame-env construction.

**Approach:** Detect callees that are: small (< 20 RIR insts after lowering), pure (no `Call` / `CallGeneral` inside), and called from a JIT-eligible body. During bytecode-to-RIR translation (or as an RIR pass), splice the callee's RIR into the caller with proper SSA renumbering and ownership analysis.

**Why iter6 failed:** The pass-through of `MakeClosure` / `CallGeneral` during inlining introduced SSA renaming bugs — spectral-norm produced 1.2595 instead of 1.2742. The fix requires:
- Explicit SSA value remapping table (caller-side → inlined-side new values).
- Proper handling of ownership: which side owns each refcount, who decrements on early exit.
- Block remapping for multi-block callees (iter6 only handled single-block; matrix-elt is single-block but the general case isn't).
- Eligibility gate that rejects callees containing recursive self-reference (depth-cap fallback isn't enough — needs static rejection).

**Expected payoff:** Eliminates IC dispatch entirely for the inlined calls. Spectral-norm's `matrix-elt` is the motivating case (3-op leaf, called 50k+ times). Conservative estimate: +25-50% on spectral-norm; +5-15% geomean.

**Iter plan sketch:**
1. Eligibility analyzer — detect inlineable callees (size + purity + non-recursion gates).
2. SSA remap infrastructure — value table, block table, helper utilities.
3. Single-block-callee inlining at translator (the iter6 case, done correctly).
4. Multi-block-callee inlining (extends 3 to general case).
5. Ownership audit — refcount transfer semantics in inlined Drop/Clone ops.
6. Per-bench measurement + regression hunt.
7. Closeout: commit + iter doc.

**Exit criteria:**
- spectral-norm at ≥1.8× (currently 1.48×); preferably ≥2×.
- Geomean ≥2.5× (up from 2.31×).
- Zero correctness regression on 884 tests + 8 benches.
- IC dispatch count drops ≥50% on spectral-norm (measurable via `(jit-stats)`).

### Stage B — Escape analysis + allocation elimination

**Target bottleneck:** Heap allocation pressure for ephemeral values in hot loops. The motivating case is spectral-norm's `(/ (* ij (+ ij 1)) 2)` — the product of consecutive integers is always even, so the division is always exact and returns a Fixnum, but the JIT can't prove this statically and conservatively allocates a Rational. ~50k Rationals per run. Similar pattern in mandelbrot's pixel loop (intermediate Flonum boxes if not properly inlined).

**Approach:** Dataflow analysis over RIR to determine which heap allocations are *consumed* within the same JIT body and never escape (no return, no capture in MakeClosure, no store to a long-lived location). For non-escaping allocs:
- Replace `Gc<Rational>` allocations with stack-allocated value temporaries.
- Replace `Gc<Flonum>` (when not inline-NB) with direct f64 register storage.
- Inline `Rc<Pair>` allocations as register pairs when they don't escape.

The dataflow framework is the bulk of the work. Once it exists, the actual elimination rewrites are mechanical.

**Existing infrastructure:** Minimal — RIR has SSA values but no def-use graph. Need to build:
- Forward dataflow framework (`cs-rir` analysis crate or module).
- Liveness analysis (often a prerequisite to escape analysis).
- Use-def chains for SSA values.

**Expected payoff:** Eliminates the alloc-pressure tax on spectral-norm + mandelbrot. Bench-dependent — conservative estimate: +30-60% on spectral-norm, +20-40% on mandelbrot, +5-10% on alloc-stress; +10-25% geomean.

**Iter plan sketch:**
1. Build SSA def-use graph for RIR. Standalone analysis pass; no codegen changes.
2. Build liveness analysis on top of (1).
3. Escape analysis pass — for each heap-allocating Inst, determine if the result escapes.
4. Rewrite pass: eliminate non-escaping `BoxTyped(Rational)` etc.
5. Specifically: handle `Inst::Div` returning Rational (the spectral-norm case).
6. Per-bench measurement; verify no regressions.
7. Closeout: commit + iter doc.

**Exit criteria:**
- spectral-norm at ≥3× (assumes Stage A landed for ~1.8× base).
- mandelbrot maintains ≥4× (no regression from Stage A baseline).
- Geomean ≥3× (up from Stage A's ≥2.5× target).
- alloc-stress allocation count drops ≥30% (measurable via GC stats).

### Stage C — Type-feedback-driven specialization

**Target bottleneck:** Per-op type checks in hot monomorphic loops. Today's uniform-NB tier emits `emit_nb_arith_fixnum_fast` per arith op (mask + compare + branch + payload extract). For a tight loop with 5 arith ops per iter running 1M times, that's 5M branches that could be hoisted to a single entry-point check.

**Approach:** Track arg type histories per-IcSlot. When a slot sees N consecutive monomorphic calls with stable types (e.g. all-Flonum for matrix-elt after Stage A's inlining lands), trigger re-compilation of the callee for a type-stripped specialized variant.

**Existing infrastructure:**
- `LambdaProfile` already records arg-type feedback (per-call type observation).
- The deopt mechanism already exists for the specialized tier — type guards at function entry + deopt path back to bytecode.
- Re-compilation trigger (`clear_jit_for_recompile`) already exists.

**Missing pieces:**
- A "specialization decision" hook that watches IC slot type histories and triggers re-compilation when a slot becomes monomorphic at scale.
- A new lowering tier (`compile_typed_monomorphic_nb`?) that emits the type-stripped body with entry-point guards only.
- A re-deopt path that handles type mismatches by re-compiling once more with widened type assumptions.

**Expected payoff:** Largest payoff on Fixnum-heavy benches (fib/tak/ack/nqueens). Flonum benches already benefit from `fbinop`'s direct lowering, so per-op type checks are largely absent in spectral-norm/mandelbrot. Conservative estimate: +20-50% on Fixnum benches, +10-20% on mixed; +10-15% geomean.

**Iter plan sketch:**
1. IC slot type-history tracking — extend IcSlot to record observed param types per call.
2. Specialization-decision hook — `should_respec(slot) -> Option<Vec<NibbleType>>`.
3. New lowering path — `compile_typed_monomorphic_nb` that strips per-op type checks given entry-point hints.
4. Re-compilation trigger — when `should_respec` returns Some, mark callee for recompile.
5. Deopt-and-rewiden path — when specialized body sees type mismatch, fall back to uniform-NB and demote slot.
6. Per-bench measurement.
7. Closeout: commit + iter doc.

**Exit criteria:**
- Fixnum benches (fib/tak/ack) at ≥3× each.
- nqueens at ≥1.8× (up from Stage A/B's expected ~1.5×).
- Geomean ≥4× (up from Stage B's ≥3× target).
- Zero correctness regression on 884 tests + 8 benches.

### Phase 6 closeout

**Exit criteria for Phase 6 as a whole:**
- Geomean ≥5× → tag `m6-phase6-complete`, reframe pre-1.0 perf gate as MET.
- Geomean 4×-5× → tag `m6-phase6-complete`, write ADR proposing perf gate reframe to 4× (close to gate, justifies acceptance).
- Geomean <4× → tag `m6-phase6-partial`, document blockers, escalate gate reframe.

---

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Stage A SSA renumbering bugs (the iter6 wall) | Build an explicit remap table + ownership-tracking type discipline before touching codegen. Test exhaustively with both monomorphic and polymorphic callees before measurement. |
| Stage B dataflow framework is bigger than expected | Land it as a standalone analysis module first with extensive unit tests. Don't combine framework + rewrites in one commit. |
| Stage C deopt-and-rewiden path destabilizes hot loops | Make the demote-to-uniform-NB path soundness-bounded — at most N deopts before permanent demotion. Test under polymorphic call sites that thrash. |
| Each stage's bench wins regress under the next stage's changes | After each stage, capture per-bench numbers in a measurement doc. Treat regressions as commit-revert candidates. |
| 5× geomean still unreachable after all three stages | This is the prepared fallback — write the gate-reframe ADR and ship 1.0 RC with the actual measured posture. |

## Out of scope (deferred or rejected)

- **AOT compilation track (M10)** — separate milestone; would inherit the same RIR and benefit from Phase 6's work, but doesn't help close the JIT perf gate.
- **Cranelift fork or alternative codegen backend** — too much surface area for the expected payoff. Cranelift is fine; the bottlenecks are above it.
- **Vectorization / SIMD lowering** — possible follow-on after Phase 6 lands; out of scope for this phase.
- **Generational GC** — separate from JIT; would help alloc-stress but doesn't address the IC dispatch dominated benches.
- **WASM target tier** — separate (M10 track).

## Tracking

Each stage gets:
- A separate iter log committed to its directory (`docs/milestones/m6-phase6-stage{A,B,C}-*.md`).
- A per-iter commit with measurement attached.
- An exit summary at stage close, before moving to the next stage.

Phase 6 as a whole gets:
- This plan doc (updated as scope refines).
- An exit doc at close (`docs/milestones/m6-phase6-exit.md`).
- Tag `m6-phase6-complete` or `m6-phase6-partial` per exit criteria above.

## Starting point: Stage A iter 1

Stage A begins immediately. The first iter scopes the eligibility analyzer + SSA remap infrastructure as a non-codegen prep step:

1. Read iter6's commit history (look for the reverted SHA) to recover the exact failure mode + the in-progress code that was reverted.
2. Write the eligibility analyzer: given an RIR function, return Some(callee_metadata) if the callee qualifies for inlining.
3. Design the SSA remap table + value-ownership type discipline.
4. Land both as `cs-rir` infrastructure with unit tests; no codegen changes yet.

Stage A iter 2+ then applies these to the actual translator path.
