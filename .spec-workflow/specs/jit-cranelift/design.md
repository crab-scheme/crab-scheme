# M6 JIT (Cranelift) — Design

> Status: **Draft** — sketch only, fills out as we land scaffolding.
> Companion: `requirements.md`.

## Overview

Add a JIT compiler that takes hot procedures from the bytecode VM
(`cs-vm`) and produces native code via Cranelift. The JIT is per-
function, threshold-triggered, and supports deopt back to the VM
when type assumptions break.

## Components

### `cs-rir` crate (new)

Backend-agnostic Rust-flavored intermediate representation. SSA-shaped
basic blocks with terminator-style control flow. Every operation
maps to a documented `cs-vm` bytecode instruction so the differential
test in FR-5 reduces to opcode-level equivalence.

```rust
pub struct Function {
    pub name: String,
    pub blocks: Vec<Block>,
    pub params: Vec<Type>,
    pub entry: BlockId,
}

pub struct Block {
    pub insts: Vec<Inst>,
    pub terminator: Term,
}

pub enum Inst {
    LoadConst(Value, Const),
    Add(Value, Value, Value),
    Sub(Value, Value, Value),
    // ...
    Call(Value, Callee, Vec<Value>),
    DeoptCheck(Value, TypeGuard),
    // ...
}
```

`DeoptCheck` is the special instruction the VM inserts; if its guard
fails at runtime, control transfers back to the VM with the recorded
type feedback.

### `cs-jit` crate (new)

```rust
pub trait JitBackend: Send {
    fn name(&self) -> &str;
    fn compile(&mut self, rir: &cs_rir::Function) -> Result<JitFn, JitError>;
    fn dump_native(&self, jf: &JitFn) -> Vec<u8>;
}

pub struct JitFn {
    pub ptr: NonNull<u8>,
    pub backend: &'static str,
    pub feedback: TypeFeedback,
}

pub struct Tier {
    counter: AtomicU32,
    state: AtomicCell<TierState>,
}
```

`Tier` lives on each procedure value. The VM increments `counter` on
every entry; when it crosses the threshold (default 1024), the
runtime asks `cs-jit` to compile the procedure. The result is
swapped into the procedure's dispatch entry atomically.

### `cs-jit-cranelift` crate (new)

`cs-rir → clif → native code`. Uses `cranelift_jit::JITBuilder` to
allocate executable memory; uses `cranelift_codegen` to lower clif
to native. Each `cs-rir::Inst` has a 1:1 lowering to clif (or a
small sequence). `DeoptCheck` lowers to a guard + side-exit jump
to a deopt trampoline.

## Tier-up state machine

```
       ┌────────────────────────────────────────────────────────┐
       │                                                        │
       ▼                                                        │
   ┌────────┐  count >= threshold     ┌──────────┐              │
   │  Cold  │ ───────────────────────▶│ Compiling│              │
   │ (VM)   │◀──── deopt ─────────────│   (JIT)  │──────────────┘
   └────────┘                         └────┬─────┘
                                           │
                                           ▼
                                       ┌────────┐
                                       │  Hot   │
                                       │ (JIT)  │
                                       └────────┘
```

- **Cold**: bytecode VM dispatches; counter increments per call.
- **Compiling**: a background or synchronous compile produces a
  `JitFn`. While compiling, the VM continues to dispatch (no
  blocking).
- **Hot**: native code dispatches via the procedure's function
  pointer. Counter no longer increments.
- **Deopt**: a `DeoptCheck` failure pushes the procedure back to
  Cold and queues recompilation with broader type feedback.

Hysteresis prevents oscillation: once a procedure has tiered up and
deopted N times (default 3), it stays on the VM permanently.

## OSR (on-stack replacement)

Phase 1 of M6 is **call-boundary tier-up only** — OSR ships in the
M6 follow-up perf iteration, not as a gate.

When OSR lands, the trigger is: a long-running loop in a procedure
that hasn't returned yet. The loop body counter (separate from
the call counter) crosses a threshold, and the JIT emits a stub
that picks up at the loop header in JITted code.

## Deopt mechanism

Coarse-grained deopt for M6: a `DeoptCheck` failure jumps to a
trampoline that:
1. Reconstructs the VM state at the procedure entry (the JIT
   recorded enough info to do this).
2. Calls back into the bytecode VM with that state.
3. Records the type that caused deopt in the procedure's
   `TypeFeedback` for the next compile.

Fine-grained deopt (resume at the `DeoptCheck` site mid-procedure)
is an M6 follow-up. M6 ship-gate ditches partial work and restarts.

## Roots and GC interaction

The JIT calls into the runtime through stable C-ABI shim functions
(`scheme_alloc_pair`, `scheme_call_cc`, etc.) that already root
correctly. Phase 1 of M5 doesn't yet have stack maps, so JITted
frames participate via the conservative spill-slot scan that
`cs-gc` performs over the VM stack — but JITted frames don't push
on the VM stack. To bridge:

- JITted frames push a **shadow VM frame** at entry that holds
  the live values. The shadow frame is what the GC sees. The
  native code uses register allocation freely; the shadow gets
  written on each safepoint (every back-edge + every call).
- After M6 lands, an M5 follow-up adds proper stack maps, removes
  the shadow frame, and emits stackmaps for the GC.

This is "GC works correctly but JIT is slower than it could be."
The follow-up moves to "GC works correctly and JIT runs at full
speed."

## Lowering schedule

Each `cs-rir::Inst` lowers to clif via a `lower_inst` match. The
constants and arithmetic ops are simplest; calls and deopts are
complex.

Plan order:
1. **Iter 1** (this iter): scaffold the three crates, register a
   no-op backend, write the spec, write ADR 0007.
2. **Iter 2**: lower constant + arithmetic instructions, add the
   first JIT-able microbenchmark (`fib`), wire into `cs-runtime`
   behind a feature flag.
3. **Iter 3**: tier-up state machine + counter integration in
   `cs-vm`. Deopt trampoline scaffolding.
4. **Iter 4–N**: lower each remaining instruction class
   (closures, env access, set!, control flow, dynamic-wind /
   raise / values).
5. **Iter N+1**: differential test wired up; passes 10k+
   expressions identical to walker/VM.
6. **Iter N+2**: `(fib 30)` performance iter — tune the lowering
   until we hit the 1.2× C-O2 gate or document why we can't.
7. **Iter N+3**: Gabriel benchmarks land; `bench/gabriel/results.md`
   documents the geomean.
8. **Iter N+4**: `(jit-dump <proc>)` REPL primitive.
9. **Iter N+5**: M6 exit report, tag `m6-complete`.

## Open questions

1. **Cranelift release pinning** — `cranelift-codegen` has had API
   churn; we lock to a specific point release. The first scaffolding
   iter picks the version.
2. **Cross-platform deopt trampoline** — emits different prologue
   on x86_64 vs aarch64. We defer to the second-iter tier-up work.
3. **Closure capture in JITted code** — the JIT needs to read
   closure environments. Either via shadow frames or via a
   dedicated env-pointer register. Pick lands in iter 4.
4. **`call/cc` interaction** — JITted frames need to be capturable.
   M8 work; the JIT just tags its frames as "may be captured" and
   the runtime handles the rest.

## File-level diff scope (estimate)

| Crate | LOC change |
|---|---|
| `cs-rir` (new) | ~600 |
| `cs-jit` (new) | ~400 |
| `cs-jit-cranelift` (new) | ~1500 |
| `cs-runtime/src/lib.rs` | ~150 (tier dispatch) |
| `cs-vm/src/vm.rs` | ~200 (counter increment + tier-up call site) |
| `cs-cli/src/main.rs` | ~30 (`--jit=cranelift` flag) |
| Tests | ~400 (differential harness + bench scaffolding) |

---

## Tasks

`tasks.md` follows once the scaffolding iter is in flight; it'll
mirror the foundation/M5 specs' per-task format with file paths,
leverage tags, prompt scaffolds, and exit criteria per item.
