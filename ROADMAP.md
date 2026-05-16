# CrabScheme Roadmap

> A milestone-by-milestone plan from empty workspace to fully verified, vetted,
> JIT-accelerated R6RS Scheme.

This roadmap is a living document. Per-milestone work products are tracked as
specs under `.spec-workflow/specs/`; this page is the index that says *which
spec, in which order, with what exit criteria*.

| Milestone | Theme                                | Spec slug              | Exit gate                                                  | Status |
| --------- | ------------------------------------ | ---------------------- | ---------------------------------------------------------- | ------ |
| M0        | Bootstrap workspace + value type     | `foundation`           | `cargo build --workspace` green; CI gates wired            | ✅ done |
| M1        | Lexer + reader + diagnostics         | `foundation`           | round-trip property holds; ≥ 80 lexer/parser tests pass    | ✅ done |
| M2        | Tree-walker + REPL + CLI             | `foundation`           | ≥ 100 conformance tests pass; golden tests green           | ✅ done |
| M3        | Hygienic macro expander              | `expander`             | Larceny macro tests ≥ 80% pass; bootstrap stdlib in Scheme | ✅ done |
| M4        | Bytecode VM (warm tier)              | `vm`                   | differential tests pass tree-walker vs VM on ≥ 1k corpus   | ✅ tagged `m4-complete` (1460 tests) |
| M5        | Precise tracing GC                   | `gc`                   | 24-hour fuzz no leaks; sub-1ms GC pause p99 on stdlib load | ✅ tagged `m5-complete` |
| M6        | JIT abstraction + Cranelift backend  | `jit-cranelift` + `jit-optimizing-tier` (Phase 6) | Three perf gates per ADR 0013: JIT ≥ 10× walker; competitive with mature Scheme JITs; alloc-light workloads ≤ 100 allocs/ms | ✅ Phase 1–6 done (`m6-phase6-complete`); all three reframed gates MET as of 2026-05-16. Original 5× JIT-over-VM gate superseded — see `docs/adr/0013-perf-gate-reframe.md`. Post-Phase-6: JIT 10.4× walker, beats Chez/Guile/Gambit-interp, 0 allocs on 6 of 8 benches. |
| M7        | HolyJIT backend (primary)            | `jit-holy`             | JIT differential parity with Cranelift backend             | ⏸ parked (ADR 0009 — upstream stale) |
| M8        | First-class continuations + CWCC     | `continuations`        | Larceny cont tests ≥ 95% pass                              | ✅ VM-tier done (`m8-vm-complete`); walker-tier + Larceny suite deferred |
| M9        | R6RS standard library completion     | `stdlib`               | R6RS conformance ≥ 99%; Larceny suite ≥ 95%                | ✅ foundation done (`m9-foundation-complete`); R6RS conformance **99.96%** on our corpus / **100%** Racket-cross-validated subset (2026-05-15) — Larceny @ 94% on parsable slice, reader shims would expand sample |
| M10       | AOT compiler + WASM target           | `aot`, `wasm`          | Static binaries from Scheme; WASM bytecode tier shipping   | 🚧 OPEN as of 2026-05-16 — **Track W (WASM) COMPLETE** (`m10-wasm-complete`, conformance 2438/1 matches native), Track A (AOT) queued |
| M11       | Verified core (stretch)              | `verification`         | Mechanized eval semantics with extracted reference         | — |

Each milestone produces a tagged release (`milestone-N-complete`) and a written
exit report under `docs/milestones/Mx-exit.md` capturing what shipped, what was
deferred, perf/conformance baselines, and known limitations.

---

## M0–M2: Foundation

**Spec:** [`.spec-workflow/specs/foundation`](.spec-workflow/specs/foundation/)

The first three roadmap items collapse into a single spec because they all
fall under "make a tree-walking R6RS subset run". See `requirements.md`,
`design.md`, and `tasks.md` in that spec for full detail.

**Deliverable:** `crabscheme run program.scm` and `crabscheme repl` running
real Scheme programs that exercise R6RS arithmetic, lists, strings,
characters, equality, control flow, and basic I/O — with rustc-quality
diagnostics, structured tracing, and ≥ 100 conformance tests passing in CI.

**Explicitly out of scope at foundation:** hygienic macros, full continuations,
records, libraries (just monolithic top-level), bytecode VM, JIT, precise GC,
WASM. Those have their own milestones.

---

## M3: Hygienic Macro Expander

**Spec slug:** `expander`

R6RS macros (`syntax-rules`, `syntax-case`) are the linchpin of the language —
much of the "standard library" is defined as macros over a tiny core. This
milestone replaces the foundation's core-form recognizer (`cs-expand`) with a
real hygienic expander that:

- Implements `syntax-rules` per R6RS §11.19 (pattern matching, template
  expansion, hygiene via wrapping syntax objects).
- Implements `syntax-case` per R6RS §12 (low-level explicit hygiene control).
- Supports `let-syntax`, `letrec-syntax`, `define-syntax`.
- Preserves source spans through expansion so error messages still point at
  user code, not into macro internals (provenance / "marks" tracking).
- Bootstraps a meaningful subset of the R6RS stdlib written in Scheme on top
  of the macro system (e.g., `case-lambda`, `parameterize`, `do`, `when`,
  `unless` re-implemented as macros, replacing the desugarer).

**Exit gate:**
- Larceny macro test suite ≥ 80% pass.
- Self-hosted Scheme stdlib loads cleanly via the new expander.
- Macro-expansion error messages preserve user-source spans (snapshot tested).
- Fuzzing the expander for 1 hour produces no panics.

**Risks:** hygiene is genuinely hard. Mitigations: study Dybvig's "Syntactic
Abstraction in Scheme" implementation; use Larceny's expander as a reference
implementation we cross-check against; bring up `syntax-rules` first, defer
`syntax-case` to a substep if needed.

---

## M4: Bytecode VM (Warm Tier)

**Spec slug:** `vm`

Tree-walking is correct but slow. The bytecode VM is our warm tier — code that
runs more than a few times gets compiled to bytecode, which the VM dispatches
faster than the tree-walker. This is also a forcing function for the IR
design before we commit to a JIT.

Components:

- **`cs-vm` crate**: instruction set, dispatcher, value stack, frame stack.
- **Lowering pass**: `CoreExpr` → bytecode in `cs-runtime` (or extracted into
  `cs-lower`).
- **Runtime tier dispatch**: `Runtime` chooses tree-walker vs VM per procedure
  based on call count (simple counter-based heuristic for now).
- **NaN-boxed `Value`**: now safe to introduce because the VM benefits most
  from it. Foundation's tagged enum still works for the tree-walker; the VM
  uses NaN-boxed encoding behind the same `Value` API.
- **Differential testing** between tree-walker and VM on the full
  conformance corpus + property-test corpus.

**Exit gate:**
- Differential tests pass on ≥ 10,000 generated expressions.
- VM `(fib 25)` ≥ 5× faster than tree-walker.
- Conformance pass rate equal to tree-walker (no regression).

---

## M5: Precise Tracing GC ✅ DONE

**Spec slug:** `gc`
**Exit report:** [`docs/milestones/m5-exit.md`](docs/milestones/m5-exit.md) (tag `m5-complete`)

Foundation/M4 use `Rc<RefCell<…>>` reference counting. That's fine for
correctness but leaks on cycles unless we run a cycle collector, and it
generally underperforms a real tracing GC. M5 replaces RC with a precise
tracing GC.

Design choices to spec:

- **Algorithm**: start with a simple precise mark-and-sweep + bump allocation;
  upgrade to generational copying once stable.
- **Roots**: per-`Runtime` root set; the VM's value stack is a root set; the
  JIT (when it lands) emits stackmaps so JITted frames participate.
- **Barriers**: write barriers for generational; deferred until generational
  upgrade.
- **Interface**: `Gc<T>` smart pointer replaces `Rc<T>` in `Value`.
- **Concurrent collection**: deferred — start with stop-the-world.

**Exit gate:**
- 24-hour fuzz with leak detector reports no leaks on cyclic structures.
- p99 GC pause < 1 ms on stdlib load.
- Conformance pass rate equal to M4 baseline.
- Memory usage on representative programs no worse than RC + cycle collector.

**Risks:** GC is the second-hardest thing in this project after the JIT. Bring
in a Rust GC veteran if available; pattern-match against Servo's Spidermonkey
GC binding for precise rooting techniques.

---

## M5b: Rust FFI ✅ DONE

**Spec slug:** `ffi`
**Exit:** [`docs/milestones/m5b-exit.md`](docs/milestones/m5b-exit.md), tag `m5b-complete`.

Rust FFI surface so CrabScheme programs can call Rust libraries and Rust
applications can embed CrabScheme as a scripting layer. Independent of the
JIT track (M6/M7) — can ship in parallel — but depends on M5's GC for
correct value rooting across the boundary.

Components to spec:

- **`cs-ffi` crate**: `FromValue` / `IntoValue` marshaling traits, `Pinned<'rt>`
  RAII rooting, `FfiError` uniform error type, versioned C-ABI struct for
  shared-library plugins.
- **`cs-ffi-macros` crate**: `#[crabscheme::host_proc]` proc-macro that wraps
  user functions with the marshaling layer.
- **`Runtime` API additions**: `register_host_procedure`, `pin`,
  `load_shared_library`.
- **Two registration paths**: compile-time (Rust crate depending on
  `cs-runtime`) lands first; runtime dlopen via `(load-shared-library)`
  follows.
- **Error propagation**: every `FfiError` variant becomes a catchable Scheme
  condition.

**Exit gate:**
- Compile-time-registered host procedures are callable from Scheme on both
  walker and VM tiers.
- A separately-built shared library loads via `(load-shared-library "path")`
  and exposes its registered procedures.
- A fuzz test that holds a `Pinned` value across 100k allocations confirms
  GC rooting.
- Each `FfiError` variant produces a catchable Scheme condition.
- An `examples/embedded_runtime.rs` doctest shows the standard host pattern
  and compiles green.
- ADR 0008 written.

**Risks:** Lifetime confusion at the boundary; panic across FFI must not
abort; ABI drift between runtime + shared lib. All mitigated via the
`Pinned<'rt>` borrow, `catch_unwind`, and the versioned ABI struct.

---

## M6: JIT Abstraction + Cranelift Backend ✅ DONE (Phase 1)

**Spec slug:** `jit-cranelift`
**Exit:** [`docs/milestones/m6-exit.md`](docs/milestones/m6-exit.md), tag `m6-complete`.

Before HolyJIT, we land Cranelift as the *first* JIT backend so we have a
known-working tier and a baseline to differentially test the HolyJIT backend
against in M7.

Components:

- **`cs-jit` crate**: `JitBackend` trait, dispatch glue, deopt handling,
  recompilation on type-feedback changes.
- **`cs-rir` crate**: Rust-flavored backend IR consumed by JIT backends. SSA-ish.
- **`cs-jit-cranelift` crate**: `cs-rir → clif → native code`.
- **Tier transition**: from VM, hot procedures (call count > threshold) get
  JITted. Hot loops trigger on-stack-replacement (OSR).
- **Deopt**: when a type-specialized JIT path receives unexpected types, deopt
  to bytecode VM; recompile with broader type feedback.

**Exit gate** (per ADR 0013 — reframed 2026-05-16; originals superseded):
- Differential tests pass: tree-walker == VM == JIT on ≥ 10,000 expressions.
- **JIT geomean ≥ 10× over tree-walker** on the 8-bench microbench.
- **JIT competitive with mature Scheme JITs/interpreters** — geomean ≥ 0.8× Chez/Guile/Gambit-interp respectively.
- **Allocation pressure ≤ 100 allocs/ms** on alloc-light workloads (fib, tak, ack, mandelbrot, spectral-norm, n-body).
- Conformance pass rate unchanged.
- `(jit-dump <proc>)` REPL primitive emits Rust IR + clif IR + native disassembly.

*(Originals: JIT `(fib 30)` within 1.2× of `gcc -O2`, and Gabriel benchmarks geomean ≥ 5× over interpreter. Both superseded by ADR 0013 — the 1.2× gcc gate is physics-bound for managed runtimes, and the 5× JIT-over-VM ratio penalizes shared-infrastructure improvements that benefit absolute perf.)*

---

## M7: HolyJIT Backend ⏸ PARKED (evaluation only)

**Spec slug:** `jit-holy`
**ADR:** [`docs/adr/0009-holyjit-parked.md`](docs/adr/0009-holyjit-parked.md).

> Per the ADR: HolyJIT's last upstream commit is from 2018 (~7 years stale). Integrating would mean fork-and-modernize, not "contribute to upstream." The fallback clause below was drafted into the original ROADMAP precisely for this case; we invoke it here. Cranelift remains the project's sole JIT; the `JitBackend` trait holds the seam open for a future peer backend.

The headline feature. With Cranelift proven, we add HolyJIT as a peer backend
behind the same trait. HolyJIT's value proposition: it specializes Rust
functions annotated `#[jit]`, so we can write the runtime hot path as plain
Rust and let HolyJIT JIT-compile it specialized to the program. This is the
"meta-JIT" model — closer to Truffle/RPython than a hand-rolled IR.

Components:

- **`cs-jit-holy` crate**: implements `JitBackend` by lowering `cs-rir` to
  HolyJIT's annotation API or by exposing the Rust runtime functions directly
  to HolyJIT for specialization.
- **Upstream contributions**: HolyJIT may need fixes to work on modern Rust;
  we budget contribution time to nbp/holyjit.
- **Differential testing**: HolyJIT backend must produce identical results to
  Cranelift backend on the full corpus.
- **Performance comparison**: per-procedure perf telemetry comparing HolyJIT
  vs Cranelift; document tradeoffs.

**Exit gate:**
- HolyJIT produces correct results on ≥ 99% of differential corpus (any gaps
  documented and filed upstream).
- Performance within 50% of Cranelift on Gabriel benchmarks (HolyJIT may be
  faster or slower depending on workload).
- HolyJIT can be selected at runtime via `--jit=holy` flag.

**Fallback:** If HolyJIT integration proves infeasible despite reasonable
upstream effort, this milestone is reframed as "evaluation report" — the
project ships with Cranelift as the primary JIT and HolyJIT integration is
parked with a clear postmortem ADR.

---

## M8: First-class Continuations ✅ DONE (VM tier; walker tier deferred)

**Spec slug:** `continuations`
**Exit:** [`docs/milestones/m8-exit.md`](docs/milestones/m8-exit.md), tag `m8-vm-complete`.

R6RS mandates `call-with-current-continuation`. Foundation explicitly punted
on this; the macros and JIT spec did too. M8 implements full continuations.

Design choices to spec:

- **Stack representation**: heap-allocated frames so `call/cc` is a constant-
  time copy of a pointer rather than O(stack-depth) memcpy. Standard
  technique: the runtime's Scheme stack is *always* heap-allocated; "the
  stack" is just a linked list.
- **One-shot vs general**: detect call/cc usage patterns; one-shot
  continuations get a fast path that doesn't require copying.
- **Interaction with JIT**: JITted frames must be representable in the
  heap-stack format; OSR triggers when call/cc captures a JIT frame.
- **Interaction with `dynamic-wind`**: already handled in foundation for the
  raise/handler case; extended to general continuations here.

**Exit gate:**
- Larceny continuation test suite ≥ 95% pass.
- Generators, coroutines, and amb-style backtracking patterns all work.
- JIT remains correct under continuation capture/invocation.
- Microbenchmark: `call/cc` overhead within 3× of a typical procedure call
  on the JIT.

---

## M9: R6RS Standard Library Completion

**Spec slug:** `stdlib`

Fill in the rest of R6RS:

- **Records** (`(rnrs records syntactic)`, `(rnrs records procedural)`).
- **Conditions** (the full type hierarchy beyond foundation's stub).
- **Libraries** (R6RS `library` form, `import`, `export`, version handling).
- **Hash tables** (`(rnrs hashtables)`).
- **Enumerations** (`(rnrs enums)`).
- **Bytevectors** (foundation has the data type; M9 adds the full operation
  set).
- **Sorting** (`(rnrs sorting)`).
- **I/O ports** (the full R6RS port API: text/binary, transcoders, custom
  ports).
- **Programs vs scripts** (R6RS §8 program syntax).

Plus prioritized SRFIs (1, 13, 14, 19, 27, 41, 69) ported as
`(srfi srfi-N)` libraries.

**Exit gate:**
- R6RS conformance pass rate ≥ 99%.
- Larceny test suite ≥ 95% pass.
- Racket R6RS test suite ≥ 90% pass.
- Public 1.0 release candidate ready.

---

## M10: AOT Compiler + WASM

**Spec slug:** `aot`, `wasm` (two parallel tracks)

Two stretch deliverables in parallel:

### `aot` — Scheme → Rust source → static binary

- **`cs-aot` crate**: emits Rust source from `cs-rir` that, when compiled with
  `rustc`, produces a self-contained binary with the program embedded.
- **Use cases**: distributing Scheme programs as native binaries; reducing
  startup latency to zero; opening the door to Rust LTO over the whole
  program.
- **Relationship to JIT**: AOT and JIT share `cs-rir`; the same lowering
  produces both. AOT is essentially "JIT but emit Rust source instead of
  native bytes".

### `wasm` — CrabScheme on WASM

- **Target**: `wasm32-wasi` for the runtime; eventually `wasm32-unknown-unknown`
  for browser embedding.
- **JIT degradation**: WASM doesn't support runtime native codegen, so the
  WASM build ships only the bytecode VM (no JIT).
- **Stdlib**: Scheme-source stdlib loads identically; Rust-implemented builtins
  recompile to WASM unchanged.

**Exit gate:**
- AOT: a non-trivial Scheme program compiles to a static binary that runs
  identically to the JIT version.
- WASM: CrabScheme runs in `wasmtime` and in a browser; bytecode VM
  conformance pass rate within 2 percentage points of native.

---

## M11: Verified Core (Stretch)

**Spec slug:** `verification`

The optional capstone: machine-checked semantics for CrabScheme's evaluation
core, with the Coq/Lean reference interpreter cross-checked against the Rust
implementation in CI.

This is a research-flavored milestone with a year-or-more timeline; it doesn't
gate any other release but lifts CrabScheme into "formally verified" territory
which is rare among Scheme implementations.

**Exit gate:**
- Mechanized small-step semantics for `CoreExpr` in Coq or Lean.
- Soundness theorem proved.
- Extracted reference interpreter compares equal to Rust interpreter on the
  full conformance corpus.

---

## Cross-cutting Concerns (continuous, not milestone-bound)

These run *throughout* the project lifetime, not at any one milestone:

| Concern                          | Cadence                       |
| -------------------------------- | ----------------------------- |
| Conformance scoreboard published | Per commit (CI)               |
| Differential tests across tiers  | Per commit (CI)               |
| Property tests (10k cases)       | Per commit (CI)               |
| Fuzzing                          | Nightly (1 hour each target)  |
| Benchmarks                       | Nightly + per-release         |
| Security advisories review       | Weekly (`cargo deny`)         |
| Upstream HolyJIT contributions   | As needed, ongoing from M6    |
| Documentation review             | Per release                   |
| ADR backfill audit               | Per release                   |

---

## How this roadmap is updated

- **Milestone scope changes** require an ADR.
- **Milestone insertion or deletion** requires an ADR + roadmap PR.
- **Per-milestone work** lives in the spec under `.spec-workflow/specs/<slug>/`,
  with `requirements.md → design.md → tasks.md` per the spec-workflow process.
- **Status flips** (M0 → M1 → … complete) update the table at the top of this
  file at milestone exit-gate time.
