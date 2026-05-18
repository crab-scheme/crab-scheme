# Parallel Runtime — Requirements

> Status: **Draft** (proposed post-1.0 architecture track).
> Spec slug: `parallel-runtime`
> Roadmap slot: post-1.0; **completes** the deferred work captured in
> tracking issues #107 (B3 work-stealing scheduler + auto-yield), #20
> (cs-opt parameter dep-inversion), and the layer-4 + layer-5 memory
> gaps documented in `countable-memory` + `region-memory` +
> `escape-analysis` specs.
> Predecessor: `beam_runtime_spec.md`, `countable-memory`,
> `region-memory`.
> Companion: `design.md`, `tasks.md` (this spec).

This spec finishes the runtime's transition from "structurally
parallel, operationally bounded" to **true per-core parallelism
with a memory model that supports it**. Both halves — the actor
scheduler and the memory layer — need coordinated changes; doing
either alone leaves a load-bearing gap (async actors without a
work-stealing-aware region stack get use-after-free panics; better
cycle collection without async actors hits the 4096-thread ceiling
long before the cycle pressure matters).

## Why now

The post-PR-#24 review of "actors as a parallelization principle"
and "memory helpers" surfaced a coordinated picture: the two
subsystems share a deliberate first-cut implementation with
documented future state. Each piece works in isolation but the
combined story has the following gaps:

### Gap A — Actor scaling wall at 4096

`crates/cs-actor/src/lib.rs:311-313` configures
`worker_threads(1).max_blocking_threads(4096)`. Every actor today
runs on its own OS thread via `tokio::spawn_blocking`, which
parks indefinitely in `blocking_recv()`. N-actor workloads are
hard-capped at 4096; beyond that, the runtime blocks new spawns.
BEAM workloads commonly have 100k+ actors.

The fix is "actor bodies become `async fn`" — replace
`spawn_blocking` with `tokio::task::spawn`, replace
`blocking_recv` with `recv().await`. This collapses the ceiling
because M worker threads can multiplex N actors (N ≫ M).

### Gap B — No automatic reduction preemption

`REDUCTIONS` thread-local and `bump-reductions!` builtin scaffold
exist (`crates/cs-runtime/src/builtins/beam.rs:350-351,795-811`),
but the bytecode dispatch loop never calls the yield hook
automatically. A CPU-bound Scheme actor can starve its OS thread
indefinitely. Today this just exhausts the blocking-thread pool;
post-Gap-A it would starve the async runtime entirely.

The fix is a yield-on-N-reductions hook fired from
`cs-vm::vm::run_dispatch` and (post-JIT) from a safepoint poll
in JIT-compiled code.

### Gap C — Region stack is thread-local, but async tasks migrate

The region scope stack (`crates/cs-runtime/src/regions.rs:37`) is
`thread_local!` — a `Vec<Rc<Region>>` per OS thread. Today this
is correct because each actor lives on one OS thread for its
entire lifetime. Post-Gap-A, an actor that yields at a
`recv().await` point may resume on a different worker thread; if
that actor had an open `(with-region ...)` scope and is in the
middle of a `cons-in-region` chain, the TLS stack on the new
thread does not contain its region.

Three viable resolutions, ordered by risk:
1. **Forbid `with-region` across `await` points.** Easiest to
   verify; loses ergonomics (every actor body that wants regions
   has to bracket carefully).
2. **Move the region stack from TLS to the actor's per-task local
   storage** (tokio task-local). Region stack travels with the
   actor across worker hops.
3. **Pin actors that hold open regions to their current worker
   thread until the region scope closes.** Lowest user-facing
   churn; complicates the scheduler.

Option 2 is the design recommendation (see `design.md`).

### Gap D — Cycle collector handles 1- and 2-pair cycles only

Layer-4 sweep (`crates/cs-gc/src/cycle_registry.rs:200-206`)
explicitly defers Bacon-Rajan trial deletion. Today's sweep
calls `try_break_cycle` per candidate which works for 1- and
2-pair cycles but fails on N-pair (3+) chains. The candidate
just stays registered and leaks at process exit.

Post-Gap-A this becomes worse: per-actor heap pressure
accumulates if any actor's body forms a 3+ pair cycle. The fix
is to ship the deferred Bacon-Rajan algorithm in the sweep
(detect strongly-connected components, color-mark, decrement
inside the SCC, reclaim if external refcount drops to zero).

### Gap E — Memory primitive ergonomics around regions

Several sharp edges surfaced by the review:

- `Gc::downgrade` on a region-allocated handle silently returns a
  dead `Weak` in release (`cs-gc/src/rc_only.rs:247-258`). Debug
  has a `debug_assert!`. Release builds get silent data loss.
- `(with-region ...)` doesn't propagate the region scope into an
  L2 sandbox eval (TLS isn't shared with the sandbox guest's
  separate Runtime). Cross-feature gap.
- Contracts don't track allocator origin: a contract wrap on a
  procedure that returns a region-allocated pair will panic at
  the consumer site if the region drops between the contract
  check and the consumer call.

These are correctness footguns that get worse under parallel
actors because the "region was dropped on a different thread
than the one that holds the handle" failure mode becomes
reachable.

### Gap F — `SendableValue` deep-clone cost at scale

`crates/cs-runtime/src/builtins/beam.rs:64-143`. Every cross-actor
message is a deep clone of the entire value tree. Correct
BEAM-style copy-on-send semantics, but the cost scales with value
size, not just complexity. A 1MB list sent 1000 times across
actors copies 1GB total.

The post-1.0 escape-analysis work + a future "shareable immutable
value" type could let some values cross actor boundaries by
`Arc`-shared reference instead of deep-clone. Not in scope for
this spec's MVP but called out as a follow-up.

## Goals

This spec defines a coordinated set of changes that, when shipped
together, deliver:

1. **G1 — 1M+ actor capability.** Async actor bodies + work-stealing
   tokio runtime. Tested via a 1M-spawn / 10M-message soak.
2. **G2 — Automatic preemption.** No CPU-bound actor can starve
   the worker pool. Tested via an actor that loops without calling
   `(yield)`, verified that other actors continue receiving messages.
3. **G3 — Region scopes survive async yields.** Region stack
   travels with the actor across worker thread migrations. Tested
   via an actor that opens a region, awaits, resumes on a different
   worker, and continues `cons-in-region` calls without
   `assert_region_live` panic.
4. **G4 — N-pair cycle reclamation.** Bacon-Rajan trial deletion
   in layer-4 sweep. Tested via a 100-pair cycle and verified
   complete reclamation on `(collect)`.
5. **G5 — Hardened region ergonomics.** `Gc::downgrade` on region
   handles is a clean error (not silent), and contracts can refuse
   region-backed values from crossing a contract boundary.
6. **G6 — No regression on single-actor / no-region workloads.**
   The existing bench corpus continues to perform within 5% of
   pre-spec numbers on workloads that don't exercise the new
   parallelism / cycle paths.

## Non-goals

- **`SendableValue` shareable-immutable optimization.** Tracked as
  a follow-up; this spec keeps the deep-clone semantics. Cost
  improvement is opportunistic; correctness is the bar.
- **Cross-actor shared mutable state beyond `cs-table`.** BEAM's
  isolation guarantee is preserved; we don't add `tokio::Mutex`-
  guarded Values.
- **JIT-aware automatic preemption.** This spec ships
  bytecode-tier preemption only. JIT preemption needs Cranelift
  safepoints and is tracked as #105 (already deferred).
- **Full layer-5 escape analysis.** Stubbed (`Lifetime::Traced`
  fallback in `alloc_dispatch.rs:56`). Layer-5 is a separate spec.

## Acceptance criteria

Each goal has a measurable test. See `tasks.md` for the iteration
breakdown; the acceptance gates are listed there per-iteration.
The spec is complete when:

- All G1–G6 tests pass on the main worktree
- `bench/realworld/runner.sh` shows ≤ 5% regression on existing
  benches (G6 sanity)
- The new bench suite (`bench/parallel-runtime/`, added in this
  spec) shows the headline numbers: 1M actors, 10M messages, p99
  < 50ms message latency, no leaked cycles after a 1-hour soak

## Out-of-scope follow-ups (filed as issues)

- **#107** (B3 second half) — closed by G1+G2 in this spec
- **#20** (cs-opt parameter dep-inversion) — independent, stays open
- **#14** (ticker leak) — independent, stays open
- **#15** (nested-eval L1 bypass) — independent, stays open
- **#23** (JIT_ACTIVE_HEAP regression) — independent, stays open

The `SendableValue` shareable-immutable optimization gets a new
follow-up issue once G1 lands and we have a measurable baseline.
