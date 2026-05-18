# ADR 0016: Region Types — Bump-Arena Allocation as Layer 3

> Status: Accepted (layer 3 of the unified memory management
> architecture — see [ADR 0015](./0015-unified-memory-management.md))
> Date: 2026-05-17
> Spec: `.spec-workflow/specs/region-memory/` (CLOSED)
> Builds on: [ADR 0014 — Countable Memory](./0014-countable-memory.md)
> Enables: forthcoming `escape-analysis` spec (layer 5)

## Context

ADR 0015 lays out a five-layer memory architecture combining
ownership, reference counting (layer 2, ADR 0014), regions
(layer 3, this ADR), opt-in tracing (layer 4), and compiler-
driven allocation dispatch (layer 5). Layer 2 ships in M5b /
countable-memory: every heap-allocated value is `Gc<T>`
backed by `Rc<T>` with a synchronous cycle detector.

Pure refcounting has well-understood per-allocation overhead:
each `Rc::new` traverses the system allocator, each clone
issues an atomic-free increment, and each drop issues a paired
decrement and may chain into a recursive cleanup. For values
whose lifetime is provably bounded by some surrounding scope
(`let` bodies, function calls, `map` pipelines) most of that
machinery is wasted — the value can be allocated, mutated,
and bulk-freed in a single bump-arena pass.

Layer 5 (escape analysis) will identify those bounded-lifetime
sites and emit region allocations directly. Before layer 5 can
exist, the runtime needs a region primitive it can target.
This ADR ratifies that primitive.

## Decision

Add a `cs_gc::Region` type and extend `Gc<T>` to internally
discriminate between Rc-backed and Region-backed allocations.
The two arms share API surface byte-for-byte from the caller's
perspective; the discrimination is invisible to consumer code.

Specifically:

1. **`cs_gc::Region`** — a single-threaded bump arena owning
   a `bumpalo::Bump` and a unique `RegionId`. Constructed by
   `Region::new()`, drops bulk-free every allocation regardless
   of outstanding `Gc<T>` handles.

2. **`Gc<T>` as a discriminated union** —
   ```rust
   enum GcRepr<T: ?Sized> {
       Rc(Rc<T>),
       Region {
           ptr: NonNull<RegionSlot<T>>,
           region_id: RegionId,
       },
   }
   pub struct Gc<T: ?Sized>(GcRepr<T>);
   ```
   - The Rc arm preserves M5b semantics exactly.
   - The Region arm holds a raw pointer into a `Region`'s
     bump arena, plus the region's id for debug-mode
     validity checking. An in-line 8-byte per-allocation
     header carries a refcount that the JIT raw-handle ABI
     (ADR 0012 D-2) requires and that lets `Gc::strong_count`
     report a meaningful value — but the count does **not**
     drive reclamation.

3. **`Gc::new_in(region, value)`** — the public constructor
   for region-allocated `Gc<T>`. `Gc::new` continues to
   allocate Rc-backed values, preserving the default semantics
   M5b shipped.

4. **`Gc::promote`** — the escape hatch. When a region-
   allocated value's lifetime turns out to extend past its
   region (layer-5 escape analysis missed it, or a manual
   region user explicitly hands the value out), `Gc::promote`
   deep-clones the payload into a fresh Rc allocation. The
   Promote trait (`cs_core::Promote::promote_deep`) walks a
   `Value` tree promoting every Region-backed inner handle.

5. **Debug-mode validity check** — a thread-local
   `LIVE_REGION_IDS` set tracks every alive region.
   `assert_region_live(region_id)` fires on every Region-arm
   `Gc<T>` operation; under `#[cfg(debug_assertions)]` it
   panics with a clear diagnostic if the region has already
   dropped. In release, the check compiles to nothing —
   correctness depends on layer-5 escape analysis (or
   manual discipline for direct `new_in` callers).

6. **Cycle-detector skip on region mutations** —
   `b_set_car` / `b_set_cdr` / `b_vector_set` /
   `b_hashtable_set` guard the synchronous cycle detector
   behind `!Gc::is_region(container)`. Region cycles
   reclaim via the region's bulk free regardless of
   internal back-edges, so running the detector would
   waste CPU and could falsely refuse to break a benign
   cycle (the strong-count guard from countable-memory
   iter 7.1.x.y would treat a region pair as the only
   strong holder and refuse to demote).

7. **Single-region-per-Gc** — a `Gc<T>` belongs to one
   region (or to the global Rc heap) and stays there. We
   considered region polymorphism (a `Gc<T>` that could
   migrate between regions) but rejected it for v1: the
   semantic complexity wasn't justified by any concrete use
   case, and migration is already covered by `Gc::promote`
   for the only direction that mattered (region → Rc).

## Trade-offs

### What we accept

- **Release-mode UB on dangling region handles.** When a
  release build outlives the use-after-region-drop check,
  a stray handle into a freed bump arena is UB. The bet:
  layer 5 (escape analysis) prevents this statically for
  compiled programs, and direct `new_in` users are vetted
  by code review.
- **No per-allocation Drop on region-allocated values.**
  `bumpalo::Bump` does not run `Drop` on its allocations —
  the region's drop tears down the buffer but doesn't visit
  individual payloads. Values placed in a region must
  be POD-like (or their cleanup must happen on a different
  path). The `region_drop_does_not_run_payload_drop` test
  documents this contract.
- **8-byte per-allocation overhead.** The `RegionSlot<T>`
  header (4-byte refcount + 4-byte pad) is unconditional;
  even pure-bump allocations with no refcount semantics
  pay for it. Justification: ABI parity with the JIT raw-
  handle path (which must hand out an opaque integer
  whose `raw_incref` operation can bump a count without
  knowing which arm it came from).
- **`Region` is `!Send`/`!Sync`.** Single-threaded only.
  Multi-threaded regions are out of scope; the threading
  story can revisit this if/when cs-runtime grows real
  thread support.

### What this buys

- A primitive layer-5 can target. The cs-typer effect
  inferencer can emit `Gc::new_in(region, …)` at every
  allocation site it proves bounded — no further runtime
  surface needed.
- A path to `O(1)` per-allocation cost for the hot loop
  pattern (allocate-mutate-discard within a single dynamic
  scope), at the cost of one bump-pointer increment plus a
  zeroed slot header.
- Bulk free reduces reclamation cost to a single
  `Bump::drop` per region, replacing N individual
  `Rc::drop` chains.
- A clean separation of concerns: region-allocated values
  participate in **no** cycle detection. The detector
  stays correct for the Rc world; the region world handles
  cycles via the bulk-free.

## Status and outlook

Implemented in `.spec-workflow/specs/region-memory/` iters
1–6 (closed 2026-05-17). The feature is on by default in all
crates; opt out with `--no-default-features --features
countable-memory`.

The forthcoming `escape-analysis` spec (layer 5) consumes
this primitive. Until that spec lands, regions are usable
only via explicit `Gc::new_in` calls — meaning today's
production allocation path is still 100% Rc-backed. The
infrastructure is in place, waiting for layer 5 to start
exercising it.

`tracing-revival` (layer 4) remains spec-only until a
concrete cycle workload demands it. Region allocation
removes the most common motivation for tracing
(bounded-lifetime values that escape refcount analysis),
so layer 4's priority drops accordingly.

## References

- ADR 0006 — Garbage Collector Design (the original M5 tracing GC)
- ADR 0012 — JIT Boxed Value ABI / D-2 raw-handle ABI
- ADR 0014 — Countable Memory (layer 2; the immediate predecessor)
- ADR 0015 — Unified Memory Management (the layered architecture)
- `.spec-workflow/specs/region-memory/` — full spec (closed)
- `docs/milestones/region-memory-exit.md` — exit report
