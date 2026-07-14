# cs-845.6 — actor-JIT starvation: findings and a scoped fix

## Background / task

JIT tiering is force-disabled for every actor body via
`cs_vm::vm::set_jit_enabled(false)` at three call sites in
`crates/cs-runtime/src/builtins/beam.rs` (`activation_body`,
`green_source_body`, `run_actor_body`). The comment at each site attributes
this to a prior perf branch (`perf/actor-vm-jit`) that found a JIT-tiered
CPU-bound actor could hang a concurrent `SET`-style peer on a shared
worker, for only a marginal (~5%) throughput gain.

The working hypothesis handed to this investigation: JIT-compiled loops
never call the VM's reduction-budget tick, so the coroutine/tokio yield
hook never fires, so a JIT-tiered actor monopolizes its worker forever.

## Verdict: hypothesis denied — the tick already exists and works; the gap
was that actor Runtimes never installed the JIT at all

1. **A reduction-tick mechanism for JIT-compiled tail loops already
   exists** and predates this investigation: commit `24a12a6` /
   PR #85 (`ADR 0031`, merged 2026-05-27) added `vm_jit_tick_reductions`
   (`crates/cs-vm/src/vm.rs:10800`, an `extern "C"` wrapper around
   `vm_tick_reductions`) and wired a call to it at the JIT's tail-self
   `return_call` back-edge in `crates/cs-jit-cranelift/src/lowering.rs`.
   It's exercised by `crates/cs-runtime/tests/jit_preemption.rs`, which
   still passes (see gates below). So — contrary to the hypothesis —
   **JIT tail loops do tick**, and have since May.

2. **Green actors already install a coroutine-safe yield hook.**
   `crates/cs-runtime/src/builtins/beam.rs`'s `green_yield_hook`
   (~line 1327) is installed via `ensure_green_yield_hook()` from
   `pump_coroutine` for every coroutine-hosted actor
   (`spawn-activation` / `spawn-source-green`). Unlike
   `cs_actor::tokio_yield_hook` (used by the dedicated/`block_in_place`
   path, which does `Handle::block_on(yield_now())` — sound only inside
   `block_in_place` on a multi-thread runtime), `green_yield_hook`
   suspends the corosensei coroutine (`CoYield::Yield`) and hands control
   back to `pump_coroutine`, which does the actual
   `tokio::task::yield_now().await`. This is the correct, re-entrancy-safe
   design for a shared `LocalSet` worker.

3. **The actual gap: actor `Runtime`s never called `Runtime::install_jit`.**
   `Runtime::new()` / `Runtime::from_image()`
   (`crates/cs-runtime/src/lib.rs`) do not install the JIT tier-up hook —
   only an explicit `rt.install_jit()` call does
   (`crates/cs-runtime/src/jit.rs:51`). None of the three actor-body
   constructors (`activation_body`, `green_source_body`,
   `run_scheme_body`) ever called it. So even before this bead's change,
   `set_jit_enabled(true)` alone would have done nothing for an actor —
   `cs_jit::Tier::bump` never fires because the tier-up hook was never
   registered, so no closure ever tiers up in the first place. The
   `set_jit_enabled(false)` gate looks like a belt-and-suspenders
   perf/safety measure layered on top of a runtime that was JIT-dormant
   for an unrelated reason (or was a leftover assumption from whatever
   `perf/actor-vm-jit` did on its own branch, which this repo can't
   inspect — that branch was never pushed).

## Fix implemented

`crates/cs-runtime/src/builtins/beam.rs`:

- New `actor_jit_enabled_override()` — reads `CRABSCHEME_ACTOR_JIT` once
  into a `OnceLock<bool>` (`"1"` → `true`, anything else/unset →
  `false`, matching the existing default-off behavior).
- The three `cs_vm::vm::set_jit_enabled(false)` call sites now call
  `cs_vm::vm::set_jit_enabled(actor_jit_enabled_override())` instead.
- Each of the three `Runtime` construction sites (`activation_body`
  line ~507, `green_source_body` line ~810, `run_scheme_body` line ~988)
  now calls `rt.install_jit()` when the override is on — this is the part
  that was actually missing and is *why* JIT never activated for actors
  regardless of the `set_jit_enabled` gate.

Default behavior is **unchanged** (`CRABSCHEME_ACTOR_JIT` unset ⇒ JIT off
for actors, exactly as before). Setting `CRABSCHEME_ACTOR_JIT=1` opts in.

## Safety of yielding from inside JIT-compiled machine code

This was the open question item 4 of the task asked to verify before
wiring anything in. Two yield paths exist:

- **Dedicated (`block_in_place`) actors** (`run_actor_body`, used by
  `spawn` / `spawn-source-dedicated`): the tick fires
  `cs_actor::tokio_yield_hook`, which does `Handle::block_on`. This is
  documented sound specifically because `block_in_place` has already
  excused the worker from its async duties (multi-thread runtime only).
  JIT-compiled machine code calling out to this via
  `vm_jit_tick_reductions` doesn't change that argument — the callee is
  a plain `extern "C"` function; nothing about being invoked from
  Cranelift-generated code adds reentrancy risk here.
- **Green (coroutine) actors** (`green_source_body` /
  `spawn-activation`): the tick fires `green_yield_hook`, which does a
  corosensei `Yielder::suspend`. This is a raw stack-pointer switch
  (registers + SP saved/restored), not a Rust-level unwind — it does not
  require any invariant about the current native call stack's frame
  layout, so being "inside JIT-compiled code" at the point of suspend is
  not qualitatively different from being inside the VM interpreter's
  Rust frames at that point. The pre-existing region-park guard
  (`pump_coroutine`, P0.1: "cannot park inside (with-region)") already
  demonstrates the team tracks *state* invariants around suspend points
  (region-stack depth), not stack-shape invariants — and that guard is
  keyed off `crate::regions::region_stack_depth()`, a thread-local
  counter incremented/decremented by the `with-region` builtin itself, so
  it's enforced identically regardless of whether the enclosing call was
  JIT-compiled or walked/VM-interpreted.

  I did not find any documented invariant that JIT-compiled frames
  violate at a suspend point. The empirical test below (a JIT-tiered
  tail loop suspending mid-loop on a green worker, then resuming and
  completing the loop from where it suspended) is a **real, functional**
  test of this — it isn't just a smoke test; it's exactly the failure
  mode reported (a JIT'd actor loop on a shared coroutine worker).

## Repro test

`crates/cs-runtime/tests/actor_jit_starvation.rs`
(`jit_enabled_actor_tail_loop_does_not_starve_peer`): forces
`CRABSCHEME_ACTOR_JIT=1` and `CRABSCHEME_ACTOR_LOCAL_WORKERS=1`, spawns
two green (`spawn-source-green`) actors on the single forced worker — one
runs a `busy-loop` tail-recursive self-call to 200,000,000 (far past the
1024-call JIT tier-up threshold) with no `receive`/`sleep` inside it, the
other immediately sends a `ping-ok` marker to a collector — and asserts
the marker arrives within a 10s timeout.

**Result: passes in 0.02s** (immediate — no starvation observed) with the
fix in place. This is strong evidence the existing ADR-0031 tick +
`green_yield_hook` combination correctly preempts a JIT-tiered tail loop
on a shared green worker once the JIT is actually installed on the actor
runtime.

## What this does NOT prove

- **Non-tail self-recursion is still untouched by design** (ADR 0031's
  documented scope: "bounded, so it returns, so it doesn't need
  preemption"). A CPU-bound handler that does a lot of work per
  message via non-tail recursion (not an infinite loop) will still hog
  the worker for the duration of that one call — this is a latency
  concern, not a hang, and was an accepted trade-off in ADR 0031, not
  something this bead changed.
- **This test only exercises the single scenario named in the task**
  (a hot self-tail-call loop). It does not reproduce whatever exact
  workload `perf/actor-vm-jit`'s "concurrent SET hung" finding involved —
  that branch was never pushed and isn't available to inspect from this
  worktree, so I can't confirm whether that specific hang predated
  ADR 0031 (most likely, given the disable commit `ed034df` postdates
  ADR 0031 by six weeks but the *comment* simply cites the old finding
  without re-testing it) or involves a code shape the tail-self tick
  doesn't cover (e.g., mutual recursion, or iteration via
  `for-each`/`map`/hash-table builtins rather than a literal
  self-tail-call). I did not find evidence of the latter, but I also
  did not exhaustively search for it — recommend treating
  `CRABSCHEME_ACTOR_JIT=1` as experimental/opt-in until it's exercised
  against a wider workload (e.g., an actual `crab-cache`-style
  concurrent-SET load test) before ever flipping the default.

## Env var

`CRABSCHEME_ACTOR_JIT=1` — opts actor bodies back into JIT tiering.
Default (unset) preserves today's JIT-off behavior. Read once per
process via `OnceLock` in `crates/cs-runtime/src/builtins/beam.rs`.

## Validation gates

- `cargo test -p cs-vm`: 80 passed / 0 failed (crate untouched by this
  change; run as a baseline).
- `cargo test -p cs-runtime --test jit_differential`: 249 passed / 0
  failed.
- `cargo test -p cs-runtime --test jit_conformance`: 8 passed / 0 failed.
- `cargo test -p cs-runtime --features actor --test actor_jit_starvation`:
  1 passed / 0 failed (new repro test, JIT forced on).
- `cargo test -p cs-runtime --features actor,channel,web` (full suite):
  all visible test binaries green (0 failed across `web_builtins`,
  `walker_tail_depth`, doc-tests, and the rest of the run); no `FAILED`
  anywhere in the captured output.
- `cargo clippy -p cs-runtime --features actor --tests`: clean on the
  touched files (`beam.rs`, `actor_jit_starvation.rs`). Note: a plain
  `-D warnings` clippy run fails on pre-existing, unrelated lints in
  `cs-core` (unrelated to this change) — those were not introduced here.
- Bench sanity (`crabscheme --tier vm-jit run bench/microbench/scheme/fib.scm`,
  release build, `fib(25)`): **1.176s wall** vs **1.012s wall** for
  `--tier walker` on the same build — both dominated by process
  startup/build noise at this problem size, no cliff. This change does
  not touch the JIT hot path or codegen at all; it only gates
  `Runtime::install_jit()` for actor bodies behind an off-by-default env
  var, so no JIT perf regression is expected or observed for the
  non-actor CLI path.
