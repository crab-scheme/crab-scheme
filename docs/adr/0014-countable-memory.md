# ADR 0014: Countable Memory — Refcount-Only Reclamation

> Status: Accepted
> Date: 2026-05-17
> Supersedes (in part): [ADR 0006 — Garbage Collector Design](./0006-gc-design.md)
> Spec: `.spec-workflow/specs/countable-memory/`

## Context

CrabScheme shipped M5 with a precise tracing GC: `Gc<T>` backed by
`Rc<Slot<T>>`, plus a `Heap` of root closures, a `Trace` trait
threaded through every heap-bearing value, a `Marker` walker, and
a stop-the-world mark-sweep `collect()`. The exit gate was met
(see `docs/milestones/m5-exit.md`): 24-hour fuzz green, p99 GC
pause 4.1 µs (240× under the 1 ms target), memory within budget,
conformance parity at 2150 individual tests.

A year on, the steady-state cost of the tracing layer paid no
dividend:

1. **`auto_collect` defaulted to false.** `Heap::new()` constructs a
   heap with `auto_collect: false` (`crates/cs-gc/src/lib.rs:343`);
   no embedder turns it on. Every reclamation in production is an
   `Rc::drop` on the inner `Slot<T>` — i.e., pure refcounting.

2. **The deferred Phase 2 arena swap was the path to making the
   tracing layer pay for itself** (bump alloc, generational
   copying). Phase 2 stayed deferred; without it the tracing layer
   is overhead with no offsetting benefit.

3. **The cycle-leak trade-off that M5 accepted as a Phase-1
   limitation is the one remaining justification for tracing.** It
   only matters for the small enumerable set of mutations that
   can construct cycles (`set-car!`, `set-cdr!`, `vector-set!`,
   `hashtable-set!`, mutually-`set!`'d closures). A targeted
   synchronous cycle detector at those mutation sites is cheaper
   steady-state than a tracing layer that walks the whole heap.

4. **WASM (M10 Track W) shipped on the Rc-backed Phase 1.** Nothing
   in the WASM tier depended on the tracing layer.

5. **JIT/AOT integration goes through the raw-handle ABI
   (ADR 0012 D-2)**, which is a `Rc::into_raw`/`Rc::from_raw`/
   `Rc::increment_strong_count` pass-through regardless of whether
   a `Slot<T>` wrapper is in place.

This ADR ratifies the move to **pure refcount reclamation plus a
synchronous local cycle detector at mutation sites**, removing
the tracing infrastructure that no production code path
exercised.

## Decision

CrabScheme commits to **`Rc<T>`-backed `Gc<T>` as the sole heap
representation**, augmented by:

1. A small synchronous cycle detector (`cs_gc::cycle`) invoked
   from `set-car!` / `set-cdr!` / `vector-set!` / `hashtable-set!`
   after each mutation. The detector walks the mutated subgraph
   and reports cycles via a thread-local counter
   (`cs_runtime::countable_memory_cycle::cycle_detection_count`).

2. A `CycleVisit` trait — narrow replacement for `Trace` — that
   per-type impls use to enumerate `Gc<...>` children. `Frame` /
   `Env` / `Closure` / `VmClosure` register their own `Rc<T>`
   address via `CycleVisitor::visit_addr` so the detector dedup
   prevents re-entry into closure-over-self cycles.

3. **`Weak<T>` re-export** from `cs_gc` (a thin newtype over
   `std::rc::Weak`) so consumer crates hold weak back-edges
   without importing `std::rc` directly. Reserved for a future
   iter (7.1 / 8) that introduces Strong/Weak storage slot enums
   on `Pair` / `Vector` / `Hashtable` and refactors `Frame.parent`
   / `Continuation` parent chains to use weak references — closing
   the residual cycle leaks documented under "Consequences" below.

### Alternatives considered

#### Stay on Phase-1 tracing + ship Phase 2 arena

The original M5 plan. Rejected because:
- The arena swap is non-trivial work (relocation requires updating
  every spilled `Gc<T>` handle, including those in JIT-emitted
  code). The cost-benefit hadn't justified scheduling it in a year
  of production use.
- The cycle-leak trade-off it would close is also closeable via
  a targeted synchronous detector at mutation sites at lower
  steady-state cost.
- Removing the tracing scaffolding makes the WASM / AOT story
  strictly simpler (fewer cfg-paths, fewer feature combinations).

#### Move to a third-party GC crate (`gc-arena`, `rust-gc`)

Rejected for the same reasons as ADR 0006: rooting flexibility,
audit surface, and shape-match to our `Value` layout.

#### Defer cycle handling entirely (refcount-only, leak cycles)

Rejected because R6RS programs can construct mutable cycles
intentionally; reclaiming them is a correctness contract. The
synchronous detector closes the gap with bounded per-call cost.

## Consequences

### Positive

- **Zero stop-the-world pause.** Reclamation is interleaved with
  `Rc::drop` at deterministic points. The M5 `p99 GC pause < 1
  ms` gate is trivially met (no pause exists).
- **Deterministic port finalization.** File-output ports flush
  and close on the last `Rc<Port>` drop without any
  `(close-port)` or explicit `collect()` call. Verified by
  `crates/cs-runtime/tests/port_finalization.rs`.
- **Smaller per-allocation overhead.** Phase-1's `Slot<T>` mark
  word is gone; the only per-allocation header is the standard-
  library `Rc` strong/weak count (16 bytes on 64-bit).
- **No `unsafe` outside the JIT raw-handle ABI.** The cycle
  detector is `unsafe`-free; the only `unsafe` in `cs-gc` is the
  three JIT ABI functions (`into_raw_jit` / `from_raw_jit` /
  `raw_incref`), unchanged from M5.
- **WASM / AOT story stays clean.** Both targets continue to
  build without modification.
- **Smaller maintenance surface.** `cs-gc` shrinks from ~656 LOC
  to under 500 across the rc-only + cycle modules; consumer
  crates lose the `Trace` boilerplate.

### Negative / residual gaps

- **Cycles still leak refcount-wise until iter 7.1 / 8 land.** The
  iter-11 detector reports cycles via a counter but does not flip
  the offending storage edge to `Weak<T>`. User code that
  intentionally creates cycles (`(set-cdr! x x)`) succeeds; the
  cyclic structure leaks at refcount-drop time. Conformance and
  benchmarks do not construct unbounded cycles, so this does not
  regress test green / perf numbers, but pathological programs
  that grow cycles unboundedly will OOM under refcount-only.

  **Closure of this gap is tracked as follow-up iters 7.1 and 8
  in `.spec-workflow/specs/countable-memory/tasks.md`** —
  introduce Strong/Weak slot enums on `Pair` / `Vector` /
  `Hashtable`; refactor `Frame.parent` / `Continuation` parent
  chains to `Weak<T>`. Both are non-trivial because every read
  site on those types needs to go through an accessor that
  resolves Weak transparently.

- **`gc-stats` returns `(0 0)`** under the new representation
  (no heap to query). Tests and embedders that consulted alloc /
  collect counts get zero values.

- **`Runtime::collect()` becomes a no-op shim.** Programs that
  forced a collection cycle for deterministic timing get no
  behavior change — RC reclamation already runs at deterministic
  drop points.

### Things that don't change

- `Value`'s variant set and pattern-match shape.
- The JIT raw-handle ABI (`Gc::into_raw_jit` / `Gc::from_raw_jit`
  / `Gc::raw_incref` — ADR 0012 D-2).
- Conformance: 117 cs-cli conformance files green, 0 failures
  workspace-wide, both under default (countable-memory on) and
  under `--no-default-features --features jit,ffi-dynamic`
  (tracing path, preserved until iter 12b deletes it).
- The 2150 individual Scheme assertion count from M5 exit.

## Follow-ups

- [~] iter 7.1 — Strong/Weak storage tombstone infrastructure
  on `Pair` shipped (`WeakValue` type, `Pair::car_weak` /
  `cdr_weak` tombstone fields, `Pair::car()` / `cdr()` /
  `set_car()` / `set_cdr()` accessors, ~250 reader sites
  migrated workspace-wide, `Pair::break_car_cycle` /
  `break_cdr_cycle` available but not currently invoked). The
  naive "demote the freshly-mutated slot to Weak" break
  attempted in iter-7.1 orphans the demoted value when the
  slot was its only strong holder — common with
  `(set-car! env (cons name val))` closures-over-env where
  the cons cell has no external strong references. Detection
  still fires (counter increments via
  `cs_runtime::countable_memory_cycle::record_cycle_detected`)
  but no structural break runs. The follow-up iter (7.1.x)
  must implement a Bacon-Rajan-style trial-deletion that picks
  a safe cycle edge to weaken. Vector and Hashtable tombstone
  infrastructure (analogous to Pair) is also outstanding.
- [x] iter 8 — `Frame.parent` / `Continuation` parent chain
  refactored to `Weak<Frame>` for structural cycle prevention.
  **Not applicable — closed as won't-do.** See "Iter 8
  architectural mismatch" below.
- [ ] iter 12b — delete the cfg-gated tracing path entirely
  (point of no return; gates on iter 7.1 being mature enough
  that no rollback path is needed).

### Iter 8 architectural mismatch

The countable-memory spec's iter 8 specifies refactoring
`Frame.parent` from `Rc<Frame>` to `cs_gc::Weak<Frame>` so that
continuation-captured frame chains form structurally
non-cyclic graphs. This rationale assumed two things that
turned out not to hold for CrabScheme:

1. **Continuations don't capture frame chains in CrabScheme.**
   `cs_runtime::proc::Continuation { id: u64 }` is an escape-
   only continuation holding only a numeric id. There is no
   captured frame, so making `Frame.parent` weak doesn't break
   any cycle that actually exists in the runtime.

2. **The walker's TCO loop overwrites `cur_env`.** In
   `eval_inner` (`crates/cs-runtime/src/eval.rs` Letrec / Lambda
   bodies / If branches / Begin sequences), the active env is
   updated via `cur_env = new_env;` and the previous outer
   env's `Rc` is dropped. The new env's lookups walk up the
   parent chain to find globals; if `parent` were `Weak`, that
   upgrade would fail because the only strong reference to
   the outer env (the original `cur_env`) just got dropped.
   The walker would need a fundamentally different ownership
   model (a strong env stack) to survive Weak parents.

The same architectural fact rules out making `closure.env`
weak: closures escape their defining scope (`(let ([x 1])
(lambda () x))` returns a closure whose env outlives the
walker's strong reference to that env). A weak `closure.env`
would dangle on first invocation after the let returned.

The cycle that iter 8 was meant to close — closures whose env
contains a binding back to the closure itself
(`(define (f) f)` and the letrec-self family) — is not closed
by frame-parent weakening regardless. The cycle goes through
the binding storage, not the parent chain. iter 7.1's
Strong/Weak storage slot refactor is the appropriate fix
because it targets the actual cycle edge.

**Conclusion**: iter 8 is retired without action. iter 7.1
remains the path to refcount reclamation of cyclic structures.

These follow-ups close the residual cycle-leak gap. The
iter-12a documentation (this ADR + amendment to ADR 0006 +
exit report) ratifies the iter-11 state.

## References

- `.spec-workflow/specs/countable-memory/{requirements,design,tasks}.md`
- `docs/milestones/m5-exit.md` (the M5 GC exit this builds on)
- `docs/milestones/countable-memory-exit.md` (this iter's exit
  report; covers iter 1–11 + the iter 12a docs)
- ADR 0006 — supersedes the Phase 1 → Phase 2 commitment; the
  "hand-rolled vs gc-arena" decision stays in force.
- ADR 0012 — JIT boxed Value ABI; the raw-handle ABI surface
  preserved here.
