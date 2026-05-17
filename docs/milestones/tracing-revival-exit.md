# Tracing Revival — Exit Report

> Tagged at the merge commit of this report.
> Predecessor: escape-analysis (`docs/milestones/escape-analysis-exit.md`).
> Spec: `.spec-workflow/specs/tracing-revival/` (CLOSED, with
> known deferrals — see "Deferred work" below).
> ADR: `docs/adr/0018-tracing-cycle-collector.md`.

This report closes all 5 iters of the tracing-revival spec.
Layer 4 of the unified memory architecture ships as an
opt-in candidate-registry-based residual cycle collector.
The full Bacon-Rajan trial-deletion phase is deferred; what
landed is the infrastructure (registry + sweep triggers)
ready for a future iter to drop the trial-deletion algorithm
into.

---

## Acceptance summary

| Gate | Spec § | Result |
|---|---|---|
| FR-1: `tracing-cycle-collector` feature, off by default | requirements.md | **✅** added to cs-gc / cs-core / cs-vm / cs-runtime with forward chain. |
| FR-2: `cs_gc::cycle_registry` API | requirements.md | **✅** `register_cycle_candidate`, `unregister_cycle_candidate`, `candidate_count`, `set_auto_trigger_threshold`, `run_sweep`, `take_sweep_pending`, `reset_for_tests`. |
| FR-3: triggers (manual, auto, embedder) | requirements.md | **✅** `(collect)` Scheme builtin + `Gc::new` auto-trigger + `Runtime::set_tracing_policy`. Background-sweep deferred (see below). |
| FR-4: layer-2 detector populates registry | requirements.md | **✅** `record_cycle_with_candidate` + all 6 mutation-builtin callsites. |
| FR-5: region values excluded | requirements.md | **✅** `is_region` short-circuit before registration. |
| FR-6: sweep reclaims residual cycles | requirements.md | **Partial** — Phase 1 (drop dead-Weak entries) shipped; Phase 2/3 (Bacon-Rajan trial-deletion) deferred. |
| FR-7: integration with iter 7.1.x-z design | requirements.md | **Future** — the trial-deletion when it lands will reuse iter 7.1.x.z's algorithm in a controlled candidate-set context. |
| NFR-1: zero overhead when feature off | requirements.md | **✅** all layer-4 code paths cfg-gated; without `tracing-cycle-collector` the binary is identical. |
| NFR-2: per-thread registry | requirements.md | **✅** `thread_local!`-backed; no cross-thread sync. |
| NFR-3: embedder configurability | requirements.md | **✅** `TracingPolicy { auto_trigger_threshold }` + `Runtime::set_tracing_policy`. |
| NFR-4: conformance preserved | requirements.md | **✅** 117/117 cs-cli conformance. |
| NFR-5: ADR 0018 written | requirements.md | **✅** `docs/adr/0018-tracing-cycle-collector.md`. |

---

## What shipped per iter

### Iter 1 — `tracing-cycle-collector` feature

Added the feature to cs-gc / cs-core / cs-vm / cs-runtime
Cargo.toml files with the forward chain (cs-runtime →
cs-core → cs-gc). Off by default. No-op with the feature
off; future iters' code paths are all gated.

### Iter 2 — `cs_gc::cycle_registry` module

New `crates/cs-gc/src/cycle_registry.rs` (~250 LOC):
- `REGISTRY: RefCell<HashMap<usize, Box<dyn AnyWeak>>>` —
  per-thread, keyed by allocation address.
- `AUTO_TRIGGER_THRESHOLD: Cell<usize>` (default 10_000).
- `SWEEP_PENDING: Cell<bool>` — set by registration when
  threshold crosses.
- `AnyWeak` trait erases `Weak<T>`'s type parameter so the
  registry can hold mixed `T`s. Blanket impl for
  `Weak<T: 'static + CycleVisit>`.
- `register_cycle_candidate<T>(addr, weak)` — idempotent on
  addr; arms SWEEP_PENDING on threshold crossing.
- `unregister_cycle_candidate(addr)`,
  `candidate_count() -> usize`,
  `set_auto_trigger_threshold(n)`,
  `take_sweep_pending() -> bool` (read+clear),
  `run_sweep()` (stub: Phase 1 only),
  `reset_for_tests()` (cross-test/embedder cleanup).

7 unit tests cover registration semantics, threshold arming,
and the stub-sweep dead-entry pruning.

### Iter 3 — detector wiring

`crates/cs-runtime/src/countable_memory_cycle.rs` gains
`record_cycle_with_candidate(p)` — always increments the
cycle counter and, under tracing-cycle-collector + non-
region, registers `p` as a candidate.

All 6 mutation-builtin cycle-break callsites (`b_set_car`,
`b_set_cdr`, `b_vector_set`, `b_hashtable_set` ×3) swapped
from `record_cycle_detected()` to
`record_cycle_with_candidate(g)`. Each callback now binds
its parameter (`|v|`, `|h|`) so the right Gc value flows in.

3 integration tests in `tests/tracing_registry.rs`:
self-cycle registers, region cycle does NOT register (FR-5),
many cycles populate.

### Iter 4 — sweep triggers

- **`(collect)` Scheme builtin**: new `b_collect` registered
  in `pure_builtins`. Calls `run_sweep` under feature; no-op
  otherwise.
- **`Gc::new` auto-trigger**: reads + clears
  `take_sweep_pending`; if true, runs `run_sweep` before
  the new alloc. Single TLS read on the hot path.
- **Sweep itself**: Phase 1 (drop dead-Weak entries). Phase
  2/3 documented as deferred — requires per-type cycle-break
  dispatch.

2 new tests: `auto_trigger_fires_sweep_on_next_alloc`,
`collect_builtin_runs_sweep`.

### Iter 5 — embedder API + ADR + exit

`TracingPolicy { auto_trigger_threshold }` struct +
`Runtime::set_tracing_policy(policy)`. Cfg-gated; with
feature off, no Runtime API addition (use the parallel
`set_tracing_policy_noop` if embedder code wants to compile
unconditionally).

`Runtime::start_background_sweep` is documented as deferred
in the ADR — the registry is `thread_local!`-backed and
doesn't compose with a foreign sweep thread without
redesign.

1 new test: `tracing_policy_overrides_threshold`.

ADR 0018 + this report close the spec. Spec status flipped
to CLOSED in requirements.md, design.md, tasks.md.

---

## Deferred work

| Component | Why deferred | Where documented |
|---|---|---|
| Phase 2/3 Bacon-Rajan trial-deletion | Per-type cycle-break dispatch (Pair/Vector/Hashtable) needs a `BreakCycle` trait spanning cs-gc + cs-core; out of scope for v1. Layer-2 detector still handles what it can; layer 4 just maintains the candidate set for future enhancement. | ADR 0018 §"Scope" |
| `Runtime::start_background_sweep` | Per-thread registry doesn't compose with foreign sweep thread without redesign (Mutex<HashMap> or per-Runtime registry handle). | ADR 0018 §"Scope" |
| `Marker`-based mark phase | Spec referenced M5's `Marker`; implementation uses `CycleVisitor` (already walks the `CycleVisit` trait) so the M5 `Heap`/`Trace` infrastructure stays retired (avoids `Gc<T>` conflict). | ADR 0018 §"Scope" |

---

## Test status

- cs-gc cycle_registry unit tests: **7/7 passing**.
- cs-runtime tracing_registry integration: **6/6 passing**
  (self_cycle, region_cycle exclusion, many_cycles,
  auto_trigger, collect_builtin, tracing_policy).
- Existing cs-runtime cycle_break / cycle_detection: **5/5
  passing** — no regression from the rewiring.
- Default-features workspace tests: green (modulo the
  pre-existing `jit_conformance` stack overflow unrelated
  to this spec).

---

## File map

New files:
- `crates/cs-gc/src/cycle_registry.rs` (~250 LOC).
- `crates/cs-runtime/tests/tracing_registry.rs` (~140 LOC).
- `docs/adr/0018-tracing-cycle-collector.md`.
- `docs/milestones/tracing-revival-exit.md` (this file).

Modified files:
- `crates/cs-gc/Cargo.toml` — `tracing-cycle-collector` feature.
- `crates/cs-gc/src/lib.rs` — pub mod cycle_registry.
- `crates/cs-gc/src/rc_only.rs` — `Gc::new` auto-trigger
  check.
- `crates/cs-core/Cargo.toml` — feature forwarding.
- `crates/cs-vm/Cargo.toml` — feature forwarding.
- `crates/cs-runtime/Cargo.toml` — feature forwarding.
- `crates/cs-runtime/src/countable_memory_cycle.rs` —
  `record_cycle_with_candidate`.
- `crates/cs-runtime/src/builtins/mod.rs` — 6 callsites
  swapped to `record_cycle_with_candidate`; new `b_collect`.
- `crates/cs-runtime/src/lib.rs` — `TracingPolicy` +
  `Runtime::set_tracing_policy`.
- `.spec-workflow/specs/tracing-revival/{requirements,design,tasks}.md`
  — marked CLOSED.
