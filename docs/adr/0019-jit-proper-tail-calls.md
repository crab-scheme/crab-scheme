# ADR 0019: JIT Proper Tail Calls — Bounce Trampoline

> Status: Accepted (implemented on `perf/jit-sweep`, commit 6d04a98)
> Date: 2026-05-20
> Depends on: ADR 0012 (uniform-NB JIT tier, IC dispatch)
> Closes: the JIT's R6RS proper-tail-call gap (host-stack overflow
> on cross-function / mutual / non-simple-self tail recursion)

## Context

The Cranelift JIT (`cs-jit-cranelift`) compiles hot Scheme bodies to
native code. R6RS §3.5 mandates *proper tail calls*: a procedure call
in tail position must run in constant control-stack space. The bytecode
VM honours this (its `Inst::TailCall` handler reuses the caller frame —
`cs-vm/src/vm.rs`, the `Inst::Call(n) | Inst::TailCall(n)` arm). The JIT
**did not**.

### What the JIT did before this ADR

The JIT only converted **self-recursion** in tail position to a
constant-stack loop. `detect_tail_call_self` (`lowering.rs`) recognizes
a block whose last instruction is `CallSelf(dst, args)` and whose
terminator returns `dst` (directly, or via a `Jump` to an empty
return-block), and emits Cranelift `return_call(self_fn, args)`. The
inner body uses `CallConv::Tail` precisely so this is legal.

Every **other** tail call — a call to a *different* function, or even a
self-call the translator didn't classify as `CallSelf` — lowered to a
regular `call` (through the inline cache → `vm_ic_dispatch` /
`vm_call_general`) followed by a `return`. Each such call leaves the
caller's native frame live across the callee, so a tail-recursive chain
grows the host stack until it overflows and **aborts the process**
(`fatal runtime error: stack overflow`). A code comment at
`lowering.rs` (the CallGeneral lowering) flagged this:
*"future iters can promote this to `return_call` for tail-recursion
ergonomics."*

### Evidence (measured on `perf/jit-sweep`, 2026-05-20)

| program | VM | JIT | note |
|---|---|---|---|
| `fib(38)` | 8.23 s | 0.12 s | self-recursion: JIT ~68× (already great) |
| self-tail loop, 50M | 2.52 s | 0.09 s | self-tail TCO works |
| `ping`/`pong` mutual, 20M | 1.08 s | **overflow** | cross-function tail call |
| `grid(20000)` O(n) | ok | **overflow** | `col-loop → row-loop` cross-call |
| `grid(150)` w/ let*+helper, O(n²) | ok | **overflow** | self-call demoted to `CallGeneral` |
| `mandelbrot(≥150)` | ok (`n=600`) | **overflow** | both of the above |

RIR dump of the `grid` shape (mandelbrot's nested named-let) shows the
loop compiles to **one** function whose three loop calls are *all*
`CallGeneral` (dynamic), each structurally in tail position:

```
blk1: CallGeneral(d=12,…); Jump(blk6,[12]); blk6: Return   # col→row  TAIL
blk2: CallGeneral(d=40,callee,n=1); Branch …               # helper    non-tail
blk5: CallGeneral(d=54,…); Jump(blk6,[54]); blk6: Return   # col self  TAIL
```

Even the *self*-recursion is a `CallGeneral` (the named-let closure
isn't recognized as "self"), which is why `detect_tail_call_self` never
fires for it and the body goes O(n²).

### Why `return_call` alone is not enough

`CallGeneral`/`Call` dispatch dynamically through the inline cache:
hit → `vm_ic_dispatch(...)`, miss → `vm_call_general(...)`. Both are
Rust `extern "C"` helpers that *call the callee and return its result*.
Rust has no guaranteed tail call, so the helper frame cannot be
eliminated — `return_call` into the helper just relocates the frame, and
each bounce still adds a helper frame. Constant stack therefore requires
a **trampoline**: the tail call must *return* to a loop that
re-dispatches, rather than recurse.

## Decision

Implement a **bounce trampoline**, modelled on the existing JIT deopt
sentinel (`JIT_DEOPT_REQUESTED` / `jit_request_deopt` / `jit_take_deopt`
in `cs-vm/src/vm.rs`).

### Mechanism

1. **Thread-local bounce slot** (`cs-vm/src/vm.rs`):
   `JIT_PENDING_TAILCALL: RefCell<Option<(i64 callee, SmallVec<[i64; 6]> args)>>`.
   The callee and args are *owned* NB handles.

2. **Runtime helper** `vm_jit_set_tailcall(callee: i64, args_ptr: *const i64,
   n: i64) -> i64`: moves `(callee, args[0..n])` (already owned by the
   emitting body) into the slot and returns a placeholder `0`. Registered
   as a JIT symbol next to `vm_call_general`.

3. **JIT codegen** (`lowering.rs`, uniform-NB block driver near the
   `detect_tail_call_self` site, and the specialized tier's equivalent):
   a new `detect_tail_call_general` recognizes a block whose last inst is
   `Call`/`CallGeneral(dst, callee, args)` with the same Return /
   Jump-to-empty-return-block terminator pattern. When matched, lower the
   block's other insts, then emit
   `vm_jit_set_tailcall(callee, args_buf, n)` and `return placeholder`
   instead of the IC hit/miss dispatch. (Ownership of callee+args
   transfers to the slot exactly as it would have transferred to
   `vm_ic_dispatch`/`vm_call_general`.)

4. **Trampoline loop** — a single runner that every JIT-body invocation
   funnels through. After a native body returns, check the bounce slot:
   * empty → decode and return the body's result;
   * `Some((callee, args))` → if the callee is JIT-dispatchable
     (`jit_ptr` non-null, arity ≤ 6, type-guard passes) re-run its body
     in the **loop** (constant stack); otherwise hand off **once** to
     `vm_call_sync(callee, args)` (the bytecode VM, which has its own
     proper TCO) and return its result.

   Invocation sites that must route through the trampoline:
   `try_dispatch_jit_nb` (VM `Call`/`TailCall` opcode + `try_dispatch_jit`
   + `vm_call_sync`'s JIT path all reach it), and `vm_ic_dispatch`'s
   fast path (it runs a cached body directly). `vm_call_general` falls
   back to `vm_call_sync` → `try_dispatch_jit_nb`, so it inherits the
   trampoline.

### Correctness invariants

* **All-or-nothing per invoker.** A bounce makes the body *return* to its
  invoker; if any invoker of a JIT body fails to trampoline, it would
  hand a placeholder up as the real result. Hence the codegen change and
  *all* invoker trampolines land together.
* **Refcounting.** Bounce args are owned by the body, moved to the slot,
  then consumed by the next dispatch — symmetric with the existing
  `vm_ic_dispatch`/`vm_call_general` arg ownership. On deopt of a bounce
  target, the owned args route to `vm_call_sync` (which consumes them).
* **Deopt interaction.** A deopt during a bounce iteration can't fall
  back to "bytecode of the original closure"; it falls back to
  `vm_call_sync` of the *bounce target*.

## Alternatives considered

* **`return_call` into a Rust trampoline helper** — rejected: the helper
  still makes a non-tail call into the callee body (Rust lacks guaranteed
  tail calls), so frames accumulate.
* **`return_call` to a statically-resolved sibling inner FuncRef** —
  only applies to direct `Call` to a known, already-compiled function;
  the failing cases are all `CallGeneral` (dynamic), and the callee may
  be VM-only or not-yet-compiled. Doesn't cover the class.
* **Narrow fix (extend `detect_tail_call_self` to the let*+helper
  shape)** — would drop mandelbrot from O(n²) to O(n) (works at realistic
  N) but leaves true mutual recursion (`ping`/`pong`, state machines)
  broken. Rejected in favour of the full fix per the sweep decision.

## Consequences

* JIT honours R6RS proper tail calls; mutual recursion, state machines,
  CPS, and deep named-let loops run in constant stack on every tier.
* Tail-call-heavy JIT code gets *faster* (no per-iteration frame
  setup/teardown; the trampoline loop is cheaper than IC re-entry).
* Small added cost: one thread-local check after each JIT body return
  (mirrors the existing deopt check) and, on a bounce, a slot move.
* `fib`/`tak`/`ack` self-recursion path is unchanged (still
  `return_call(self_fn)`), preserving the ~65× speedups.

## Verification plan

* Probes: `ping`/`pong` (20M), `grid(100000)`, `mandelbrot(600)` run in
  constant stack on `--tier vm-jit` and match VM results.
* No regression: `fib`/`tak`/`ack`/`spectral-norm`/`binary-trees` JIT
  timings within noise; full conformance suite stays 100%.
* New JIT unit tests in `cs-jit-cranelift/tests/jit_from_bytecode.rs`
  for tail-`CallGeneral` constant-stack behaviour.
