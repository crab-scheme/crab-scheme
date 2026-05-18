# ADR 0017: Escape Analysis — Compiler-Driven Allocation Dispatch

> Status: Accepted (layer 5 of the unified memory management
> architecture — see [ADR 0015](./0015-unified-memory-management.md))
> Date: 2026-05-17
> Spec: `.spec-workflow/specs/escape-analysis/` (CLOSED, with
> known deferrals — see "Scope" below)
> Depends on: [ADR 0016 — Region Types](./0016-region-types.md)
> Enables: per-allocation dispatch from layer 5 to layers 3/2/4

## Context

ADR 0015 lays out a five-layer memory architecture. Layers 1
(ownership), 2 (RC / countable-memory), and 3 (regions, ADR
0016) ship as runtime primitives. Layer 4 (opt-in tracing) is
spec-drafted. Layer 5 is the compiler analysis that decides,
per allocation site, which layer's primitive to use.

Without layer 5 the runtime has only one allocation tier
exposed to Scheme code: `Gc::new`, the global Rc heap. Layer
3's `Gc::new_in(region, …)` requires the caller to know
*which* region — knowledge the typer's effect inferencer can
derive but unannotated Scheme code can't supply directly.

This ADR ratifies the effect-inference machinery that closes
that gap.

## Decision

Extend `cs-typer` with an `AllocEffect` for every expression,
propagate it down to `cs-rir` as a `Lifetime` tag, and have
the runtime dispatch each allocation on its `Lifetime`.

Specifically:

1. **`AllocEffect`** = `{ allocates: bool, escapes: EscapeKind,
   may_cycle: bool }`. Tracks per-expression allocation
   behaviour. Composable via a lattice-join (pointwise lub).

2. **`EscapeKind`** is a 4-point lattice:
   `Local ⊑ Region ⊑ Unknown ⊑ Heap`. `Heap` is the
   pessimistic top (must Rc-allocate); `Local` is the
   optimistic bottom (could stack-allocate).

3. **`primitive_effect(name)`** classifies every cs-typer
   builtin: arithmetic / predicates / accessors → `PURE`;
   `cons` / `make-vector` / `list` / etc. →
   `allocates + escapes=Region`; mutators → `Heap + may_cycle`;
   opaque control (`apply`, `call/cc`, `dynamic-wind`,
   `eval`) → `Unknown`; port constructors →
   `allocates + Heap`.

4. **`infer_effect(expr, env)`** walks the cs-ir CoreExpr
   bottom-up, joining sub-expression effects with the shape's
   own. Lambda expressions allocate a Heap-escaping closure;
   App calls join arg effects with the callee's effect
   (primitive lookup for Ref-to-known, conservative `Unknown`
   for opaque computed callees). Set/Letrec flag `may_cycle`
   when the RHS captures the LHS (free-var check, respecting
   parameter shadowing).

5. **`cs_rir::Lifetime`** = `{ Stack, Region(RegionTag), Rc,
   Traced }`. Default is `Rc` — preserves today's production
   semantics exactly. Each allocating SSA value can carry one.

6. **`lifetime_from_effect(effect, region_tag)`** is the
   bridge: pure / Heap / Unknown effects → `Rc`; Region /
   Local effects → `Region(tag)` if a tag is supplied, else
   `Rc` as a safe fallback.

7. **`crates/cs-runtime/src/regions.rs`** — per-thread
   `REGION_STACK: Vec<Rc<cs_gc::Region>>` + `RegionScope`
   RAII guard + `current_region()` accessor. Walker/VM/AOT
   tiers consult `current_region()` when dispatching a
   `Lifetime::Region(_)` allocation.

8. **`crates/cs-runtime/src/alloc_dispatch.rs`** — the
   lifetime-aware wrappers: `cons_in`, `make_vector_in`,
   `vector_in`, `make_string_in`, `make_bytevector_in`,
   `make_hashtable_in`, `list_in`. Each picks the right
   constructor based on its `Lifetime` argument; missing
   region scope on a `Region` lifetime is a typer-bug-class
   error with a clear diagnostic.

## Scope: shipped vs. deferred

### Shipped (iters 1–6)

| Component | File(s) | Status |
|---|---|---|
| AllocEffect / EscapeKind types + lattice | `cs-typer/src/effect.rs` | ✅ |
| Per-primitive effect table | same (extended) | ✅ |
| `infer_effect` over CoreExpr | same (extended) | ✅ |
| `cs_rir::{Lifetime, RegionTag}` | `cs-rir/src/lifetime.rs` | ✅ |
| `rir_bridge::lifetime_from_effect` | `cs-typer/src/rir_bridge.rs` | ✅ |
| Region-scope stack | `cs-runtime/src/regions.rs` | ✅ |
| Allocation dispatch wrappers | `cs-runtime/src/alloc_dispatch.rs` | ✅ |
| ADR + exit report | this file + `docs/milestones/escape-analysis-exit.md` | ✅ |

### Deferred (spec tasks #9–#10)

| Component | Rationale |
|---|---|
| cs-vm bytecode opcode extensions (`AllocPairRegion`, `OpenRegion`, `CloseRegion`, …) | Additive new opcodes; not needed for benchmark parity (existing bytecode continues to emit Rc-backed allocations). Can land in a follow-up iter without invalidating any iter-1-6 work. |
| cs-aot emit extensions | Same rationale — AOT-emitted Rust code continues to call `Pair::new` etc.; lifetime-aware emit is a forward optimization. |
| Inter-procedural effect sharpening | Today's `infer_effect` treats App-of-non-Ref conservatively as `Unknown`. Real inter-procedural analysis (fixpoint over the call graph) would shrink false-positive `Unknown` escapes. Spec leaves this to a future iter; current behaviour is correct, just not as tight as possible. |
| Pipeline integration into cs-typer's main `infer` walk | `infer_effect` exists as a standalone callable; cs-typer's `Checker::infer` doesn't yet thread effects into its `AnnotationTable`. The infrastructure is in place; the wiring is a future iter once a downstream consumer (VM or AOT codegen) needs the table. |

These deferrals are intentional: they're additive optimizations
that don't change existing semantics. Iters 1–6 ship enough to
exercise the layer-5 → layer-3 path end-to-end via direct
calls into `alloc_dispatch::*_in` from test or hand-written
code.

## Trade-offs

### What we accept

- **No automatic Scheme → Region dispatch yet.** Unannotated
  Scheme code that runs through the walker/VM continues to
  use the Rc heap. Layer-5 dispatch activates only at call
  sites that explicitly invoke `alloc_dispatch::*_in` (typed
  programs once iters 9–10 land; manual region users today).
- **Conservative `Unknown` for opaque calls.** A computed
  callee (`(define f (some-expr))` then `(f args)`) gets
  `escapes = Unknown`. This is correct but pessimistic — a
  fixpoint analysis could often prove the closure's actual
  escape. Defer until the conservative case shows up as a
  perf bottleneck in real workloads.
- **No region polymorphism.** A single allocation site picks
  one `RegionTag`; no migrating between regions. `Gc::promote`
  (region → Rc) is the only direction supported and is
  already in layer 3.

### What this buys

- A clean separation between the inferencer (decides what
  lifetime each value needs) and the allocator (carries out
  that decision). The two communicate exclusively through
  `Lifetime` tags in cs-rir.
- The infrastructure for the bytecode VM, AOT pipeline, and
  walker to all share the same dispatch primitive — no
  per-tier reinvention.
- A clear extension point for layer 4 (tracing): when the
  `tracing-revival` spec lands, `Lifetime::Traced` already
  exists; the dispatch wrappers just need a third arm.
- Zero overhead for unannotated code: `Lifetime::Rc` (the
  default) routes to the existing `Pair::new` etc., unchanged.

## References

- ADR 0014 — Countable Memory (layer 2)
- ADR 0015 — Unified Memory Management (the 5-layer plan)
- ADR 0016 — Region Types (layer 3)
- `.spec-workflow/specs/escape-analysis/` — full spec
- `docs/milestones/escape-analysis-exit.md` — exit report
