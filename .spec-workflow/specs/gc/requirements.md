# M5 GC — Requirements

> Status: **CLOSED**. Exit report: `docs/milestones/m5-exit.md`.
> Spec slug: `gc`
> Roadmap slot: M5
> Predecessor: M4 Bytecode VM (see `docs/milestones/m4-exit.md`)
> Phase 2 (generational copying / arena swap) tracked as M5 follow-up.

This spec replaces foundation/M4's `Rc<RefCell<...>>`-based heap with
a precise tracing garbage collector. The exit gate per the roadmap:

- 24-hour fuzz with leak detector reports no leaks on cyclic structures.
- p99 GC pause < 1 ms on stdlib load.
- Conformance pass rate equal to M4 baseline (1460 tests).
- Memory usage no worse than RC + cycle collector.

---

## Functional requirements

### FR-1. Replace `Rc<T>` in `Value`'s heap-pointer variants with `Gc<T>`

Every heap-allocated variant in `cs_core::Value` switches its inner
pointer type from `Rc<...>` to a new `Gc<T>` smart pointer:
- `Pair`, `Vector`, `String`, `ByteVector`, `Hashtable`, `Port`,
  `Procedure`, `Record`, `Closure`, `Continuation`, `Promise`,
  `Parameter`.

Acceptance: `grep "Rc<" crates/cs-core/src/value.rs` shows no remaining
heap-data uses (interned `Rc<str>` for symbol names is acceptable —
those are immortal once interned).

### FR-2. Mark-sweep stop-the-world first; generational copying second

Phase 1 ships a precise mark-and-sweep collector with bump allocation
inside per-class freelists. Phase 2 (a follow-up to M5, not gated by
this milestone) upgrades to generational copying.

Acceptance: a `cs-gc` crate exposes `Heap::collect()` that walks the
root set, marks reachable objects, sweeps unreachable.

### FR-3. Per-Runtime root set; VM stack roots; dynamic-wind hooks

Roots:
- The Runtime's top-level `Frame` chain.
- The VM's value stack and frame stack.
- Any `call/cc`-captured continuation values still alive.
- The `pending_values` channel (multi-value returns in flight).
- The `COND_PARENTS` and `BUILTIN_ERR_IRRITANT` thread-local
  registries.

Acceptance: a fuzz target that allocates, drops, and re-allocates
cyclic structures across N iterations stays bounded in memory.

### FR-4. Conformance parity

Every test currently passing on either tier (1460 individual cases)
must still pass after the GC swap. No new failures introduced.

Acceptance: `cargo test --release --test conformance` and
`cargo test --release --test vm_conformance` both green.

### FR-5. Fuzz green for 24 hours

The fuzz target from FR-3 plus a property test that constructs random
heap shapes (deep trees, cycles, mixed types) runs for 24 hours under
`cargo fuzz` without leaks (per a tracking allocator) or panics.

Acceptance: a CI workflow `m5-fuzz.yml` runs the target nightly for
1 hour; release blocks until 24h cumulative no-failure runtime.

### FR-6. Pause time < 1ms p99 on stdlib load

A microbenchmark loads the entire `tests/conformance/foundation/`
prelude + a representative subset of test files (≥10) in one run and
records GC pause times. p99 of pause times must be < 1ms.

Acceptance: a criterion bench `bench/gc_pause.rs` emits p50/p95/p99
pause-duration histograms.

### FR-7. Memory baseline

For each of three reference programs (factorial recursion to 1000,
fibonacci, ackermann small), peak resident memory under the new GC
must be ≤ 1.2× the same program under the M4 RC baseline. (We accept
some overhead for headers / freelist metadata; we do not accept a 2×
regression.)

Acceptance: a criterion harness records peak RSS via
`procfs`/`mach_task_basic_info` on Linux/macOS.

---

## Non-functional requirements

### NFR-1. No `unsafe` outside the `cs-gc` crate

The GC implementation may use `unsafe` (rooting needs raw pointers).
The rest of the workspace stays `unsafe`-free.

### NFR-2. `Gc<T>` must be `Clone` (for the same ergonomics as `Rc<T>`)

Cloning a `Gc<T>` increments a refcount-like reachability marker (or
is a no-op in a mark-sweep design — depends on FR-2 phase). The
external API mirrors `Rc<T>`'s shape so the call-site diff is minimal.

### NFR-3. Public API stability

`Value`'s match shape stays compatible with all current pattern-match
sites in `cs-runtime`, `cs-vm`, `cs-cli` etc. Changes are confined to
the inner pointer type per variant.

### NFR-4. Documentation

A new ADR (`docs/adr/0006-gc-design.md`) ratifies:
- The choice of mark-sweep first vs copying first
- The rooting strategy (precise via stack maps later? RSet-style now?)
- Why we picked a hand-rolled GC over `gc` / `gc-arena` crates.

---

## Out of scope (deferred to follow-up milestones)

| Item | Where it lives |
|---|---|
| Generational copying | M5 follow-up (Phase 2) |
| Concurrent / incremental collection | post-M5 |
| JIT-stack roots (stackmaps) | M6 |
| Per-library scope frames (M9) | independent of GC |
| Multi-shot continuations | M8 (interacts with stack rep) |

---

## Risks

1. **Rooting bugs.** Forgetting to root a temp during a borrow cycle
   leads to use-after-free.
   *Mitigation:* code review checklist; `Heap::trace_test_unmark`
   self-check in debug builds.
2. **Pause time regression.** Stop-the-world may exceed 1ms on
   moderate heaps.
   *Mitigation:* incremental marking flag in Phase 2; benchmark-driven
   tuning of the white/black/grey transitions.
3. **`unsafe` scope creep.** GC innards need `unsafe` for raw
   pointers; the temptation is to bleed it into `Value` itself.
   *Mitigation:* keep `Gc<T>` opaque; provide only safe accessors.
4. **Test-suite flakes from finalization timing.** Some tests may
   currently rely on Rc destructors running synchronously.
   *Mitigation:* audit drop-order-sensitive tests (e.g. file-output
   ports flushing on `close-port`); make them explicit not implicit.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `Rc<T>` removed from `Value` heap-data variants | `value.rs` grep |
| Mark-sweep collector in `cs-gc` | crate present |
| Conformance ≥ 1460 individual tests passing | both harnesses green |
| 24h fuzz no leaks | nightly CI |
| p99 GC pause < 1ms | criterion bench |
| Memory ≤ 1.2× M4 baseline | criterion bench |
| ADR 0006 written | `docs/adr/0006-gc-design.md` |
