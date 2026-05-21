# Crab Scheme 1.0-rc5 — performance results

> Tag: `1.0-rc5` (`a5c625f`), default features.
> Host: Apple Silicon (arm64), macOS 26.2.
> Comparison Schemes: Chez 10.x, Guile 3.0, Gambit; `rustc -O`.
> Reproduce: `bench/microbench/run.sh` (cross-impl table),
> `bench/microbench/scaled.sh` (compute-bound VM-vs-JIT, hyperfine).

## 1. Compute-bound JIT vs VM (hyperfine, 5 runs, ±σ)

The honest signal — N scaled so compute dominates the ~10 ms process
startup floor.

| benchmark (scaled N)  | VM (bytecode)   | JIT (Cranelift) | speedup |
|-----------------------|-----------------|-----------------|---------|
| `fib(32)`             | 452.3 ± 3.7 ms  | 11.5 ± 0.4 ms   | 39.4×   |
| `spectral-norm(500)`  | 7.93 ± 0.09 s   | 575.3 ± 2.2 ms  | 13.8×   |
| `binary-trees(16)`    | 4.60 ± 0.02 s   | 810.8 ± 7.3 ms  | 5.7×    |
| `alloc-stress(6000)`  | 843.8 ± 38.6 ms | 283.2 ± 4.5 ms  | 3.0×    |

Deeper recursion widens the gap (single-run wall time):

| benchmark         | VM     | JIT    | speedup | note                                  |
|-------------------|--------|--------|---------|---------------------------------------|
| `fib(35)`         | 1.87 s | 0.03 s | ~62×    |                                       |
| `fib(38)`         | 8.03 s | 0.12 s | ~67×    |                                       |
| `mandelbrot(600)` | 3.39 s | 0.28 s | ~12×    | JIT crashed pre-rc5; fixed by ADR 0019 |

The JIT is strongest on call-dispatch / integer arithmetic (~60×), strong
on flonum (14×), and tapers on allocation/GC-bound work (3×) — the
long-tail optimization target (issues #28, #47).

## 2. Cross-implementation microbench (small N)

```
benchmark        cs-walker  cs-vm   cs-jit    chez   guile  gambit  rust-O
fib                0.697s   0.030s  0.014s   0.038  0.048   0.040   0.013
tak                0.045s   0.018s  0.011s   0.037  0.023   0.019   0.013
ack                0.111s   0.027s  0.011s   0.042  0.019   0.017   0.013
nqueens            0.112s   0.036s  0.029s   0.053  0.029   0.029   0.013
mandelbrot         0.338s   0.072s  0.017s   0.046  0.040   0.046   0.013
spectral-norm      0.283s   0.091s  0.021s   0.040  0.024   0.028   0.014
binary-trees       0.132s   0.053s  0.019s    ERR    ERR    0.020   0.014
alloc-stress       0.125s   0.038s  0.019s   0.036  0.022   0.019   0.016
```

**Caveat:** at these N the `cs-jit` / Chez / Guile / Gambit / `rust-O`
columns are dominated by process startup (~10 ms floor) — read them as
"same class," not precise ratios; §1 is the real compute signal. Within
the floor, `cs-jit` is competitive with mature JIT/AOT Schemes (it leads
on fib / mandelbrot / spectral here). `racket` errored on the import
dialect; Chez/Guile error on `binary-trees`.

## 3. Real-world (Boyer-Moore theorem provers, p50)

| bench    | VM       | JIT      |
|----------|----------|----------|
| `nboyer` | 10.58 s  | 10.37 s  |
| `sboyer` | 12.69 s  | 10.47 s  |

Both run correctly on the JIT as of rc5 (issue #19 fix). JIT ≈ VM here
because the #19 fix conservatively routes the hot map-style rewriter
functions to the VM rather than miscompile them on the legacy pure-fixnum
tier — recovering that JIT speedup is issue #47.

## Tier summary

- walker → VM: ~3–11×
- VM → JIT: ~3–67× (workload-dependent)
- `cs-jit` matches/beats Chez/Guile/Gambit on CPU-bound micros.
- Remaining headroom: allocation-bound and map-style code
  (#28 inline small-pair storage, #47 map-style JIT coverage).
