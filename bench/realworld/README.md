# Real-world benchmark harness

Phase C of the real-world bench suite spec
(`docs/research/realworld_benchmarks_spec.md`). Wraps the Scheme-level
GC instrumentation primops (Phase B) into a per-iter timing loop and
emits machine-readable JSONL.

## Layout

```
bench/realworld/
├── README.md
├── runner.sh          # entry point: build crabscheme, run benches, emit JSONL
├── render.py          # JSONL → markdown table
├── lib/
│   └── harness.scm    # (realworld-bench name params thunk) — timing loop + JSON emit
├── schemes/           # bench wrappers, one per workload
│   ├── ack.scm
│   ├── alloc-stress.scm
│   ├── binary-trees.scm
│   ├── fib.scm
│   ├── mandelbrot.scm
│   ├── nqueens.scm
│   ├── spectral-norm.scm
│   └── tak.scm
└── results/           # JSONL outputs (git-ignored)
```

## Usage

```bash
# Run everything (all detected engines/tiers, all benches).
bench/realworld/runner.sh

# Single bench, single tier, override defaults.
bench/realworld/runner.sh --bench fib --tier vm --measure 20

# Custom output path.
bench/realworld/runner.sh --output bench/realworld/results/2026-05-17.jsonl

# Render to markdown.
python3 bench/realworld/render.py bench/realworld/results/latest.jsonl
```

Flags:

| Flag | Default | Effect |
|------|---------|--------|
| `--bench NAME` | (all) | run one bench |
| `--engine NAME` | (all) | filter by engine |
| `--tier NAME` | (all) | filter by tier (`walker`, `vm`, eventually `aot`) |
| `--warmup N` | 3 | untimed warmup iters |
| `--measure N` | 10 | max measured iters |
| `--time-budget N` | 60 | stop after N seconds wall time |
| `--output PATH` | `results/latest.jsonl` | JSONL output |

`--measure` and `--time-budget` are an `OR`: the loop stops at whichever
hits first. Short benches at 10 iters, long benches at 60 s — same
default for both classes.

## JSONL schema

One JSON document per line. See
`docs/research/realworld_benchmarks_spec.md` for the full spec; in
brief:

```json
{
  "schema_version": "1.0",
  "engine": "crabscheme",
  "engine_tier": "vm",
  "engine_version": "0.0.1",
  "benchmark": "fib",
  "params": {"n": 25},
  "config": {"warmup_iters": 3, "max_iters": 10, "time_budget_seconds": 60, "measured_iters": 10},
  "wall_time_seconds": {
    "iters": [0.018, 0.019, ...],
    "min": 0.017, "p50": 0.019, "p95": 0.021, "p99": 0.022, "max": 0.022,
    "mean": 0.019, "stddev": 0.0013
  },
  "memory": {
    "bytes_allocated_total": 2880,
    "alloc_count_total": 40,
    "collect_count": 0,
    "live_slots": 0,
    "alloc_rate_mb_per_sec": 0.0,
    "gc_time_ms": 0.0,
    "gc_time_pct": 0.0,
    "last_pause_ms": 0.0,
    "max_pause_ms": 0.0
  }
}
```

## Engine matrix (Phase C)

| Engine | Tier | Status |
|--------|------|--------|
| `crabscheme` | `walker` | works but walker's per-frame stack is heavy — the harness preamble pushes some benches over the limit. Use `vm` until walker stack discipline is tightened. |
| `crabscheme` | `vm` | working baseline |
| `crabscheme` | `aot` | deferred — AOT's primop subset doesn't yet include `(current-memory-use)` etc.; would need either Scheme-prelude prelude wrapping or AOT primop expansion. Plan: pick up in Phase D when porting Tier-2 benches that need AOT comparison. |

Cross-impl engines (Chez, Gambit, Racket, Guile) land in Phase D
when Tier-2 benches arrive. They need a different timing strategy
because they don't have our GC primops — the harness will fall back
to external `/usr/bin/time` for those rows, with memory fields
elided.

## Variance + statistics

Default 10 measured iters per bench + 3 warmup. Per-iter wall time
on the existing microbenches is 10–250 ms. At that scale, macOS
scheduling jitter is multi-ms and run-to-run p50 variance is
typically 5–25 %. The harness reports stddev alongside the
percentiles so consumers can judge confidence.

For tight numbers either:

- Increase `--measure` to 30+ (recommended for regression-detection
  comparisons of microbenches).
- Move to Tier 2/3 benches (longer per-iter, smaller relative
  variance).
- Use the min, not p50, as the comparison metric — min is least
  affected by scheduling jitter (effectively the platform's
  best-case run).

## GC metrics

`gc_time_ms` and `gc_time_pct` are 0 in most rows because auto-collect
is off by default and the microbenches don't manually invoke
`(collect-garbage)`. Real GC behavior will show up when:

1. Tier-3 benches that allocate enough to cross the auto-collect
   threshold land (the heap-pressure tree-rewriter, hashtable-bench,
   etc.).
2. The runtime defaults are flipped to enable auto-collect (post-1.0
   GC migration work, per the cs-gc Phase 1 doc).

For now, `bytes_allocated_total` / `bytes/iter` are the meaningful
columns — those track every `Gc::new` and `Heap::alloc` on the
process-global counter.

## Cross-impl correctness check

Each bench's RESULT (independent of timing) is verified to match
Chez Scheme via `check-result-vs-chez.sh`. Both engines run the
same source file with a minimal "result-only" shim
(`crab-shim.scm` / `chez-shim.scm`) that defines
`(realworld-bench name params thunk)` to run the thunk once and
`(write)` the result.

```bash
bench/realworld/check-result-vs-chez.sh             # all benches
bench/realworld/check-result-vs-chez.sh --bench fib # one bench
```

Output shows shasums of each engine's stringified result + a
MATCH/DIFFER status. Used as the Phase D correctness gate: a
DIFFER means we computed a wrong answer relative to Chez (the
reference R7RS implementation) and the bench needs investigation
before its timing numbers are meaningful.

## Bench coverage matrix

Phase C + D + E ports as of 2026-05-17:

| Bench               | Tier | Source           | Crab walker | Crab VM | Crab AOT | Chez result |
|---------------------|------|------------------|-------------|---------|----------|-------------|
| fib                 | 1    | microbench       | (heavy)     | ok      | (Phase D follow-up) | match |
| tak                 | 1    | microbench       | (heavy)     | ok      | "        | match |
| ack                 | 1    | microbench       | (heavy)     | ok      | "        | match |
| nqueens             | 1    | microbench       | (heavy)     | ok      | "        | match |
| mandelbrot          | 1    | microbench       | (heavy)     | ok      | "        | match |
| spectral-norm       | 1    | microbench       | (heavy)     | ok      | "        | match |
| binary-trees        | 1    | microbench       | (heavy)     | ok      | "        | match |
| alloc-stress        | 1    | microbench       | (heavy)     | ok      | "        | match |
| maze                | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| lattice             | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| paraffins           | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| sboyer              | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| nboyer              | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| earley              | 2    | ecraven/r7rs     | (heavy)     | ok      | "        | match |
| t3a-tree-rewriter   | 3    | authored (Phase E) | (heavy)     | ok    | "        | match |
| t3b-hashtable-bench | 3    | authored (Phase E) | (heavy)     | ok    | "        | match |
| t3c-metacircular    | 3    | authored (Phase E) | (heavy)     | ok    | "        | match |
| t3e-stateful-loop   | 3    | authored (Phase E) | (heavy)     | ok    | "        | match |
| (t3d-sxml)          | 3    | spec deferral    | -           | -       | -        | -     |

## Tier-3 long-running benches

The Phase-E synthetics target what Tier 1/2 misses: minute-scale
steady-state, per-iter variance under 1.5×, real allocation
patterns rather than tight CPU-bound loops.

| Bench               | Per-iter | Allocs/iter | p95/p50 | Notes |
|---------------------|----------|-------------|---------|-------|
| t3a-tree-rewriter   | 25 ms    | 3.5 MB      | 1.01    | Rebuilds + folds a 32k-node expr tree each iter. |
| t3b-hashtable-bench | 1.2 s    | 1 MB        | 1.14    | 4k inserts + 16k Zipf-skewed lookups + 2k deletes. Final ht size verified. |
| t3c-metacircular    | 9.5 s    | 454 MB      | 1.008   | SICP eval/apply running 8x quicksort(200) per iter. |
| t3e-stateful-loop   | 365 ms   | 1.9 MB      | 1.020   | 50k KV-store requests per iter, 256-key working set. |

p95/p50 numbers from 3–5 measurement iters on M3 Max, VM tier.
All four meet the spec's < 1.5× variance gate. t3a's variance is
exceptionally tight (1.01) — the per-iter work is large enough
that scheduling jitter is amortized out.

T3-D (SXML transformer) is deferred — the spec's design requires
a Scheme XML parser, which isn't trivially available. The other
four cover the spec's targeted axes (heap pressure, hashtable
scaling, interpretive dispatch, stateful steady-state).

### GC % column is 0 across all Tier-3 rows

CrabScheme's runtime still allocates most values via
`cs_gc::Gc::new` (the unregistered constructor, used by
`Pair::new` / `Hashtable::new` / etc. during the in-progress
heap-rooting migration). Those allocs bump the process-global
byte / count counters (so `bytes_allocated_total` and
`alloc_rate_mb_per_sec` are honest) but don't fire the heap's
auto-collect-on-threshold trigger. Until the migration completes,
the harness's `gc_time_ms` and `max_pause_ms` columns will read
zero for all benches that don't manually invoke
`(collect-garbage)` inside their thunk.

Benches that want to exercise the collector deliberately today
can call `(collect-garbage)` at their iter boundary — the harness
will capture the resulting `last_pause_ms` and roll it into
`max_pause_ms` / the pause histogram.

Walker "(heavy)" — runs, but the per-frame stack cost combined
with our harness's let-heavy preamble blows the test-thread / CLI
default stack on benches with deep recursion (which is most of
them). Workaround: invoke from a larger-stack thread, like the
`jit_conformance_cross_lambda_loop` test does. Tracked under the
existing walker stack-discipline follow-up.

AOT "(Phase D follow-up)" — AOT's primop subset doesn't yet
include `(current-memory-use)` / `(gc-stats)` / `(collect-garbage)`,
so the harness's timing loop won't run inside AOT-compiled code.
Two paths to fix:
1. Extend AOT's primop subset to call back into the runtime for
   these (small wiring change in cs-aot).
2. Compile only the bench's workload via AOT and have the
   harness invoke it from VM-side timing wrappers (changes
   runner.sh shape but no AOT changes needed).

Plan: pick option 2 when we revisit AOT coverage.

## Tier-2 ports that didn't land

Source files from ecraven/r7rs-benchmarks that we attempted but
parked due to CrabScheme subset limits:

| Bench   | Blocker | Fix path |
|---------|---------|----------|
| gcbench | internal `define` inside a `let` body (the bench has `(let () (define-record-type ...) (define ...) (define ...) body)`). CrabScheme expander rejects the second `(define)` in expression position. | Extend the expander to body-position-hoist all internal defines in `(let)` and `(lambda)`, not just the leading one. R7RS allows this. |
| mperm   | `(define (f . rest) ...)` rest-args dotted-pair shorthand in the placeholder definitions for `setup-boyer`. | Either teach the expander the shorthand, or rewrite manually to `(define f (lambda args ...))` like we did for nboyer. |
| ray     | The bench's entry function writes to a file via `(tracer output-file res)` — not a pure thunk, doesn't fit our timing-loop shape. | Restructure to render into a string port instead, or skip and use a different Flonum-heavy bench. |
| nucleic | not yet attempted (3500 LoC) — likely works but is large to vet. | Port in a follow-up; expected to be a clean Tier-2 row once vetted. |
| peval   | not yet attempted — its input is a quoted lambda from stdin; needs to be embedded in the wrapper. | Port in a follow-up; mechanical, not blocked. |
| compiler| not yet attempted (11k LoC) — likely hits multiple subset limits. | Defer; biggest bench, lowest ROI vs the others. |
| conform | not attempted | follow-up |
| dynamic | not attempted | follow-up |

The first three (gcbench, mperm, ray) are concretely blocked by
CrabScheme features rather than porting effort. Fixing the
expander's internal-define handling and rest-args shorthand
would unblock 2 of 3 — file as follow-up issues.

## Spec link

See `docs/research/realworld_benchmarks_spec.md` for the full
multi-tier design (Tier 1 = these microbenches; Tier 2 = curated
r7rs-benchmarks; Tier 3 = long-running synthetic workloads).
