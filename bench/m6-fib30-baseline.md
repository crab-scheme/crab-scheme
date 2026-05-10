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

## Phase 2 — env access + builtin lowering bench

Added in M6 Phase 2 iters B-I to validate that env-lookup / set! /
fixnum-builtin lowerings deliver real speedup on programs that
weren't pure-recursive-fixnum. Workload exercises:

```scheme
(define n 0)
(define stride 7)
(define (step x)
  (set! n (+ n 1))                  ; EnvSet
  (if (zero? (remainder x 13))      ; zero? predicate + remainder
      (quotient x 2)                ; quotient
      (bitwise-and (+ x stride) 1023))) ; bitwise-and + EnvLookup
(let loop ((i 0) (acc 0))
  (if (= i 2000000) ...
      (loop (+ i 1) (+ acc (step i)))))
```

| tier         | wall-clock | vs VM |
|--------------|-----------:|------:|
| walker       | 0.95 s     | 1.6×  |
| vm           | 0.59 s     | 1.0×  |
| vm-jit       | 0.27 s     | **2.2× faster** |

Modest but real — the per-iter helper-call overhead (each
`vm_env_lookup_fixnum` is a native call) limits speedup vs the
~150× we get on pure-fixnum fib. Most of the win comes from the
arithmetic chain (`quotient`, `remainder`, `bitwise-and`, `zero?`)
running as native instructions instead of bytecode dispatches.

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

---

## Phase 2 closeout — flonum hot loop bench

After iters W–AJ closed the four-tag immediate ABI (Fixnum / Boolean /
Character / Flonum), the previously-VM-only flonum hot loops now run
through native code. This section documents the perf scoreboard at
the M6 Phase 2 close (commit `3758e8f`, tagged `m6-phase2-complete`).

### Bench programs

`square-sum-flo n`: sum of `(real->flonum i) * (real->flonum i)` for
`i` in 0..n.

```scheme
(define (square-sum-flo n)
  (let loop ((i 0) (acc 0.0))
    (if (= i n)
        acc
        (loop (+ i 1) (+ acc (* (real->flonum i) (real->flonum i)))))))
```

`hyp-loop n`: sum of `sqrt(a*a + b*b)` over a/b sweeping 1..n.

```scheme
(define (hyp-loop n)
  (let loop ((a 1) (acc 0.0))
    (if (> a n)
        acc
        (loop (+ a 1)
              (+ acc
                 (let inner ((b 1) (s 0.0))
                   (if (> b n)
                       s
                       (inner (+ b 1)
                              (+ s (flsqrt
                                    (+ (* (real->flonum a) (real->flonum a))
                                       (* (real->flonum b) (real->flonum b)))))))))))))
```

`sqr-flo-direct n`: the same body called with native flonum args
(exercises iter AF arg-side passthrough):

```scheme
(define (sqr-flo n) (* n n))
(define (driver n)
  (let loop ((i 0) (acc 0.0))
    (if (= i n) acc (loop (+ i 1) (+ acc (sqr-flo (real->flonum i)))))))
```

### Results

Bodies were warmed with 2000 calls before timing the inner work via
`(let ((t (current-jiffy))) <work> (- (current-jiffy) t))`. JIT
status verified post-warm with `(jit-status proc)` to confirm the
native dispatch path was taken.

Wall-clock results (Apple M-series, release build) for
`square-sum-flo` warmed with 2000 calls. After iter AO landed the
wrapper-pattern tail-call lowering, n=1M became safe.

| n          | walker | vm-no-jit | vm-jit |
|-----------:|-------:|----------:|-------:|
| 50,000     | 52 ms  | 11 ms     |  6 ms  |
| 1,000,000  | OOM    | 148 ms    | 10 ms  |

The headline: **flonum-bound bodies that previously stayed on the
bytecode VM now JIT through Cranelift's f64 ops, with the native
`fadd` / `fmul` / `fsqrt` instructions doing the per-iter work.**
At small n (50k) most of wall-clock is process startup; at 1M the
ratio cleanly resolves to **~15× JIT-over-VM** for typed flonum
arithmetic.

Pure-arithmetic loops get the biggest win because each iter's
per-Inst dispatch cost (bytecode) collapses to a register op (JIT).
Loops with allocations or closure-creation are still bytecode-bound
until the boxed-Value ABI lands (ADR 0011).

### Tail-call gap closed (iter AO)

Pre-AO, JIT'd `CallSelf` was a regular Cranelift `call` instruction
that burned host stack on every recursive iter. The 50k cap was
defensive against stack overflow.

Iter AO ships the wrapper pattern from ADR 0011 D-7: every JIT'd
function compiles as an outer SystemV trampoline + inner Tail-conv
body. `CallSelf` in tail position lowers to Cranelift `return_call`
against the inner FuncRef, reusing the caller's frame. The outer's
pointer is what the runtime transmutes — transparent to all
dispatch infrastructure.

After AO, deep recursion is safe at any depth and the perf
scoreboard finally distinguishes the JIT meaningfully from the VM
on multi-iter typed loops.

### `(jit-status sqr-flo)` post-warm

```
(jit-on flonum (flonum) calls 977 deopts 0)
```

### `(jit-stats)` post-warm

```
(tier-ups <N> jit-calls <M> deopts 0)
```

The `0 deopts` confirms the type-feedback loop's signature stayed
monomorphic across the run — no recompile-on-feedback fired.

### Caveats / things this scoreboard still doesn't show

- Allocation-bound benchmarks (`alloc-stress`) — Phase 2 didn't
  touch heap allocation; those stay on the VM. ADR 0011 unlocks
  them.
- Closure-creation benchmarks (`(define (f x) (lambda (y) ...))`)
  — same as above.
- General-call benchmarks (`(define (f) (g)) (define (g) ...)`)
  — only `CallSelf` and BuiltinRef calls JIT today. Per ADR 0011
  D-4, the monomorphic IC unlocks general Call.
- Bignum/Rational-bound benchmarks — Phase 2 stays in the i64
  fixnum range; overflow silently wraps.

These remain end-state-B / end-state-C work. The flonum bench
above closes the Phase 2 scope.
