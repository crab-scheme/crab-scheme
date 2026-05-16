# M6 Phase 6 Stage A — Interim Progress Report

> Status: **In progress** as of 2026-05-16. Iter 1 + iter 2 landed; iter 3 (multi-block splice) provisionally deferred. Iter 4 (closeout + measurement) pending.
> Parent: `docs/milestones/m6-phase6-plan.md`.
> Predecessor: M6 Phase 5 (`m6-phase5-complete`, geomean 2.31×).

## What landed

### Iter 1 (`08feff3`) — eligibility analyzer + SSA remap scaffolding

Built `cs-rir::inline` as a standalone analysis module:
- `analyze_for_inline(func)` — pure eligibility analyzer with explicit rejection variants for every "no" reason (size, internal-call, MakeClosure, env-mutation, unsupported-Inst, multiple-Return, no-Return).
- `InlineMetadata` (size, max_value, max_block, return_block, return_value).
- `ValueRemap` / `BlockRemap` offset-based renumbering primitives.
- 12 unit tests; no codegen wiring.

### Iter 2a (`37efa66`) — walker + splice scaffolding

Extended `cs-rir::inline` with the per-Inst walker and splice driver:
- `for_each_value_in_inst(inst, f)` / `for_each_value_in_term(term, f)` walkers over the matrix-elt-class supported variant set (~60 Inst variants).
- `is_inline_supported(inst)` / `is_term_inline_supported(term)` predicates kept in lockstep with the walker's match arms.
- `SpliceRequest { param_subst, value_offset, block_offset }` — the remap shape iter 2b builds at each call site.
- `splice_single_block(caller_insts, callee, metadata, splice) -> Value` — appends callee body with proper SSA renumbering, returns the caller-side value bound to the callee's return result.
- 14 more unit tests; still no codegen wiring.

### Iter 2b (`37f477a`) — translator splice site + env demote

Wired iter 2a into `bytecode_to_rir_with_hints` at the `CallGeneral` splice site:
- New `bytecode_to_rir_full(..., caller_env, inline_depth)` entry. The runtime hook passes `Some(&closure.env)` + depth 0; tests pass None and inlining stays silent.
- `find_envlookup_sym` recovers the callee binding sym from the producing `EnvLookup`/`EnvLookupAny` inst.
- `try_inline_leaf_callee` resolves sym → `VmClosure` → `CompiledLambda`, recursively translates the callee body to RIR (depth+1 to gate further inlining), runs the analyzer, and splices on accept.
- `demote_env_to_ssa_in_first_block` rewrites iter9-emitted `EnvDefineLocal` / `EnvLookupAny` round-trips into direct operand substitution (not `Move` insts; `Move` in uniform-NB would preserve the source's NB tag, but downstream consumers may expect the destination's type).
- `next_value_id` migrated from `let mut u32` to `Cell<u32>` so the splice path can read+bump it from outside the alloc closure; 168 existing `alloc()` call sites unchanged.

## Bench impact (post-iter2b)

Median of 3 runs vs the Phase 5 exit baseline:

| Bench         | Phase 5 | post-iter2b | Δ      |
|---------------|--------:|------------:|-------:|
| fib           | 2.56×   | 2.63×       | +3%    |
| tak           | 2.14×   | 1.88×       | -12% (within noise) |
| ack           | 2.25×   | 2.25×       | 0%     |
| nqueens       | 1.36×   | 1.32×       | -3%    |
| mandelbrot    | 4.31×   | **4.60×**   | +7%    |
| spectral-norm | 1.48×   | **1.73×**   | **+17%** |
| binary-trees  | 4.00×   | 3.88×       | -3%    |
| alloc-stress  | 1.89×   | 1.90×       | 0%     |
| **geomean**   | **2.31×** | **2.33×** | **+1%** |

IC dispatch count on spectral-norm dropped from **100,937 → 2,943** (-97%). The matrix-elt inlining is the headline win: every j-loop iter that previously dispatched through `vm_ic_dispatch` now runs the inlined body directly.

The geomean gain is modest because matrix-elt is one specific call shape (single-block, pure-arithmetic leaf, top-level binding lookup). Other benches' hot calls don't fit the iter 2 inlineable profile.

## Iter 3 (multi-block splice) — deferred

Iter 3's planned scope was extending the splice to multi-block callees. The motivating cases would be small predicates like `(define (pred x) (if cond a b))` — bodies whose RIR has Branch/Jump terminators.

Surveying our benchmark targets:
- **nqueens** `safe?`: multi-block (let-loop with cond chain) — but the inner loop is a recursive call (`safe-loop` → `safe-loop`), and the analyzer rejects `HasInternalCall`. Multi-block support alone doesn't unlock it.
- **nbody** physics functions: multi-block, also recursive in places.
- **eq3** / **is-bool?** / **is-num?** test-only: would benefit, but not in the production bench suite.

The expected payoff from iter 3 alone is small (~1-3% geomean). Reasonable to defer until Stage B/C results show whether the inlineable-leaf set is a real bottleneck or just one trick of many.

## What's next

### Decision point — push iter 3 or pivot to Stage B?

**Argument for iter 3 (multi-block splice):**
- Completes Stage A's planned scope.
- Establishes the multi-block remapping infrastructure that may be useful in later stages.
- Provides cleaner closeout messaging.

**Argument for pivot to Stage B (escape analysis):**
- Phase 6's stages were ordered for compounding benefit. Stage A's modest gain (+2%) means the remaining 2.67× to hit the 5× gate has to come from B and C. Investing in iter 3 polish over starting B reduces the runway.
- Allocation pressure is the next-largest measured bottleneck on spectral-norm (Rational allocs in matrix-elt's `(/ (* ij (+ ij 1)) 2)` — even after inlining, these allocs remain). Stage B directly attacks this.
- Iter 3's multi-block infrastructure can be revisited if Stage B/C surface a need.

**Recommendation:** pivot to Stage B. Track iter 3 + iter 4 as Stage A backlog under the same task tree; revisit after Stage B's iter 1-2 produce a measurement.

### If Stage B opens next

Stage B iter 1 (per the plan doc): build the SSA def-use graph as a standalone `cs-rir` analysis pass. Foundation for escape analysis. Standalone module with unit tests, no codegen wiring.

Stage B iter 2: build liveness analysis on top of the def-use graph.

Stage B iter 3+: escape analysis pass that classifies each heap-allocating Inst as "escapes" vs "doesn't escape"; rewrite pass to eliminate non-escaping allocs.

## Test posture

- 911/0 workspace tests (+26 since Phase 5 exit, all from iter 1-2's new cs-rir tests).
- 8/8 microbench cases produce correct results on all tiers.
- `cargo test --release` clean.

## Tracking

This doc is interim. A full Stage A exit report lands when iter 3 + iter 4 close OR Stage B's measurement shows iter 3 was unnecessary. Either path produces `docs/milestones/m6-phase6-stageA-exit.md` with the final per-bench numbers.

Tasks:
- #20 ✅ iter 1 scaffolding
- #21 ✅ iter 2 single-block translator splice
- #22 ⏸ iter 3 multi-block (deferred unless B/C demand it)
- #23 ⏸ iter 4 ownership audit + measurement + closeout
