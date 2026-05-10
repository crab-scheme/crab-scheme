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
| `alloc-stress`   | 200k short-lived pair allocations (M5 Phase 2 baseline)            |

Five of the seven (everything except `fib` and `tak`) are direct
adaptations of Benchmarks Game tasks. The Scheme and Rust files
solve the same problem with the same N so results are comparable.

## Running

The cleanest path is via the project's `devenv` shell, which ships
all the comparison Schemes pinned to a known nixpkgs version:

```bash
devenv shell        # or `direnv allow` if direnv is installed
bench-micro         # the script alias defined in devenv.nix
```

Without devenv, you can still run the suite directly:

```bash
bench/microbench/run.sh
```

Either way, the runner builds `crabscheme` once with `cargo build
--release`, then `rustc -O` each `rust/*.rs` into
`target/release-microbench/`, then runs every benchmark on every
implementation and prints a wall-time table.

The runner auto-detects these comparison Schemes on `PATH` and adds
a column for any it finds:

| binary  | upstream            | provided by `devenv shell`? |
|---------|---------------------|-----------------------------|
| `racket`| Racket              | yes (`racket-minimal`)      |
| `chez`  | Chez Scheme         | yes                         |
| `guile` | GNU Guile           | yes (`guile_3_0`)           |
| `gsi`   | Gambit Scheme       | yes (`gambit`)              |

If you don't use devenv, install whichever ones you want via your
package manager.

## Output shape

Run inside the project's `devenv shell`, comparing CrabScheme's two
tiers against Chez Scheme, Guile, Gambit, and `rustc -O`:

```
benchmark              crabscheme-walker  crabscheme-vm   chez    guile  gambit  rust-O
fib                            0.380s         0.031s    0.044s  0.036s  0.031s  0.009s
tak                            0.049s         0.016s    0.046s  0.026s  0.014s  0.009s
ack                            0.105s         0.027s    0.040s  0.022s  0.018s  0.009s
nqueens                        0.115s         0.030s    0.048s  0.027s  0.019s  0.010s
mandelbrot                     0.343s         0.075s    0.045s  0.038s  0.042s  0.013s
spectral-norm                  0.312s         0.118s    0.046s  0.025s  0.038s  0.014s
binary-trees                   0.172s         0.047s     ERR     ERR    0.028s  0.016s
alloc-stress                   0.145s         0.037s    0.042s  0.129s  0.019s  0.143s
```

(Apple M-series, macOS. Chez 10.3.0, Guile 3.0.11, Gambit 4.9.5,
rustc 1.95 stable. ERR = the comparison Scheme rejected our source —
typically because it expects a different module-import dialect.)

Rough takeaways at these N values:

- The **VM tier holds its own against mature Schemes** on small,
  CPU-bound programs. On `fib` it actually beats Chez (33 ms vs
  44 ms) and matches Guile (35 ms) / Gambit (31 ms). On
  `tak`/`ack`/`nqueens` it's within ~1.5× of all three.
- The **VM tier is ~2× slower** than Chez/Guile/Gambit on the
  flonum-heavy benchmarks (`mandelbrot`, `spectral-norm`). All three
  comparison Schemes JIT or AOT-compile; ours doesn't yet, so this
  gap is the JIT delta.
- The **walker tier** is 3×–11× slower than our VM tier, mostly from
  per-call EvalCtx threading and frame allocation that the VM tier
  amortizes.
- **Process startup** dominates Rust at this scale. Don't read "VM
  is 3× slower than Rust" — at the actual workload scale the gap
  is real but smaller than this table suggests. Use `hyperfine
  --warmup 3` or scale `N` upward for a fair single-engine
  comparison.

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
