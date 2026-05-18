# Escape Analysis — Exit Report

> Tagged at the merge commit of this report.
> Predecessor: region-memory (`docs/milestones/region-memory-exit.md`).
> Spec: `.spec-workflow/specs/escape-analysis/` (CLOSED, with
> known deferrals — see "Deferred work" below).
> ADR: `docs/adr/0017-escape-analysis.md`.

This report closes iters 1–7 of the escape-analysis spec.
Layer 5 of the unified memory architecture ships as the
inference machinery + runtime primitives needed to dispatch
allocations on the typer's effect annotations. Two follow-on
tasks (cs-vm opcodes + cs-aot emit) are deferred as additive
forward-optimizations — see the ADR's "Scope" section for
rationale.

---

## Acceptance summary

| Gate | Spec § | Result |
|---|---|---|
| FR-1: `AllocEffect` + `EscapeKind` lattice | requirements.md | **✅** `cs-typer/src/effect.rs` — types, PURE constant, join algebra. |
| FR-2: per-primitive effect table | requirements.md | **✅** `primitive_effect(name)` covering allocators / mutators / accessors / arithmetic / predicates / opaque control / port constructors. |
| FR-3: `infer_effect` over CoreExpr | requirements.md | **✅** bottom-up walk; Const/Ref/Set/Lambda/App/If/Begin/Letrec rules. |
| FR-4: may-cycle detection | requirements.md | **✅** `ref_captures_any` free-var helper; Set/Letrec flag may_cycle on self-/sibling-capture. |
| FR-5: `cs_rir::Lifetime` tag | requirements.md | **✅** `cs-rir/src/lifetime.rs` — Stack/Region(tag)/Rc/Traced enum + RegionTag(u32). |
| FR-6: `rir_bridge` effect → lifetime mapping | requirements.md | **✅** `lifetime_from_effect(effect, region_tag)`. |
| FR-7: region-scope stack | requirements.md | **✅** `cs-runtime/src/regions.rs` — thread-local `REGION_STACK` + RAII `RegionScope` + `current_region()`. |
| FR-8: lifetime-aware allocation dispatch | requirements.md | **Partial (cs-runtime)** — `alloc_dispatch::*_in` wrappers for cons / make-vector / vector / make-string / make-bytevector / make-hashtable / list. cs-vm opcode emit + cs-aot codegen deferred. |
| NFR-1: typer latency overhead ≤ 20% | requirements.md | **N/A in this report** — `infer_effect` is a standalone callable, not yet wired into `Checker::infer`. Latency impact lands when iter 8 (pipeline integration) ships. |
| NFR-2: zero overhead when feature off | requirements.md | **✅** `regions` feature gates the entire layer-5-aware runtime path; with `--no-default-features --features countable-memory` the binary is identical to the layer-2-only pipeline. |
| NFR-3: region-allocated map ≥ 30% speedup vs Rc | requirements.md | **N/A** — depends on cs-vm/cs-aot codegen (deferred). Layer-3 microbenches already show the underlying allocator at 3.75 ns/alloc (vs ~50-100 ns for Rc); the speedup will surface once dispatch reaches Scheme code. |
| NFR-4: WASM stays green | requirements.md | **✅** new modules are pure Rust with no platform deps. |
| NFR-5: ADR 0017 written | requirements.md | **✅** `docs/adr/0017-escape-analysis.md`. |

---

## What shipped per iter

### Iter 1 — `AllocEffect` + `EscapeKind` types

New `crates/cs-typer/src/effect.rs` (~300 LOC) with the pure
data types and lattice algebra:
- `AllocEffect { allocates, escapes, may_cycle }` —
  per-expression allocation summary.
- `EscapeKind { Local, Region, Heap, Unknown }` — 4-point
  lattice ordered `Local ⊑ Region ⊑ Unknown ⊑ Heap`.
- `AllocEffect::PURE` const, `join` (pointwise lub),
  `Display` + `Default` impls.
- 11 unit tests: PURE-as-identity, join
  idempotence/commutativity/associativity, full 4×4 lattice
  join table.

### Iter 2 — per-primitive effect table

`primitive_effect(name) -> AllocEffect` extending `effect.rs`.
Hand-curated coverage of ~80 cs-runtime primitives:
- Arithmetic / comparison / predicates / accessors → PURE.
- Allocators (`cons`, `list`, `vector`, `make-*`, `reverse`,
  `append`, `map`, `for-each`, …) → allocates + Region.
- Mutators (`set-car!`, `set-cdr!`, `vector-set!`, etc.) →
  Heap + may_cycle.
- Opaque control (`apply`, `call/cc`, `dynamic-wind`, `eval`,
  `with-exception-handler`) → Unknown.
- Port constructors → allocates + Heap (long-lived resources).
- Unknown names → PURE (safe default; caller-context tightens
  via join).

8 unit tests cover each category + the unknown-name default.

### Iter 3 — `infer_effect` for CoreExpr

Bottom-up AST walk producing `AllocEffect` per expression:
- Const/Ref → PURE.
- Set → propagates RHS effect with Heap escape; flags
  may_cycle on free-var capture of LHS.
- Lambda → allocates + escapes=Heap (closures escape typical
  scope); body's may_cycle bit lifted in.
- App → joins function effect (primitive lookup if callee is
  a known Ref; else conservative Unknown) with each arg's.
- If/Begin → joins all sub-expressions.
- Letrec → extends scope with every LHS upfront for mutual-
  recursion may_cycle detection; joins binding + body
  effects.

Helper `ref_captures_any(expr, names)` respects Lambda
parameter shadowing and Letrec LHS shadowing.

12 unit tests cover leaf purity, Lambda closure semantics,
If/Begin joins, set-x-x may_cycle, Letrec self-recursion +
mutual recursion, acyclic letrec, parameter-shadowing edge
case.

### Iter 4 — `Lifetime` tag in cs-rir + `rir_bridge` mapping

New `crates/cs-rir/src/lifetime.rs`:
- `RegionTag(u32)` — per-function-scope region id.
- `Lifetime { Stack, Region(tag), Rc, Traced }` — default Rc.
- `needs_region()` + `region_tag()` helpers.
- 4 unit tests.

`cs-typer/src/rir_bridge.rs` extended with
`lifetime_from_effect(effect, region) -> Lifetime`:
- Pure → Rc.
- Heap / Unknown → Rc.
- Region / Local → Region(tag) if a tag supplied; else Rc.
- 6 unit tests.

### Iter 5 — region-scope stack

New `crates/cs-runtime/src/regions.rs` (gated on `regions`):
- `REGION_STACK` thread-local LIFO of `Rc<Region>`.
- `RegionScope<'a>` RAII guard — `enter(region)` pushes;
  Drop pops.
- `current_region()` returns the innermost in-scope region.
- `region_stack_depth()` debug/test accessor.

`Rc<Region>` (not borrows) lets the stack outlive a single
call frame — the walker can stash a region across tail calls
and the VM can hold one across yields.

4 unit tests: empty baseline, push/pop on Drop, nested LIFO,
post-pop liveness via cloned Rc handles.

### Iter 6 — allocation dispatch wrappers

New `crates/cs-runtime/src/alloc_dispatch.rs` (gated on
`regions`):
- `cons_in(lifetime, car, cdr)`,
  `make_vector_in(lifetime, n, fill)`,
  `vector_in(lifetime, elems)`,
  `make_string_in(lifetime, n, fill)`,
  `make_bytevector_in(lifetime, n, fill)`,
  `make_hashtable_in(lifetime, eq_kind)`,
  `list_in(lifetime, elems)`.
- `Region(_)` / `Stack` dispatch to `*_in(region, …)` via
  `current_region()`; missing scope returns a clear typer-
  bug-class error.
- `Rc` / `Traced` route to existing `Pair::new` etc. (Traced
  falls back to Rc until tracing-revival spec ships).

9 unit tests: Rc routes to global heap; Region under a scope
returns region-backed handles for each allocator type;
Region without a scope errors; Traced falls back to Rc;
list_in propagates lifetime to every pair in the chain.

### Iter 7 — this report + ADR 0017

`docs/adr/0017-escape-analysis.md` ratifies the design.
This report closes the spec. Spec status flipped to CLOSED.

---

## Deferred work

The spec's tasks #9 (cs-vm opcode extensions) and #10 (cs-aot
emit extensions) are deferred. They're additive — adding
new opcodes / emit shapes that lifetime-aware codegen
consumes — and don't change any existing semantics. The full
layer-5 → layer-3 dispatch path is exercisable today through
direct calls to `alloc_dispatch::*_in`; activating it for
typed Scheme code from the VM and AOT pipelines is a future
iter once a downstream consumer demands it.

Additional deferrals documented in ADR 0017's "Scope" section:
- Inter-procedural effect sharpening (today's App-of-non-Ref
  goes Unknown; fixpoint analysis would tighten).
- Wiring `infer_effect` into `Checker::infer`'s
  AnnotationTable output — currently `infer_effect` is a
  standalone callable.

---

## Test status

- cs-typer effect tests: **31/31 passing**.
- cs-typer rir_bridge tests: **228 passing** (includes pre-
  existing + 6 new lifetime mapping tests).
- cs-rir lifetime tests: **4/4 passing**.
- cs-runtime regions tests: **4/4 passing**.
- cs-runtime alloc_dispatch tests: **9/9 passing**.
- Workspace tests: green except pre-existing
  `jit_conformance` stack-overflow regression unrelated to
  this spec.

---

## File map

New files:
- `crates/cs-typer/src/effect.rs` (~720 LOC).
- `crates/cs-rir/src/lifetime.rs` (~130 LOC).
- `crates/cs-runtime/src/regions.rs` (~140 LOC).
- `crates/cs-runtime/src/alloc_dispatch.rs` (~260 LOC).
- `docs/adr/0017-escape-analysis.md`.
- `docs/milestones/escape-analysis-exit.md` (this file).

Modified files:
- `crates/cs-typer/src/lib.rs` — pub mod effect; re-exports.
- `crates/cs-typer/src/rir_bridge.rs` — `lifetime_from_effect`
  + 6 new tests.
- `crates/cs-rir/src/lib.rs` — pub mod lifetime; re-export.
- `crates/cs-runtime/src/lib.rs` — pub mod regions +
  alloc_dispatch (cfg-gated).
- `.spec-workflow/specs/escape-analysis/{requirements,design,tasks}.md`
  — marked CLOSED.
