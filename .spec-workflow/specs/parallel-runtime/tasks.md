# Parallel Runtime — Tasks

> Status: **Draft**.
> Companion: `requirements.md`, `design.md`.

Six iteration tracks (one per design change C1–C6) split into
small, independently-mergeable iterations. Each iteration has a
specific acceptance gate. Order matters: C2 needs C1's
runtime-installed yield hook; C3 needs C1 (no async actors → no
task-locals to use); C6 needs C1–C4 to have anything to bench.

## Track C1 — Async actor bodies

### C1.1 — Add async API alongside sync (no removal)

- [ ] Add `Actor::recv_async(&mut self) -> impl Future<Output = Result<Message, RecvError>>`
- [ ] Add `ActorSystem::spawn_async<F, Fut>(&self, body: F)` where
  `F: FnOnce(Actor) -> Fut + Send + 'static`, `Fut: Future + Send`
- [ ] Existing sync `Actor::receive` and `ActorSystem::spawn` stay,
  marked with a `// LEGACY` comment block pointing at the async
  variants
- [ ] Unit test: 1k actors spawned via `spawn_async`, each receives
  and replies once, total round-trip
- [ ] **Gate:** existing cs-actor test suite (the 4 tests in
  `crates/cs-actor/tests/`) stays green AND the new async test
  passes

### C1.2 — Migrate cs-runtime Scheme builtins to async

- [ ] `b_beam_spawn` (`crates/cs-runtime/src/builtins/beam.rs`)
  switches from `ActorSystem::spawn` to `ActorSystem::spawn_async`
- [ ] `primop_raw_receive` switches from `inbox.blocking_recv` to
  `inbox.recv().await`
- [ ] Timed receive replaces the `try_recv + thread::sleep(1ms)`
  spin loop with `tokio::time::timeout(dur, inbox.recv()).await`
- [ ] **Gate:** `cargo test -p cs-runtime --features actor --test 'beam_*'`
  green; the soak test in `beam_verification.rs` runs cleanly

### C1.3 — Worker thread count + capacity

- [ ] Change `Runtime::Builder::worker_threads(1)` →
  `worker_threads(num_cpus::get_physical())` in `ActorSystem::new`
- [ ] Add `CRABSCHEME_ACTOR_WORKERS` env var override (numeric or
  "physical" / "logical" keywords)
- [ ] Add `CRABSCHEME_ACTOR_MAX_BLOCKING` env var (default keeps
  4096 for now; this is sync-path safety net only)
- [ ] **Gate:** smoke test `bench/parallel-runtime/schemes/spawn-1m.scm`
  completes in < 30s

### C1.4 — Deprecate sync API

- [ ] Add `#[deprecated(note = "use spawn_async; see parallel-runtime spec")]`
  on `ActorSystem::spawn` and `Actor::receive`
- [ ] **Gate:** workspace `cargo build` compiles with `-W deprecated`,
  no new warnings in our own code (only in tests if any)

## Track C2 — Reduction-driven yield

### C2.1 — Yield hook in cs-vm

- [ ] Add `thread_local! { static YIELD_HOOK: Cell<Option<fn()>> }`
  in `cs-vm/src/vm.rs`
- [ ] Add `pub fn install_yield_hook(f: fn())` API
- [ ] In `run_dispatch`, after every N (default 2000) instructions,
  decrement a counter; on zero, call hook if installed, reset
  counter
- [ ] `REDUCTION_BUDGET` is a `thread_local! { static BUDGET: Cell<u32> }`
  configurable at runtime (so tests can shrink to force preemption)
- [ ] **Gate:** unit test in cs-vm that the hook fires after exactly
  N bytecode ops

### C2.2 — Wire hook from cs-runtime to tokio

- [ ] `Runtime::install_actor_runtime(&mut self, ...)` (called from
  `ActorSystem` at task spawn) registers a yield hook:
  ```rust
  cs_vm::vm::install_yield_hook(|| {
      // Park the current task until tokio re-schedules it.
      // Since the hook is `fn()` not async, we use a
      // tokio::runtime::Handle::current().block_on(yield_now())
      // — sound here because we're already inside a tokio task.
      let _ = tokio::runtime::Handle::try_current()
          .map(|h| h.block_on(tokio::task::yield_now()));
  });
  ```
  *(Note: the block_on-inside-task pattern is normally a deadlock
  hazard. yield_now is the exception — it's a no-op poll that
  always returns Poll::Ready after one yield. Safe.)*
- [ ] **Gate:** integration test — spawn two actors, one runs a
  CPU-bound loop, other sends + expects timely receive. Verify
  the second actor's message is processed within 100ms.

### C2.3 — Tests for starvation prevention

- [ ] `crates/cs-runtime/tests/parallel_starvation.rs` — adversarial
  test: 100 CPU-bound actors + 1 "responder" actor. Verify
  responder gets a message within 1s under contention.
- [ ] **Gate:** test passes deterministically (10 runs, 0 timeouts)

## Track C3 — Region stack as task-local

### C3.1 — Dual-stack region storage

- [ ] Add `tokio::task_local! { static REGION_STACK_TASK: RefCell<Vec<Rc<Region>>>; }`
  in `crates/cs-runtime/src/regions.rs`
- [ ] Keep existing `thread_local!` as `REGION_STACK_TLS` (renamed)
- [ ] `RegionScope::enter` and `Drop` detect context (`Handle::try_current().is_ok()`
  + `REGION_STACK_TASK::try_with().is_ok()`) and push/pop the
  matching stack
- [ ] `current_region()` checks task-local first, falls back to TLS
- [ ] **Gate:** unit tests prove
  - Non-actor context still works (TLS path)
  - Actor context uses task-local
  - Mixed (actor that nests a direct eval) consistently uses one
    stack per RegionScope lifetime

### C3.2 — `LIVE_REGION_IDS` task-local mirror

- [ ] Same dual-stack pattern for `LIVE_REGION_IDS` in `cs-gc/src/region.rs`
- [ ] `assert_region_live` checks task-local first, then TLS
- [ ] **Gate:** the C3 acceptance test
  (`bench/parallel-runtime/schemes/region-actor.scm`) passes —
  actor opens region, awaits, resumes on different worker,
  continues `cons-in-region` without panic

### C3.3 — Documentation + diagnostic message

- [ ] Update `crates/cs-runtime/src/regions.rs` module doc to call
  out the dual-stack pattern
- [ ] If `current_region()` finds neither stack populated, the error
  message points at both possible causes ("no `(with-region)` in
  scope" + "inside an actor body but task-local stack lookup failed")
- [ ] **Gate:** docs review

## Track C4 — Bacon-Rajan trial deletion

### C4.1 — `Slot<T>` color field

- [ ] Add `pub color: AtomicU8` to `Slot<T>` in `cs-gc/src/rc_only.rs`
- [ ] Define color constants: `BLACK = 0`, `GRAY = 1`, `WHITE = 2`,
  `PURPLE = 3` (PURPLE = "buffered as cycle root")
- [ ] All allocations init to BLACK
- [ ] Increment refcount sets BLACK; decrement (without reaching 0)
  sets PURPLE and adds to candidate buffer (existing
  cycle_registry)
- [ ] **Gate:** existing cs-gc tests pass (allocation hot path
  hasn't grown beyond an atomic store)

### C4.2 — Children-for-trial-deletion trait

- [ ] Add `pub trait CycleChildren { fn children(&self, visit: &mut dyn FnMut(&dyn AnyCycleNode)); }`
  in `cs-gc/src/cycle_registry.rs`
- [ ] Implement on `Pair`, `Vector<Value>`, `Hashtable`, `Promise`
  in cs-core
- [ ] **Gate:** unit test exercising children traversal for each
  type returns the right node count

### C4.3 — `mark_gray` + `scan_gray` + `collect_white` phases

- [ ] Implement the three Bacon-Rajan walk phases in
  `cs-gc/src/cycle_collector.rs` (new file)
- [ ] `mark_gray(root)`: recursively decrement child refcounts,
  set node colors GRAY
- [ ] `scan_gray(root)`: if refcount > 0, restore via `scan_black`
  (re-increment recursively, set BLACK); else stay GRAY and
  recurse
- [ ] `collect_white(root)`: walk; if WHITE, reclaim
- [ ] **Gate:** unit test: 100-pair cycle, no external refs. Call
  the 3 phases. Assert all 100 reclaimed.

### C4.4 — Wire into `run_sweep`

- [ ] `run_sweep` (in cs-gc/src/cycle_registry.rs) replaces the
  current "try_break_cycle per candidate" loop with the 3-phase
  walk
- [ ] Add `cycle_collector::stats()` returning
  `{candidates_checked, cycles_collected, time_ms}`
- [ ] Surface via `(gc-stats)` Scheme builtin
- [ ] **Gate:** soak test — `bench/parallel-runtime/schemes/cycle-n-pair.scm`
  creates 10000 random cycles (varying size 3–20 pairs), `(collect)`,
  assert `gc-stats` shows all reclaimed

### C4.5 — Sweep yields under reduction pressure

- [ ] If C2's yield hook is installed, sweep checks reduction
  budget after each candidate and yields if expired
- [ ] **Gate:** integration test — 100k candidates in registry,
  trigger sweep from one actor while another actor expects
  message receipt within 100ms. Both succeed.

## Track C5 — Region/contract hardening

### C5.1 — Hard `downgrade(region-handle)`

- [ ] Replace `debug_assert!` in `Gc::downgrade` with `panic!`
- [ ] Panic message includes region_id and points at `to_rc_deep`
- [ ] Workspace grep: confirm no caller depends on the silent
  behavior. Audit the one cycle-break call site already guards
  against region inputs (`Pair::break_*_cycle` returns false for
  region).
- [ ] **Gate:** unit test that `Gc::downgrade(region_gc)` panics
  with the expected message

### C5.2 — `(gc-allocator v)` Scheme builtin

- [ ] Add to `syms_builtins()` in `crates/cs-runtime/src/builtins/mod.rs`
- [ ] Returns `'rc | 'region | 'traced` based on `Gc::repr`
- [ ] **Gate:** unit test exercising all three return values

### C5.3 — Contract boundary refuses region values

- [ ] Update `__apply-range` (and `__apply-domain` for
  callee→caller flow) in `lib/contract/contract.scm` to check
  `(gc-allocator result)`; reject `'region` with a clear
  `&contract` condition
- [ ] **Gate:** new test in `crates/cs-runtime/tests/parallel_runtime_contract_region.rs`:
  define a contracted proc that returns a `cons-in-region` result;
  expect contract failure (NOT silent corruption)

## Track C6 — Bench harness

### C6.1 — Skeleton + runner

- [ ] Copy `bench/realworld/runner.sh` → `bench/parallel-runtime/runner.sh`
- [ ] Adjust default measure budget (longer — these benches need
  60s+)
- [ ] **Gate:** runner.sh executes one test successfully

### C6.2 — Acceptance benches

- [ ] `schemes/spawn-1m.scm` (G1)
- [ ] `schemes/echo-10m.scm` (G1)
- [ ] `schemes/cpu-bound-vs-responder.scm` (G2)
- [ ] `schemes/region-actor.scm` (G3)
- [ ] `schemes/cycle-n-pair.scm` (G4)
- [ ] `schemes/long-soak.scm` (G6 — 1h mixed workload, leak check)
- [ ] **Gate:** each bench runs to completion + meets the headline
  numbers from `requirements.md`

### C6.3 — CI integration

- [ ] Add `.github/workflows/parallel-runtime-bench.yml` that runs
  the suite nightly + on tagged releases
- [ ] Soak bench excluded from PR CI (takes too long); runs on
  release/nightly only
- [ ] **Gate:** CI green on first push

## Dependency graph

```
C1.1 ──┬─→ C1.2 ──→ C1.3 ──→ C1.4
       │
       └─→ C2.2

C2.1 ──→ C2.2 ──→ C2.3
                    │
                    └─────────→ C4.5

C1.3 ──→ C3.1 ──→ C3.2 ──→ C3.3
                            │
                            └────→ C6.2 region-actor

C4.1 ──→ C4.2 ──→ C4.3 ──→ C4.4 ──→ C4.5
                                        │
                                        └→ C6.2 cycle-n-pair

C5.1 ─────┐
C5.2 ─────┼──→ C5.3
          │
          └────────────────→ C6.2 (uses gc-allocator in benches)

C1.4, C2.3, C3.3, C4.5, C5.3 all ──→ C6.3
```

## Milestone gates

The spec ships in three observable milestones:

### M1 — async actors (C1 complete)

After C1.1–C1.4 land, 1M+ actor capability ships behind the
`actor` feature. C2 yield-hook is wired but not yet exercised in
production. Benchmarks show spawn-1m passes; long-running CPU-bound
workloads might starve (without C2).

### M2 — preemption + region migration (C2 + C3 complete)

After M1 + C2 + C3 land, the full async actor story is correct:
no starvation, regions survive task migration. Bench gates G1, G2,
G3 all pass.

### M3 — cycle completeness + ergonomics (C4 + C5 complete)

After M2 + C4 + C5 land, the memory model is consistent (no
silent leaks, no silent footguns). Bench gate G4 passes. C6.3
ships the CI pipeline.

Each milestone is independently mergeable. Recommend landing as
three separate PRs in sequence.
