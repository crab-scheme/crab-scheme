# ADR 0007 — JIT Architecture (Cranelift first)

> Status: **Accepted** for M6.
> Companion: `.spec-workflow/specs/jit-cranelift/{requirements,design}.md`.
> Predecessor: ADR 0006 (GC).

## Context

M6 adds a JIT compiler tier above `cs-vm`'s bytecode interpreter.
The roadmap commits to a two-backend story (Cranelift in M6,
HolyJIT in M7) so we need an abstraction that admits both. We
also need to decide on tier-up policy, deopt mechanism, and the
shape of the per-procedure state machine.

## Decisions

### D-1. Cranelift first, HolyJIT second

We ship Cranelift as the first JIT backend.

**Rationale:**
- Stable, mature, used in production by Wasmtime.
- API documented and versioned.
- `cranelift-codegen` and `cranelift-jit` cover lowering and
  memory management out of the box.
- Differential testing against a stable backend lets us harden the
  shared `cs-rir` IR before exposing it to a second consumer
  (HolyJIT in M7).

**Alternatives considered:**

- **HolyJIT first**: closer to the headline "Rust meta-JIT" story but
  HolyJIT is a research project with intermittent maintenance and
  Rust-version sensitivity. Front-loading it risks blocking on
  upstream issues. We'd rather discover those with the safety net
  of a working Cranelift backend already in place.
- **QBE / LLVM**: heavier dependencies, slower compile times, less
  Rust-native API. Out.
- **Hand-rolled native lowering**: too much surface area for one
  milestone. Out.

### D-2. Per-function JIT, not whole-program

Each procedure is JITted independently when it crosses the call
threshold. No whole-program analysis at M6.

**Rationale:**
- Matches the bytecode VM's per-procedure dispatch shape.
- Simplifies tier-up: swap the function pointer in the procedure
  value, leave everything else alone.
- Inlining lands as a follow-up in the M6 perf track. Cranelift
  doesn't do cross-function inlining anyway in current versions;
  whole-program would need a different backend.

### D-3. Threshold-triggered tier-up at call boundaries

Cold procedures stay on the VM. A counter on each procedure
increments at every call. Crossing the threshold (default 1024)
triggers JIT compilation. The next call after compile completes
hits native code.

**Rationale:**
- Simple, well-understood mechanism. Ahead-of-time JITting on first
  call would penalize one-shot scripts; lazy at-call JITting on the
  first call would burn time on never-hot code.
- 1024 is the V8 / SpiderMonkey ballpark for warmup. Tunable via
  config; expect to tune downward as the JIT matures.

**Alternatives considered:**
- **Hot-loop OSR only**: ships in a follow-up perf iter. M6
  scaffolding focuses on the call-boundary path.
- **Always-JIT**: too aggressive; hurts startup. Could be a flag
  for benchmark mode.
- **Profile-guided AOT**: out of M6 scope (M10).

### D-4. Coarse-grained deopt: abandon JITted frame, restart on VM

When a `DeoptCheck` fails (e.g. the JITted version was
specialized to fixnums and a flonum arrives), the JIT trampoline
reconstructs the VM state at procedure entry, transfers control
back to the VM, and queues recompilation with broader type
feedback. The partially-executed JIT work is discarded.

**Rationale:**
- The fine-grained version (resume at the deopt site mid-procedure)
  requires precise reconstruction of the deopt site's VM state from
  the JIT register state. That's a major engineering effort and
  pushes M6 by months.
- The coarse version is correct, simple, and slow only in the
  pathological case of repeated deopts. Hysteresis (D-5) catches
  the pathology.

### D-5. Tier-up hysteresis

After 3 deopt events the procedure stays on the VM permanently
for that runtime instance.

**Rationale:**
- Prevents oscillation around the threshold.
- Keeps the type-stable common case on the JIT, kicks the
  unstable case off.
- 3 is arbitrary; tunable.

### D-6. Shadow VM frame for GC roots (M6 only)

Phase 1: JITted code writes a shadow VM frame at every safepoint
(call sites + back-edges) so the existing GC stack scan keeps
working. Phase 2: stack maps replace the shadow frame.

**Rationale:**
- M5's GC scans VM frames. JITted code doesn't push on the VM
  stack; without a shim the GC misses the live JIT roots.
- Shadow frames are correct but slow. Stack maps are correct and
  fast. M6 ships correct; the perf track ships fast.

### D-7. Backend trait `JitBackend`

```rust
trait JitBackend: Send {
    fn name(&self) -> &str;
    fn compile(&mut self, rir: &cs_rir::Function) -> Result<JitFn, JitError>;
    fn dump_native(&self, jf: &JitFn) -> Vec<u8>;
}
```

**Rationale:**
- Minimal surface; HolyJIT in M7 implements the same trait.
- `dump_native` enables the `(jit-dump <proc>)` REPL primitive
  uniformly across backends.
- `Send` enables future async compilation if we want it (out of
  M6 scope).

### D-8. `cs-rir` is the IR; `clif` is an internal lowering target

`cs-rir` is the public IR consumed by all backends. Cranelift's
`clif` is the lowering target inside `cs-jit-cranelift`; it's not
exposed to other crates. HolyJIT in M7 lowers `cs-rir` directly to
its own internal form.

**Rationale:**
- Decouples the IR (which we control) from the backend (which we
  don't).
- Lets us evolve the IR without breaking backends, and vice versa.
- Simplifies the differential test: every `cs-rir::Inst` has a
  documented `cs-vm` opcode equivalent; the test asserts the
  per-instruction equivalence holds across backends.

### D-9. Cranelift version pinning

We pin to a specific `cranelift-codegen` and `cranelift-jit`
release in `Cargo.toml`. Bumps are deliberate and gated by a
regression-bench check before they merge.

**Rationale:**
- Cranelift has had ABI churn between minor releases.
- Predictable JIT behavior across team machines.

## Consequences

- **Pro:** One coherent JIT story; HolyJIT lands as a peer in M7
  without architectural rework.
- **Pro:** Differential test gives high confidence across tiers.
- **Pro:** Coarse deopt + hysteresis keeps the design tractable.
- **Con:** Shadow VM frame is a known perf overhead. Documented;
  fixed in the M6 perf-track follow-up.
- **Con:** No OSR in M6 ship version. Long-running loops in cold
  procedures don't tier up until they return.

## Out of scope (deferred follow-ups)

- OSR (on-stack replacement)
- Stack maps (replaces shadow VM frame)
- Fine-grained deopt (mid-procedure resume)
- Profile-guided recompilation
- Inlining (cross-procedure)
- Async / parallel compilation
- HolyJIT backend (M7)
