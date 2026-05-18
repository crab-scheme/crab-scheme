# Countable Memory — Requirements

> Status: **Draft** (proposed post-1.0 perf/simplification track).
> Spec slug: `countable-memory`
> Roadmap slot: post-1.0 perf track; **supersedes** the deferred M5
> Phase 2 (arena swap + generational copying) follow-up listed in
> `docs/milestones/m5-exit.md`.
> Predecessor: M5 Phase 1 (`docs/milestones/m5-exit.md`,
> `docs/adr/0006-gc-design.md`).
> Companion: `design.md`, `tasks.md` (this spec).

This spec retires the precise tracing GC scaffolding (`cs_gc::Heap`,
`cs_gc::Trace`, `cs_gc::Marker`, root closures, stop-the-world
`collect()`) and commits to **Rust's native reference counting
(`Rc<T>` / `Weak<T>`) as the sole memory-reclamation mechanism**,
augmented by a small synchronous cycle collector targeted at the
handful of Scheme operations that can actually construct cycles in
the user heap.

## Why now

M5 Phase 1 shipped a `Gc<T>` whose inner representation is
`Rc<Slot<T>>` — i.e., the runtime is already paying refcount cost
on every clone/drop. The tracing layer's `auto_collect` defaults
to `false` in `Heap::new()` (`crates/cs-gc/src/lib.rs:343`) and
no embedder turns it on. In practice:

- Every reclamation today is a refcount drop. Tracing infra is dead
  weight in the steady-state allocator path.
- The 24-hour fuzz gate, the p99 < 1ms pause gate, and the ≤ 1.2×
  memory gate from the M5 spec all passed under what is effectively
  pure refcounting. The numbers in `bench/m5-phase1-baseline.json`
  are RC numbers.
- The deferred Phase 2 arena swap was the path to *making the
  tracing layer pay for itself*. Without that swap, the tracing
  layer is overhead with no offsetting benefit.
- WASM (M10 Track W) shipped on the Rc-backed Phase 1; nothing
  about the WASM tier relies on tracing.

The cycle-leak trade-off that M5 explicitly accepted as a Phase 1
limitation is the only remaining justification for tracing. This
spec replaces *general* tracing with *targeted* cycle detection
driven by the small, enumerable set of mutating Scheme operations
that can produce cycles — at lower steady-state cost than tracing
and with the same correctness story.

The worktree name (`countable-memory`) signals intent: memory whose
liveness is *counted* (deterministic), not *traced* (sampled at
collection time).

---

## Functional requirements

### FR-1. Replace `Gc<T>`'s inner representation with `Rc<T>` (no `Slot`/mark wrapper)

Today: `Gc<T>` wraps `Rc<Slot<T>>` where `Slot<T>` adds a mark
`Cell<bool>` consumed by the tracing layer. After this spec:
`Gc<T>` is a thin wrapper around `Rc<T>` directly (or a `pub type
Gc<T> = Rc<T>;` alias) — the mark cell is gone, the per-allocation
header memory cost drops by one word, and `Gc::clone` is a single
`Rc::clone` with no extra branches.

The external API surface stays compatible:
- `Gc::new`, `Gc::clone`, `Deref<Target = T>`, `Gc::ptr_eq`,
  `Gc::as_addr`.
- `Gc::into_raw_jit`, `Gc::from_raw_jit`, `Gc::raw_incref` — these
  already delegate to `Rc::into_raw` / `Rc::from_raw` /
  `Rc::increment_strong_count` per ADR 0012 D-2. They become
  trivial under the new representation.

**Acceptance**: `crates/cs-gc/src/lib.rs` defines `Gc<T>` as a
`Rc<T>`-backed newtype (or alias) with no internal `Slot`, no
`Marker`, no `Trace`. The `Heap`, `Marker`, and `Trace` symbols
are either deleted or kept as compatibility shims (decision in
`design.md`).

### FR-2. Delete the tracing infrastructure across the workspace

All `impl Trace for ...` blocks across `cs-core`, `cs-runtime`,
`cs-vm` go away. The `Runtime::heap` field is removed (or replaced
with a placeholder for the cycle-collector). `Heap::add_root` /
`heap.add_root(...)` call sites are deleted. The pinned-value slab
in `Runtime` no longer needs a root closure — `Pinned<'rt>` already
holds a strong `Rc` to its value (via the map entry), so RC alone
keeps the pin alive.

Workspace-wide deletion targets:
- `crates/cs-gc/src/lib.rs` — `Heap`, `Marker`, `Trace`,
  `add_root`, `set_auto_collect`, `collect`, `Slot`, `SlotValue`,
  `Marked`, `trace_leaf!` macro, the unit tests for those.
- `crates/cs-core/src/value.rs` — `impl Trace for {Pair,
  Hashtable, Port, Promise, Parameter, Value}`, the `Trace`
  supertrait on `Procedure`.
- `crates/cs-runtime/src/{env.rs,proc.rs}` — `impl Trace for
  {Frame, Builtin, Closure, Continuation, HostBuiltin}`.
- `crates/cs-vm/src/vm.rs` — `impl Trace for {VmClosure, Bindings,
  Env}`, the `trace_leaf_proc!` macro and its ~47 invocations.
- `crates/cs-runtime/src/lib.rs` — the three `heap.add_root(...)`
  blocks in `Runtime::new`.

**Acceptance**: `rg 'impl.*Trace for|trace_leaf|add_root|\.collect\(\)'
crates/` returns no GC-related matches (the existing `vec.collect()`
iterator hits don't count). `cs-gc/src/lib.rs` is under 150 LOC.

### FR-3. Synchronous cycle collection on potentially-cycle-creating mutations

Cycles in a Scheme heap can only be created by a small,
enumerable set of mutating operations: `set-car!`, `set-cdr!`,
`vector-set!`, `bytevector-u8-set!` (no — bytevectors hold no
heap pointers), `hashtable-set!`, `record-set!`, `string-set!`
(no — strings hold no heap pointers), `set!` over a closed-over
variable, and the box-mutation primitives used by the VM tier's
mutable lexical bindings.

After each such mutation, the runtime runs a synchronous, *local*
cycle-detection pass rooted at the mutated cell, following the
Bacon–Rajan "trial deletion" / synchronous cycle-collection
algorithm scoped to the cell's transitive children. If a cycle
is found, the runtime breaks it by clearing one slot in the cycle
(documented choice per type — typically the `cdr` for `set-cdr!`,
the element for `vector-set!`, the value for `hashtable-set!`).

For continuations and self-referential closures (the other common
cycle source), the runtime uses `Weak<T>` back-edges so cycles
*never form in the first place* (see FR-5).

**Acceptance**: a regression test corpus in
`crates/cs-runtime/tests/cycle_break.rs` constructs every kind of
cycle the language can produce (`(let ([x (cons 1 2)]) (set-cdr!
x x))`, vector self-loop, hashtable value-self-loop, mutually
recursive closures via `set!`, etc.) and asserts that the heap
returns to its pre-mutation live-slot count within one allocation
of the mutation completing.

### FR-4. Deterministic finalization for `Port` and other resource-holding types

R6RS implementations are expected to flush and close file-output
ports when they become unreachable. Today this is technically
deferred to GC; under the new model it's guaranteed by `Rc`'s drop
chain. The `Drop` impl on each `Port::*` variant runs immediately
on the last `Rc` drop, with no batching delay and no possibility
of an output port surviving past program shutdown unflushed.

**Acceptance**: a regression test
`crates/cs-runtime/tests/port_finalization.rs` opens a file output
port, writes to it, drops the only handle, then reads the file
from a fresh `std::fs::read_to_string` and asserts the bytes are
on disk — without any explicit `(close-port)` or
`runtime.collect()` call.

### FR-5. Continuation and closure cycle prevention via `Weak<T>`

`Continuation` values capture parent `Frame` chains; closures over
`set!`-mutable variables can construct (closure → env → closure)
cycles. Where the static shape of the cycle is known, the back-edge
is `Weak<T>` so the cycle is structurally impossible.

Concretely:
- `Continuation`'s parent-frame chain uses `Weak<Frame>` for the
  parent pointer; the captured continuation always keeps the leaf
  frame strong, and walks up via `Weak::upgrade` (frames stay alive
  through the strong path from the leaf).
- `VmClosure`'s capture of the env it was created in uses `Weak<Env>`
  when the env in turn holds the closure (detected at closure-
  allocation time by an arity heuristic; see design.md §"Cycle
  prevention strategy").

Where the cycle shape is *not* statically known (general
`set-car!` / `vector-set!` / `hashtable-set!` cycles), FR-3's
synchronous cycle collector handles it.

**Acceptance**: regression test
`crates/cs-runtime/tests/closure_cycle.rs` constructs a
self-referential closure via `(let loop () (loop))` plus a
`call/cc`-captured continuation that re-invokes itself, runs each
in a fresh `Runtime`, drops the runtime, and asserts (via a leak
counter wrapping `Rc::strong_count` on a sentinel allocation) that
no strong refs survive runtime drop.

### FR-6. Conformance parity

All 2150 individual conformance assertions passing under the M5
Phase 1 GC (per `docs/milestones/m5-exit.md`) must still pass.
No new test failures introduced.

**Acceptance**: `cargo test --release --test conformance` and
`cargo test --release --test vm_conformance` both green; the
aggregate assertion count published in the exit report matches or
exceeds 2150.

### FR-7. JIT raw-handle ABI unchanged

The Cranelift JIT (M6) and the AOT backend (M10) both spill live
`Gc<Value>` handles to the host stack as raw `i64` words and round-
trip via `Gc::into_raw_jit` / `Gc::from_raw_jit` / `Gc::raw_incref`
(ADR 0012 D-2). The semantics — one strong-count owned per spilled
slot, increment by `raw_incref`, decrement by either `from_raw_jit`
or the implicit `Rc::drop` path on stack unwind — stay byte-
compatible with the existing implementation.

**Acceptance**: all M6 / M10 differential parity tests
(`crates/cs-vm/tests/jit_*`, `crates/cs-aot/tests/*`) stay green
with zero source-level changes to the stackmap walker
(`crates/cs-vm/src/jit_stackmap.rs`).

### FR-8. `Procedure(Rc<dyn Procedure>)` migration consistency

ADR 0006 documented `Procedure(Rc<dyn Procedure>)` as a Phase 1
exception (blocked on stable `CoerceUnsized`). Under the new
design `Rc<dyn Procedure>` is no longer the exception — it's the
*standard form*, consistent with every other heap-bearing variant.
The `Trace` supertrait on `Procedure` is removed; the `Procedure`
trait's only supertraits become `fmt::Debug + 'static`.

**Acceptance**: `Value::Procedure(Rc<dyn Procedure>)` continues to
work without `Trace` plumbing; the 47 `trace_leaf_proc!` invocations
in `crates/cs-vm/src/vm.rs` are deleted in one block.

---

## Non-functional requirements

### NFR-1. Per-allocation overhead reduction

The current `Slot<T>` adds a `Cell<bool>` mark word (1 byte
+ padding, typically 8 bytes per slot on a 64-bit target) plus the
slot is tracked in `Heap::slots: RefCell<Vec<Weak<dyn Marked>>>`
(another 16-byte `Weak<dyn Trait>` fat pointer per slot, plus
`Vec` amortization). After this spec: zero per-allocation
bookkeeping outside the `Rc` strong/weak count header that the
standard library already maintains.

**Acceptance**: a `criterion` benchmark
`bench/alloc_overhead.rs` allocates 10⁶ small `Gc<Pair>` values
and asserts peak RSS is ≤ 90% of the M5 Phase 1 baseline for the
same workload.

### NFR-2. Match or improve M5's perf gates

The three M5 exit-gate numbers (p99 GC pause < 1 ms, memory ≤ 1.2×
M4 baseline, alloc-stress microbenchmark within published range)
must hold. Under refcount-only the "GC pause" gate becomes
*trivially zero* (no stop-the-world pause exists), but the alloc-
stress benchmark — which measures total allocator throughput —
must not regress.

**Acceptance**:
- `crates/cs-runtime/tests/gc_timing.rs` is replaced by
  `crates/cs-runtime/tests/cycle_collect_timing.rs` asserting p99
  of the synchronous cycle-collector pass over a 1k-node graph
  is < 100 µs (10× tighter than the old GC gate; cycle collection
  runs per-mutation, not stop-the-world, so the bound is per-call).
- `bench/microbench/alloc-stress` numbers stay within 5% of the
  M5 Phase 1 baseline in `bench/m5-phase1-baseline.json`.

### NFR-3. No `unsafe` outside the JIT raw-handle ABI

The current `cs-gc` Phase 1 is `unsafe`-free *except* for the JIT
ABI (`into_raw_jit`/`from_raw_jit`/`raw_incref`). The new design
preserves that property: zero `unsafe` in the cycle collector;
the only `unsafe` is the existing JIT ABI surface, which is a
mechanical pass-through to `std::rc::Rc::{into_raw, from_raw,
increment_strong_count}`.

**Acceptance**: `cargo-geiger` on the workspace reports the
`unsafe` counter in `cs-gc` is unchanged (still equal to the JIT
ABI's count, currently three `unsafe fn`s).

### NFR-4. Public API stability of `cs-core::Value`

`Value`'s variant set, the inner pointer types (`Gc<T>`,
`Rc<dyn Procedure>`), and all pattern-match shapes in
`cs-runtime`, `cs-vm`, `cs-cli`, `cs-aot` stay byte-compatible.
This is a *backing* change, not a *surface* change.

**Acceptance**: `rg 'Value::\w+\(' crates/ | wc -l` is identical
before and after the migration; no call site outside `cs-gc`
needs a code change beyond removing dead `Trace`/`heap` symbols.

### NFR-5. WASM target stays green

WASM (M10 Track W) builds with no `Heap`/`Trace` plumbing today
*because the tracing layer is already idle*. Removing the
infrastructure should be a strict simplification for the WASM
target.

**Acceptance**: `cargo build --target wasm32-unknown-unknown -p
cs-runtime --no-default-features --features ffi-trait` succeeds;
the existing WASM conformance harness (per
`docs/milestones/m10-trackW-exit.md`) stays green.

### NFR-6. AOT pipeline stays green

Per `docs/milestones/aot-hardening-plan.md` the AOT path is a
post-1.0 hardening focus. This spec must not regress the M10
Track A numeric-kernel pipeline (`crabscheme aot foo.scm
--build` for self-recursive numeric kernels). The AOT-emitted
Rust code uses the same `cs_core::Value` and `Gc<T>` surface as
the VM; the swap of `Gc<T>`'s inner representation is invisible
to the emitted code.

**Acceptance**: `crates/cs-aot/tests/*` (RawI64 fib parity vs
`rustc -O`, etc.) stays green; the AOT-emitted `Cargo.toml` /
emitted Rust source is byte-identical before and after the
migration.

### NFR-7. Documentation

A new ADR (`docs/adr/0014-countable-memory.md`) records:
- Why the tracing infrastructure is being removed (idle since
  M5; arena swap deferred; cycle handling via targeted detection
  is cheaper steady-state than tracing).
- What supersedes ADR 0006 (the algorithm choice, the rooting
  choice, the hand-rolled-vs-crate choice — all rendered moot
  by going RC-only).
- The cycle-collector algorithm choice (Bacon–Rajan synchronous
  local) and the `Weak<T>` back-edge inventory.
- The migration sequencing and the rollback story.

ADR 0006 is amended with a "Superseded by ADR 0014" header but
left in place as project history.

---

## Out of scope (explicitly)

| Item | Why excluded |
|---|---|
| Generational copying | Was deferred from M5 Phase 2 — RC-only obviates it. If a future workload demonstrably needs copying, that's a separate spec; nothing here forecloses it. |
| Multi-threaded `Arc<T>` | CrabScheme is single-runtime/single-thread per `Runtime` instance today; multi-threaded execution is a post-M11 stretch and would need its own spec for `Arc<T>` and concurrent cycle handling. |
| Conservative stack scan | Already rejected by ADR 0006. Refcounting needs no stack scan at all. |
| Weak references at the Scheme surface (R7RS `make-weak-ref`) | Independent feature; can land on RC-only or tracing, no dependency either way. |
| Removing `Rc<RefCell<...>>` in favor of pure functional structures | Massive refactor for marginal benefit; out of scope. |

---

## Risks

1. **Cycle-collector correctness for `hashtable-set!`-induced cycles.**
   Hashtables can hold cycles where the key reaches back to the
   table itself (rare in idiomatic Scheme but legal). The
   synchronous local detector must traverse the table's items
   vector plus the key-side `custom hash`/`equiv` `Value` slots
   from the `Hashtable` struct.
   *Mitigation*: enumerate the heap-pointer fields per variant in
   `design.md`; property-test the detector against random graph
   shapes that include hashtable cycles.

2. **`Procedure` cycle through `Rc<dyn Procedure>`.**
   Procedures hold closure environments which can refer back to
   the procedure (via `letrec`, `define`-in-let, or `set!`). The
   `Procedure` trait is `dyn`, so the cycle detector can't
   statically enumerate its children — we need a small `Procedure`
   trait method (`fn closure_refs(&self, visit: &mut dyn
   FnMut(&dyn Any))` or similar) to expose closure fields for the
   detector.
   *Mitigation*: a focused trait surface for cycle-detection
   visibility, audited per impl; covered in `design.md`.

3. **Performance regression on hot mutation paths.**
   Running cycle detection after *every* `set-car!` could regress
   programs that mutate aggressively (e.g., `binary-trees`
   benchmark, queue implementations).
   *Mitigation*: lazy / deferred cycle detection — buffer suspect
   slots and process in batches; or use a cheap precheck (only
   run the detector if the value being written-in could form a
   cycle, e.g., it's already a heap value reachable from the
   mutated container). Quantify in NFR-2 acceptance.

4. **Continuation `Weak<Frame>` upgrade failures.**
   If we mis-identify which frame should be `Strong` vs `Weak`,
   a `call/cc`-restored continuation could fail to upgrade its
   parent and panic. This is the highest-stakes risk.
   *Mitigation*: continuation Trace impls already enumerate the
   exact parent-frame chain — that data drives the Weak/Strong
   decision algorithmically (the leaf is strong, all parents
   are weak; the leaf's `Rc<Frame>` keeps the chain alive via
   the existing parent pointer in `Frame`, which we keep
   strong). Differential test against the M8 continuation suite.

5. **AOT-emitted code coupling to `Gc<T>` internals.**
   If the AOT backend currently emits code that pattern-matches
   on `Gc<T>` internals (e.g., reading the `Slot<T>` mark bit),
   the swap breaks it.
   *Mitigation*: `rg 'Slot\|mark' crates/cs-aot/` shows zero
   hits today — AOT only uses `Gc::into_raw_jit` /
   `Gc::from_raw_jit`, which stay stable.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `cs-gc` collapsed to Rc-backed `Gc<T>` facade, < 150 LOC | `wc -l crates/cs-gc/src/lib.rs` |
| Workspace-wide `Trace`/`Heap::add_root` removed | `rg` audit per FR-2 |
| Synchronous cycle collector on mutating ops | `crates/cs-runtime/tests/cycle_break.rs` |
| Port deterministic finalization | `crates/cs-runtime/tests/port_finalization.rs` |
| Continuation / closure cycle prevention | `crates/cs-runtime/tests/closure_cycle.rs` |
| Conformance ≥ 2150 individual tests | both harnesses green |
| JIT raw-handle ABI byte-compatible | M6/M10 parity tests green |
| `Procedure` no longer the exceptional variant | trait supertraits cleaned |
| Per-allocation overhead ↓ ≥ 10% | `bench/alloc_overhead.rs` |
| `alloc-stress` within 5% of M5 Phase 1 baseline | `bench/microbench/` |
| WASM build green | `cargo build --target wasm32-unknown-unknown` |
| AOT pipeline green | `crates/cs-aot/tests/*` |
| `cargo-geiger` `unsafe` count unchanged | report diff |
| ADR 0014 written; ADR 0006 marked superseded | `docs/adr/` |
