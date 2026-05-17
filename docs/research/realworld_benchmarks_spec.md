# Real-world benchmark suite — spec

> Status: draft (2026-05-17). Scope is forward-looking: this spec
> describes what we want, not what's currently implemented.

## TL;DR

The current `bench/microbench/` suite gives us strong steady-state
ratios on small CPU-bound programs (fib 1.4×, mandelbrot 1.7×,
spectral-norm 1.1× vs Rust) but tells us nothing about three things
that matter for real users:

1. **GC behavior under load** — allocation rate, pause distribution,
   peak heap, throughput tax from collection.
2. **Steady-state runtime** — every microbench finishes in < 1 s,
   so we never see whether perf degrades over minutes (heap
   fragmentation, regression in collection cost as live set grows,
   icache pressure from a fully warmed JIT).
3. **Real-world dispatch and data shapes** — the microbenches use
   uniform-typed Fixnum arithmetic or tight Flonum loops. Production
   Scheme code is hashtable-heavy, symbol-heavy, mixes types, and
   calls procedures through dynamic dispatch most of the time.

This spec proposes three tiers of benchmarks, runtime instrumentation
(`(gc-stats)`, `(time-apply)` etc.) modeled on Chez + Racket APIs,
a JSON-structured harness output, and a phased rollout that adds
GC instrumentation first, then ports a curated subset of the
[ecraven/r7rs-benchmarks](https://github.com/ecraven/r7rs-benchmarks)
suite, then designs 3-5 long-running synthetic workloads.

## Goals

| # | Goal | Measured by |
|---|------|-------------|
| G1 | Detect perf regressions in **steady-state** code, not just
       cold-start cost. | Tier-3 long-running benches with p50/p95/p99
       per-op latencies. |
| G2 | Detect GC regressions: alloc rate spikes, pause growth, peak
       heap growth. | Per-bench memory section (alloc bytes, GC time
       %, max pause, peak RSS). |
| G3 | Compare CrabScheme against the rest of the Scheme ecosystem
       on workloads other implementers also measure. | Tier-2
       benches (r7rs-benchmarks subset) with side-by-side Chez /
       Gambit / Racket / Guile numbers. |
| G4 | Provide signal for the AOT pipeline specifically: which
       benches AOT-compile, which run only walker/VM, where the
       AOT subset limits us. | Per-bench tier matrix in the
       output table. |
| G5 | Be machine-readable so future tooling can plot trends, gate
       PRs on regression thresholds, etc. | JSON output schema. |

## Non-goals

- **Not** a marketing benchmark. We want signal we can act on,
  not numbers that look good in a blog post. Specifically: no
  cherry-picking workloads where CrabScheme already wins, no
  tuning N to make a Crab/Rust ratio land at exactly 1.0×.
- **Not** a replacement for the microbenches. Those stay as
  fine-grained regression detectors; this suite layers on top.
- **Not** a concurrency or networking benchmark. Scheme's
  thread/socket story isn't standardized enough across
  implementations for cross-impl comparison. Single-process,
  single-threaded only.
- **Not** an attempt to beat Rust on real-world Scheme. The
  comparison anchor for Tier 2 and 3 is **other Schemes**
  (Chez, Gambit, Racket, Guile), not Rust. Rust comparisons
  stay in the microbench tier where the workload is small
  enough that the language gap is the dominant signal.

## What we measure

For each bench-run we capture:

```
- Wall time (real)
- CPU time (user + sys)
- Bytes allocated (cumulative, from cs-gc)
- GC time (cumulative, ms in collect())
- Max single GC pause (ms)
- p95 GC pause (ms, from the per-collection histogram)
- Collection count
- Peak RSS (from /usr/bin/time -v on Linux, ru_maxrss on macOS)
- stdout SHA (correctness check across runs / impls)
- Exit code
```

For long-running benches that report per-iteration latency we also
capture:

```
- Per-iter wall time: min, p50, p95, p99, max
- Throughput (ops/sec, steady-state window only)
- Allocation rate (MB/sec, steady-state window only)
```

"Steady-state window" = after N warmup iters (default 3 or 10% of
total, whichever is larger), so we drop cold-start cost.

## What we don't yet measure (and why)

- **Per-generation GC stats**: cs-gc is currently non-generational
  (single mark-and-sweep heap). The instrumentation API leaves
  room for generation breakdowns when we add them (deferred to
  post-1.0 GC work).
- **Cache misses / branch mispredicts**: these need `perf stat`
  (Linux-only) or DTrace (macOS). Out of scope for v1; can be
  added as an optional `--perf` flag later.
- **Code-cache size for JIT**: deferred until JIT tier is wired
  back into the harness (currently the JIT is enabled but the
  scorecard tests AOT, not JIT-of-VM).
- **Pause-time tail beyond p99**: would need every-collection
  histogram, not just a max. Defer until we have a workload
  where p99 isn't representative.

## Benchmark tiers

### Tier 1 — Microbenchmarks (existing, unchanged)

Path: `bench/microbench/`. 8 benches (`fib`, `tak`, `ack`,
`nqueens`, `mandelbrot`, `spectral-norm`, `binary-trees`,
`alloc-stress`). All run < 1 s. Each has a Rust reference at
`-O3` for floor comparison.

These stay as the **fast feedback loop**: a CI gate on the
Tier-1 scorecard catches obvious perf regressions in minutes,
not hours.

### Tier 2 — Standard cross-implementation benches (curated subset)

Source: [ecraven/r7rs-benchmarks](https://github.com/ecraven/r7rs-benchmarks)
— the actively-maintained R7RS-portable harness that 14+ Scheme
implementations publish numbers for. Full suite is 54 benches;
we pick a curated 12 that:

1. Run for >= 1 s at the published default N (so per-run overhead
   doesn't dominate).
2. Cover diverse subsystems (parsing, GC, floats, symbols,
   hashtables, recursion shapes).
3. Have public published results from Chez / Gambit / Racket /
   Guile we can cross-check against.

**Curated picks** (mapped to the r7rs-benchmarks taxonomy):

| Bench | r7rs category | What it stresses | Why this one |
|-------|---------------|------------------|--------------|
| `nboyer` | GC-heavy Gabriel | sharing-aware theorem prover; long-lived cons cells | Stress test for non-generational GC; the original Gabriel `boyer` was unsuited to copying GCs and Will Clinger's `nboyer` variant fixes that. Published in the 1991 GC benchmark survey. |
| `sboyer` | GC-heavy Gabriel | nboyer with sharing | Distinguishes pointer-eq optimizations from value-eq. |
| `earley` | parsing | Earley parser on grammars up to ~200 productions | Sustained allocation + complex linked-list / tree data. Heap reaches ~50 MB on default N. |
| `nucleic` | numerical + alloc | computes nucleic acid 3D structure | Mixes floats and structural data (records / vectors); the canonical "real numeric kernel" Scheme benchmark since DeRoure's 1990 paper. |
| `compiler` | application | runs Scheme48's compiler on a sample input | Real application code path: symbols, hashtables, dispatch. |
| `peval` | application | partial evaluator | Hashtable + symbol pressure; pattern matching. |
| `maze` | recursion + alloc | random maze generator + solver | Allocation interleaved with recursion; bounded heap growth. |
| `ray` | float-heavy | small ray tracer | Floats + list traversal + dispatch (closure-of-record). |
| `lattice` | symbol-heavy | computes lattice of inheritance graphs | Symbol interning + eq?-keyed hashtables. |
| `paraffins` | combinatorial | enumerates paraffin isomers | Tree allocation + structural sharing. |
| `gcbench` | GC-targeted | Boehm's GC benchmark (Java heritage) | Multi-phase: long-lived data + short-lived churn. Industry-standard GC stress test. |
| `mperm` | GC stress | generates permutations, retains half | Half-life-of-data pattern that breaks generational assumptions. |

That's 12 picks out of 54. The other 42 are skipped because either
they overlap with what we already have (`fib`, `tak`, `ack`,
`mbrot`, `nqueens`, `array1`, `pi` etc. are Tier 1), they're
trivially short (`sum`, `string`, `cat`), or they don't exercise
anything Tier 1 + Tier 3 miss.

**Output format**: same JSON schema as Tier 1 (see Measurement
methodology). Comparison columns: CrabScheme walker, VM, AOT (if
compilable), Chez, Gambit, Racket, Guile. Mark cells as `aot:fail`
or `impl:unsupported` where applicable rather than dropping rows.

### Tier 3 — Long-running synthetic workloads

These are designed (not ported) and target what Tier 2 still
misses: minute-scale steady-state, p99 latency tails, real
allocation patterns from idiomatic Scheme code.

**T3-A: tree-rewriter** (heap-pressure + sustained alloc)

Build a 200k-node symbolic expression tree (random ops:
`+`, `-`, `*`, `if`, `let`, `lambda` from a fixed pool),
then repeatedly apply a normalization pass (constant folding +
algebraic identities) for 60 s. Each pass allocates ~5 MB of new
nodes and drops the old tree; live heap oscillates between ~5 MB
and ~50 MB.

Reports:
- Throughput: passes / second (steady-state window).
- Per-pass latency: p50, p95, p99.
- GC: % wall time in collection, max pause.
- Peak RSS over the 60 s.

Expected signal: pause-time growth as heap fragments; alloc-rate
ceiling; whether `(collect)` triggers per pass or per N passes.

**T3-B: hashtable-bench** (scaling + symbol pressure)

Insert 1M string keys (drawn from a Zipfian distribution, alpha=1.0,
to model real-world key skew). Then do 10M lookups across two
phases: half on existing keys, half on miss-keys. Then delete 500k
keys (the bottom-frequency half) and repeat one more round.

Reports:
- Time per phase (insert / lookup / delete).
- Allocation rate during insert vs lookup vs delete.
- Memory footprint at each phase boundary (`(current-memory-use)`).
- Hashtable rehash count (would require exposing this from cs-vm).

Expected signal: O(1) amortized hashtable ops; whether symbol
interning becomes a bottleneck; whether eqv?-hashtable vs
equal?-hashtable performance differs the way it should.

**T3-C: metacircular evaluator** (real dispatch + nested allocation)

Write a SICP-style metacircular evaluator (~200 lines of Scheme;
the eval/apply pair from chapter 4) and run it on a workload Scheme
program (e.g., `(quicksort some-1k-element-list)` for ~50 outer
iterations). The eval'd interpreter runs the workload; the workload
runs sort and so on.

Reports:
- End-to-end wall time.
- Outer-iter latency distribution.
- GC stats (this is the canonical Scheme-eats-its-own-tail
  benchmark; lots of pair / closure allocation).

Expected signal: how dispatch performance translates when the
caller is the interpreter itself (no inline caches in the eval'd
layer); how well closure allocation scales when every eval'd
function allocates fresh.

**T3-D: SXML transformer** (string + symbol + dispatch heavy)

Load a 5 MB XML document parsed to SXML (`((root (child ...) ...))`
S-expression form). Apply a series of SXSLT-style transformations
(rename elements, filter children, project attributes). Output the
transformed document back to a string.

Reports:
- Load / transform / serialize phase times.
- Allocation rate per phase.
- Final string length sanity check.

Expected signal: how the system handles string-heavy + symbol-heavy
real-world data; whether `assq` / `assoc` chains hold up at 50k+
keys; whether the I/O path adds appreciable cost.

**T3-E: long-running stateful loop** (alloc + lookup steady-state)

Simulate 100k "requests" against an in-memory key-value store:
each request reads a key (hashtable lookup), allocates a response
record (3-5 fields), updates a per-request counter (hashtable
update). Run for at least 60 s; report per-1000-requests latency
percentiles.

Reports:
- Throughput (req/s, steady-state).
- Per-1000-req p50, p95, p99 latency.
- Allocation rate.
- GC pause distribution (full histogram, not just max).

Expected signal: closest workload to "server doing real work";
catches subtle regressions in dispatch + allocation interaction.

## Runtime instrumentation (cs-gc / cs-vm API additions)

The benches need the runtime to expose stats. Currently cs-gc
exposes:

```
Heap::alloc_count(&self) -> usize
Heap::collect_count(&self) -> usize
Heap::live_slots(&self) -> usize
```

We need to add:

```rust
// in cs-gc::Heap
pub fn bytes_allocated_total(&self) -> u64;    // cumulative since startup
pub fn collect_duration_total(&self) -> Duration;
pub fn last_pause(&self) -> Duration;          // most recent collect()
pub fn max_pause(&self) -> Duration;           // peak since startup
pub fn pause_histogram(&self) -> &PauseHist;   // for p50/p95/p99
pub fn reset_stats(&self);                     // for warmup/measure split
```

`bytes_allocated_total` requires each `alloc<T>` to record
`size_of::<T>()` (currently we only count slots, not bytes). The
size lookup is cheap (compile-time constant per T) but threads
a small overhead through every alloc.

Pause timing requires high-res clock samples around the body of
`collect()`. Default-off, opt-in via `Heap::set_stats_enabled(true)`
to keep the non-bench cost at zero.

The `PauseHist` should be a fixed-bucket histogram (HdrHistogram
shape, log-binned, ~64 buckets covering 1µs to 1s) so reporting
percentiles doesn't require storing every sample.

Exposed to Scheme as primops (cs-vm layer):

```scheme
; Returns an alist with the full stats record. Stable keys.
;
; ((bytes-allocated-total . 12345678)
;  (collect-count . 87)
;  (collect-time-ms . 145.3)
;  (max-pause-ms . 4.32)
;  (live-slots . 1024)
;  (current-rss-bytes . 134217728))
(gc-stats)

; Force a collection and return the new live-slot count.
; (Compatible with Chez's (collect) shape.)
(collect-garbage)

; Bytes reachable from roots right now. (Racket-compatible.)
(current-memory-use)

; Wraps a thunk, returns (values result cpu-ms real-ms gc-ms bytes).
; (Racket-compatible.)
(time-apply thunk args)

; Convenience macro: (time expr) prints all five numbers + value.
; (Chez/Gambit-compatible shape.)
(time expr)
```

We model the API on Chez + Racket because those are the two
projects whose users most often write benchmark harnesses; staying
shape-compatible means external benchmarks that already work on
Chez can drop into the CrabScheme harness with minimal porting.

## Measurement methodology

### Warmup + iteration

For each Tier-2 or Tier-3 bench:

1. Spawn one process per (engine, tier) combination — each gets a
   clean heap.
2. Run `WARMUP_ITERS` iterations (default 3) without recording.
3. Reset stats: `(gc-stats)` snapshot, `Heap::reset_stats()`.
4. Run until either `MEASURE_ITERS` iterations complete (default
   10) or `TIME_BUDGET_SECONDS` elapses (default 60). Whichever
   first.
5. Capture per-iter wall-time samples and the end-of-run aggregate
   stats.
6. Compute summary statistics (min, p50, p95, p99, max) over the
   per-iter samples.

The default of "10 iters OR 60 s" means short benches (Tier 1
microbench style) get high iteration counts and tight statistics,
while long benches (Tier 3) get fewer iters but each runs full
duration. The harness picks based on which limit is hit.

### Process-per-run vs in-process

We run **one process per (engine, bench, tier)**. Reasons:

- Clean heap state across benches: no cross-contamination from a
  previous bench's lingering live data.
- Crash isolation: an OOM or panic in one bench doesn't affect
  the rest of the suite.
- Easy stats: `/usr/bin/time -v` works on the whole process; no
  need to subtract previous-bench stats.

Tradeoff: per-process startup cost. Mitigated by running multiple
iters within the one process, so startup amortizes across N iters
of the same bench.

### Iteration timing

Use `clock_gettime(CLOCK_MONOTONIC_RAW)` (Linux) / `mach_absolute_time`
(macOS) for per-iter wall time; resolution is sub-microsecond. The
Scheme-facing API is `(time-apply)`, which the harness uses for
each iter inside the bench file.

For CPU time and bytes allocated, use the deltas captured from
`(gc-stats)` snapshots around each iter.

For peak RSS, parse the output of `/usr/bin/time -v` (Linux) or the
exit status of the wrapper (macOS uses `ru_maxrss` from `getrusage`
in KB on Linux, in bytes on macOS — harness normalizes to bytes).

### Statistics shape

Per-iter samples → percentiles via standard formula (linear
interpolation between two nearest ranks). No bootstrapping or
confidence intervals in v1; if a bench's p95 / median ratio
exceeds 1.5 the harness flags it as "high variance" and the user
should re-run with more iters.

GC pause percentiles come from the PauseHist (each collect() adds
one entry); these are reported separately from per-iter wall-time
percentiles.

### Output format

Each bench-run emits one JSON document. Example:

```json
{
  "schema_version": "1.0",
  "timestamp": "2026-05-17T14:23:10Z",
  "host": {
    "os": "darwin",
    "arch": "arm64",
    "cpu": "Apple M3 Max",
    "memory_gb": 64
  },
  "engine": "crabscheme",
  "engine_version": "1.0.0-rc",
  "engine_tier": "vm",
  "benchmark": "nboyer",
  "params": {"n": 4},
  "config": {
    "warmup_iters": 3,
    "measure_iters": 10,
    "time_budget_seconds": 60
  },
  "result": {
    "status": "ok",
    "stdout_sha256": "ab12...",
    "exit_code": 0
  },
  "wall_time_seconds": {
    "iters": [1.21, 1.19, 1.20, 1.18, 1.22, 1.20, 1.21, 1.19, 1.18, 1.20],
    "min": 1.18,
    "p50": 1.20,
    "p95": 1.22,
    "p99": 1.22,
    "max": 1.22,
    "mean": 1.198,
    "stddev": 0.013
  },
  "cpu_time_seconds": {
    "min": 1.15, "p50": 1.17, "p95": 1.19, "max": 1.20
  },
  "memory": {
    "bytes_allocated_total": 142857142,
    "alloc_rate_mb_per_sec": 119.0,
    "collections": 87,
    "gc_time_ms_total": 145.3,
    "gc_time_pct": 1.2,
    "max_pause_ms": 4.32,
    "p95_pause_ms": 1.85,
    "p99_pause_ms": 3.14,
    "peak_rss_bytes": 268435456,
    "final_live_slots": 8421
  }
}
```

Multiple JSON docs are written one-per-line to a results file
(JSONL), so the harness can append incrementally and a downstream
tool can stream them.

A markdown renderer reads the JSONL and produces the human-readable
table similar to the current `typer-scorecard.sh` output, but with
columns for memory metrics.

## Harness implementation

Path: `bench/realworld/`. New directory parallel to `bench/microbench/`.

```
bench/realworld/
├── README.md
├── runner.sh              # entry point: run one or all benches
├── render.py              # JSONL → markdown
├── schemes/               # ported r7rs-benchmarks + Tier-3 source
│   ├── nboyer.scm
│   ├── earley.scm
│   ├── ...
│   ├── tier3-treerewriter.scm
│   ├── tier3-hashbench.scm
│   ├── ...
├── inputs/                # shared input data (sxml, dictionaries, etc.)
└── results/               # JSONL outputs, git-ignored
```

`runner.sh` accepts:

```
runner.sh                          # run all benches on all detected engines
runner.sh --bench earley           # one bench, all engines
runner.sh --engine crabscheme-vm   # one engine, all benches
runner.sh --tier 3                 # only Tier-3 benches
runner.sh --warmup 5 --measure 20  # override defaults
runner.sh --time-budget 120
runner.sh --output results/2026-05-17.jsonl
```

Detection: each engine has a probe (similar to typer-scorecard.sh).
The harness skips engines not on `PATH`.

Each bench file must export a top-level `(main)` that takes the
bench-specific params from argv. The harness invokes:

```
ENGINE_CMD path/to/bench.scm  PARAM1 PARAM2 ...
```

For CrabScheme, that maps to:

- walker tier: `crabscheme run bench.scm` (or via the planned
  `--tier walker` flag).
- VM tier: `crabscheme run --tier vm bench.scm`.
- AOT tier: pre-compile via `crabscheme aot --multi --build`, then
  run the resulting binary.

For comparison Schemes, the bench file uses the existing
`(import (scheme base) ...)` R7RS shim so the same source works
across Chez / Gambit / Racket / Guile.

## Comparison baselines

| Engine | Version pin | Why included |
|--------|-------------|--------------|
| CrabScheme walker | this repo's `target/release/crabscheme` | floor signal for our tree-walker tier |
| CrabScheme VM | same, with `--tier vm` | most-used tier today |
| CrabScheme AOT | `aot --multi --build` then run | covers the supported subset |
| Chez Scheme | 10.3.0 (devenv-pinned) | fastest commodity Scheme, our north star |
| Gambit | 4.9.5 (devenv-pinned) | AOT compiler — most directly comparable to our AOT story |
| Racket | latest stable (devenv-pinned) | most-deployed; tests interop / R7RS portability |
| Guile | 3.0.11 (devenv-pinned) | GNU stack baseline |

Rust is **not** in this matrix. The point of Tier 2 / 3 is to
compare against other Schemes, where the workload shape is fair.
Tier 1 already gives us the Rust-floor signal.

## Phased rollout

| Phase | Deliverable | Acceptance |
|-------|-------------|------------|
| **A** | cs-gc instrumentation: bytes_allocated, pause timing, histogram, reset_stats | unit tests in cs-gc; ≤2 % overhead with stats on; ~0 % with stats off |
| **B** | Scheme-facing API: `(gc-stats)`, `(time-apply)`, `(time)`, `(current-memory-use)`, `(collect-garbage)` | conformance test that each returns documented shape; works in walker, VM, AOT |
| **C** | JSON harness skeleton: `bench/realworld/runner.sh` + render.py, runs the existing 8 microbenches and emits JSONL | regression test: rerun + diff produces stable output (≤5 % variance) |
| **D** | Tier 2 ports: 12 r7rs-benchmarks, document which work in walker / VM / AOT | each bench has a stdout-sha that matches across (CrabScheme walker, CrabScheme VM, Chez) |
| **E** | Tier 3 designs: 5 long-running benches (T3-A through T3-E) authored, tuned to ~60 s on a 2026-era laptop | per-iter p95 / median variance < 1.5× in steady-state |
| **F** | CI integration: PR gate runs Tier 1 + Tier 2 on CrabScheme tiers; nightly runs full suite + comparison Schemes | regression threshold: > 10 % slowdown on any bench fails the PR |

Phases A-B unblock the instrumentation; C is the harness; D-E are
the benches; F is the productization. Each phase is independently
useful — we don't need to wait for F to start gathering signal.

## Open questions

1. **Pause-time histogram detail**: HdrHistogram-style logarithmic
   buckets, or simpler fixed-bucket? The former is right but adds a
   dependency or ~150 LoC for a homegrown impl.
2. **AOT-blocked bench policy**: when an AOT compile fails for a
   Tier-2 bench (closure-elision can't reach, dynamic dispatch the
   AOT pipeline doesn't yet support), do we:
   - (a) skip the AOT column entirely for that bench,
   - (b) report `aot:fail` and continue with walker/VM,
   - (c) gate the bench out of the suite until AOT support lands?
   - Recommendation: **(b)** — visible signal that AOT coverage is
     incomplete + still gives walker/VM data.
3. **Steady-state window definition**: drop first N iters (current
   plan) vs drop iters where pause time > rolling mean × 2σ vs
   GC-aware "next iter after stable RSS"? Current plan is simplest;
   refine if Tier-3 noise demands it.
4. **Per-iter or per-batch instrumentation**: for very fast benches
   (T3-E at high throughput, ~100k iter/sec), per-iter `(time-apply)`
   adds measurable overhead. Trade off: batched timing (every 1000
   iters) reduces overhead but coarsens percentiles. Recommendation:
   adaptive — fall back to batched when per-iter time < 100 µs.
5. **Subprocess vs in-process for comparison Schemes**: do we want
   to amortize startup by running multiple benches per process?
   Probably no — risks state contamination, and the cleanliness wins
   outweigh the seconds saved per suite run.
6. **Where the metacircular evaluator (T3-C) comes from**: write
   fresh, or extract from SICP exercises, or port chibi-scheme's
   `eval` module? SICP version is most pedagogically clean (and
   public domain).
7. **Bench-file dialect portability**: not every R7RS-purport ed
   bench actually runs on every implementation without tweaks
   (Chez vs Racket vs Gambit have different `(import)` shapes,
   number-tower defaults, etc.). Plan: maintain a per-engine shim
   prelude that we prepend, similar to how r7rs-benchmarks does it.

## References

- ecraven, "r7rs-benchmarks":
  https://github.com/ecraven/r7rs-benchmarks
- ecraven, results dashboard:
  https://ecraven.github.io/r7rs-benchmarks/
- Larceny R7RS benchmark spec:
  http://www.larcenists.org/benchmarksAboutR7.html
- Chez Scheme statistics API:
  https://cisco.github.io/ChezScheme/csug9.5/system.html
- Racket garbage collection API:
  https://docs.racket-lang.org/reference/garbagecollection.html
- Boehm GCBench (Java; ported to Scheme in the r7rs suite):
  https://hboehm.info/gc/gc_bench/
- DeRoure & Hopkins, "Microcoding the Functional Programming
  Language SASL" (1990) — origin of `nucleic`.
- Will Clinger, "Benchmarking implementations of Scheme" — the
  Larceny benchmark suite's design doc.
