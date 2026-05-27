# ADR 0031: JIT reduction-tick at tail-self back-edges (actor preemption)

> Status: Accepted
> Date: 2026-05-26
> Authors: crab-scheme contributors

## Context

Issue #30 (B3 second half) asks for an "automatic yield hook (preempt
without explicit `(yield)`)" and a work-stealing scheduler. Scoping the
first part revealed that automatic, reduction-based preemption **already
works at the bytecode-VM level**: `vm_tick_reductions()` runs on every
dispatch-loop instruction (`cs-vm/src/vm.rs`), counts toward
`VM_REDUCTION_BUDGET` (default 2000), and fires the installed
`VM_YIELD_HOOK` when the budget is hit. For actors, that hook
(`cs_actor::tokio_yield_hook`) performs a one-tick `tokio::task::yield_now`,
so a CPU-bound actor yields the worker every ~2000 ops.

The gap: **JIT-compiled native code bypasses the dispatch loop**, so it
never calls `vm_tick_reductions`. Once a hot tail loop tiered up (after
1024 calls), it stopped ticking entirely and could hold its worker thread
for the full duration of the loop — including *forever* for an unbounded
busy loop. This is the concrete, contained piece of #30's "automatic
yield" deliverable; the work-stealing scheduler is deferred to a later
iter.

## Decision

Emit a reduction tick at the **JIT tail-self back-edge** — the single
`return_call(self_fn, …)` site in `lower_*_uniform_nb`
(`cs-jit-cranelift/src/lowering.rs`). A self-recursive tail call is the
JIT's loop construct, so ticking there counts one reduction per loop
iteration (the BEAM "reduction = function call" model). At budget the
existing yield hook fires — a one-tick yield inside an actor, a no-op
outside one.

Mechanics:
- A new C-ABI wrapper `vm_jit_tick_reductions()` (cs-vm) delegates to
  `vm_tick_reductions()`. The wrapper keeps the hot VM-loop fn at the
  plain Rust ABI (preserving its `#[inline]`) while giving Cranelift a
  callable `extern "C"` symbol.
- The helper is declared for both backends but the **call is gated to the
  JIT** (`NbHelpers::tick_reductions: Option<FuncRef>`, `None` on the
  object/AOT backend via an `is_aot` flag threaded into
  `finish_construction`). AOT binaries have no actor scheduler, and gating
  keeps `vm_jit_tick_reductions` out of the AOT object so the
  toolchain-free L3 `cc` link against `libcs_aot_rt.a` is unaffected.

### Scope: tail-self only (not non-tail recursion)

The tick lands **only** on the tail-self `return_call`, not on the
general (non-tail) `CallSelf` path. Non-tail recursion (e.g. `fib`,
`fact`) is bounded — it returns — so it does not starve a worker and does
not need preemption. Leaving it untouched keeps the perf-sensitive
non-tail microbenchmarks at zero overhead: `fib(32)` under `--tier vm-jit`
measures **12.6 ms before and after** this change. Tail loops (the actual
starvation case, including pure-fixnum busy loops, which take the same
`return_call` site under the raw-ABI lane) pay one cheap tick per
iteration — a thread-local increment + budget compare, with the hook call
only at budget.

## Consequences

### Positive
- A JIT-compiled hot tail loop now preempts: it ticks reductions and
  yields at budget, so a CPU-bound JIT'd actor no longer holds its worker
  thread. Closes the "automatic yield" half of #30 for JIT code.
- Non-tail recursion and AOT are untouched (no perf or link impact).

### Negative / limitations
- Tail-recursive loops pay a small per-iteration tick (a void helper call
  + TLS increment). Negligible for real actor loops (which do real work
  per iteration) and bounded benchmarks; it is the cost of correct
  preemption. A future iter could amortize it (tick every K iterations) if
  a tail-loop-heavy workload shows measurable overhead.
- The work-stealing scheduler (the other half of #30) is still deferred —
  actors currently ride tokio's scheduler. That is a separate iter with
  its own design fork (ride tokio vs custom BEAM scheduler).

## Testing
- `jit_preemption` (new): a JIT-compiled tail loop ticks reductions
  (`yield_count` ≫ the ~20 a pre-tier-up-only VM would give); a non-tail
  recursion stays correct and untouched.
- No breaks: cs-runtime **1088/0** (default) and the `actor,channel,web`
  feature suite green; `jit_differential` 247/0 (tier parity),
  `jit_proper_tail_calls` 4/0 (the tick before `return_call` preserves
  TCO / constant stack), `jit_conformance`, scalar-replace, tier-up all
  green; **`aot-doctor` L1 + L3 green** (AOT gate verified); fmt + clippy
  clean (zero new warnings).
- Perf: `fib(32)` (non-tail) unchanged at 12.6 ms.

## References
- Issue #30 (B3 second half); the work-stealing scheduler remains open.
- `cs-vm/src/vm.rs` — `vm_tick_reductions`, `vm_jit_tick_reductions`,
  `VM_REDUCTION_BUDGET` / `VM_YIELD_HOOK`.
- `cs-actor/src/lib.rs` — `tokio_yield_hook`.
- `cs-jit-cranelift/src/lowering.rs` — the tail-self `return_call` site,
  `NbHelpers::tick_reductions`, `is_aot` gating.
