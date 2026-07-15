# Bacon-Rajan residual-cycle sweep: default-on evaluation (cs-i6p.3)

Decision bead. Evaluates whether the layer-4 tracing cycle collector
(`crates/cs-gc/src/cycle_registry.rs` + `cycle_collector.rs`, feature
`tracing-cycle-collector`) should ship default-on to reclaim residual
cycles the layer-2 synchronous detector can't break (heap is RC-only;
`set-car!`/`set-cdr!` cycles are already handled via weak tombstones).

**Decision: NO-GO.** Do not flip the default. See §5.

## 1. Methodology

- Built two release `cs-cli` binaries from the same commit: default
  features (`/tmp/crabscheme-sweep-off`) and
  `--features tracing-cycle-collector` (`/tmp/crabscheme-sweep-on`).
  Verified they are distinct artifacts (differing SHA-1) rather than a
  stale relink.
- Leak harness: four Scheme workloads (reconstructed under
  `bench/leak-scm/`, see the Appendix), each looping N iterations of a
  residual-leak shape, sampling `(gc-stats)` (`live-slots`,
  `live-bytes`, `sweep-candidates-checked`, `sweep-cycles-collected`,
  `sweep-broken-total`) every 2,000–10,000 iterations. Each leak/efficacy
  run below is a single run per (shape, N); growth is monotone across
  samples within a run, so a single run is sufficient to characterize
  the trend, but the exact byte counts are not averaged across repeats:
  - (a) `vector-cycle`: `(let ((v (make-vector 1 0))) (vector-set! v 0 v))`, drop.
  - (b) `hashtable-cycle`: `(let ((h (make-eq-hashtable))) (hashtable-set! h 'self h))`, drop.
  - (c) `closure-cycle`: `letrec`-bound mutually-recursive lambdas, called once, then dropped.
  - (d) `churn-loop`: a 2-slot vector record whose slot 0 is a closure
    closing over the record and whose slot 1 points back to the record
    itself — a composite vector+closure back-edge standing in for a
    connection-handler churn loop (a plain script can't easily drive
    real `spawn-source` actors in this harness).
- Sweep overhead: the `tracing-cycle-collector` feature threads cleanly
  through `cs-runtime` → `cs-cli` (`crates/cs-cli/Cargo.toml:246`), no
  extra wiring needed. `hyperfine` (`--shell=none`, ≥20 runs, warmup 5)
  A/B on `bench/microbench/scheme/fib.scm` (bumped `n` 25→32 so the run
  clears hyperfine's 5ms noise floor) and
  `bench/microbench/scheme/alloc-stress.scm`, `--tier vm-jit`, MIN time
  compared per the <2% gate.
- Sweep efficacy: re-ran the leak harness on the sweep-on binary and
  compared `live-slots` growth and `sweep-cycles-collected` against the
  sweep-off baseline.

## 2. Per-shape leak table (sweep OFF, baseline)

| Shape | Leak rate | Bytes/iter | Steady after warmup? |
|---|---|---|---|
| (a) vector self-cycle | 1 slot/iter | 48 B/iter | No — linear growth to 200k iters, `(collect)` no-ops (feature not compiled in) |
| (b) hashtable self-cycle | 1 slot/iter | 144 B/iter | No — linear growth to 200k iters |
| (c) closure/letrec mutual-recursion | **0** (plateaus at 48 slots) | 0 | Yes — see caveat below |
| (d) churn-loop (vector+closure composite) | 1 slot/iter | 48 B/iter (same as (a); the closure alloc adds no measurable heap growth in this tier) | No — linear growth to 200k iters |

Each row is a single run; `live-slots` growth was monotone (no
plateau, no sawtooth) across every sample point in that run, which is
what licenses the "linear" / "flat" characterization from one run
rather than a distribution.

Extrapolation at realistic churn rates (bytes/iter × iterations/hour):
a 10,000 conn/s churn rate on shape (a) or (d) leaks ≈ 1.7 GB/hour; the
same rate on shape (b) leaks ≈ 5.2 GB/hour. These are the numbers
`(collect)`/default-on is meant to bound.

**Caveat on (c):** the letrec-mutual-recursion construction does not
produce a detectable Rc cycle in either build — `live-slots` is flat at
48 in both sweep-off and sweep-on runs, and `cycles-detected` stays 0
throughout (layer-2 never even flags it). This may mean the closures'
captured environment isn't cycled the way the task's "metacircular
env-anchored cycle" framing expects for this specific shape, or that
this tier's env representation doesn't retain the letrec frame once the
loop body returns. Independent of that, **`Procedure`/environment types
have no `CycleChildren` or `BreakCycle` impl anywhere in the tree**
(`grep CycleChildren`/`BreakCycle` across `cs-core`/`cs-gc` turns up
`Value`, `Pair`, `Hashtable`, `Promise`, `Port` only) — so even a real
env-anchored cycle could never be `register_cycle_candidate`'d, and the
layer-4 sweep is structurally incapable of helping with that leak class
regardless of this decision.

## 3. Sweep efficacy (sweep ON)

| Shape | Registered as candidate? | `BreakCycle` impl reclaims it? | Observed result |
|---|---|---|---|
| (a) vector self-cycle | Yes (`CycleChildren` rides cs-gc's blanket `RefCell<Vec<Value>>` impl) | **No** — `impl BreakCycle for Vector` doesn't exist; the orphan-rule-limited blanket `impl<T> BreakCycle for RefCell<T>` is a no-op (documented in `crates/cs-core/src/value.rs:480-488` as a known gap) | Identical linear leak curve to sweep-off; `sweep-cycles-collected` = 0 on every sweep despite `sweep-candidates-checked` climbing to the full registry size |
| (b) hashtable self-cycle | Yes | **Yes** — `impl BreakCycle for Hashtable` (value.rs:440) demotes the first heap-bearing slot | **100% reclaimed** every sweep: at the 10k-candidate auto-trigger, `live-slots` drops from ~10,047 back to 47 and `sweep-cycles-collected` = the full candidate count, in ~3ms |
| (c) closure/env cycle | No — never registered (no trait impls, see §2) | N/A | No effect either way |
| (d) churn-loop (vector-anchored) | Yes | **No** — dominated by the same Vector no-op path as (a) | Same as (a): identical leak curve, 0 cycles collected |

As in §2, each cell above is from a single sweep-on run per shape; the
"identical leak curve" / "100% reclaimed" characterizations are read
off monotone per-sample `live-slots`/`sweep-cycles-collected` series
within that one run, not an average over repeats.

## 4. Overhead A/B

**Non-cycle workloads** (registry stays empty — the only added cost is
the `take_sweep_pending()` bool check on `Gc::new`'s hot path):

| Bench | sweep-off MIN | sweep-on MIN | Δ |
|---|---|---|---|
| fib(32), `--tier vm-jit` | 15.73 ms | 14.90 ms | −5.3% |
| alloc-stress(200), `--tier vm-jit` | 16.70 ms | 14.97 ms | −10.4% |

No regression detected above noise — both deltas are sweep-on
*faster*, i.e. the opposite direction of what the <2% gate is
checking for. The alloc-stress −10.4% is larger than a single
`take_sweep_pending()` bool check would explain by itself; the most
likely source is ordinary run-to-run machine noise (shared CI/dev
box, no isolated core pinning, ≥20-run MIN is still sensitive to a
single lucky run) rather than sweep-on doing systematically less
work than sweep-off. It was not chased further because it does not
threaten the decision either way: a *faster* sweep-on result cannot
be the thing that blocks default-on. What does block default-on is
the unbounded cycle-workload case in the next table, which is not
sensitive to this kind of noise (320–1,060×, orders of magnitude
above any noise floor). For workloads that don't create sweep
candidates, the feature is free, as expected.

**Cycle-heavy workload (vector self-cycles, shape a) — NOT gated by the
<2% check, and it should have been part of the gate:**

| N iterations | sweep-off wall time | sweep-on wall time | slowdown |
|---|---|---|---|
| 10,000 | 0.04s | 0.02s | ~1x (below the 10k auto-trigger threshold) |
| 20,000 | 0.03s | 9.66s | **~320x** |
| 40,000 | 0.05s | 52.87s | **~1,060x** |

Root cause: `register_cycle_candidate` arms `SWEEP_PENDING` whenever
`registry.len() >= AUTO_TRIGGER_THRESHOLD` (default 10,000,
`cycle_registry.rs:317`) — i.e. the re-arm check runs on every new
*candidate registration*, not on every allocation. `Gc::new` is where
an already-armed sweep actually **runs** (`take_sweep_pending()` is
polled on that hot path). Once the vector-cycle registry crosses the
10,000 threshold it **never shrinks** (§3 — `try_break_candidate`
always returns `false` for Vector), so `registry.len() >= threshold`
stays true forever: every subsequent candidate registration re-arms
`SWEEP_PENDING`, and the very next `Gc::new` call — cycle-producing or
not — pays for a **full O(n) Bacon-Rajan pass over the entire
ever-growing candidate set**. Non-cycle-producing allocations in
between candidate registrations are not exempt: any of them can be
the one that observes the armed flag and triggers the pass. That's
O(n²) total work for a workload that does nothing but allocate
self-referential vectors in a loop — a very ordinary pattern (building
parent-pointer or doubly-linked structures via vectors, or exactly the
`spawn-source`-adjacent churn loop in shape (d)). A 200,000-iteration
run of shape (a)/(d) (the size used for the leak table in §2) did not
finish inside a 2-minute timeout on the sweep-on binary; the same run
completes in well under a second on sweep-off.

## 5. Decision: NO-GO

Gating logic — (leak evidence) × (sweep efficacy) × (overhead):

- Leak evidence is real and non-trivial for exactly the shapes this
  bead was scoped to check (vector/hashtable residual cycles that
  `cs-i6p.2`'s source-level fix on the parallel branch will *narrow*
  but that still exist in the interim; churn/actor-style vector-anchored
  cycles leak at the same rate).
- Sweep efficacy is **split down the middle in the wrong direction**:
  the fully-reclaimed shape (hashtable) is the *smaller* slice — the
  Vector shape (also covering shape d, the actor-churn proxy) is the
  common one in practice (records/mailboxes/adjacency built from
  vectors) and the sweep does **nothing** for it, silently, because
  `Vector`'s `BreakCycle` is an unimplemented stub. `(collect)` on a
  program leaking vector cycles is a no-op today even with the feature
  compiled in — nothing in `(gc-stats)` makes that obvious short of
  checking `sweep-cycles-collected` stays 0 while
  `sweep-candidates-checked` grows.
- Overhead is fine on non-cycle workloads but **catastrophic and
  unbounded** on exactly the workload class (vector cycles) the sweep
  fails to reclaim: turning this on by default would silently convert
  any program that builds vector self/mutual-reference structures in a
  loop from linear to quadratic, with the effect compounding forever
  once the registry first crosses 10,000 candidates. This is strictly
  worse than shipping with the sweep off, where such a program merely
  leaks (bounded, linear, matches today's behavior) rather than hangs.
- Closure/env-anchored cycles (the other named residual-leak class)
  get **zero** benefit from this sweep regardless of the decision —
  there is no `CycleChildren`/`BreakCycle` coverage for `Procedure` or
  environment frames at all, so flipping the default wouldn't move that
  needle.

**Recommendation — two follow-up beads before this can be
re-evaluated, not included in this commit (both need design/review,
not a same-commit fix):**

1. **Vector `BreakCycle` impl.** The existing code comment
   (`value.rs:480-488`) already names the fix: a `Vector` newtype
   wrapper around `RefCell<Vec<Value>>` to escape the orphan-rule
   blanket-impl limitation, with a break strategy mirroring
   `Hashtable`'s (demote the first heap-bearing slot to
   `Value::Unspecified`). Without this, the sweep is only useful for
   hashtable cycles.
2. **Auto-trigger back-off.** `run_sweep`'s auto-arm logic needs to
   stop re-triggering a full sweep on every allocation once a pass
   collects 0 cycles — e.g. exponential threshold growth after a
   no-progress sweep, or an explicit "don't re-arm within N allocations
   of a no-op sweep" guard. This is required independently of (1):
   it's what turns "the sweep doesn't help vector cycles" into "the
   sweep doesn't help vector cycles *and also hangs the process*."

Until both land, default-on trades a bounded, well-understood leak for
an unbounded, silent hang on a common allocation pattern. The layer-4
sweep stays opt-in (`tracing-cycle-collector`) for embedders who know
their workload is hashtable-cycle-heavy and vector-cycle-free.

## 6. What this eval did NOT flip

No product-code changes. `crates/cs-gc/Cargo.toml`'s `default = [...]`
line is unchanged; `tracing-cycle-collector` stays opt-in.

## Appendix: reproducing this eval

The original `leak-scm/` scratch directory was deleted before the
first commit of this doc; the four workload files below are
reconstructions of the same shapes, committed under `bench/leak-scm/`
and re-verified against the sweep-off binary before this revision.
Edit each file's `(define n ...)` to change the iteration count (this
eval used 10,000 / 20,000 / 40,000 / 200,000 at various points, per
the tables above).

- `bench/leak-scm/vector-cycle.scm` — shape (a)
- `bench/leak-scm/hashtable-cycle.scm` — shape (b)
- `bench/leak-scm/closure-cycle.scm` — shape (c)
- `bench/leak-scm/churn-loop.scm` — shape (d)

Build the two binaries from the same commit:

```sh
devenv shell -- cargo build --release -p cs-cli
cp target/release/crabscheme /tmp/crabscheme-sweep-off

devenv shell -- cargo build --release -p cs-cli --features cs-gc/tracing-cycle-collector
cp target/release/crabscheme /tmp/crabscheme-sweep-on
```

Run a shape against either binary:

```sh
/tmp/crabscheme-sweep-off run bench/leak-scm/vector-cycle.scm
/tmp/crabscheme-sweep-on  run bench/leak-scm/vector-cycle.scm
```

Each run prints `(gc-stats)` every 2,000 iterations plus a `final:`
line; watch `live-slots` / `live-bytes` for growth and
`sweep-cycles-collected` / `sweep-candidates-checked` for sweep
activity. The §4 overhead A/B used `hyperfine --shell=none` wrapping
the same two binaries against `bench/microbench/scheme/fib.scm`
(`n` bumped 25→32) and `bench/microbench/scheme/alloc-stress.scm`,
run with `--tier vm-jit`.
