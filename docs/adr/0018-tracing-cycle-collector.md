# ADR 0018: Tracing Cycle Collector — Candidate-Registry Sweep

> Status: Accepted (layer 4 of the unified memory management
> architecture — see [ADR 0015](./0015-unified-memory-management.md))
> Date: 2026-05-17
> Spec: `.spec-workflow/specs/tracing-revival/` (CLOSED, with
> known deferrals — see "Scope" below)
> Depends on: [ADR 0014 — Countable Memory](./0014-countable-memory.md)
> Companions: [ADR 0016 — Region Types](./0016-region-types.md), [ADR 0017 — Escape Analysis](./0017-escape-analysis.md)

## Context

ADR 0015 lays out five memory-management layers. Layers 1–3
and 5 ship. Layer 4 (tracing) was originally M5's whole-heap
mark-sweep collector — retired by countable-memory (ADR 0014)
once analysis showed that `auto_collect` was never enabled in
production and the Rc-only path was the right default.

But Rc has a known limitation: pure-internal cycles can leak
when the cycle's only strong refs are from inside the cycle
itself. The synchronous detector (countable-memory iter 7.1.x)
catches some of these at mutation time, but its strong-count
guard correctly refuses to break cycles where no external
strong holder exists — those would orphan still-live values
if broken.

A tracing GC for those residual cycles is the natural
solution. But a full whole-heap mark-sweep has the overhead
that retired M5's collector. The right shape is a
**candidate-based** tracing collector: only the values the
synchronous detector flagged as cycle-suspicious enter a
registry; the sweep operates only on that registry.

This ADR ratifies that design.

## Decision

Ship a candidate-registry-based residual cycle collector as
the opt-in layer 4, gated on a new
`tracing-cycle-collector` feature flag. The flag is **off by
default** — most CrabScheme programs don't need it (the
layer-2 detector handles common cases, layer 3 region drops
handle bounded-lifetime cycles, layer 5 keeps allocation in
the cheapest safe tier).

Specifically:

1. **`tracing-cycle-collector` feature** — added to cs-gc /
   cs-core / cs-vm / cs-runtime with the standard forward
   chain. Off by default. With it off, layer-4 code paths
   compile to nothing (`#[cfg(feature = "tracing-cycle-
   collector")]`-gated everywhere).

2. **`cs_gc::cycle_registry` module** — per-thread
   `HashMap<usize, Box<dyn AnyWeak>>` keyed by allocation
   address, storing `Weak<T>` handles to cycle candidates.
   `AnyWeak` trait erases the `T` so mixed types co-exist in
   one registry.

3. **API**: `register_cycle_candidate(addr, weak)` (idempotent
   on addr), `unregister_cycle_candidate(addr)`,
   `candidate_count() -> usize`,
   `set_auto_trigger_threshold(n)`, `run_sweep()`,
   `take_sweep_pending() -> bool`, `reset_for_tests()` (also
   useful as embedder teardown).

4. **Registration trigger**: `cs-runtime`'s
   `record_cycle_with_candidate(p)` always increments the
   cycle counter and, when the tracing feature is on, also
   registers `p` in the registry. Region-allocated values
   are excluded (their bulk-free handles reclamation).

5. **Sweep triggers**:
   - **Manual**: Scheme `(collect)` builtin calls
     `run_sweep()`.
   - **Auto on threshold**: `register_cycle_candidate` sets a
     TLS `SWEEP_PENDING` flag when the registry crosses
     `auto_trigger_threshold`. The next `Gc::new` reads +
     clears the flag and runs `run_sweep` before the new
     alloc lands. Single TLS read on the hot path when the
     flag is false.
   - **Embedder API**: `Runtime::set_tracing_policy(policy)`
     adjusts the threshold per Runtime.

6. **Sweep algorithm (Phase 1)**: drops registry entries
   whose Weak no longer upgrades. The full Bacon-Rajan trial-
   deletion (Phase 2/3) is deferred — see below.

## Scope: shipped vs. deferred

### Shipped (iters 1–5)

| Component | File(s) | Status |
|---|---|---|
| `tracing-cycle-collector` feature flag | each crate's Cargo.toml | ✅ |
| `cs_gc::cycle_registry` module + API | `cs-gc/src/cycle_registry.rs` | ✅ |
| Wire detector to populate registry | `cs-runtime/src/countable_memory_cycle.rs` + 6 mutation builtins | ✅ |
| `(collect)` Scheme builtin | `cs-runtime/src/builtins/mod.rs` | ✅ |
| Auto-trigger via `Gc::new` | `cs-gc/src/rc_only.rs` | ✅ |
| `Runtime::set_tracing_policy` | `cs-runtime/src/lib.rs` | ✅ |
| ADR + exit report | this file + `docs/milestones/tracing-revival-exit.md` | ✅ |

### Deferred

| Component | Rationale |
|---|---|
| **Phase 2/3 Bacon-Rajan trial-deletion** | Requires per-type cycle-break dispatch (Pair vs Vector vs Hashtable) and a `BreakCycle` trait spanning cs-gc + cs-core. The layer-2 synchronous detector still breaks what it can at mutation time; this iter ships the registry hygiene so future iters can layer trial-deletion against a well-defined candidate set. |
| **`Runtime::start_background_sweep`** | The registry is `thread_local!`-backed and doesn't compose with a foreign sweep thread without redesign (would need `Mutex<HashMap>` or per-Runtime registry handle). Embedders who need background sweep today can poll `(collect)` from their own scheduler. |
| **`Marker`-based mark phase** | Spec design referenced M5's `Marker`; the actual implementation uses `cs_gc::cycle::CycleVisitor` (the layer-2 visited-set) instead since both walk the same `CycleVisit` trait. Simpler and avoids reviving M5's `Heap`/`Trace` infrastructure (which would conflict with the countable-memory `Gc<T>`). |

These deferrals are honest about what shipped: a clean
candidate-registry + sweep-trigger infrastructure that
maintains the registry, with the cycle-break sophistication
left for a future iter once a real workload demonstrates the
gap.

## Trade-offs

### What we accept

- **Phase 1 sweep doesn't actually break cycles.** It only
  prunes dead-Weak entries from the registry. Residual
  cycles that the layer-2 detector couldn't break (because
  the strong-count guard refused) continue to leak until
  either: (a) something external drops the cycle root, (b)
  the program ends, or (c) future Phase 2/3 ships. This
  matches today's behaviour (no regression) but doesn't
  *yet* improve it.
- **Per-thread registry.** Multi-threaded Scheme isn't in
  scope today; if it ever lands, each thread / Runtime
  instance gets its own registry. Cross-thread sweep
  coordination is future work.
- **8 bytes per registry entry** (HashMap overhead + Box<dyn>
  fat pointer). Trivially bounded by
  `auto_trigger_threshold`.

### What this buys

- A clean place to put residual-cycle reclamation when the
  algorithm sophistication catches up. The candidate set is
  already in place; the layer-2 detector is wired to
  populate it; the trigger paths are wired to invoke the
  sweep.
- Off-by-default opt-in: embedders running long-lived
  workloads where cycle leaks matter (servers, REPLs that
  process many sessions) can flip the feature on and
  configure the threshold; everyone else pays zero
  overhead.
- Clean integration with layer 3 (regions are skipped) and
  layer 2 (detector already wired). The five-layer
  architecture from ADR 0015 has all its consumer points
  hooked.

## References

- ADR 0014 — Countable Memory (layer 2)
- ADR 0015 — Unified Memory Management (the 5-layer plan)
- ADR 0016 — Region Types (layer 3)
- ADR 0017 — Escape Analysis (layer 5)
- `.spec-workflow/specs/tracing-revival/` — full spec
- `docs/milestones/tracing-revival-exit.md` — exit report
