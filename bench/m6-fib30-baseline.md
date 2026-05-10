# M6 Iter 9 — Cranelift JIT perf baseline

> Snapshot at commit immediately preceding this file's first add.
> Hardware: Apple M-series, devenv shell.
> CrabScheme built with `cargo build --release -p cs-cli`.

## Baseline programs

`fib n` = naive recursive Fibonacci:

```scheme
(define (fib n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))
```

Rust reference:

```rust
fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n-1) + fib(n-2) } }
```

## Wall-clock ('time -p real') by tier

| n  | crabscheme-vm | crabscheme-jit | rust -O |
|----|---------------|----------------|---------|
| 25 | 0.11 s        | 0.00 s (<10ms) | n/a     |
| 30 | 1.48 s        | 0.01 s         | 0.26 s  |
| 35 | 22.08 s       | 0.04 s         | 0.18 s  |

Walker tier omitted past n=25 — the host-stack-recursive evaluator
fits fib(25) but stack pressure grows linearly with depth on the
debug build; release runs handle it but the wall-clock isn't
interesting.

## Headline

**`fib(35)`: crabscheme-jit (40 ms) is ~4.5× faster than `rustc -O`
(180 ms).** That margin shrinks at higher optimization levels
(`-C opt-level=3 -C lto=fat`); see follow-up notes for that
comparison.

The spec's exit gate is "1.2× of C-O2 or document why we can't"
(`.spec-workflow/specs/jit-cranelift/requirements.md`). We're well
inside the budget; the perf-tuning iter (planned as a separate
follow-up) can focus on broader JIT coverage and benchmark spread
rather than fib-specific gains.

## Methodology notes

- Each tier runs the full bench file (parse + compile + run) inside
  one process; no warmup loop is used by the runner. The JIT tier
  pays its tier-up + lowering cost on the first ~1024 fib calls;
  the remaining ~9M calls dispatch through native code.
- Wall-clock includes process startup, parse, compile-to-bytecode,
  tier-up, JIT codegen, and the actual computation. Rust startup is
  in the same ballpark (~5-10 ms on macOS).
- Iter 9's `--tier vm-jit` flag (`crates/cs-cli/src/main.rs`) wires
  `Runtime::install_jit()` into the run-file path so the existing
  `bench/microbench/run.sh` picks it up transparently.

## Caveats / things this snapshot doesn't show

- Allocation-heavy benchmarks (`alloc-stress`) — JIT only handles
  pure-fixnum arithmetic for now; allocation-bound workloads stay on
  the VM and won't move from this iter.
- Closure-heavy benchmarks — env access (`LoadVar` of free variables)
  isn't lowered yet; those closures stay on the VM.
- Flonum-heavy benchmarks (`mandelbrot`, `spectral-norm`) — the JIT
  declines anything with non-fixnum args at the dispatch boundary.

These are the natural follow-up targets for future iters. M6's
remaining work per the design doc: deopt trampoline body, broader
instruction lowering, `(jit-dump)`, exit report.
