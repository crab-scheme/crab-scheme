# ADR 0006: Garbage Collector Design (M5)

> **Status: Superseded by [ADR 0014 — Countable Memory](./0014-countable-memory.md)** as of 2026-05-17.
> The Phase 1 → Phase 2 commitment recorded here is retired in
> favor of refcount-only reclamation plus a synchronous local
> cycle detector. The "hand-rolled vs `gc-arena`" decision and
> the "precise vs conservative rooting" decision are unaffected.
>
> Originally Accepted: 2026-05-09
> Roadmap milestone: M5 (Precise Tracing GC)
> Spec: `.spec-workflow/specs/gc/`

## Context

CrabScheme through M4 stores every heap value as `Rc<RefCell<…>>` /
`Rc<T>`. That's correct on a flat data structure but leaks on cycles —
`(set-cdr! x x)` constructs a self-cycle no refcount-only scheme can
reclaim. R6RS guarantees `equal?` terminates on cycles, which we
satisfy via a visited-set fallback, but live cyclic data still grows
the heap monotonically until the Runtime drops.

M5's mandate per ROADMAP.md: replace `Rc` with a **precise tracing
GC** so cycles get collected, with these exit gates:
- 24-hour fuzz with leak detector reports no leaks on cyclic structures.
- p99 GC pause < 1 ms on stdlib load.
- Conformance pass rate equal to M4 baseline (1460 tests).
- Memory usage no worse than RC + cycle collector.

This ADR ratifies three high-stakes design choices made during the
M5 spec work:

1. **Algorithm**: mark-sweep stop-the-world first vs. copying first.
2. **Rooting**: precise (typed) vs. conservative (stack scan).
3. **Crate**: hand-rolled `cs-gc` vs. external `gc` / `gc-arena`.

## Decision

CrabScheme M5 ships a **hand-rolled, precise, stop-the-world,
mark-and-sweep collector** in a workspace-internal `cs-gc` crate.
Phase 1 (this milestone) backs `Gc<T>` with `Rc<Slot<T>>` to validate
the API surface and trace plumbing. Phase 2 (M5 follow-up, not the
exit gate) swaps the inner representation for a hand-rolled bump
arena. Generational copying is deferred to a post-M5 milestone.

### Alternatives considered

#### Algorithm: mark-sweep first vs. copying first

| Option | Pros | Cons |
|---|---|---|
| **Mark-sweep first** ✅ | Drop-in compatible with `Rc<RefCell<…>>` ergonomics; no semantic relocation; small first-iter diff against the current codebase; easier to debug because allocations don't move | Higher per-op overhead than copying once memory pressure rises; fragmentation; sweep is O(allocations) |
| Copying first | Bump allocation makes `alloc()` essentially free; survivors compact automatically; simpler write barriers when generational lands | Every `Gc<T>` becomes a moving handle (relocation rewrites pointers); blocks any unsafe pointer arithmetic against the heap; significantly larger first-iter diff |

We picked mark-sweep because the foundation/M2/M4 codebase has many
`Rc<RefCell<…>>` patterns (`Pair`, `Vector`, `Hashtable`, `Port`)
where the pointer is treated as stable for the lifetime of the
clone. Switching to copying GC in one milestone would force every
borrow site to re-resolve the pointer, which is a design regression
for the ergonomic surface that took M0–M4 to settle.

The Phase 2 arena (post-this-ADR follow-up) keeps mark-sweep but
replaces the Rc backing with arena-owned `NonNull<Header>` slots.

#### Rooting: precise vs. conservative

| Option | Pros | Cons |
|---|---|---|
| **Precise rooting** ✅ | No false retention; works on any host arch including WASM (M10); supports relocating GC if we ever switch; scope-local roots are explicit | Forces every `Gc<T>` user to think about reachability; needs a typed `Trace` impl per heap variant; brittle to forget a root |
| Conservative stack scan | Zero per-call-site rooting boilerplate; works on existing Rust code unchanged | False retention pinning everything that looks pointer-like on the stack; doesn't survive WASM (no stack scan); non-portable |

Precise is the only option that works on WASM (M10) and survives a
move to relocating GC (post-M5). The cost — explicit `Trace` impls
on every heap-bearing type — was paid in step 4.C of the M5 plan;
it's done.

#### Crate: hand-rolled `cs-gc` vs. `gc` / `gc-arena`

| Option | Pros | Cons |
|---|---|---|
| **Hand-rolled `cs-gc`** ✅ | Full control over rooting strategy when JIT lands (M6/M7); shaped to our `Value` layout; no audit/license/compat ceiling; arena-vs-Rc backing is a one-iter swap behind the same external API | Carries the maintenance burden of a GC implementation |
| External `gc` crate | Existing implementation; community-maintained | API doesn't match our needs (each managed type must be `Trace + Finalize`; their GC ↔ our Procedure DST friction); no path to an arena allocator |
| External `gc-arena` | Sound; type-system-enforced rooting | Heavily lifetime-driven API; would force every cs-runtime call site to thread a `MutationContext` lifetime; doesn't compose with our existing `&mut EvalCtx` shape |

The runtime API surface is small enough (Pair / Vector / String /
ByteVector / Hashtable / Port / Promise / Procedure — 8 heap variants)
that the GC matches our `Value` shape better as a hand-rolled
component than as an external crate's idiom we adopt. The `cs-gc`
crate today is ~500 lines.

## Consequences

### Positive
- Cycle collection works (Phase 2; Phase 1 still relies on Rc).
- WASM target (M10) is reachable — no host-stack scan needed.
- JIT (M6/M7) can emit stackmaps that drop into the existing precise
  rooting story.
- The `Trace` trait surface is small and stable — Phase 2's arena
  swap is mechanical at the cs-gc internals, invisible at call
  sites.
- ADR-recorded Decision means downstream contributors can read this
  doc and understand the constraints before proposing changes.

### Negative / risks
- Phase 1's `Rc<Slot<T>>` backing means cycles still leak until
  Phase 2 lands. We accept this; the M5 exit gate explicitly says
  "Phase 1 collect() with Rc backing is the seam Phase 2 swaps."
- `Procedure` stays on `Rc<dyn Procedure>` because `CoerceUnsized`
  for our `Gc<T>` wrapper requires nightly. Documented in the
  pre-M5 plan; cycle-via-procedure leaks on Phase 1 are acknowledged
  in the M5 spec.
- Precise rooting is brittle: forgetting a `marker.mark(...)` call
  in a `Trace` impl silently drops the slot when its only reference
  is a Gc handle. The pre-M5 stress tests (`gc_stress.rs`) catch
  most cases.

### Things that *don't* change
- `eval`/`apply` semantics. The whole point of Phase 1 staying
  Rc-backed is to keep behaviour identical while the GC machinery
  comes online.
- The 1460-test conformance corpus. M5 is a swap of the Value
  pointer backing, not a feature change.
- The `Value` enum's variant set. M5 only modifies the inner
  pointer type per variant.

## Follow-ups

- [x] M5 step 4.A — `cs-gc` crate scaffold (commit `6cbfe52`).
- [x] M5 step 4.B — `Gc::new` + `cs-core` re-export (commit `edec4d5`).
- [x] M5 step 4.C — migrate Value variants (7 of 8; Procedure is the
  documented exception).
- [x] M5 step 4.D — Runtime persistent root set wired
  (commit `34e9fc2`).
- [x] M5 step 4.G partial — fuzz + pause-time harness
  (commit `dea6629`).
- [ ] M5 step 4.F — Phase 2 arena swap.
- [ ] M5 exit-gate sweep: 24h fuzz CI workflow, criterion bench,
  memory-baseline measurement.

## References

- ROADMAP.md M5 entry.
- `.spec-workflow/specs/gc/requirements.md` (functional + NFR).
- `.spec-workflow/specs/gc/design.md` (component sketch).
- `.claude/pre-m5-plan.md` (per-step iter trace).
- `docs/milestones/m4-exit.md` (the M4 baseline this builds on).
- Dybvig, "The Scheme Programming Language" — chapter on memory.
- "Garbage Collection Handbook" (Jones, Hosking, Moss) — mark-sweep
  algorithm.
