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

## Spec link

See `docs/research/realworld_benchmarks_spec.md` for the full
multi-tier design (Tier 1 = these microbenches; Tier 2 = curated
r7rs-benchmarks; Tier 3 = long-running synthetic workloads).
