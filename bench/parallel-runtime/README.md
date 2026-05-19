# parallel-runtime bench harness

Six acceptance benchmarks for the parallel-runtime spec
(`.spec-workflow/specs/parallel-runtime/`). Each gate from
the spec maps to one Scheme file in `schemes/`.

## Running

```bash
bench/parallel-runtime/runner.sh                 # all six
bench/parallel-runtime/runner.sh --bench echo-10m
bench/parallel-runtime/runner.sh --time-budget 600
```

The runner builds `crabscheme` with `--features
"actor,regions,tracing-cycle-collector"` then runs each
`schemes/*.scm` file under that binary. Each bench script
prints a final `OK <metric>` line on success or `FAIL
<reason>` and exits non-zero. The runner reports
pass/fail per bench.

Per-bench stdout is captured in `results/<name>.log`.

## Benches → spec gates

| File                          | Gate     | Exercises                                       |
|-------------------------------|----------|--------------------------------------------------|
| `spawn-1m.scm`                | G1 spawn | C1.1 spawn-async (no thread-per-actor ceiling) |
| `echo-10m.scm`                | G1 echo  | C1.1 + cs-actor mpsc + payload conversion       |
| `cpu-bound-vs-responder.scm`  | G2       | C2.1 + C2.2 cooperative-yield seam              |
| `region-actor.scm`            | G3       | C3.1 dual-stack regions + auto-promotion        |
| `cycle-n-pair.scm`            | G4       | C4 full BR collector + gc-stats surface         |
| `long-soak.scm`               | G6       | mixed workload, leak / steady-state check       |

## Scale

The shipped benches run at **smoke scale** (100k spawns, 10k
echoes, 1k cycles, 10s soak) rather than the full headline
targets (1M spawns, 10M echoes, 10k cycles, 1h soak). Smoke
scale validates the mechanisms work end-to-end without
exceeding CI runtime budgets. Bump the constants at the top
of each bench file to exercise the full gate.

## What's not here yet

- **Metric capture / JSON output.** The realworld bench
  harness (`bench/realworld/`) emits one JSON line per run
  for `render.py`. The parallel-runtime harness currently
  just gates pass/fail. Adding the JSON path is the natural
  next iteration once the headline-scale targets stabilize.
- **CI workflow.** C6.3 calls for
  `.github/workflows/parallel-runtime-bench.yml` to run the
  suite nightly + on tagged releases. Deferred — adding CI
  is invasive enough to warrant separate review.
- **Cross-implementation comparison.** The realworld
  harness compares against Chez / Gambit / Racket / Guile.
  None of those run actors the way parallel-runtime
  defines them, so there's no apples-to-apples cell to
  fill. crabscheme-only is the right shape here.
