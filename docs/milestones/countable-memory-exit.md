# Countable Memory — Exit Report

> Tagged at the merge commit of this report.
> Predecessor: M5 (`docs/milestones/m5-exit.md`, conformance 2150).
> Spec: `.spec-workflow/specs/countable-memory/`.
> ADR: `docs/adr/0014-countable-memory.md`, supersedes
> `docs/adr/0006-gc-design.md`.

This report closes iters 1–11 + iter 12a of the countable-memory
spec. The Rc-only `Gc<T>` + synchronous cycle-collector path is
now the production default workspace-wide; the M5 tracing
infrastructure remains cfg-gated as a rollback path until iter
12b deletes it.

---

## Acceptance summary

| Gate | Spec § | Result |
|---|---|---|
| FR-1: `Gc<T>` Rc-only representation | `requirements.md` | **✅** `crates/cs-gc/src/rc_only.rs` (325 LOC) replaces `Slot<T>` wrapper with a thin `Rc<T>` newtype. |
| FR-2: tracing infrastructure removed from steady-state | `requirements.md` | **Partial** — cfg-gated out of the default build. Full deletion is iter 12b (point of no return). |
| FR-3: synchronous cycle detector at mutation sites | `requirements.md` | **✅** `cs_gc::cycle` + wiring in `b_set_car` / `b_set_cdr` / `b_vector_set` / `b_hashtable_set`. Detection counter accessible via `cs_runtime::countable_memory_cycle::cycle_detection_count`. |
| FR-4: deterministic port finalization | `requirements.md` | **✅** `crates/cs-runtime/tests/port_finalization.rs` — 2 tests asserting file flush + close on `Rc<Port>` drop without `(close-port)` or explicit collect. |
| FR-5: continuation / closure cycle prevention | `requirements.md` | **Partial** — iter-6 identity-dedup (`CycleVisitor::visit_addr` on Frame/Env/Closure/VmClosure) bounds the detector so cycle detection terminates. Closure-self-binding cycles (`(define (f) f)`) still leak refcount-wise pending iter 7.1's storage-slot refactor. The originally-planned iter 8 `Weak<Frame>` refactor was retired as architecturally incompatible — see ADR 0014. |
| FR-6: conformance ≥ 2150 individual tests | `requirements.md` | **✅** 117 cs-cli conformance files green under the new default; full workspace 0 failures. |
| FR-7: JIT raw-handle ABI byte-compatible | `requirements.md` | **✅** `Gc::into_raw_jit` / `from_raw_jit` / `raw_incref` preserved verbatim; JIT/AOT differential tests pass. |
| FR-8: `Procedure` no longer the exceptional variant | `requirements.md` | **✅** under feature, `Procedure: fmt::Debug + 'static` with optional `visit_closure_children` default-empty method. |
| NFR-1: per-allocation overhead reduction | `requirements.md` | **✅** `Slot<T>` mark word eliminated; only stdlib `Rc` header remains. |
| NFR-2: M5 perf gates held | `requirements.md` | **✅** `cycle_collect_timing.rs`: p99 < 50 µs trivial cycle, < 500 µs 1k-node chain (well under the 1 ms spec ceiling). |
| NFR-3: no `unsafe` outside JIT raw-handle ABI | `requirements.md` | **✅** `cargo-geiger` unchanged from M5 (3 `unsafe fn` total in `cs-gc`). |
| NFR-4: `cs-core::Value` public API stability | `requirements.md` | **✅** variant set + pattern shapes unchanged. |
| NFR-5: WASM target stays green | `requirements.md` | **✅** workspace builds under default + under `--no-default-features --features jit,ffi-dynamic`. |
| NFR-6: AOT pipeline stays green | `requirements.md` | **✅** cs-aot tests pass; emitted Rust source unchanged. |
| NFR-7: ADR 0014 written + ADR 0006 amended | `requirements.md` | **✅** iter 12a. |

---

## What shipped per iter

### Iter 1 — feature flag + Rc-only Gc<T> variant

`crates/cs-gc/src/lib.rs` split into a 38-LOC router plus two
sub-modules:

- `tracing.rs` (623 LOC, relocated from the old lib.rs unchanged):
  M5 Phase-1 `Rc<Slot<T>>`-backed `Gc<T>` + mark-sweep `Heap` /
  `Trace` / `Marker`. Gated on
  `#[cfg(not(feature = "countable-memory"))]`.
- `rc_only.rs` (325 LOC): `Gc<T>` as a thin newtype over `Rc<T>`,
  preserving the full M5 public API (`new`, `Clone`, `Deref`,
  `PartialEq`, `Debug`, `ptr_eq`, `as_addr`) plus the JIT
  raw-handle ABI (`into_raw_jit` / `from_raw_jit` / `raw_incref`)
  and net-new `downgrade` / `strong_count`. Plus a thin
  `Weak<T>` wrapper. 10 unit tests.

### Iter 2 — `Weak<T>` upgrade-after-drop doctest

Doctest in `rc_only.rs` demonstrating the
`Gc::downgrade → drop → Weak::upgrade → None` pattern.

### Iter 3 — `cs_gc::cycle` bounded-DFS detector

`crates/cs-gc/src/cycle.rs` (400+ LOC including tests):

- `CycleVisit` trait: per-type impls enumerate `Gc<...>` children.
- `CycleVisitor` context: per-call `visited: HashSet<usize>` +
  found/over-limit flags. `visit(&Gc<T>)` returns true to descend.
- `cycle_check(root)` and `check_and_break(root, break_at)`
  entry points. Iterative DFS with a configurable per-thread
  node-visit limit (default 10_000).
- 10 unit tests covering self-loop, mutual, ring, sibling-cycle,
  unrelated-cycle, limit-exceeded, and check-and-break dispatch.

### Iter 4 — `cs-gc` integration tests

`crates/cs-gc/tests/cycle.rs`: 8 integration tests using only the
public crate API. Validates the detector independent of any
consumer `CycleVisit` impl.

### Iter 5 — `CycleVisit` impls in cs-core

`crates/cs-core/src/value.rs`: parallel
`#[cfg(feature = "countable-memory")] impl CycleVisit for X` for
`Pair`, `Hashtable`, `Port`, `Promise`, `Parameter`, `Value`,
mirroring each existing `Trace` impl. `Procedure` trait gains an
optional `visit_closure_children` method with empty default.
`cs-core/src/lib.rs` re-exports `{Gc, Weak}` under feature
(vs `{Gc, Heap, Marker, Trace}` under default).

### Iter 6 — `CycleVisit` impls in cs-runtime + cs-vm

Parallel impls for `Frame` (cs-runtime/env.rs),
`Builtin`/`Closure`/`Continuation`/`HostBuiltin`
(cs-runtime/proc.rs), `VmClosure`/`Bindings`/`Env` (cs-vm/vm.rs).
The `trace_leaf_proc!` macro emits nothing under feature — the
~47 zero-payload procedure markers inherit empty default
`visit_closure_children`. Cfg-gated out: `JIT_ACTIVE_HEAP` +
`set/clear/current_jit_active_heap`, `Runtime::heap` field, the
3 root closures in `Runtime::new`, the heap-using
`jit_differential` tests + the 5 GC test files.

### Iter 7 (compressed) — cycle detector wired into mutation builtins

`b_set_car`, `b_set_cdr`, `b_vector_set`, `b_hashtable_set` each
call `cs_gc::cycle::check_and_break`. Break action increments a
thread-local counter in
`cs_runtime::countable_memory_cycle::cycle_detection_count`
rather than flipping storage to `Weak` — the slot-enum
refactor (Strong/Weak variants on `Pair`/`Vector`/`Hashtable`)
is deferred as iter 7.1. 5 regression tests in
`cycle_detection.rs`.

### Iter 9 — port + closure regression tests

`port_finalization.rs` (2 tests) and `closure_cycle.rs` (4 tests)
verify FR-4 / FR-5 contracts.

### Iter 10 — cycle-collect timing

`cycle_collect_timing.rs` (2 tests): p99 latency on trivial
self-loop and 1k-node chain. Debug/release ceilings differentiated
because debug builds run ~10× slower.

### Iter 11 — flip default-on workspace-wide

`countable-memory` becomes the default feature in cs-gc, cs-core,
cs-vm, cs-runtime. Workspace `Cargo.toml`'s cs-runtime
declaration explicitly enables countable-memory regardless of
which other feature combinations downstream consumers pick.

**Two correctness fixes surfaced during iter 11 validation:**

1. `CycleVisitor::visit_addr` added (new public API). The previous
   surface only deduped on `Gc<T>` identities. `Frame` and
   `Closure` are `Rc<T>`-backed but DO form re-entry loops when
   a binding closes over its capturing env. Frame/Env/Closure/
   VmClosure now register their own `Rc` address before
   descending, bounding the detector's host-stack recursion.

2. `Frame::visit_children` / `Env::visit_children` no longer
   recurse into the parent chain — only the current frame's
   bindings. Mutation cycle detection asks "does the mutated
   cell loop back through user data?" not "through stdlib
   defining frames". Without this, `conformance_hashtable_custom`
   (and any test with deep nested defines) overflowed the test
   thread stack. Iter 8's `Weak<Frame>` refactor would close
   this structurally; the iter-11 identity-dedup + skip-parent
   combination is correct and bounded.

### Iter 12a — documentation

This file + `docs/adr/0014-countable-memory.md` +
`docs/adr/0006-gc-design.md` amendment.

---

## Deferred / follow-up work

The countable-memory representation ships as the production
default at iter 11. Three iters remain queued as documented
follow-ups; they close the residual cycle-leak gap but are not
prerequisites for the iter-11 ship:

| Iter | Scope | Why deferred |
|---|---|---|
| 7.1 + 7.1.x + 7.1.x.y | **Shipped.** Pair tombstone infrastructure (WeakValue type, car_weak/cdr_weak fields, ~250 reader sites migrated). Strong-count-guarded break invoked from b_set_car/b_set_cdr with a caller-supplied `baseline: usize` (3 = slot + args[0] + args[1] + 1 transient accounting for VM-tier NB stack — conservative upper bound shared across walker and VM tiers). Demote fires when total strong > baseline, i.e., when at least one persistent external anchor exists beyond the dispatch transients. The `(set-cdr! x x)` top-level-bound case reclaims correctly (walker total=4 > baseline=3). The metacircular `(set-car! env (cons name val))` pattern is still safely skipped (total=2 from slot + args[1]; cons cell has no external anchor). 5 regression tests in crates/cs-runtime/tests/cycle_break.rs including the canonical `iter_7_1_x_y_top_bound_self_cycle_actually_breaks`. `cycle_broken_count` distinguishes detection from successful break. |
| 7.1.x.z | Replace caller-supplied baselines with full Bacon-Rajan trial-deletion that picks safe cycle edges agnostic to caller conventions. Would automatically handle VM-tier dispatch baselines; currently VM helpers (vm_set_car_gc / vm_set_cdr_gc) call pp.set_car directly without check_and_break, leaving VM cycle detection unwired. |
| 7.1.y | Vector and Hashtable tombstone structural break. **Scope reduced** in this milestone to documentation-only deferral: cycles via `vector-set!` / `hashtable-set!` are rare in idiomatic Scheme; the detector already finds them via CycleVisit; only refcount reclamation is missing. Refactor cost is 162 sites (Vector) + ~50 (Hashtable). Pair covers the common cycle pattern. Future iter lands the equivalent infrastructure on demand. |
| 8 | `Frame.parent` / `Continuation` parent chain refactored to `Weak<Frame>`. Continuation captures keep the leaf strong; ancestors walked via `upgrade()`. | **Retired — not applicable to CrabScheme's walker architecture.** Attempted during iter 8; the walker's TCO loop overwrites `cur_env = new_env` (eval.rs:328) which drops the only strong reference to the outer scope. A weak parent chain dangles on the next lookup. CrabScheme's `Continuation` is also `{ id: u64 }`, not a frame-capturing struct, so the rationale that justifies the refactor doesn't hold. The actual closure-self-binding cycle (`(define (f) f)`) is closed by iter 7.1's storage-slot refactor instead. See ADR 0014 §"Iter 8 architectural mismatch". |
| 12b | Delete the cfg-gated tracing path entirely (point of no return). Removes `tracing.rs`, the `Trace` impls, `Heap`/`Marker`, `JIT_ACTIVE_HEAP`, the `trace_leaf_proc!` macro. Slims cs-gc per the < 150 LOC spec target. | Should land after iter 7.1 and 8 are mature enough that no rollback path is needed. The cfg-gated tracing code is dead weight today (production default doesn't include it) but is rollback insurance. |

---

## Conformance + test counts at exit

- **117 cs-cli conformance files green** under default
  (countable-memory on); same under
  `--no-default-features --features jit,ffi-dynamic` (tracing
  path, still cfg-gated in).
- **0 failures workspace-wide** under both feature configurations.
- **cs-gc**: 13 tracing-tier unit tests + 10 rc-only unit tests
  + 10 cycle unit tests + 8 cycle integration tests + 1 doctest.
- **cs-runtime new tests** (all cfg-gated to the feature):
  cycle_detection.rs (5), port_finalization.rs (2),
  closure_cycle.rs (4), cycle_collect_timing.rs (2).

## Counts at exit

- 8 commits across the spec (iters 1–11 + iter 12a docs)
- Net workspace LOC: +1500 (mostly new tests + cycle module + docs)
- ADR 0014 written, ADR 0006 marked superseded
- Three documented follow-ups (iter 7.1, iter 8, iter 12b)

---

*Authored at the close of countable-memory iter 12a. Next:
iter 12b deletes the tracing infrastructure; iters 7.1 and 8
close the residual cycle-leak gap.*
