# Tasks Document — Countable Memory

> Companion: `requirements.md`, `design.md`.
> Format mirrors the M5/foundation spec tasks layout: per-task file
> paths, leverage hooks, prompt scaffolds, and exit criteria.
> Sequencing follows `design.md` §"Migration plan" Steps A–F.

The work is split into **eight numbered iters**. Each iter is a
self-contained commit per the project's per-iter commit policy
(`feedback_milestone_commits.md`). The feature flag
`countable-memory` gates the swap so any iter can be reverted
independently until iter 7 flips the default.

---

## Step A — Foundation

- [ ] 1. Add `countable-memory` feature flag and Rc-only `Gc<T>` variant
  - File: `crates/cs-gc/Cargo.toml`, `crates/cs-gc/src/lib.rs`
  - Add `[features] countable-memory = []` to `crates/cs-gc/Cargo.toml`. Workspace `Cargo.toml` registers the feature in `[workspace.dependencies]`'s `cs-gc` entry so consumer crates can forward it.
  - In `crates/cs-gc/src/lib.rs`, introduce a `#[cfg(feature = "countable-memory")] mod rc_backed { ... }` module that defines `Gc<T>` as `struct Gc<T: ?Sized>(Rc<T>)` with the full public API from design.md §"Component 1": `new`, `Clone`, `Deref`, `PartialEq`, `ptr_eq`, `as_addr`, `into_raw_jit`, `from_raw_jit`, `raw_incref`, plus the new `downgrade` and `strong_count` accessors.
  - Re-export the active `Gc` from the crate root depending on the feature flag.
  - The existing `Heap`/`Trace`/`Marker`/`Slot` paths stay live under `#[cfg(not(feature = "countable-memory"))]`.
  - Purpose: stand up the new representation behind a flag so the workspace can still build under the current default; nothing observable changes for existing consumers.
  - _Leverage: existing `Gc::into_raw_jit` / `from_raw_jit` / `raw_incref` impls in `crates/cs-gc/src/lib.rs:115-156` — they already wrap `Rc::into_raw` etc., so the new variant inherits the same bodies verbatim._
  - _Requirements: FR-1, NFR-3, NFR-4_
  - _Prompt: Role: Rust systems engineer with deep knowledge of `std::rc::Rc` semantics and feature-flag-gated module patterns | Task: Stand up an Rc-backed `Gc<T>` newtype under a `countable-memory` feature in `crates/cs-gc/src/lib.rs` per FR-1 and design.md §"Component 1", preserving every method the existing M5 Phase 1 `Gc<T>` exposes (with byte-compatible semantics on `into_raw_jit`/`from_raw_jit`/`raw_incref`) and adding `downgrade`/`strong_count`. Keep the existing tracing-backed `Gc<T>` available under `#[cfg(not(feature = "countable-memory"))]`. Mirror the doc-comment style of the existing crate | Restrictions: do not delete any existing symbol yet; do not change the default feature set; do not touch consumer crates; preserve the `#![allow(clippy::missing_safety_doc)]` and the SAFETY discipline on every `unsafe fn` | Success: `cargo build -p cs-gc` passes under default features; `cargo build -p cs-gc --features countable-memory` passes; `cargo test -p cs-gc` green; `cargo test -p cs-gc --features countable-memory` exercises at least the existing 11 cs-gc unit tests against the new variant where applicable (the Heap/Marker/Trace tests stay default-only)._

- [ ] 2. Re-export `Weak<T>` from `cs-gc`
  - File: `crates/cs-gc/src/lib.rs`
  - Under `#[cfg(feature = "countable-memory")]`, add `pub use std::rc::Weak as RawWeak;` and a thin `pub struct Weak<T>(RawWeak<T>);` whose `upgrade(&self) -> Option<Gc<T>>` returns the workspace `Gc<T>` type (not `std::rc::Rc<T>`).
  - Implement `Clone`, `Default::default() -> Weak<T> = Weak(RawWeak::new())`, `Weak::new()` factory.
  - Purpose: consumer crates hold weak back-edges without importing `std::rc` directly, preserving the abstraction wall.
  - _Leverage: `std::rc::Weak`._
  - _Requirements: FR-5, NFR-4_
  - _Prompt: Role: Rust library author | Task: Add a thin `cs_gc::Weak<T>` wrapper that mirrors `std::rc::Weak<T>` but whose `upgrade` returns `Option<cs_gc::Gc<T>>`, per design.md §"Component 2". Gate on `#[cfg(feature = "countable-memory")]` | Restrictions: do not add APIs beyond `new`, `upgrade`, `Clone`, `Default`; do not expose `RawWeak` outside the crate as anything other than the implementation detail | Success: cargo build / test green under both feature configurations; a doctest demonstrates the upgrade-after-drop pattern returning `None`._

---

## Step B — Cycle collector skeleton

- [ ] 3. Implement `cs_gc::cycle` module with bounded-DFS detector
  - File: `crates/cs-gc/src/cycle.rs` (new), `crates/cs-gc/src/lib.rs`
  - Define the `CycleVisit` trait per design.md §"Component 3":
    ```rust
    pub trait CycleVisit {
        fn visit_children(&self, visit: &mut dyn FnMut(usize) -> bool) -> bool;
    }
    ```
  - Implement `cycle_check<T: CycleVisit + 'static>(root: &Gc<T>) -> Option<CyclePath>` using bounded DFS with a `HashSet<usize>` of visited slot addresses (via `Gc::as_addr`). Default node-visit limit: 10_000; configurable via a `cs_gc::cycle::set_limit(u32)` thread-local setter.
  - Implement `check_and_break<T>(root: &Gc<T>, break_at: impl FnOnce(&Gc<T>))` convenience.
  - Define `CyclePath` as `pub struct CyclePath { pub nodes: Vec<usize> }` — opaque to consumers; only used for tests and diagnostics.
  - Gate the whole module on `#[cfg(feature = "countable-memory")]`.
  - Purpose: implement the cycle-detection primitive that every mutation builtin will call.
  - _Leverage: nothing — pure new code. The DFS pattern is standard graph traversal._
  - _Requirements: FR-3, NFR-2_
  - _Prompt: Role: Rust developer comfortable with iterative graph algorithms and trait-object visitors | Task: Implement `cs_gc::cycle` per design.md §"Component 3", with a bounded-DFS algorithm that never recurses on the host stack (use an explicit Vec worklist), a per-call `HashSet<usize>` visited set keyed on `Gc::as_addr`, and a `CycleVisit` trait whose `visit_children` returns `bool` to short-circuit on first cycle | Restrictions: no host-stack recursion (adversarial graphs must not overflow); no global mutable state beyond the thread-local limit setter; do not allocate beyond the visited set + worklist; no `unsafe` | Success: 5 unit tests in the same file covering "no cycle / direct self-loop / 2-node mutual cycle / 3-node ring / deep chain (no cycle, near limit)" pass; a fuzz-style proptest with random `Node { children: Vec<Gc<Node>> }` graphs of up to 100 nodes finds every cycle the test fixture knows it constructed._

- [ ] 4. Standalone cycle-collector tests in `cs-gc`
  - File: `crates/cs-gc/tests/cycle.rs` (new)
  - Define a test-only `Node { children: RefCell<Vec<Gc<Node>>> }` type with `impl CycleVisit`.
  - Tests:
    - `no_cycle_linear_chain` — N=10 chain returns `None`.
    - `direct_self_loop` — node points to itself; returns `Some(path)` with the right node count.
    - `two_node_mutual` — A→B, B→A.
    - `three_node_ring` — A→B→C→A.
    - `unrelated_graphs_dont_confuse` — two disjoint cycles; only the one reachable from `root` shows up.
    - `limit_exceeded_returns_none` — chain of 10⁵ with limit set to 10³ returns `None` without panicking.
  - Purpose: lock in cycle-detection semantics independent of any Scheme code, so Step C can wire mutation builtins to a known-good detector.
  - _Leverage: `cs_gc::cycle::CycleVisit`, `cs_gc::Gc` (countable-memory variant)._
  - _Requirements: FR-3_
  - _Prompt: Role: Rust test engineer | Task: Write `crates/cs-gc/tests/cycle.rs` covering the six scenarios listed in the task above, with `#![cfg(feature = "countable-memory")]` at file scope | Restrictions: no dependency on `cs-core` (the cs-gc crate must remain leaf); use only the test-fixture `Node` type | Success: `cargo test -p cs-gc --features countable-memory` runs the new tests green._

---

## Step C — Consumer-crate migration under the flag

- [ ] 5. Add `CycleVisit` impls in `cs-core` parallel to the existing `Trace` impls
  - File: `crates/cs-core/src/value.rs`
  - For each `impl cs_gc::Trace for {Pair, Hashtable, Port, Promise, Parameter, Value}`, add an `#[cfg(feature = "countable-memory")] impl cs_gc::CycleVisit for ...` whose `visit_children` enumerates the same `Gc<...>` children, calling `visit(child.as_addr())` and returning early on `true`.
  - `Port`'s and most leaf types' `visit_children` is empty (returns `false`).
  - Remove the `cs_gc::Trace` supertrait from `Procedure` under the feature; add `visit_closure_children` method with default `false` impl per design.md §"Component 7".
  - Forward the feature in `crates/cs-core/Cargo.toml`: `cs-gc = { workspace = true, default-features = false }` plus a feature `countable-memory = ["cs-gc/countable-memory"]`.
  - Purpose: every heap-bearing type knows how to enumerate its `Gc<...>` children for the detector.
  - _Leverage: the existing `Trace` impls at the file-grep locations from FR-2 — they enumerate exactly the same children._
  - _Requirements: FR-2, FR-3, FR-8_
  - _Prompt: Role: Rust developer doing a mechanical refactor across a stable codebase | Task: Add `CycleVisit` impls in `crates/cs-core/src/value.rs` parallel to every existing `Trace` impl, mapping the same set of `Gc<...>` children to `visit(addr)` calls per design.md §"Component 4". Add a `Procedure::visit_closure_children` method with a default empty impl. Forward the `countable-memory` feature through `crates/cs-core/Cargo.toml` | Restrictions: do not remove the existing `Trace` impls yet (iter 8 does that); do not change any `Value` variant; do not touch consumer crates beyond Cargo.toml | Success: `cargo build -p cs-core --features countable-memory` passes; the new `CycleVisit` impls compile against the iter-3 `cs-gc` module._

- [ ] 6. Add `CycleVisit` impls in `cs-runtime` and `cs-vm`
  - File: `crates/cs-runtime/src/{env.rs, proc.rs}`, `crates/cs-vm/src/vm.rs`, both Cargo.toml files
  - Mirror iter 5 for `Frame`, `Builtin`, `Closure`, `Continuation`, `HostBuiltin`, `VmClosure`, `Bindings`, `Env`.
  - For the ~47 zero-payload procedure markers currently using `trace_leaf_proc!`, leave them on the default `visit_closure_children -> false` impl — no per-type code needed.
  - Forward the feature in both crates' `Cargo.toml`.
  - Purpose: complete the `CycleVisit` coverage so the detector can walk every heap-bearing type.
  - _Leverage: existing `Trace` impls at `crates/cs-runtime/src/env.rs:58`, `:proc.rs:44,68,99,146`, `crates/cs-vm/src/vm.rs:10583,10654,10754`._
  - _Requirements: FR-2, FR-3_
  - _Prompt: Role: Rust developer doing a parallel refactor across runtime and VM crates | Task: Add `CycleVisit` impls in `crates/cs-runtime/src/{env,proc}.rs` and `crates/cs-vm/src/vm.rs` parallel to each existing `Trace` impl, plus forward the `countable-memory` feature in both `Cargo.toml`s | Restrictions: leave the `trace_leaf_proc!` macro and its 47 invocations in place — they're trivially-default under the new trait; do not delete `Trace` impls yet; do not modify the JIT stackmap module | Success: `cargo build --workspace --features countable-memory` passes; full test suite green under both feature configurations._

---

## Step D — Mutation-site cycle checks and Weak<T> back-edges

- [ ] 7. Wire `cycle_check_after_mutation` into mutation builtins
  - File: `crates/cs-runtime/src/builtins/mod.rs`, `crates/cs-vm/src/vm.rs`
  - In each of `b_set_car`, `b_set_cdr`, `b_vector_set`, `b_hashtable_set` (and the corresponding VM-tier opcode handlers for mutation), append:
    ```rust
    #[cfg(feature = "countable-memory")]
    cs_gc::cycle::check_and_break(&parent_gc, |p| {
        // Per-type cycle-break action: replace the offending slot
        // with a Weak edge (design.md §"Component 5").
    });
    ```
  - Implement the slot-downgrade machinery for `Pair`, `Vector`, `Hashtable` per design.md §"Data models" — introduce `PairSlot::{Strong,Weak}`, `VectorSlot::{Strong,Weak}` enums; update the accessors to transparently `Weak::upgrade()` and fall back to `Value::Unspecified` on `None`.
  - Purpose: mutation-induced cycles are detected and structurally broken via a `Weak` edge while preserving R6RS-observable identity.
  - _Leverage: `cs_gc::cycle::check_and_break` from iter 3; the existing `b_set_car` / `b_set_cdr` / `b_vector_set` / `b_hashtable_set` definitions._
  - _Requirements: FR-3, FR-4_
  - _Prompt: Role: Rust developer building runtime mutation primitives with cycle-aware storage | Task: For each of the four list/vector/hashtable mutation builtins in `crates/cs-runtime/src/builtins/mod.rs` and their VM-tier opcode counterparts in `crates/cs-vm/src/vm.rs`, integrate `cs_gc::cycle::check_and_break` per design.md §"Component 5", with the per-type slot-downgrade action that flips the offending edge from `Strong(Value)` to `Weak(WeakValue)`. Introduce the `PairSlot`/`VectorSlot` enums and `WeakValue` per design.md §"Data models" | Restrictions: the user-observable semantics of `(set-cdr! x x)` must still produce a cyclic list reachable via `(car x)`/`(cdr x)`; do not eagerly break user cycles — only flip the *internal* representation; gate all new code on `#[cfg(feature = "countable-memory")]` | Success: a new test file `crates/cs-runtime/tests/cycle_break.rs` covering `(set-cdr! x x)`, vector self-loop, hashtable value-self-loop, and a 3-way mutual `set-car!` cycle, each asserting that after dropping the outer handle the runtime's allocation counter (a sentinel wrapping `Gc::new`) returns to its pre-allocation value, runs green under the feature flag._

- [ ] 8. Refactor `Frame.parent`, continuations, and closures to use `Weak<T>` back-edges
  - File: `crates/cs-runtime/src/env.rs`, `crates/cs-runtime/src/proc.rs`, `crates/cs-vm/src/vm.rs`
  - Under `#[cfg(feature = "countable-memory")]`:
    - Change `Frame { parent: Rc<Frame>, ... }` to `parent: cs_gc::Weak<Frame>`; update the walker (`crates/cs-runtime/src/eval.rs`) to call `parent.upgrade().expect("frame parent dropped")` at each ascend.
    - Refactor `Continuation` construction to keep the leaf frame strong and parent chain weak.
    - Refactor `Closure` and `VmClosure` to use the two-phase letrec-style allocation for self-referential bindings, writing the closure handle as `Weak` into self-bindings.
  - Purpose: structurally prevent the most common known cycle shapes from forming at all, leaving the synchronous detector (iter 7) to handle the residual general-purpose cases.
  - _Leverage: the existing letrec / `define` two-phase binding mechanism in the walker (search for `RecBinding` / placeholder patterns in `crates/cs-runtime/src/eval.rs`)._
  - _Requirements: FR-5, FR-7_
  - _Prompt: Role: Rust developer with familiarity in Scheme evaluator implementation and continuation capture | Task: Refactor `Frame`, `Continuation`, `Closure`, and `VmClosure` under `#[cfg(feature = "countable-memory")]` to use `Weak<T>` for the parent / self back-edges per design.md §"Component 6", with the two-phase placeholder-then-back-fill allocation for closures whose env contains self-bindings | Restrictions: never `.upgrade().unwrap()` without a clear diagnostic on `None` (an unwrap-fail here is a runtime bug, not a user error — the message must point at the construction site); leave the non-feature-gated path unchanged; differential parity with the VM tier must hold | Success: `crates/cs-runtime/tests/closure_cycle.rs` (new) constructing self-referential closures plus call/cc-captured continuations and asserting no leaks via the iter-7 sentinel counter is green; the M8 continuation-suite tests stay green under the feature flag._

---

## Step E — Validation gates

- [ ] 9. New regression tests: cycle_break, port_finalization, closure_cycle
  - File: `crates/cs-runtime/tests/cycle_break.rs` (extend from iter 7), `crates/cs-runtime/tests/port_finalization.rs` (new), `crates/cs-runtime/tests/closure_cycle.rs` (extend from iter 8)
  - `port_finalization.rs` opens a file output port, writes 100 KiB, drops the only handle (no explicit `close-port`), then reads the file from a fresh `std::fs::read_to_string` and asserts content correctness. Run on Linux + macOS; skip on WASM.
  - Extend `cycle_break.rs` to include hashtable-with-cyclic-key-value, and the iter-7 R6RS conformance check that `(set-cdr! x x)` followed by `(length x)` (which R6RS defines as undefined behavior on cycles) does not panic — it errors or loops within the existing cycle-detection length wrappers.
  - Extend `closure_cycle.rs` with a call/cc continuation that re-invokes itself N times then drops; verify the recorded `Rc::strong_count` on a sentinel object returns to 1 (the test-held handle) after the runtime drops.
  - Purpose: lock in the FR-3/FR-4/FR-5 acceptance contracts with explicit tests.
  - _Leverage: existing `Runtime` test helpers in `crates/cs-runtime/tests/common/`._
  - _Requirements: FR-3, FR-4, FR-5_
  - _Prompt: Role: Rust QA engineer | Task: Author or extend the three test files listed to cover the FR-3/FR-4/FR-5 acceptance criteria, with a thread-local allocation counter wrapping `Gc::new` to track leaks via sentinel objects per design.md §"Testing strategy" | Restrictions: tests must work under `--features countable-memory` only (the tracing-backed default has different leak characteristics); skip filesystem-dependent tests on `target_arch = "wasm32"`; do not depend on test order | Success: `cargo test --workspace --features countable-memory --test cycle_break --test port_finalization --test closure_cycle` is green._

- [ ] 10. Benchmarks: alloc_overhead and cycle_collect_timing
  - File: `bench/alloc_overhead.rs` (new), `crates/cs-runtime/tests/cycle_collect_timing.rs` (new, replaces gc_timing.rs under the flag)
  - `alloc_overhead.rs` allocates 10⁶ small `Gc<Pair>` values, measures peak RSS via the same `procfs`/`mach_task_basic_info` hook used by `crates/cs-runtime/tests/gc_memory.rs`. Asserts ≤ 90% of the M5 Phase 1 baseline captured in `bench/m5-phase1-baseline.json`.
  - `cycle_collect_timing.rs` builds a 1k-node graph (chain + occasional back-edge), runs `cycle_check_after_mutation` per write, captures p50/p95/p99 per-call durations. Asserts p99 < 100 µs.
  - Capture results in `bench/countable-memory-baseline.json`.
  - Purpose: lock in NFR-1 and NFR-2 numerically before flipping the default.
  - _Leverage: `crates/cs-runtime/tests/gc_memory.rs` for the RSS hook; `crates/cs-runtime/tests/gc_timing.rs` for the timing-histogram pattern._
  - _Requirements: NFR-1, NFR-2_
  - _Prompt: Role: Rust performance engineer | Task: Implement the two new benches/tests covering NFR-1 (per-allocation overhead reduction ≥ 10%) and NFR-2 (cycle-collect p99 < 100 µs on a 1k-node graph), capturing both in `bench/countable-memory-baseline.json` for the exit report | Restrictions: read the M5 Phase 1 baseline from `bench/m5-phase1-baseline.json` (don't hardcode); skip on WASM; don't `panic!` if the host can't read RSS — emit a `skipping: no RSS hook available` and pass | Success: numbers captured and committed; both assertions hold on the developer's reference machine; the run takes < 30 s in CI._

---

## Step F — Flip the default and delete tracing

- [ ] 11. Flip the `countable-memory` feature to default-on across the workspace
  - File: `crates/cs-gc/Cargo.toml`, `crates/cs-core/Cargo.toml`, `crates/cs-runtime/Cargo.toml`, `crates/cs-vm/Cargo.toml`, root `Cargo.toml`
  - Set `default = ["countable-memory"]` in each crate's `[features]` block.
  - Verify `cargo test --workspace` (no flags) runs the same suite that previously ran under `--features countable-memory`.
  - Capture the result in `bench/countable-memory-baseline.json` alongside the M5 numbers for comparison.
  - Purpose: make the new representation the only production path.
  - _Leverage: Step E acceptance numbers._
  - _Requirements: All FRs + NFR-2, NFR-5, NFR-6_
  - _Prompt: Role: Rust release engineer | Task: Flip `countable-memory` to default-on in every workspace crate's `[features]` block. Validate the full test suite, conformance harness, and WASM build all stay green | Restrictions: do not delete the non-feature-gated tracing code yet (iter 12 does that); do not bump version numbers; do not change emitted AOT Cargo.toml templates (the feature is local to cs-gc/cs-core/cs-runtime/cs-vm) | Success: `cargo test --workspace` and `cargo build --target wasm32-unknown-unknown -p cs-runtime --no-default-features --features ffi-trait` both green; conformance ≥ 2150 maintained._

- [ ] 12. Delete tracing infrastructure and ratify ADR 0014
  - File: `crates/cs-gc/src/lib.rs`, all `impl cs_gc::Trace` sites workspace-wide, `crates/cs-runtime/src/lib.rs` (heap construction + root closures), `docs/adr/0014-countable-memory.md` (new), `docs/adr/0006-gc-design.md` (header amend)
  - Delete: `Slot`, `SlotValue`, `Marked`, `Trace`, `Marker`, `Heap`, `add_root`, `set_auto_collect`, `collect`, `trace_leaf!`, `trace_leaf_proc!`, `Runtime::heap` field, the three `heap.add_root` blocks in `Runtime::new`, every `impl cs_gc::Trace for ...` block.
  - Remove the `countable-memory` feature flag — the new representation is unconditional.
  - Write `docs/adr/0014-countable-memory.md` ratifying the move per NFR-7, with the supersession of ADR 0006's algorithm/rooting/crate decisions.
  - Amend `docs/adr/0006-gc-design.md` with a "**Status: Superseded by ADR 0014**" header. Leave the body intact as project history.
  - Update `crates/cs-gc/src/lib.rs`'s module doc to reflect the new model.
  - Write `docs/milestones/countable-memory-exit.md` summarizing what shipped and the perf numbers.
  - Mark the spec status `CLOSED` (update the header in this `tasks.md`, `requirements.md`, `design.md`).
  - Purpose: complete the simplification; lock the new shape into the project history.
  - _Leverage: `docs/adr/0006-gc-design.md` for ADR style; `docs/milestones/m5-exit.md` for exit-report style._
  - _Requirements: All FRs, NFR-7_
  - _Prompt: Role: Rust developer + documentation author | Task: Delete every tracing-related symbol per the file list in this task, write `docs/adr/0014-countable-memory.md` per NFR-7 covering the four decision points (why retire tracing, what supersedes ADR 0006, cycle-collector algorithm choice, Weak<T> back-edge inventory), amend ADR 0006 with the supersession header, write `docs/milestones/countable-memory-exit.md` in the M5 exit-report style, and mark the spec status CLOSED in all three spec files | Restrictions: do not delete the JIT raw-handle ABI (`into_raw_jit` / `from_raw_jit` / `raw_incref`) — those stay; do not modify `crates/cs-vm/src/jit_stackmap.rs`; do not regress any conformance test; the resulting `wc -l crates/cs-gc/src/lib.rs` must be < 150 | Success: `rg 'impl.*Trace for|trace_leaf|add_root|Heap::|Marker' crates/` returns no GC-related hits (test fixtures aside); `wc -l crates/cs-gc/src/lib.rs` < 150; `cargo-geiger` `unsafe` count in `cs-gc` unchanged from M5 Phase 1 (still just the three JIT ABI fns); conformance ≥ 2150 maintained; ADR 0014 landed and 0006 amended._

---

## Sequencing summary

| Step | Iter | Title | Reversible? | Default-on? |
|------|------|-------|-------------|-------------|
| A | 1 | Feature flag + Rc-only Gc<T> variant | yes | no |
| A | 2 | `cs_gc::Weak<T>` re-export | yes | no |
| B | 3 | `cs_gc::cycle` module | yes | no |
| B | 4 | Standalone cycle-collector tests | yes | no |
| C | 5 | CycleVisit impls in cs-core | yes | no |
| C | 6 | CycleVisit impls in cs-runtime + cs-vm | yes | no |
| D | 7 | Wire mutation builtins + Strong/Weak slots | yes | no |
| D | 8 | Frame.parent / Continuation / Closure → Weak | yes | no |
| E | 9 | Regression tests (cycle_break, port_finalization, closure_cycle) | yes | no |
| E | 10 | Benchmarks (alloc_overhead, cycle_collect_timing) | yes | no |
| F | 11 | Flip default-on | yes until iter 12 | **yes** |
| F | 12 | Delete tracing infra + ADR 0014 | **no** (point of no return) | yes |

Iters 1–10 are pure additions behind a flag. Iter 11 swaps the
default but the old path remains available via `--no-default-features`.
Iter 12 is the deletion commit and the point of no return; it
lands only after Steps A–E have demonstrated parity.

## Rollback story

- Iters 1–10: revert the iter's commit.
- Iter 11: revert to restore the old default.
- Iter 12: revert is structurally possible but practically painful
  because conformance / benchmarks land downstream. The exit report
  captures the M5 Phase 1 baseline and the countable-memory baseline
  side-by-side so a future maintainer can quantify what to restore.
