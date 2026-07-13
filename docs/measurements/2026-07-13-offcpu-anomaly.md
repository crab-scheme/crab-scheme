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

## Repro: plain execution (no profiler)

`/usr/bin/time -l ./target/release/crabscheme --tier vm-jit run prof-scm/<bench>.scm`,
2 repeats each, under the load conditions above:

| Bench | real (s) | user (s) | sys (s) | CPU util |
|---|---|---|---|---|
| alloc-stress | 2.02–2.23 | 1.96–2.16 | 0.03–0.04 | 97–99% |
| binary-trees | 2.10–2.38 | 1.77–1.98 | 0.28–0.30 | 93–98% |
| nqueens | 1.81–1.84 | 1.69–1.73 | 0.08–0.09 | 97–98% |

**The anomaly does not reproduce.** Wall time tracks CPU time almost exactly (93-99%
utilization) across all three benches, across repeats, even under the elevated
background load described above. This is the opposite of "wall ≈ 3-4x cpu."

## Repro: same benches wrapped in `samply record` (the .1 methodology)

`/usr/bin/time -l samply record --save-only --unstable-presymbolicate -o out.json --
./target/release/crabscheme --tier vm-jit run prof-scm/<bench>.scm` (using samply's
resolved store path directly, to avoid `devenv shell` activation overhead confounding
the wall-clock number):

| Bench | real (s) | user (s) | sys (s) | CPU util |
|---|---|---|---|---|
| alloc-stress | 3.23 | 2.20 | 0.14 | 72% |
| binary-trees | 3.19 | 1.92 | 0.41 | 73% |
| nqueens | 3.20 | 1.85 | 0.17 | 63% |

Wrapping the identical binary/benches in `samply record` reproduces a real wall-vs-cpu
gap (63-73% utilization) that is entirely absent from the unwrapped runs above. The gap
is not as extreme as the .1 doc's reported 30-40%; the residual difference is plausibly
the .1 build's `CARGO_PROFILE_RELEASE_DEBUG=true` (debug info makes presymbolication
write-out — the dominant term, see below — much heavier) plus a busier box that day,
but that attribution is **untested**, not verified here.

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

Split measurement (verification pass): timestamping the samply-wrapped nqueens run's
output shows the workload finished at t+2.24s (vs ~1.9s unwrapped → only ~0.3s in-run
sampling tax, ~85% util during execution), with ~1.1s spent AFTER the workload exited
on presymbolication/write-out (total 3.33s real / 2.16s CPU = 65%). The gap is
dominated by the post-run write-out term, confirming factor 1 as the primary mechanism.

The clean signal is the plain-execution table: with the profiler out of the loop,
`crabscheme` is genuinely CPU-bound (93-99% utilization) on these benches, even under
non-quiet conditions. There is no scheduler/paging/mimalloc-arena issue to chase here —
the .1 doc's caveat was measuring the profiler, not the runtime.

## Recommendation

No fix warranted (nothing in `crabscheme` to fix — this is `samply`'s own overhead).
For future JIT-alloc profiling work: treat `samply`-wrapped wall-clock numbers as
profiler-inclusive, not representative of standalone run time; use a bare `/usr/bin/time
-l` (or `hyperfine`) run alongside any `samply` session for the real wall/cpu numbers,
as done here. This bead is closed as **vanished / explained** — the .1 doc's caveat
was a measurement artifact of the profiling wrapper, not a product concern.
