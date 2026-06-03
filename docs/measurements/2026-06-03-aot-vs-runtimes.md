# AOT vs. other runtimes — perf comparison

> Date: 2026-06-03 · Branch: `perf/aot-vs-runtimes` (off `685191b`)
> Harness: [`bench/aot-vs-runtimes.sh`](../../bench/aot-vs-runtimes.sh)
> Machine: Apple M-series, macOS (darwin-aarch64) · single-process, single-thread

This is the comparison `bench/microbench/run.sh` never had: the **AOT
native-binary tier put side-by-side with the walker / VM / JIT tiers,
`rustc -O`, and the mature comparison Schemes** (Chez, Guile, Gambit, plus
Gambit's own AOT path) — all on the *same* workload, timed with `hyperfine`.

## TL;DR

- **AOT correct coverage is 5/8 of the microbench corpus.** 6 of 8 compile;
  `mandelbrot` compiles but **aborts at runtime**; `spectral-norm` and
  `binary-trees` don't compile (`Inst::MakeClosure` — capturing nested
  lambdas / named-let loops).
- **AOT vs. JIT (both are cranelift):** workload-dependent. AOT *wins* on
  arithmetic recursion (`fib` 1.17×, `ack` 1.27×) by amortising the JIT's
  per-run compile. But it *loses* on `tak` (**10× slower** — recursion
  lowering), `alloc-stress` (1.65× — no escape opt) and `nqueens` (1.19×), so
  across the 5 AOT-capable benches the **cold-process JIT is ~1.7× faster on
  geomean**. At *small* N the picture flips: AOT's near-zero startup makes it
  the fastest crabscheme tier (see canonical regime).
- **AOT vs. native `rustc -O`:** Rust is the unreachable ceiling — 2× to
  ~170× faster — because it is LLVM-optimised and fully unboxed; AOT keeps
  crabscheme's NaN-boxed value representation.
- **AOT vs. Chez / Guile / Gambit:** mixed. Competitive-to-winning on the
  arithmetic kernels; well behind the production Schemes on the
  closure/flonum-heavy benches (a crabscheme-wide gap, not AOT-specific).

## Setup

| Component | Version / detail |
|---|---|
| crabscheme | `1.0.0-rc7`, release build, default features (`jit ffi-dynamic aot`) |
| AOT path | `crabscheme aot --multi --build` → cranelift-object → system `cc`, linked against the `cs-aot-rt` staticlib |
| rustc | 1.95.0 stable, `rustc -O` |
| Chez Scheme | 10.3.0 (`scheme --script`) |
| GNU Guile | 3.0.11 (`guile -q`) |
| Gambit | 4.9.5 — `gsi` (interpreter) and `gsc -dynamic` → native `.o1` (Gambit AOT) |
| timing | `hyperfine` (warmup + statistical runs), `--shell=none` |

Racket is **excluded**: it rejects a bare `.scm` with no `#lang` line, so it
errored on every bench (consistent with `microbench/run.sh`).

## Methodology

### One workload per bench, every runtime

The pre-existing `bench/aot-comparison.sh` timed AOT at an N that had drifted
out of sync with the `.scm` files — it ran `fib(35)` while the other tiers
(via `run.sh`) ran `fib(25)`, a ~123× workload mismatch, so its AOT column was
never comparable. **This harness drives every runtime — AOT binary, the three
crabscheme tiers, `rustc -O`, and each external Scheme — at one shared N per
bench**, and cross-checks the printed answer (last stdout token) across all of
them before timing.

### Two regimes, because they answer different questions

The process-startup floor on this machine is ~3-4 ms (`rustc -O` ~1.7 ms, the
AOT binary ~3.1 ms, `crabscheme run` ~4.2 ms). At the `.scm` files' own small
N the *compute* of the fast benches is sub-millisecond — so a single regime
would only measure startup. Hence:

- **`canonical`** — each bench at the `.scm` file's own (small) N. Wall time
  is **startup-inclusive**; this is the *small-program / cold-invocation*
  picture, where AOT's near-zero startup and the JIT's per-run compile cost
  dominate over compute. Includes the **walker** tier and `rustc -O`.
- **`heavy`** — N scaled up per bench so compute dominates the startup floor.
  This is the **steady-state codegen** comparison. The walker tier is dropped
  (30-40× the VM; minutes at heavy N).

### `jit` here is cold-process, on purpose

`hyperfine` restarts the process for every sample, so the **`jit` column pays
the cranelift compile + warm-up on every run** — it is *not* warm steady-state
(that is what `bench/microbench/warmup_curve.sh` measures in-process). This is
deliberate: it is exactly the "should I AOT-compile this program?" comparison.
AOT and the JIT share the *same* cranelift backend, so any steady-state
codegen quality is identical; AOT's entire value is **amortising the compile
to build time**. A win for AOT over the JIT here means "AOT skipped the
per-run compile," not "AOT generated faster code."

---

## Results

Times are `hyperfine` mean wall-clock in **milliseconds**. `—` = the runtime
could not run that bench (AOT compile/runtime failure, or the external Scheme
rejected the source).

### Canonical regime — startup-inclusive (small N)

| bench | aot | jit | vm | walker | rust-O | gambit-aot | chez | guile | gambit |
|---|---|---|---|---|---|---|---|---|---|
| fib | **3.7** | 6.3 | 25.2 | 142.8 | 2.0 | 6.5 | 34.0 | 14.8 | 15.8 |
| tak | 7.8 | 5.9 | 13.7 | 47.2 | 1.9 | 5.7 | 32.0 | 13.1 | 8.4 |
| ack | **4.2** | 7.0 | 19.3 | 120.6 | 2.2 | 5.8 | 33.0 | 13.8 | 12.9 |
| nqueens | 26.3 | 26.0 | 34.4 | 124.0 | 2.1 | 5.7 | 32.7 | 14.7 | 14.3 |
| mandelbrot | — | 13.9 | 76.5 | 396.2 | 1.8 | 7.2 | 34.5 | 25.6 | 34.7 |
| spectral-norm | — | 14.9 | 90.5 | 303.1 | 1.6 | 6.3 | 31.6 | 16.0 | 23.5 |
| binary-trees | — | 12.5 | 51.2 | 148.3 | 3.6 | 5.9 | — | — | 16.0 |
| alloc-stress | 19.2 | 15.9 | 34.9 | 145.5 | 5.4 | 5.4 | 31.0 | 13.8 | 13.1 |

At these N the *compute* is sub-millisecond, so this table is dominated by
**process startup + (for `jit`) per-run cranelift compile**. The takeaway is
the startup ranking: AOT (~3 ms) and `rustc -O` (~2 ms) are the cheapest to
launch; crabscheme's `run` tiers ~4-6 ms; **Chez carries a ~32 ms startup**, so
at small N the AOT binary *beats every production Scheme* (geomean: JIT 1.12×,
Guile 1.54×, Gambit 1.39×, **Chez 3.58×** slower than AOT). This is the real
"run a small program / CLI tool" picture — AOT's headline strength.

### Heavy regime — compute-dominated (scaled N)

| bench | aot | jit | vm | rust-O | gambit-aot | chez | guile | gambit |
|---|---|---|---|---|---|---|---|---|
| fib | **35.1** | 41.1 | 2155.3 | 16.0 | 95.1 | 61.5 | 138.2 | 1070.8 |
| tak | 110.4 | **10.7** | 303.0 | 3.1 | 11.8 | 30.4 | 19.7 | 107.5 |
| ack | **44.7** | 56.9 | 845.6 | 29.5 | 30.2 | 39.4 | 61.6 | 480.6 |
| nqueens | 420.2 | 354.5 | 647.3 | 4.0 | 14.3 | 33.9 | 22.0 | 191.3 |
| mandelbrot | — | 70.0 | 799.9 | 4.2 | 32.9 | 71.4 | 165.7 | 360.1 |
| spectral-norm | — | 68.4 | 769.8 | 2.1 | 17.5 | 40.0 | 45.5 | 163.2 |
| binary-trees | — | 160.4 | 1075.8 | 43.7 | 30.2 | — | — | 276.0 |
| alloc-stress | 594.5 | 359.9 | 1177.5 | 121.7 | 26.7 | 44.0 | 63.8 | 322.4 |

**Slowdown vs. AOT** (>1 = slower than AOT; geomean over the 5 AOT-capable benches):

| | jit | vm | rust-O | gambit-aot | chez | guile | gambit |
|---|---|---|---|---|---|---|---|
| geomean vs AOT | **0.59** | 6.27 | 0.11 | 0.20 | 0.30 | 0.35 | 2.40 |

The single most important number: **across the benches AOT can actually run,
the cold-process JIT is ~1.7× faster than AOT on geomean** (jit 0.59×). AOT's
`fib`/`ack` wins are outweighed by `tak` (10× slower), `alloc-stress` (1.65×)
and `nqueens` (1.19×). Native `rustc -O` is ~9× faster than AOT (the unboxed-
LLVM ceiling vs. crabscheme's NaN-boxed values).

---

## Per-bench analysis (heavy regime)

- **`fib` — arithmetic recursion (AOT wins).** AOT 35 ms beats the JIT's
  41 ms (1.17×) and is **61× faster than the bytecode VM** — the textbook AOT
  result: same cranelift codegen as the JIT, minus the per-run compile. Still
  2.2× off `rustc -O` (16 ms), the NaN-boxing tax.
- **`ack` — arithmetic recursion (AOT wins).** Same shape: AOT 45 ms < JIT
  57 ms (1.27×), 19× over the VM. ✓
- **`tak` — non-tail triple recursion (AOT's worst case).** AOT 110 ms is
  **10× slower than the JIT (10.7 ms)** and only 2.7× faster than the VM. The
  emitted Rust uses the fast NB-i64 inline arithmetic (verified — *not* generic
  dispatch), so the cost is structural: AOT lowers control flow to a
  `loop+match` state machine, and `(tak (tak …) (tak …) (tak …))`'s nested
  non-tail calls are expensive there versus the JIT/native call frames.
  Single-`--entry` `tak` doesn't compile, so `--multi` is the only build path.
- **`nqueens` — per-call closure allocation (crabscheme-wide cost).** AOT
  420 ms and JIT 354 ms are both **~100× slower than `rustc -O` (4 ms)** and
  10-25× behind Chez/Guile/Gambit-AOT. The `(lambda (col) …)` allocated on
  every inner iteration dominates; AOT is ~1.2× *worse* than the JIT (which has
  some closure/escape handling the AOT path lacks).
- **`mandelbrot` — flonum loops (AOT compiles but crashes).** The binary
  aborts with `vm_call_aot_procedure: NB carrier 0x0 is not NB_TAG_PROCEDURE`
  from the emitted `col_loop` calling the sibling `mandelbrot-pixel`. JIT runs
  it in 70 ms. Real AOT codegen bug (null procedure handle on a `--multi`
  cross-proc call inside a nested loop).
- **`spectral-norm` / `binary-trees` — AOT can't compile.** `Inst::MakeClosure`
  on their capturing named-let loops. JIT 68 ms / 160 ms. (`binary-trees` is
  also rejected by Chez and Guile — a source-dialect issue — leaving Gambit +
  crabscheme + Rust.)
- **`alloc-stress` — GC churn (AOT slower than JIT).** AOT 594 ms is **1.65×
  slower than the JIT (360 ms)** and ~13× behind Chez (44 ms). crabscheme's
  allocation path is expensive and the AOT lowering lacks the JIT's
  escape-analysis / scalar-replacement of cons cells, so it pays full heap
  traffic.

### Bottom line: "should I AOT-compile this program?"

- **Yes** for **cold/short invocations** (AOT has the lowest startup of any
  crabscheme tier and beats the production Schemes' startup) and for
  **arithmetic-kernel hot paths** (`fib`/`ack`-shaped: AOT matches the JIT's
  codegen with zero warm-up).
- **Not yet** for **deeply-recursive non-tail control flow** (`tak`),
  **allocation-heavy** code (`alloc-stress`), **closure-heavy** code
  (`nqueens`), or anything that hits the `MakeClosure` gap or the `mandelbrot`
  runtime crash. For those, the JIT is the better tier today.

## AOT coverage & correctness scorecard

| bench | compiles? | runs correctly? | heavy: AOT vs JIT | blocker / note |
|---|---|---|---|---|
| `fib` | ✓ | ✓ | **1.17× faster** | — |
| `ack` | ✓ | ✓ | **1.27× faster** | — |
| `tak` | ✓ (`--multi` only) | ✓ | 10× slower | `loop+match` recursion lowering; single-`--entry` won't compile |
| `nqueens` | ✓ | ✓ | 1.19× slower | crabscheme-wide per-call closure-alloc cost |
| `alloc-stress` | ✓ | ✓ | 1.65× slower | no escape-analysis / cons scalar-replacement |
| `mandelbrot` | ✓ | ✗ **aborts** | — | `vm_call_aot_procedure` null handle (cross-proc in nested loop) |
| `spectral-norm` | ✗ | — | — | `Inst::MakeClosure` (capturing named-let) |
| `binary-trees` | ✗ | — | — | `Inst::MakeClosure` (capturing named-let) |

**6/8 compile · 5/8 compile-and-run-correctly · 2/5 of the runnable beat the JIT.**

## Findings filed (AOT track)

These gaps this comparison surfaced are filed as issues **#108–#111**; none
block the perf story, all are post-1.0 AOT-quality work:

1. **`mandelbrot` AOT binary aborts at runtime** ([#108](https://github.com/crab-scheme/crab-scheme/issues/108)) — `vm_call_aot_procedure: NB
   carrier 0x0 is not NB_TAG_PROCEDURE`, thrown from the emitted `col_loop`
   when it calls the sibling top-level `mandelbrot-pixel`. The `--multi`
   cross-procedure call passes a null procedure handle in this nested-loop
   shape. `crabscheme aot --verify <args>` (which runs AOT against the JIT and
   diffs) would catch this at build time; the plain `--build` path does not.
2. **`tak` is ~10× slower under AOT than the JIT, and only ~2.7× faster than
   the bytecode VM.** ([#109](https://github.com/crab-scheme/crab-scheme/issues/109)) The emitted source uses the fast NB-i64 inline arithmetic
   (verified — *not* generic dispatch), so the cost is structural: AOT lowers
   control flow to a `loop+match` state machine, and `tak`'s deeply-nested
   *non-tail* triple recursion `(tak (tak …) (tak …) (tak …))` is expensive in
   that representation versus the JIT/native call frames.
3. **`alloc-stress` is ~1.7× slower under AOT than the JIT.** ([#110](https://github.com/crab-scheme/crab-scheme/issues/110)) The JIT applies
   escape-analysis / scalar-replacement of cons cells (#28 / #51); the AOT
   lowering appears not to, so the allocation-heavy loop pays full heap
   traffic.
4. **`Inst::MakeClosure` gap** ([#111](https://github.com/crab-scheme/crab-scheme/issues/111)) blocks `spectral-norm` and `binary-trees` — any
   named-let loop that closes over outer state (`mul-Av`'s `i-loop`/`j-loop`,
   `run`'s `loop`/`inner`). Note `mandelbrot`'s named-lets *do* compile, so the
   gap is shape-specific, not "all named-lets."
5. **Single-`--entry` `tak` fails to compile** (the `--multi` binary is its
   only build path), so the single-entry fast-ABI mode has its own coverage
   hole. (Tracked alongside [#109](https://github.com/crab-scheme/crab-scheme/issues/109).)

## Reproduce

```bash
bash bench/aot-vs-runtimes.sh --regime both          # canonical + heavy
bash bench/aot-vs-runtimes.sh --regime heavy --bench fib   # one bench
# JSON + markdown tables land under target/aot-vs-runtimes/results/
```
