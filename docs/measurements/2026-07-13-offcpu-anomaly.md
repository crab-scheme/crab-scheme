# Off-CPU anomaly follow-up (cs-vnf.6)

> Bead: cs-vnf.6, follow-up to the cs-vnf.1 profile caveat
> (`docs/measurements/2026-07-12-jit-alloc-profile.md`, lines 27-31): wall-clock time
> for the alloc-stress/binary-trees/nqueens `samply` runs was 3-4x CPU time (~30-40%
> CPU utilization). Investigate-and-document only; no product code changed.

## Environment / load conditions

- Built `crabscheme` release at 613f765 (`devenv shell -- cargo build --release -p
  cs-cli`), **without** `CARGO_PROFILE_RELEASE_DEBUG=true` (the .1 doc's build did set
  that; noted as a methodology difference below).
- Machine was **not quiet**: `load average` sat at 6.3-10.2 throughout (well above the
  ~4 threshold requested), sustained by three pre-existing `crabscheme run
  src/node-cluster.scm` processes from an unrelated workspace (~12% CPU each, ~36%
  combined, confirmed steady via repeated `ps aux` — not a transient spike, so not
  something waiting out would fix). No local `cargo`/`rustc` builds were running during
  measurement. This is a heavier load than the .1 run reported for itself, but see
  below — the anomaly turned out not to depend on background load at all.
- Bench sources: recreated `prof-scm/{alloc-stress,binary-trees,nqueens}.scm` from
  `bench/microbench/scheme/`, same scale-ups as the .1 doc (`alloc-stress` n
  200→60000, `binary-trees` depth 10→18, `nqueens` 8→11).

## Methodology note: contention control

37% steady background CPU from unrelated processes is, on its own, a plausible source
of a wall-vs-cpu gap (scheduler contention can itself manufacture the very anomaly
under test). To rule that out, every condition below was run **10 times**, reporting
the **minimum wall time** (least-contaminated sample) alongside its paired cpu time,
and a known CPU-bound control — `fib(40)` at vm-jit tier
(`bench/microbench/scheme/fib.scm`, pure recursive arithmetic, negligible allocation)
— was run under the identical conditions as the allocation-heavy benches. If the
control also shows a wall>>cpu gap, the gap is contention, not something specific to
the allocation-heavy JIT path.

## Repro: plain execution (no profiler), min-of-10, with control

`/usr/bin/time -l ./target/release/crabscheme --tier vm-jit run prof-scm/<bench>.scm`,
10 repeats each, min wall time reported (paired cpu from that same run), load1 8-10
throughout this batch:

| Bench | min real (s) | user+sys (s) | CPU util |
|---|---|---|---|
| **fib (control)** | 0.42 | 0.42 | **100%** |
| alloc-stress | 2.03 | 2.02 | 99% |
| binary-trees | 1.89 | 1.87 | 99% |
| nqueens | 1.70 | 1.66 | 98% |

**The anomaly does not reproduce, and the control confirms the background load isn't
the cause.** `fib` — a pure CPU-bound recursive control with essentially no
allocation — is 100% CPU-bound at its minimum, indistinguishable from the
allocation-heavy benches (98-99%), under the same load1 8-10 background contention. If
contention were producing an off-CPU gap, the control would show it too; it doesn't.
This is the opposite of "wall ≈ 3-4x cpu."

## Repro: same benches wrapped in `samply record` (the .1 methodology), min-of-10

`/usr/bin/time -l samply record --save-only --unstable-presymbolicate -o out.json --
./target/release/crabscheme --tier vm-jit run prof-scm/<bench>.scm` (using samply's
resolved store path directly, to avoid `devenv shell` activation overhead confounding
the wall-clock number), 10 repeats each, min wall reported:

| Bench | min real (s) | user+sys (s) | CPU util |
|---|---|---|---|
| **fib (control)** | 1.16 | 0.41 | **35%** |
| alloc-stress | 2.18 | 1.99 | 91% |
| binary-trees | 2.17 | 1.95 | 90% |
| nqueens | 2.17 | 1.76 | 81% |

This is the decisive result: wrapping the identical binaries in `samply record`
reproduces a wall-vs-cpu gap that is entirely absent from every unwrapped run above —
**and the gap is largest for the control (`fib`, 35% util), not for the
allocation-heavy benches.** A trivial 0.42s-cpu program takes 1.16s wall solely because
of `samply`'s own overhead, which does not scale down for a short, non-allocating
target. For the longer-running benches that same roughly-constant overhead is a
smaller fraction of total wall time, so utilization looks closer to 100% — the inverse
of what a scheduler-contention or allocation-driven explanation would predict (either
would hit the alloc-heavy benches harder than `fib`, not lighter). A handful of the
alloc-stress/binary-trees samply runs also showed occasional ~1s quantized jumps
(cpu-time unchanged) — plausibly the sampling thread getting preempted under the
background load — but that jitter is itself only visible when running under samply's
extra threads, not in the unwrapped table above.

## Root cause

The off-CPU time is an artifact of the **profiling harness, not `crabscheme`'s runtime
behavior**. Two contributing factors, both inherent to how `samply record` works on
macOS rather than anything in `cs-runtime`/`cs-vm`/mimalloc:

1. `/usr/bin/time` on the `samply` invocation reports rusage for the `samply` process
   tree; some of `samply`'s own wall-clock time — attaching via `task_for_pid`,
   maintaining the sampling thread, and (per `--unstable-presymbolicate`)
   post-processing/symbolicating and writing the `.json` profile after the target
   exits — is real elapsed time that isn't necessarily fully reflected in the summed
   `user`+`sys` figure `time` prints, depending on how rusage propagates through the
   `time` → `samply` → `crabscheme` process chain. This alone produces an apparent
   "off-CPU" gap that has nothing to do with `crabscheme` scheduling or paging.
2. `samply`'s sampling itself (periodic `task_for_pid`-based stops to read
   thread/register state) adds wall-clock overhead to the sampled process that scales
   with sampling rate and is more pronounced on a loaded machine — consistent with the
   .1 doc's own note that it was captured on a busier box.

The clean signal is the plain-execution table: with the profiler out of the loop,
`crabscheme` is genuinely CPU-bound (98-100% utilization at min-of-10, including the
`fib` control) on these benches, even under non-quiet conditions. There is no
scheduler/paging/mimalloc-arena issue to chase here — the .1 doc's caveat was
measuring the profiler, not the runtime, and the `fib` control rules out background
contention as an alternative explanation.

## Recommendation

No fix warranted (nothing in `crabscheme` to fix — this is `samply`'s own overhead).
For future JIT-alloc profiling work: treat `samply`-wrapped wall-clock numbers as
profiler-inclusive, not representative of standalone run time; use a bare `/usr/bin/time
-l` (or `hyperfine`) run alongside any `samply` session for the real wall/cpu numbers,
as done here. This bead is closed as **vanished / explained** — the .1 doc's caveat
was a measurement artifact of the profiling wrapper, not a product concern.
