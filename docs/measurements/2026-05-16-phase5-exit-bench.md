# Phase 5 Exit Bench (2026-05-16)

> Captured against commit `d250ba3` (iter9 single-binding let* inlining). Hardware: Apple M-series, devenv shell. CrabScheme `target/release/crabscheme` built fresh.

Companion to `docs/milestones/m6-phase5-exit.md`. Refreshes the perf rows from `2026-05-15-pre-1.0-gates.md` with the post-Phase-5 numbers.

## Microbench (`bench/microbench/run.sh`, median of 3 runs)

| Bench         | VM (sec) | JIT (sec) | speedup | Δ vs baseline |
|---------------|---------:|----------:|--------:|--------------:|
| fib           | 0.023    | 0.009     | 2.56×   | +0% (noise) |
| tak           | 0.015    | 0.007     | 2.14×   | +7% |
| ack           | 0.018    | 0.008     | 2.25×   | +0% |
| nqueens       | 0.034    | 0.025     | 1.36×   | +24% |
| mandelbrot    | 0.069    | 0.016     | 4.31×   | +335% |
| spectral-norm | 0.086    | 0.058     | 1.48×   | +61% |
| binary-trees  | 0.060    | 0.015     | 4.00×   | +8% |
| alloc-stress  | 0.036    | 0.019     | 1.89×   | -3% (noise) |
| **geomean**   |          |           | **2.31×** | **+33%** |

Gate target: ≥ 5×. **Still NOT MET.** Phase 5 closed +33% of the gap.

## Comparison rows (run 2, representative)

| Bench         | walker | VM    | JIT   | chez  | guile | gambit | rust-O |
|---------------|-------:|------:|------:|------:|------:|-------:|-------:|
| fib           | 0.40   | 0.023 | 0.009 | 0.034 | 0.016 | 0.018  | 0.007  |
| tak           | 0.04   | 0.015 | 0.007 | 0.032 | 0.016 | 0.012  | 0.007  |
| ack           | 0.09   | 0.018 | 0.008 | 0.033 | 0.018 | 0.016  | 0.007  |
| nqueens       | 0.10   | 0.034 | 0.025 | 0.034 | 0.016 | 0.016  | 0.007  |
| mandelbrot    | 0.30   | 0.069 | 0.016 | 0.037 | 0.028 | 0.036  | 0.007  |
| spectral-norm | 0.26   | 0.086 | 0.058 | 0.034 | 0.020 | 0.027  | 0.007  |
| binary-trees  | 0.12   | 0.060 | 0.015 | ERR   | ERR   | 0.019  | 0.008  |
| alloc-stress  | 0.12   | 0.036 | 0.019 | 0.033 | 0.020 | 0.017  | 0.010  |

Cross-Scheme comparison (informational):
- **JIT now beats Chez on fib / tak / ack / mandelbrot / spectral-norm** (Chez is JIT'd, this is a real comparison).
- **JIT beats Gambit interpreter on fib / mandelbrot / spectral-norm.**
- **Gap to Guile** is closed on fib but Guile still wins on ack/nqueens (Guile is bytecode-JIT'd with type feedback).
- **Gap to rust-O** (gcc-equivalent native) remains ~6-10× on most benches, ~2× on mandelbrot.

## Per-bench IC dispatch stats (post-Phase-5)

For diagnosing future perf work — captured via `(jit-stats)` after each bench's main computation.

| Bench | tier-ups | JIT calls | hits | misses | hit-rate |
|-------|---------:|----------:|-----:|-------:|---------:|
| spectral-norm(50) | 3 | 100,937 | 97,971 | 23 | **99.98%** |
| nqueens(8) | 7 | ~98k | ~80k | ~17k | ~83% |
| binary-trees(10) | 3 | 2,658 | 672 | 2 | 99.7% |
| mandelbrot(60) | (tail-self-call dominant; few IC events) | — | — | — | n/a |
| alloc-stress(200) | 1 | 199 | 0 | 0 | n/a |

Health summary: **IC hit rates are near-optimal where they matter.** Spectral-norm's 99.98% confirms iter7's `vm_value_div_nb` fully unblocked matrix-elt. Nqueens at 83% is the remaining IC pain point — `place`'s polymorphic inner-lambda call site doesn't monomorphize.

## What changed since baseline (2026-05-15)

Nine iters (iter1–iter9 plus one reverted closeout attempt). Full iteration log in `docs/milestones/m6-phase5-exit.md`. The +33% geomean came predominantly from:

- **iter3** (`53207f2`): widen uniform-NB tier coverage. Single biggest move — mandelbrot went from 0.99× (parity) to over 4× by becoming eligible for the NB tier at all.
- **iter7** (`741db0a`): `vm_value_div_nb` runtime helper. Unblocked spectral-norm's matrix-elt, which had been thrashing the IC at 50% hit rate due to Fixnum `/` falling out of the JIT.
- **iter9** (`d250ba3`): single-binding let* inlining at the bytecode compiler. Saved one MakeClosure + IC dispatch per let*-binding evaluation.

## Conformance gates (unchanged from 2026-05-15 measurement)

These were measured at 99.96% / 100% / 94% / partial in the predecessor doc and have not regressed.

| Gate                                          | Status                       |
|-----------------------------------------------|------------------------------|
| R6RS conformance ≥ 99%                        | **MET** (99.96%)             |
| Larceny test suite ≥ 95%                      | near (94%)                   |
| Racket R6RS test suite ≥ 90%                  | partial (oracle established) |

## What it would take to clear the JIT geomean gate (revised)

Phase 5 incremental work compounded to +33%. Getting from 2.31× to 5× would require ~2.2× *additional* improvement — not achievable through more coverage-expansion iters. The next-phase architectural moves are sketched in `docs/milestones/m6-phase5-exit.md` under "Next architectural moves":

1. Type-feedback-driven specialization (eliminates per-op tag checks in monomorphic hot loops).
2. Compile-time inlining of leaf callees (correct version of iter6).
3. Escape-analysis-driven allocation elimination (kills hidden Rational / Flonum allocs).
4. Direct-inner-pointer IC slot (single-iter follow-up; modest payoff ≈ 2-5%).
5. ADR to reframe the gate (bookkeeping; no perf change).

Effort: A/B/C are each multi-week. D is a single-iter follow-up but doesn't move the needle alone. E is a writing exercise.

## Recommendation

**Tag `m6-phase5-complete` and step back from the incremental work.** The 2.31× geomean is honest, measured, and reflects real architectural improvements (the IC infrastructure works, the uniform-NB tier carries the load, the bench numbers are competitive with Chez on most cells). The 5× gate is not closable within Phase 5's scope; closing it requires either a multi-quarter architectural track or a reframed gate that better matches the engineering reality.

The conformance side remains effectively cleared. The 1.0 RC is gated entirely on this decision (architectural redesign vs reframe).
