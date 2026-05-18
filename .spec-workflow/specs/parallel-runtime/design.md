# Parallel Runtime — Design

> Status: **Draft**.
> Companion: `requirements.md`, `tasks.md`.

## Overview

Six interlocking changes that, together, take the runtime from
"1 actor per OS thread, ceiling at 4096" to "1M actors with
work-stealing across `num_cpus` workers, region scopes that survive
async migrations, and Bacon-Rajan-complete cycle reclamation."
Each change is small in isolation but they depend on each other —
the spec ships them as one coordinated track.

### Change map

| # | Component | File(s) | Owner crate |
|---|---|---|---|
| C1 | Async actor bodies | `crates/cs-actor/src/lib.rs` | cs-actor |
| C2 | Reduction-driven yield in VM dispatch | `crates/cs-vm/src/vm.rs` | cs-vm |
| C3 | Region stack as task-local (not TLS) | `crates/cs-runtime/src/regions.rs` | cs-runtime |
| C4 | Bacon-Rajan trial deletion in sweep | `crates/cs-gc/src/cycle_registry.rs` | cs-gc |
| C5 | `Gc::downgrade` hardening + contract-region guard | `crates/cs-gc/src/rc_only.rs` + `lib/contract/contract.scm` | cs-gc + lib |
| C6 | New parallel-runtime bench harness | `bench/parallel-runtime/` | bench |

## Steering document alignment

### Tech standards (`steering/tech.md`)

§"Decision Log" item 7 covers BEAM-style runtime intent. This spec
makes that intent operational. The change is consistent with the
existing actor-isolation promise (no shared mutable Values across
actors).

### Memory architecture (`countable-memory`, `region-memory`,
`escape-analysis`)

This spec doesn't introduce a new memory layer. It completes layer
4 (Bacon-Rajan in cycle-registry sweep) and hardens the layer-3
API surface. Layer 5 (escape analysis) remains in its own spec.

## C1 — Async actor bodies

### Current state

```rust
// cs-actor/src/lib.rs:354
let handle = tokio::task::spawn_blocking(move || {
    let mut actor = Actor::new(pid, inbox, ...);
    body(&mut actor);  // sync closure; calls blocking_recv inside
});
```

`spawn_blocking` lands the closure on a dedicated OS thread.
Inside, `Actor::receive()` calls `inbox.blocking_recv()` which
parks the thread. At 4096 actors the blocking-thread pool is
exhausted.

### Target state

```rust
// cs-actor/src/lib.rs (post-C1)
let handle = tokio::task::spawn(async move {
    let mut actor = Actor::new(pid, inbox, ...);
    body(&mut actor).await;  // async closure
});
```

Replace `spawn_blocking` with `spawn`. Actor body becomes
`async fn` (or impl AsyncFn). `recv()` is now `await`-based —
parks the *task*, not the OS thread. M worker threads multiplex
N tasks; N ≫ M is fine.

### Migration sequence

The actor-body API change is breaking for embedders. Migration:

1. **C1a:** Add `spawn_async` / `Actor::recv_async` alongside the
   existing sync ones. Both work. Existing callers unchanged.
2. **C1b:** Migrate every in-tree caller (the Scheme builtins in
   `crates/cs-runtime/src/builtins/beam.rs`) to the async variants.
3. **C1c:** Deprecate the sync variants (still compile, doc-comment
   marks them legacy).
4. **C1d:** Bump `worker_threads(1)` → `worker_threads(num_cpus_get())`
   in the `ActorSystem::new` builder. This is safe once C1b lands
   (no more sync `blocking_recv` competing for that one async worker).

### Scheme surface

The Scheme `(spawn body-thunk)` primitive doesn't change shape —
the thunk runs as a coroutine that yields naturally at every
`(raw-receive)` call. The bytecode interpreter has to learn to
yield to the async runtime at receive points; see C2 for the
mechanism. From a Scheme user's perspective, the only observable
change is that 1M `(spawn ...)` calls no longer fail.

### Correctness preconditions

The cs-gc `Rc`-based heap is per-actor. Each actor's `Runtime` is
created inside the spawn closure (`beam.rs:307-329`). Under C1,
each tokio task owns its own Runtime; tasks don't share `Rc`
values across `await` points (the `!Send` bound on `Value`
prevents accidental cross-task sharing — the compiler refuses to
move a Value into a `Future` that crosses an `await`).

`SendableValue` (the deep-clone bridge) is `Send + Sync` and is
the only cross-task transport. Unchanged.

## C2 — Reduction-driven yield in VM dispatch

### Current state

`REDUCTIONS` thread-local exists but the bytecode loop never
checks it. Scheme code must call `(yield)` manually to give other
actors CPU time.

### Target state

The bytecode dispatch loop polls a per-task reduction counter and
yields to the runtime when it crosses a threshold (default 2000
reductions = roughly 2000 bytecode ops; matches BEAM's reduction
budget).

```rust
// cs-vm/src/vm.rs::run_dispatch (sketch)
loop {
    if REDUCTIONS_LEFT.with(|c| { let v = c.get(); c.set(v - 1); v <= 0 }) {
        // Reset and yield. In async context this calls
        // tokio::task::yield_now().await via a hook the runtime
        // installed at task-spawn time.
        if let Some(yield_hook) = YIELD_HOOK.with(|h| h.get()) {
            (yield_hook)();  // calls a fn pointer that does
                             // `tokio::task::yield_now().await`
                             // via a runtime-side trampoline
        }
        REDUCTIONS_LEFT.with(|c| c.set(REDUCTION_BUDGET));
    }
    // ... normal bytecode dispatch
}
```

The cross-crate hook avoids making cs-vm depend on tokio (cs-vm is
WASM-target-compatible; tokio isn't). cs-runtime installs the hook
during `Runtime::install_actor_runtime` (a new method added in C1).
When the hook is `None` (non-actor contexts), the reduction counter
is harmless overhead.

### Cost

One `Cell::get` + branch per N bytecode ops. At N=2000, ~0.05%
overhead in the common case. Yielding fires only when the budget
expires — typically every few ms of execution.

### JIT note

JIT-compiled code is exempt from C2 (no per-op poll). JIT
preemption needs Cranelift safepoints, tracked as #105. Under
this spec a CPU-bound JIT-compiled actor body can still starve
its worker briefly; the eventual return-to-bytecode at a receive
point picks up the yield. Acceptable since JIT loops are
typically short and exit at a Scheme-level loop boundary.

## C3 — Region stack as task-local (not TLS)

### Current state

```rust
// crates/cs-runtime/src/regions.rs:37
thread_local! {
    static REGION_STACK: RefCell<Vec<Rc<Region>>> = const { ... };
}
```

`with-region` pushes to TLS; `cons-in-region` reads TLS. Works
when actor lives on one OS thread.

### Target state

Tokio task-local storage. The actor's task carries its region
stack; on resumption (potentially on a different worker thread),
the stack is restored.

```rust
// crates/cs-runtime/src/regions.rs (post-C3)
tokio::task_local! {
    static REGION_STACK: RefCell<Vec<Rc<Region>>>;
}
```

Tokio task-locals are implemented as a per-task `HashMap<TypeId, Box<dyn Any>>`.
Access cost: one HashMap lookup per `current_region()` call vs.
one TLS read. Cost difference is ~10ns per access; in the hot
path (`cons-in-region`) this is dominated by allocation cost.

### Fallback for non-actor contexts

REPL / single-thread `crabscheme --tier vm` doesn't run inside a
tokio task. The accessor needs a fallback:

```rust
pub fn current_region() -> Option<Rc<Region>> {
    // Try task-local first (actor context); fall back to TLS
    // (non-actor context).
    if let Ok(rs) = REGION_STACK.try_with(|c| c.borrow().last().cloned()) {
        return rs;
    }
    REGION_STACK_TLS.with(|c| c.borrow().last().cloned())
}
```

Both paths must be kept in sync on push/pop. The dual-stack design
means a Runtime that's used in BOTH a tokio task AND a direct
Scheme eval (rare but possible — embedders that mix patterns) has
to use the correct stack consistently. The push site (which is
`b_with_region`) checks if it's inside a task first and uses the
matching stack throughout the scope.

### Correctness check

The `LIVE_REGION_IDS` HashSet (`cs-gc/src/region.rs:187`) stays
TLS — it's only used for `assert_region_live`, which fires from
`Gc::clone` / `Gc::deref` on whatever thread is executing. As
long as a region is registered LIVE on its owning thread when
it's allocated, accesses from the same thread succeed; if the
actor migrates, the new thread's `LIVE_REGION_IDS` doesn't
include the region. This is a bug.

**Fix:** `LIVE_REGION_IDS` also becomes task-local for actor
contexts. Same dual-stack pattern as the region stack.

## C4 — Bacon-Rajan trial deletion in sweep

### Current state

`cycle_registry.rs:218` (`run_sweep`):

```rust
for cand in surviving_candidates {
    cand.upgrade_and_try_break();  // breaks 1- and 2-pair cycles only
}
```

3+ pair cycles register, never get broken, leak at process exit.

### Target state

Implement Bacon & Rajan "Concurrent Cycle Collection in Reference
Counted Systems" (2001) trial deletion:

1. **Phase 1 — Candidate selection.** Existing logic: walk
   registered candidates, prune dead-Weak ones. Surviving
   candidates are "possibly part of a cycle".
2. **Phase 2 — Trial decrement.** For each candidate, decrement
   the refcount of every child. Use a color-mark on each visited
   node (Black/Gray/White) to track state.
3. **Phase 3 — Scan.** Walk children again; if a node's refcount
   is now zero, it was only kept alive by the cycle (White).
   Restore the refcount on Gray nodes (those reachable from
   outside the cycle).
4. **Phase 4 — Reclamation.** White nodes are cycle-garbage;
   collect them.

This requires:

- Adding `color: AtomicU8` to `Slot<T>` (currently just
  `strong: u32`)
- Implementing `BreakCycle::children_for_trial_deletion` on
  each heap type (Pair, Vector, Hashtable, Promise) — returns
  `Vec<dyn AnyWeak>` for the trial walk
- A `cycle_collector` module in cs-gc that drives the 4 phases

Cost: amortized O(candidates + their reachable set) per sweep.
For typical Scheme programs cycle pressure is low; the sweep
is rare.

### Compatibility with layer-3 regions

Region-allocated values are excluded from cycle registration
(`countable_memory_cycle.rs:84-89`). Bacon-Rajan only touches
Rc-allocated values. Regions handle their own cycle case (the
arena drop reclaims everything).

## C5 — Region/downgrade + contract hardening

### C5a — Hard error on `Gc::downgrade(region-handle)`

```rust
// cs-gc/src/rc_only.rs (post-C5a)
pub fn downgrade(g: &Gc<T>) -> Weak<T> {
    match &g.repr {
        GcRepr::Rc(rc) => Weak { repr: WeakRepr::Rc(Rc::downgrade(rc)) },
        GcRepr::Region { .. } => {
            // Was: debug_assert + silent dead Weak in release.
            // Now: hard panic with a clear message in all builds.
            panic!(
                "cs_gc::Gc::downgrade: cannot create a Weak handle \
                 from a region-allocated Gc<T> (region {}). Region \
                 values have arena lifetimes; downgrade is meaningless. \
                 Use `to_rc_deep` to promote out of the region first \
                 if you need weak references.",
                /* region_id */
            );
        }
    }
}
```

A weak handle to a region-arena value can't outlive the region
(the arena drop reclaims it regardless of weak count). Silent
dead Weak in release was a latent footgun; making it loud is
strictly an improvement.

Migration check: grep workspace for `Gc::downgrade` call sites;
audit each for region-handle inputs. The cycle-break tombstone
path is the only caller that does downgrade, and it's already
guarded against region inputs (`Pair::break_*_cycle` returns
`false` for region-allocated pairs).

### C5b — Contract region-allocator check

```scheme
;; lib/contract/contract.scm — new helper
(define (__not-region-allocated? v)
  ;; Calls the cs-runtime builtin (gc-allocator v) which returns
  ;; 'rc or 'region or 'traced. Contracts reject region-backed
  ;; values at the boundary because the consumer may outlive the
  ;; region scope that allocated them.
  (not (eq? (gc-allocator v) 'region)))

;; Wrap each apply-contract result check:
(define (__apply-range rng result name desc)
  (cond
    ((__not-region-allocated? result)
     ;; ... existing logic
     )
    (else
     (raise (make-contract-violation
              'callee name desc
              (list 'returned-region-value
                    'allocator 'region
                    'message "contract boundary refuses region-backed value; \
                              call to-rc-deep first or close the region scope"))))))
```

Plus a new cs-runtime builtin `(gc-allocator v)` returning
`'rc | 'region | 'traced` based on the Gc repr.

This catches the silent-corruption case where a region-allocated
pair leaks across a contract boundary — without the check, the
pair survives until first access, then panics on
`assert_region_live`. With the check, the contract fails
synchronously at the boundary with a clear diagnostic.

## C6 — Parallel-runtime bench harness

A new `bench/parallel-runtime/` mirror of `bench/realworld/`:

- `runner.sh` — builds the binary, runs the matrix
- `schemes/spawn-1m.scm` — spawn 1M actors, each receives one
  message, terminates. Time: total wall + p50/p95/p99
  message-delivery latency.
- `schemes/echo-10m.scm` — 1000 actors, each exchanges 10k
  messages with a router. Time: throughput + p99 latency.
- `schemes/long-soak.scm` — 100 actors, 1-hour mixed workload
  (compute + send + recv + occasional cycle creation). Verify
  no leak via `(gc-stats)` snapshots.
- `schemes/region-actor.scm` — actor that opens a region, awaits,
  resumes on a different worker, continues `cons-in-region` calls.
  Verifies C3.
- `schemes/cycle-n-pair.scm` — construct a 100-pair cycle,
  trigger sweep, verify all 100 pairs reclaimed. Verifies C4.

The bench harness becomes the G1/G2/G3/G4 gates in CI.

## Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| C1 + C2 introduce yield-deadlocks under contention | Medium | High | Add a `tokio_console` integration in bench mode; deadlock detection in test harness via Tokio's `task::Builder::name` + `JoinSet` timeouts. |
| C3 dual-stack drift (TLS vs task-local out of sync) | Medium | Critical (use-after-free) | All push/pop go through `RegionScope::enter` / `Drop` which picks the right stack atomically. No raw stack access from outside the wrapper. |
| C4 Bacon-Rajan cost spikes on large heaps | Medium | Low (latency) | Threshold-tuned; sweep can be split across multiple frames using the existing reduction-yield hook from C2. |
| C5a panic-on-downgrade breaks an unknown caller | Low | Medium | Workspace grep before landing; add a `#[deprecated]` warning for one release cycle if a caller depends on the silent behavior (unlikely). |
| `SendableValue` deep-clone cost becomes the actual bottleneck | High | Medium | Out of scope for this spec; benchmark and file follow-up after G1 lands. The shareable-immutable-Arc plan exists in notes. |

## Compatibility matrix

| Subsystem | Pre-spec | Post-spec |
|---|---|---|
| Single-threaded `crabscheme run` | ✅ | ✅ (no change) |
| BEAM actors (≤4096) | ✅ (OS thread per actor) | ✅ (async tasks, multiplexed) |
| BEAM actors (>4096) | ❌ (spawn fails) | ✅ (1M+ tested) |
| `with-region` outside actor | ✅ | ✅ (TLS path) |
| `with-region` inside actor, no await | ✅ | ✅ (task-local path) |
| `with-region` inside actor, across await | ❌ (UB) | ✅ (region travels with task) |
| Cycle: 1- or 2-pair | ✅ (synchronous detector) | ✅ |
| Cycle: 3+ pair | ❌ (leaks at exit) | ✅ (Bacon-Rajan reclaims) |
| L2 sandbox | ✅ | ✅ (unchanged) |
| Contracts on rc-allocated procs | ✅ | ✅ |
| Contracts on region-allocated procs | ❌ (silent UB later) | ✅ (clean error at boundary) |
| JIT-compiled code | ✅ | ✅ (no preemption yet, #105) |

## Open questions

1. **Per-task `Runtime` overhead.** Each actor today gets its own
   full `Runtime`. With 1M actors, that's 1M Runtimes. The hot
   working set may be too large. Consider a shared read-only
   `Runtime` skeleton + per-actor mutable overlay. Out of scope
   for MVP but worth measuring.
2. **`tokio::task_local!` vs custom task storage.** Tokio's
   task-local has a HashMap lookup cost. For the region stack
   (accessed per `cons-in-region`), a direct field on a custom
   `Actor` struct passed through every call might be cheaper.
   Defer until benchmarked.
3. **`worker_threads(num_cpus)` on systems with hyperthreading.**
   `num_cpus::get()` includes HT siblings. May want
   `num_cpus::get_physical()` for compute-bound workloads.
   Make it configurable via env var.
