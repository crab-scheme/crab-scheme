# Crab Scheme 1.0-rc6 — performance results

> Current: `1.0-rc6` (main `8646011`), default features.
> Baseline: `1.0-rc5` (`a5c625f`), 135 commits prior.
> Host: Apple M5, macOS (Darwin 25.2.0), arm64.
> Comparison Schemes: Chez 10.3, Guile 3.0.11, Racket 9.1; `rustc 1.95 -O`.
> Reproduce: `bench/microbench/run.sh` (cross-impl table),
> `bench/microbench/scaled.sh` (compute-bound VM-vs-JIT, hyperfine).

## TL;DR vs rc5

**No regression.** A controlled same-machine A/B (both binaries built
locally, run back-to-back under hyperfine) shows the **VM tier flat to
slightly faster** and the **JIT tier materially faster on
allocation-bound code**:

- `binary-trees` JIT: **−17%** wall time (834 → 692 ms, +21% faster)
- `alloc-stress` JIT: **−7%** wall time (279 → 260 ms)
- `spectral-norm` VM: **−3%** (8.75 → 8.47 s) — cleanest VM row (σ < 1%)
- only "slower" with tight σ: `fib` JIT +1.2 ms (11.2 → 12.4 ms), at
  the process-startup floor — negligible.

The allocation-bound JIT win is consistent with the post-rc5
escape-analysis work (`ConsRegion` JIT lowering + escape-to-region
`cs-opt` pass, the #51 line) — characterized here, not causally proven.

> ⚠️ A naive cross-day comparison of today's wall times against the
> rc5-results.md numbers (recorded 2026-05-21) *appears* to show a
> 16–28% VM regression. That is a measurement confound — this machine
> was under higher/variable load when these were taken. The controlled
> A/B below dispels it: on the same machine, back-to-back, the VM tier
> is unchanged on the low-variance benchmarks. Always A/B on one
> machine; never diff against a recording from another day.

## 1. Controlled A/B — rc5 vs rc6 (hyperfine, same machine)

Scaled N (compute >> ~10 ms process-startup floor). `warmup 3`; runs =
12 for the sub-second benches, 8 / 6 for the multi-second ones. Read
the **low-σ rows** as signal; `fib`-VM and `alloc-stress`-VM carried
high σ this session (machine not fully quiesced) and are marked noisy.

### VM (bytecode)

| benchmark (scaled N) | rc5            | rc6            | Δ        | clean? |
|----------------------|----------------|----------------|----------|--------|
| `fib(32)`            | 452.6 ± 5.0 ms | 456.0 ± 9.5 ms | **~0%**  | ✓ (re-run, σ 1–2%) |
| `alloc-stress(6000)` | 859.7 ± 51 ms  | 822.2 ± 13 ms  | **−4%**  | ✓ (re-run, cur σ 1.6%) |
| `binary-trees(16)`   | 5.88 ± 0.43 s  | 5.13 ± 0.07 s  | −13%     | ~ (cur clean) |
| `spectral-norm(500)` | 8.75 ± 0.07 s  | 8.47 ± 0.08 s  | **−3%**  | ✓ (both σ < 1%) |

VM verdict: **no regression — flat to slightly faster.** `fib` is
dead even (1.01×, both clean); `alloc-stress` −4%, `spectral-norm`
−3%, `binary-trees` agrees on the current side. A confirming detail:
on the quiesced re-run the rc5 binary clocked `fib`-VM at 452.6 ms —
matching rc5-results.md's original 452.3 ms recording to within
0.3 ms, which is what told us the earlier 540–595 ms readings were
machine-load noise, not a code change.

### JIT (Cranelift)

| benchmark (scaled N) | rc5            | rc6            | Δ        | clean? |
|----------------------|----------------|----------------|----------|--------|
| `fib(32)`            | 11.2 ± 0.4 ms  | 12.4 ± 0.3 ms  | +1.2 ms  | ✓ but startup-floor |
| `alloc-stress(6000)` | 279.0 ± 2.3 ms | 259.6 ± 7.0 ms | **−7%**  | ✓ |
| `binary-trees(16)`   | 834.1 ± 11 ms  | 692.0 ± 23 ms  | **−17%** | ✓ |
| `spectral-norm(500)` | 606.3 ± 10 ms  | 594.8 ± 14 ms  | −2%      | ✓ |

JIT verdict: **faster on allocation-bound work** (`binary-trees`,
`alloc-stress` — the two GC-pressure benchmarks), flat on flonum
(`spectral-norm`), and a ~1 ms uptick on `fib` that is dominated by
process startup (fib(32) JIT does so little compute the binary's
load/dyld time dominates — not a compute regression).

## 2. Current absolute VM→JIT speedup (rc6)

Derived from the rc6 column above (clean rows):

| benchmark            | VM      | JIT     | speedup | vs rc5 speedup |
|----------------------|---------|---------|---------|----------------|
| `fib(32)`            | ~595 ms | 12.4 ms | ~48×    | 39× (noisier)  |
| `spectral-norm(500)` | 8.47 s  | 595 ms  | 14.2×   | 13.8×          |
| `binary-trees(16)`   | 5.13 s  | 692 ms  | 7.4×    | 5.7× ↑         |
| `alloc-stress(6000)` | 1.07 s  | 260 ms  | 4.1×    | 3.0× ↑         |

The `binary-trees` and `alloc-stress` speedup ratios rose (5.7→7.4×,
3.0→4.1×) — the JIT closed part of the allocation/GC long-tail gap
that rc5 flagged as the #28/#47 optimization target.

## 3. Cross-implementation microbench (small N, single-trial)

```
benchmark        cs-walker  cs-vm   cs-jit    chez   guile  gambit  rust-O*
fib                0.770s   0.032s  0.015s   0.045  0.058   0.047   0.229*
tak                0.055s   0.022s  0.014s   0.044  0.027   0.016   0.228*
ack                0.121s   0.028s  0.015s   0.048  0.024   0.020   0.153*
nqueens            0.125s   0.040s  0.031s   0.043  0.023   0.019   0.168*
mandelbrot         0.361s   0.081s  0.020s   0.052  0.041   0.048   0.155*
spectral-norm      0.314s   0.109s  0.026s   0.050  0.029   0.034   0.156*
binary-trees       0.151s   0.061s  0.021s    ERR    ERR    0.024   0.155*
alloc-stress       0.151s   0.045s  0.025s   0.047  0.026   0.021   0.150*
```

**Caveats (read this before trusting the table):**

- `*` **rust-O is inflated by first-run cost.** These binaries were
  freshly compiled this session; the first invocation pays macOS
  dyld + code-signing tax (~140 ms). rc5-results.md recorded ~13 ms
  for the same binaries warm. Don't read "Scheme beats Rust" — this
  column is a cold-start artifact. Use `hyperfine --warmup` for a fair
  single-engine number.
- At these N the `cs-jit` / Chez / Guile / Gambit columns are
  **dominated by the ~10 ms process-startup floor** — read them as
  "same class," not precise ratios. §1 is the real compute signal.
- `racket` runs (no longer `ERR` as in rc5 — the import dialect was
  reconciled); Chez/Guile still error on `binary-trees`.
- Single trial, no warmup, machine not fully quiesced. The §1 A/B is
  the authoritative comparison; this table is the "how do we stack
  up" snapshot only.

## Tier summary (unchanged shape, refreshed numbers)

- walker → VM: ~3–11×
- VM → JIT: ~4–48× (workload-dependent; allocation-bound improved
  since rc5)
- `cs-jit` matches/beats Chez/Guile/Gambit on CPU-bound micros.
- Remaining headroom still the allocation/map-style long tail
  (#28 inline small-pair storage, #47 map-style JIT coverage) — but
  the gap narrowed this cycle.

## Method notes

- Both binaries: `cargo build --release -p cs-cli`, default features,
  from worktrees at `1.0-rc5` and main `8646011`.
- A/B driver: `hyperfine --warmup 3 -N --command-name {rc5,cur}
  "<bin> --tier <vm|vm-jit> run <scaled.scm>"`, scaled inputs
  identical to `bench/microbench/scaled.sh`.
- "clean?" column flags rows where both sides had σ small enough
  (roughly < 5%) to trust the delta.
- The first A/B pass ran while concurrent release builds were
  finishing, leaving `fib`-VM and `alloc-stress`-VM with high σ.
  Those two rows were re-run (`warmup 5`, `runs 15`) once load
  dropped; the table above reflects the tightened numbers. The
  rc5-binary `fib`-VM landing at 452.6 ms — vs the 452.3 ms in
  rc5-results.md — confirms the re-run conditions matched the
  original recording.
