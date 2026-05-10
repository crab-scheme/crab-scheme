# CrabScheme microbenchmarks

Cross-implementation timing harness, modeled on the Computer Language
Benchmarks Game shootout. Lets us see how CrabScheme's two execution
tiers (tree-walker, bytecode VM) stack up against native Rust, and —
optionally — other Scheme implementations on the host.

## Layout

```
bench/microbench/
├── scheme/                    # Scheme source for each benchmark
├── rust/                      # Reference Rust impls (rustc -O)
├── run.sh                     # Build everything + print timing table
└── README.md
```

## Benchmarks

| name             | what it stresses                                                   |
|------------------|--------------------------------------------------------------------|
| `fib`            | function-call dispatch, naive tree recursion                       |
| `tak`            | mutual recursion with integer arithmetic                           |
| `ack`            | non-primitive-recursive depth (no TCO possible)                    |
| `nqueens`        | recursive backtracking + per-call closure allocation               |
| `mandelbrot`     | tight loops + flonum arithmetic                                    |
| `spectral-norm`  | flonum + vector-ref/-set! + power iteration                        |
| `binary-trees`   | pair allocation + GC churn (Benchmarks Game classic)               |

Five of the seven (everything except `fib` and `tak`) are direct
adaptations of Benchmarks Game tasks. The Scheme and Rust files
solve the same problem with the same N so results are comparable.

## Running

```bash
bench/microbench/run.sh
```

Builds `crabscheme` once with `cargo build --release`, then `rustc -O`
each `rust/*.rs` into `target/release-microbench/`, then runs every
benchmark on every implementation and prints a wall-time table.

To add other Scheme implementations to the table, set them on `PATH`:
the runner auto-detects `racket`, `chez`, and `guile` and adds rows
for any it finds.

## Output shape

```
benchmark              crabscheme-walker  crabscheme-vm         rust-O
fib                            0.339s         0.028s         0.012s
tak                            0.042s         0.014s         0.009s
ack                            0.100s         0.024s         0.143s
nqueens                        0.108s         0.027s         0.012s
mandelbrot                     0.318s         0.075s         0.009s
spectral-norm                  0.283s         0.094s         0.009s
binary-trees                   0.134s         0.039s         0.013s
```

(Sample run from the host this readme was authored on, after a warm
filesystem cache. Apple M-series, macOS.)

Rough takeaways at these N values:

- The **VM tier** lands in the 1.5x–10x slower-than-Rust band for
  CPU-bound work — about what you'd expect from a non-JITed bytecode
  interpreter against `rustc -O`.
- The **walker tier** is 3x–35x slower than the VM tier, mostly from
  per-call EvalCtx threading and frame allocation that the VM tier
  amortizes.
- `ack` is the one benchmark where Rust looks slow — process startup
  dominates that one because the workload itself is small.

## Why these benchmarks

The Benchmarks Game's tasks are concurrency-heavy or
regex/IO-heavy in many cases. We picked the seven above because:

1. They run in single-process, single-threaded mode (no concurrency
   primitives required from the implementation).
2. They produce a single integer or float of output — easy to compare
   across implementations.
3. They cover the major performance axes: function-call dispatch,
   floating-point, integer arithmetic, allocation, and GC pressure.

We deliberately chose moderate `N` defaults so each benchmark runs
in the 30 ms – 1 s range on the walker tier. If you want to stress
the VM or compare against `rustc -O` more honestly, increase the `N`
constant at the bottom of each `.scm` file (and the matching
constant in the corresponding `rust/*.rs`).

## Caveats

- **Walker tier uses host stack** for recursion, so deeply recursive
  benchmarks (`fib(30)`, `ack(3, 8)`) overflow on the walker but not
  on the VM. The current N values are chosen to fit within the
  walker's depth budget so all three columns can be compared.

- **Rust startup**: a freshly-launched binary takes ~140 ms on macOS
  before `main` runs (dyld + linking). At small N this dominates.
  Don't conclude "Scheme is faster than Rust" from these numbers —
  conclude "Scheme is fast enough that Rust startup matters." Use
  `hyperfine` with `--warmup` if you want a real engine comparison.

- **Single trial per benchmark** — no warmup, no statistical
  smoothing. For real perf work use `criterion` (Rust) or
  `hyperfine` (any process).
