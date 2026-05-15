# M6 JIT (Cranelift) — Requirements

> Status: **Phase 1+2+3+4 shipped** — see exit reports:
>   - Phase 1: `docs/milestones/m6-exit.md` (tag `m6-complete`) — Cranelift JIT shipped with i64-only ABI
>   - Phase 2: `docs/milestones/m6-phase2-exit.md` (tag `m6-phase2-complete`) — four-tag immediate-value pipeline (Fixnum/Boolean/Character/Flonum)
>   - Phase 3: `docs/milestones/m6-phase3-exit.md` (tag `m6-phase3-complete`) — tail calls + mixed-tower arithmetic correctness
>   - Phase 4: `docs/milestones/m6-phase4-exit.md` (tag pending) — uniform NanboxValue across tiers + baseline NB JIT tier
>
> The Phase 1 requirements / design docs below are retained as historical scaffolding; subsequent phases were tracked through their exit reports. Gabriel-benchmark geomean and gcc-O2 perf gates remain ungated.
> Spec slug: `jit-cranelift`
> Roadmap slot: M6
> Predecessor: M5 (`docs/milestones/m5-exit.md`, conformance 2150)

This spec adds a **first** JIT backend so we have a known-working
tier and a baseline to differentially test against the HolyJIT
backend in M7. The choice of Cranelift is pragmatic: it's stable,
it's the JIT shipping in Wasmtime, and its IR (`clif`) plus
`cranelift-jit` crate already cover code-gen + memory management.

The roadmap exit gate per `ROADMAP.md`:

- Differential tests pass: tree-walker == VM == JIT on ≥ 10,000 expressions.
- JIT `(fib 30)` within 1.2× of `gcc -O2` C equivalent.
- Gabriel benchmarks geomean ≥ 5× over interpreter.
- Conformance pass rate unchanged.
- `(jit-dump <proc>)` REPL primitive emits Rust IR + clif IR + native
  disassembly.

---

## Functional requirements

### FR-1. New crates: `cs-rir`, `cs-jit`, `cs-jit-cranelift`

Three new workspace crates:
- **`cs-rir`** — backend-agnostic Rust-flavored IR consumed by all
  JIT backends. SSA-shaped values, basic blocks, terminator-style
  control flow. Lowered from `cs-ir` (the existing `CoreExpr` /
  bytecode source).
- **`cs-jit`** — abstract `JitBackend` trait, dispatch glue, deopt
  handling, recompilation on type-feedback changes. Holds the
  per-procedure tier-up state machine (cold → warm → hot).
- **`cs-jit-cranelift`** — implements `JitBackend` for Cranelift.
  `cs-rir → clif → native code` lowering, register allocation
  hints, function-pointer emission.

Acceptance: each crate builds with `cargo build -p <crate>` and has at
least one unit test demonstrating the public API.

### FR-2. `JitBackend` trait

A minimal but extensible trait:

```rust
pub trait JitBackend {
    fn name(&self) -> &str;
    fn compile(&mut self, rir: &cs_rir::Function) -> Result<JitFn, JitError>;
    fn dump_native(&self, jf: &JitFn) -> Vec<u8>;
}
```

Plus a uniform `JitFn` value type that holds a callable function
pointer, the originating Cranelift compilation cookie (or holyjit
handle), and the type-feedback metadata that triggered recompilation.

### FR-3. Tier transition (cold → VM → JIT)

Cold procedures run on the bytecode VM. A counter increments on each
call. When the counter crosses a configurable threshold (default
1024), the next call submits the procedure to the JIT backend and
swaps the procedure dispatch entry to the JITted code. Any in-
flight calls finish on the VM; subsequent calls hit native.

Hot loops trigger on-stack-replacement (OSR): if a procedure has
been on the VM stack long enough that its loop hits the threshold
mid-call, the JIT compiles a stub that picks up at the loop header
in the JITted code and the VM hands off.

Acceptance: a microbenchmark that calls a hot procedure N times
shows the post-tier-up portion running at the JIT speed (≥ 3× VM)
within the first 100 calls after the tier-up.

### FR-4. Deopt path

When a type-specialized JIT path receives unexpected types (e.g.
the JITted version of `+` was specialized to fixnum and a flonum
shows up at runtime), execution deopts to the bytecode VM and
queues a recompilation request with broader type feedback.

Acceptance: a test that (a) trains a JITted procedure on fixnums,
(b) calls it with a flonum, observes (c) the result is correct and
(d) the procedure has been recompiled with broader specialization.

### FR-5. Differential testing

Every test in the existing conformance corpus must pass on all three
tiers (walker, VM, JIT). A new harness `cs-runtime/tests/
jit_conformance.rs` runs each `.scm` file through the JIT-enabled
runtime and asserts the same pass count as the VM tier.

Acceptance: at least 10,000 expressions evaluated identically
across all three tiers.

### FR-6. Performance gate

Two perf measurements:
- `(fib 30)` running on the JIT within 1.2× of an equivalent C
  function compiled with `gcc -O2`. We pick `fib` because it's a
  pure-function, recursion-heavy workload that exercises call
  dispatch + integer arithmetic — the easy parts of any JIT.
- Gabriel benchmarks (Lisp's classic perf-suite: tak, takr, takl,
  destructive, browse, traverse, deriv, frpoly, puzzle, triangle,
  fft, ctak, dderiv) running ≥ 5× faster on JIT than interpreter
  geomean. Add a `bench/gabriel/` subdirectory.

Acceptance: scripted perf run produces a markdown table in
`bench/gabriel/results.md` showing the comparison.

### FR-7. `(jit-dump <proc>)` REPL primitive

A new builtin that takes a procedure and emits a multi-section dump:

```
=== jit-dump: my-proc ===
--- cs-rir ---
<text-encoded RIR>
--- cliff ---
<cranelift IR>
--- native (x86_64 / aarch64) ---
<disassembly>
```

Acceptance: invoking `(jit-dump my-proc)` in the REPL after the
procedure has been JITted prints all three sections; before tier-up
prints only the `cs-rir` and a "(not yet JITted)" note.

---

## Non-functional requirements

### NFR-1. The walker tier and VM tier remain correct without the JIT

The JIT is opt-in. Removing the `cs-jit` crate from the runtime's
dependencies must yield a build that matches today's behavior
exactly. This is enforced by a `feature = "jit"` flag on
`cs-runtime`.

### NFR-2. No `unsafe` outside the JIT crates

`cs-jit-cranelift` necessarily uses `unsafe` to invoke JITted code
through function pointers. Other crates remain `unsafe`-free.

### NFR-3. Deterministic JIT bytecode-equivalence audit

Every `cs-rir` instruction has a documented bytecode-equivalent
behavior; the differential test in FR-5 is the verifier. Comments
above each opcode constructor in `cs-rir/src/lib.rs` cite the
matching `cs-vm` instruction and any deopt conditions.

### NFR-4. Documentation

A new ADR (`docs/adr/0007-jit-design.md`) ratifies:
- Cranelift first vs HolyJIT first
- Per-function JIT vs whole-program JIT
- Tier-up policy (cold count threshold, OSR triggers)
- Deopt mechanism (side exits to VM with type feedback recorded)
- Why we picked Cranelift over QBE / LLVM

---

## Out of scope (deferred to later milestones)

| Item | Where it lives |
|---|---|
| HolyJIT backend | M7 |
| Whole-program AOT | M10 |
| Profile-guided recompilation | post-M6 perf track |
| First-class continuations interaction with JITted frames | M8 |
| WASM backend | M10 (`wasm` track, parallel to AOT) |

---

## Risks

1. **JIT correctness regressions.** A miscompiled instruction silently
   produces wrong answers.
   *Mitigation:* the differential test from FR-5 runs on every PR
   touching JIT crates.

2. **Tier-up jitter.** A workload that hovers around the threshold
   may oscillate between VM and JIT.
   *Mitigation:* hysteresis on the counter; once a procedure tiers
   up it stays JITted unless deopted.

3. **Cranelift API churn.** Cranelift versions evolve; pinning
   matters.
   *Mitigation:* lock to a specific Cranelift release in Cargo.toml;
   bump deliberately with a regression-bench check.

4. **Deopt complexity.** Side-exits with reconstructable VM state
   are notoriously hard to get right.
   *Mitigation:* in M6 we restrict deopt to coarse-grained "abandon
   the entire JITted invocation, restart on VM" rather than fine-
   grained inline-cache-style transitions. The fine-grained
   transitions are an M6 follow-up.

5. **Gabriel ≥ 5× geomean.** Cranelift without inlining or speculative
   optimization may not hit 5× — that's a known weakness of Cranelift's
   `Aegraph` opt level.
   *Mitigation:* if we can't hit 5×, the gate is renegotiated rather
   than the milestone delayed; the actual perf number lands in the
   M6 exit report whether or not it cleared the spec gate.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `cs-rir`, `cs-jit`, `cs-jit-cranelift` crates exist | `Cargo.toml` workspace members |
| `JitBackend` trait + Cranelift impl | crate present |
| Differential tests green on walker == VM == JIT | conformance harness extended |
| `(fib 30)` within 1.2× of `gcc -O2` C | bench script |
| Gabriel benchmarks ≥ 5× geomean | `bench/gabriel/results.md` |
| `(jit-dump <proc>)` REPL primitive | builtin registered |
| ADR 0007 written | `docs/adr/0007-jit-design.md` |
