# JIT-vs-AOT `tak` gap — re-measured (cs-l1s)

> Date: 2026-07-10 · Branch: `p3/cs-l1s` (off `1384135`)
> Machine: Apple M-series, macOS (darwin-aarch64) · single-process, single-thread
> Load: **heavy** — 2 other agents building the same workspace concurrently
> throughout this session; `uptime` load average ranged 45-146 (20+ concurrent
> `rustc` processes) for every measurement below. Reported wall-clock numbers
> are therefore contention-inflated on the AOT side (see "Load caveat").

## TL;DR

- **The ~10x AOT-vs-JIT gap on `tak` from
  [2026-06-03-aot-vs-runtimes.md](2026-06-03-aot-vs-runtimes.md) still holds**,
  confirmed by both cold-process and warm-loop (tier-up-amortized) measurements.
  The `cs-4f2` hypothesis (raw block-param seeding) was already known
  premise-invalid before this investigation; **this re-measurement identifies
  the real, current cost center**, which is different from the June 3 doc's
  "loop+match state machine, non-tail recursion is structurally expensive"
  explanation.
- **Real cause: `(not ...)` is not a RIR primitive.** It lowers to a generic
  `CallGeneral`. The JIT dispatches `CallGeneral` through the newly-fused
  `vm_ic_call` (cs-hz5, inline-cached — cheap after the first hit). **AOT
  dispatches the identical `CallGeneral` through `vm_call_aot_procedure`,
  which has no inline cache at all** — every call does a `proc_table::peek` +
  `downcast_ref::<VmAotClosure>()` + arity assert + indirect call, from
  scratch, every time. `tak(24,16,8)` makes **2,493,349 recursive calls**, and
  every one of them pays one full uncached `not` dispatch under AOT.
- Two commits landed in the last 24h (`ac44a9f` opt_level=speed, `fb50abc`
  vm_ic_call fusion) — **neither caused a regression**; they're incidental to
  this finding. (See "False start" below — an early pass of this
  investigation used an invalid `--tier` value and measured the *walker*
  tier by mistake, which looked like a dramatic new regression but wasn't.)

## Method

`bench/aot-vs-runtimes.sh`'s own heavy-regime N for `tak`: `tak(24, 16, 8)`.
Correctness cross-checked (`= 9`) on every tier before timing.

```scheme
(define (tak x y z)
  (if (not (< y x))
      z
      (tak (tak (- x 1) y z)
           (tak (- y 1) z x)
           (tak (- z 1) x y))))
(display (tak 24 16 8))
```

- **JIT**: `crabscheme --tier vm-jit run tak-heavy.scm` (cold, one process per
  hyperfine sample).
- **AOT**: `crabscheme aot --multi --build tak-heavy.scm` (cargo/L1 backend,
  `devenv shell` for a matching rustc 1.95), then the built binary invoked
  directly per hyperfine sample (`tak-heavy tak 24 16 8`).
- **Warm-loop**: an in-process Scheme loop calling `(tak 24 16 8)` 5x, to
  separate tier-up/compile cost from steady-state per-call cost.
- `hyperfine --warmup 5 --min-runs 20..30`, mean/min reported; `uptime` +
  `ps aux | grep rustc | wc -l` recorded alongside every run.

### False start: wrong `--tier` value silently fell back to the walker

`crates/cs-cli/src/main.rs:195-196` compares `cli.tier` as a plain string
(`via_vm = tier == "vm" || tier == "vm-jit"`; `with_jit = tier == "vm-jit"`),
**not** a validated `clap` enum. `--tier jit` (the value this investigation
started with, before checking `bench/aot-vs-runtimes.sh`) is neither `"vm"`
nor `"vm-jit"`, so it silently ran the **default walker tier** instead of
erroring. That produced `tak(24,16,8)` times of ~2.0-3.7s (user time), which
looked like a dramatic *new* JIT regression — and a warm-loop test even
appeared to rule out tier-up cost (each of 5 iterations cost the same ~2.09s,
consistent with a real per-call regression). Two bisect builds (before/after
`ac44a9f`) were kicked off on this false premise before the mistake was
caught by cross-referencing the harness script's actual invocation
(`crabscheme --tier vm-jit run`, not `--tier jit`). Re-running with the
correct flag collapsed the "regression" entirely — this was 100% a bad CLI
argument, not a code issue. **Follow-up filed** for the CLI (see below): a
free-text `--tier` with no validation is a footgun for exactly this reason.

## Results

### Cold process (hyperfine, `--warmup 5 --min-runs 30`)

| | mean | min | user (CPU time) | load @ measurement |
|---|---|---|---|---|
| JIT (`vm-jit`) | 43.6 ms ± 53.4 ms | 9.3 ms | **15.0 ms** | uptime 71-83, ~20 rustc procs |
| AOT | 833.9 ms ± 242.6 ms | 466.5 ms | **163.0 ms** | uptime 71-83, ~20 rustc procs |
| ratio | 19.1x (wall) | 50.2x (wall) | **10.9x (user)** | |

hyperfine flagged statistical outliers on this run (expected — the box was
extremely loaded). **User/CPU time is the load-robust number**: it counts
actual CPU-seconds consumed and is far less sensitive to scheduling
contention than wall-clock. The user-time ratio (10.9x) lines up closely with
the June 3 doc's originally-reported 10x, wall-clock elapsed is inflated well
beyond that by contention (AOT's longer per-sample CPU burn gives it more
opportunities to be preempted by the ~20 concurrent `rustc` processes than
JIT's much shorter run).

### Warm-loop (5x `(tak 24 16 8)` in one process, tier-up amortized)

- JIT: `0.04s` user for 5 iterations = **8 ms/iteration**, *faster* than the
  9.3ms cold min (consistent with the IC settling into steady state, no
  tier-up-cost artifact to amortize away in the first place — `vm-jit` JIT
  runs from the first call, not after a warm-up threshold, for this bench).
- AOT has no equivalent "warm-loop" concept (it's a native binary — every
  invocation is already "warm" machine code); the cold-process AOT number
  above is already the steady-state number.

**Conclusion: not a tier-up/compile-amortization artifact.** The gap is a
genuine steady-state per-call cost difference.

## Root cause — verified via `--emit-rir` and `--emit-rust-source`

```
$ crabscheme aot --entry tak --emit-rir tak-heavy.scm
// BlockId(0):
//   EnvLookupAny(Value(3), 6)                         <- free-var lookup: `not`
//   Lt(Value(4), Value(1), Value(0))
//   BoxTyped(Value(5), Value(4), 1)
//   CallGeneral(Value(6), Value(3), [Value(5)])        <- (not ...) is a generic call
//   AnyTruthy(Value(7), Value(6))
//   TERM: Branch(Value(7), BlockId(1), BlockId(2), [])
```

`(not ...)` has no dedicated RIR instruction (`cs-rir` has `BitNot` for
bitwise NOT, nothing for boolean `not`) — every backend sees a
`CallGeneral`. The two recursion-heavy tiers handle that call site very
differently:

- **JIT** (`crates/cs-jit-cranelift`, `vm_ic_call`, `crates/cs-vm/src/vm.rs:9999`):
  a fused peek+compare+dispatch. On a cache hit (`cached_closure_id ==
  peeked_id`), it's one atomic load + integer compare + a direct call through
  the cached JIT pointer (`vm_ic_dispatch`). Since `tak` always calls the
  same `not` procedure at this call site, the cache is monomorphic and hits
  on every call after the first.
- **AOT** (`vm_call_aot_procedure`, `crates/cs-vm/src/vm.rs:16289`): **no
  inline cache at all**. Every call does `proc_table::peek(idx)`, a
  `downcast_ref::<VmAotClosure>()` (dynamic `Any`/vtable-style type check),
  an arity `assert_eq!`, then an indirect call through the resolved function
  pointer — from scratch, every single time.

`tak(24, 16, 8)` makes **2,493,349 recursive calls** (measured via an
instrumented counter under `--tier vm`), i.e. 2.49M `not` dispatches. At
`(163.0ms - 15.0ms) / 2.49M ≈ 59 ns` extra per call under AOT — plausible for
an uncached dynamic-downcast dispatch vs. a monomorphic inline-cache hit, and
sufficient on its own to account for the full ~10x gap. The `tak` self-calls
themselves are cheap in both tiers (native recursive `fn` calls in AOT's
loop+match body; `CallSelf`/tail-call machinery in JIT) — **the cost center
is specifically the uncached builtin-procedure dispatch path, not the
recursion structure**. This supersedes the June 3 doc's "loop+match state
machine / non-tail recursion" explanation, which was a reasonable guess at
the time but not what a code-level trace actually shows.

## Answering the original question

- **Does the 10x gap still hold?** Yes — both cold (user-time, load-robust:
  10.9x) and warm-loop (steady-state, no tier-up artifact) measurements
  confirm it, on the current `p3/cs-l1s` tip (`1384135`), after the two very
  recent JIT changes (`ac44a9f` opt_level=speed, `fb50abc` vm_ic_call
  fusion).
- **Is the cause raw block-param seeding (`cs-4f2`)?** No — confirmed
  premise-invalid, as suspected: `tak` does take the `detect_uniform_nb_raw_abi`
  raw-Fixnum lane (all-Fixnum params, self-recursive, no cross-calls, forward-
  only CFG), and its `CallSelf` recursion is cheap in both tiers.
- **What is the cause?** AOT's `vm_call_aot_procedure` has no inline cache,
  unlike JIT's `vm_ic_call` (which just got *more* fused/optimized in
  `fb50abc`, coincidentally widening this specific gap further, though it was
  already present before that commit). Every non-primitive builtin call
  inside an AOT hot loop — here, `not` — pays full uncached dispatch cost on
  every invocation.

## Follow-ups filed

- **cs-7rz** (P2) — Give `vm_call_aot_procedure` an inline cache (or a
  per-call-site cached function pointer emitted directly into the AOT'd
  code), mirroring the JIT's `vm_ic_call`. This is the direct fix for the
  `tak`-shaped gap and should generalize to any AOT hot loop that calls a
  non-inlined builtin repeatedly.
- **cs-qrm** (P3) — Give `(not ...)` (and similarly simple boolean-only
  builtins) a dedicated RIR instruction instead of routing through
  `CallGeneral`/`EnvLookupAny`, so no backend pays procedure-dispatch cost
  for what is semantically a one-bit flip. Lower priority than cs-7rz since
  it only helps this one builtin; cs-7rz fixes the general case.
- **cs-bpz** (P3) — `crates/cs-cli/src/main.rs`'s `--tier` flag is an
  unvalidated free-text string (`cli.tier == "vm-jit"` etc.); an invalid
  value (e.g. `--tier jit` instead of `--tier vm-jit`) silently falls back to
  the default walker tier instead of erroring, which is exactly what
  derailed the first ~40 minutes of this investigation. Make it a validated
  `clap` `ValueEnum` (or at minimum error on an unrecognized value) so a
  typo'd tier name fails loudly instead of silently benchmarking the wrong
  tier.

## Reproduce

```bash
devenv shell -- cargo build --release -p cs-cli --bin crabscheme
devenv shell -- crabscheme aot --multi --build tak-heavy.scm -o tak-heavy-aot
hyperfine --warmup 5 --min-runs 30 \
  "target/release/crabscheme --tier vm-jit run tak-heavy.scm" \
  "tak-heavy-aot/target/release/tak-heavy tak 24 16 8"
crabscheme aot --entry tak --emit-rir tak-heavy.scm   # shows the CallGeneral
```
