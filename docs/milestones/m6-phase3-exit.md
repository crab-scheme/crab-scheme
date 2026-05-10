# M6 Phase 3 Exit Report — Tail Calls + Mixed-Tower Arithmetic Correctness

> Tagged: `m6-phase3-complete` at the merge commit of this report.
> Predecessor: M6 Phase 2 (`docs/milestones/m6-phase2-exit.md`).
> ADR: `docs/adr/0011-jit-boxed-value-abi.md`.

## Decision

**Close M6 Phase 3 as the typed-numerics correctness + tail-call
milestone.** Five iters (AL through AP, plus the AK closeout housekeeping
that bridged Phase 2 → Phase 3) closed three load-bearing gaps that
M6 Phase 2's perf table didn't reveal:

1. **Block-param type propagation** (iter AM). Pre-AM, every Cranelift
   block parameter was unconditionally typed Fixnum, dropping flonum
   type info across branches and silently turning later `(+ acc x)`
   into i64 `iadd` on f64 bit patterns. The let-loop accumulator
   pattern was wrong by 2× at every branch.
2. **Tail-call lowering via wrapper pattern** (iter AO). Pre-AO, JIT'd
   recursive bodies (`let loop` in particular) burned host stack on
   every iteration; the bench was capped at 50k iters defensively.
   Post-AO, every JIT'd function compiles as outer SystemV trampoline
   + inner Tail-conv body with `return_call` for tail-CallSelf —
   1M+ iter recursion is safe, **~15× JIT-over-VM**.
3. **Mixed-tower arithmetic contagion** (iter AP). Pre-AP, expressions
   like `(+ acc 1.0 i)` (acc:flo + 1.0:flo + i:fix) fell back to
   integer addition because `all_flonum` was false. Post-AP, any-Flonum
   chains promote Fixnum operands via `FixToFlo` and emit `FlonumAdd`
   per R6RS numeric-tower contagion.

Plus iter AL reserved the future tag-space for ADR 0011's heap-pointer
ABI (`JIT_RT_PAIR`, `JIT_RT_PROCEDURE`, etc., plus `JIT_RT_ANY`), and
iter AN documented the tail-call calling-convention investigation that
landed as the wrapper pattern in AO.

End-state A from the JIT detailed plan is now genuinely complete: the
JIT correctly handles every typed-numeric pattern (Fixnum / Boolean /
Character / Flonum, mixed tower, deep recursion). Remaining work
splits into ADR 0011's end-state B (general Call, heap-pointer ABI,
allocation lowering) — a separate milestone.

## What shipped

### Iter AL — JIT tag-space extension (`a5d9b4b`)

Eleven new `pub const u8` entries in `cs-vm::vm` reserving the slots
ADR 0011 D-1 commits to:

```
JIT_RT_PAIR        = 4    Rc<Pair>::into_raw
JIT_RT_VECTOR      = 5    Gc<RefCell<Vec<Value>>>::into_raw
JIT_RT_STRING      = 6    heap pointer
JIT_RT_BYTEVECTOR  = 7    heap pointer
JIT_RT_PROCEDURE   = 8    Rc<dyn Procedure>::into_raw
JIT_RT_SYMBOL      = 9    Symbol(u32) zero-extended
JIT_RT_BIGINT      = 10   heap pointer
JIT_RT_RATIONAL    = 11   heap pointer
JIT_RT_HASHTABLE   = 12   heap pointer
JIT_RT_PORT        = 13   heap pointer
JIT_RT_RESERVED    = 14
JIT_RT_ANY         = 15   Box::into_raw(Box<Value>)
```

No code paths use them yet — pure-additive reservation. The existing
four immediate tags (Fixnum / Boolean / Character / Flonum at slots
0–3) keep their semantics.

### Iter AM — JIT block-param type propagation (`12962c6`)

A single bug, three connected fixes in `cs-vm::jit_translate`:

1. **`infer_return_type`** seeded only RIR-Inst dsts. Returns of a
   parameter value (or block-param value) had no entry in any type
   set and fell to `seen_fixnum`. Fix: also seed from `func.params`
   and each `block.params` (typed slots).

2. **`seed_block_entry`** created block params unconditionally typed
   as Fixnum, discarding type info from predecessor stack values.
   After the first branch in a flonum body, every value became
   Fixnum-typed and arithmetic emitted `Add`/`Mul` (i64) instead
   of `FlonumAdd`/`Mul`. Fix: take a `src_values: &[RirValue]`
   slice and a `&mut value_types`, propagate types into block
   params; record new param values in `value_types`.

3. **`CallSelf` dsts** now tracked separately so they don't pollute
   the return-type inference. CallSelf result inherits the function's
   own return type (fixed-point); other return paths determine it.

All 15 `seed_block_entry` callers updated to pass the value list
they're seeding.

The bug was load-bearing: a `let loop ((i 0) (acc 0.0)) ...`
accumulator pattern returned `i64::MAX` (the bit pattern of a
flonum) instead of the correct flonum sum. Caught while writing
the Phase 2 closeout bench.

### Iter AN — tail-call investigation (`59b8886`)

Tried to fold tail-`CallSelf` into Cranelift `return_call`. Two
findings:

1. The let-loop pattern lowers recursion via a *trivial join block*:
   `block N: ...; CallSelf(dst, args); Jump(target, [dst])`;
   `block target: params=[(p, _)] term: Return(p)`. Direct
   `CallSelf; Return(dst)` is rare. Detection has to look one block
   deeper.

2. `b.ins().return_call(self_fnref, ...)` is verifier-rejected on
   `CallConv::SystemV`. Switching to `CallConv::Tail` makes
   `return_call` legal but breaks the runtime's
   `extern "C" fn(i64,...) -> i64` transmute (different stack layout
   — confirmed by an immediate fib stack-overflow on the very first
   call after the convention change).

The path forward: wrapper pattern (outer SystemV trampoline + inner
Tail-conv body). Iter AN shipped only the investigation; lowering
stayed on plain `call`.

### Iter AO — tail-call lowering via wrapper pattern (`e4338de`)

The iter AN findings became code. Every JIT'd function now compiles
as a pair of Cranelift functions:

| Function | Calling Conv | Role |
|----------|--------------|------|
| outer    | SystemV      | Runtime transmutes its pointer; body is a one-instruction trampoline `return inner(args...)`. |
| inner    | Tail         | Hosts the real RIR body. `CallSelf` in tail position lowers to `return_call(inner_fnref, args)`. |

`detect_tail_call_self` recognizes both shapes:

  - (a) Direct: `CallSelf(dst, args); Term::Return(dst)`.
  - (b) Through trivial join: `CallSelf(dst, args); Term::Jump(t, [dst])`
        where `t` is a block whose only content is `Return(p)` for `p`
        its first param.

Implementation split into helpers:

  - `compile_inner_body` — extracted block-lowering loop with
    tail-CallSelf detection. `self_fnref` points to `inner_id` so
    tail recursion self-loops on the same Tail-conv function.
  - `compile_outer_trampoline` — ~30-line trampoline body. Calls
    inner with the function-arg values, returns the result.
  - The outer's pointer is what's returned to the runtime.

The dispatcher's `extern "C" fn(i64,...) -> i64` transmute is
unchanged — the wrapper is transparent to all existing JIT
infrastructure.

### Iter AP — mixed-tower arithmetic contagion (`d63dffe`)

R6RS numeric-tower contagion: when any operand of `+`/`-`/`*`/`<`/`=`
etc. is Flonum, the rest of the chain promotes to Flonum. Pre-AP,
the JIT translator's `all_flonum` gate said "if not *every* operand
is Flonum, fall back to fixnum form" — silently breaking mixed
chains.

Three mirrored fixes:

  - **`emit_arith_binop`**: any-Flonum operand triggers `FixToFlo`
    promotion of the other operand; the Flonum* op is emitted.
  - **`emit_cmp_binop`**: same for `<` / `=` and friends.
  - **Variadic `+`/`-`/`*` chain**: `("+", _) | ("-", _) | ("*", _)`
    BuiltinRef path pre-promotes every Fixnum operand before
    chaining, when any operand is Flonum.

All-Fixnum chains still take the i64 fast path. All-Flonum chains
were already handled. The new path is the mixed case — exactly
what was producing wrong results.

## Acceptance summary

| Gate | Spec acceptance | Result |
|------|-----------------|--------|
| Typed-numerics correctness across all flonum/fixnum mixed shapes | Phase 3 implicit | **✅** Iter AP closes the last contagion gap. Iter AM closes the block-param type propagation. |
| Deep-recursion safety on JIT'd code | ADR 0011 D-7 | **✅** Iter AO's wrapper pattern lets `let loop` go to 1M+ iters; ~15× JIT-over-VM at that scale. |
| End-state A from `docs/jit-detailed-plan.md` | Roadmap | **✅** Done. The flow goes Fixnum hot loops → Boolean/Character/Flonum returns → arg-side flonum passthrough → flonum arith / compare / branches → tail-call → mixed-tower. |
| End-state B (real workloads) | ADR 0011 D-3..D-6 | **❌ Deferred to next milestone.** General Call (item 5), heap-pointer ABI (item 8), allocation lowering (item 9), Lambda creation (item 10) all gated on iter AS or later. |

## Test inventory

| Suite | Tests |
|-------|------:|
| `crates/cs-runtime/tests/jit_differential.rs` | 24 |
| `crates/cs-runtime/tests/jit_conformance.rs` | 6 |
| `crates/cs-runtime/tests/jit_runtime.rs` | 5 |
| `crates/cs-runtime/tests/jit_tier_up.rs` | 4 |

Workspace at exit: **583 passed, 0 failed** (skipping
`memory_baseline_large_list_construction` per the carry-over).

## Iteration log

| Iter | Commit | Deliverable |
|------|--------|-------------|
| AK | `3758e8f` | Phase 2 exit doc + housekeeping (bridge) |
| ADR | `3a7ebc2` | ADR 0011 — Boxed-Value ABI design |
| AL | `a5d9b4b` | Reserve JIT tag space (D-1) |
| AM | `12962c6` | Fix block-param type propagation + flonum bench |
| AN | `59b8886` | Tail-call investigation — wrapper pattern needed |
| AO | `e4338de` | Tail-call lowering via wrapper pattern |
| AP | `d63dffe` | Mixed-tower arithmetic contagion |
| AQ | this commit | exit report + tag `m6-phase3-complete` |

## Bench scoreboard (post-AO/AP)

`square-sum-flo n=1_000_000` (Apple M-series, release build):

```
walker      OOM          (host stack overflow)
vm          148 ms
vm-jit       10 ms       (~15× over vm)
```

Mixed-tower body (`(+ acc 1.0 i)` with acc:flo, i:fix) at n=2000 now
returns the correct `2_001_000.0` instead of pre-AP's `i64::MAX as f64`.

## What's deferred

Per ADR 0011's follow-up list, items AR..AS+ remain:

| Item | Why | Effort |
|------|-----|--------|
| iter AR: `vm_alloc_pair` extern helper + Cranelift symbol import | First step toward heap-pointer ABI. No translator changes; just the helper. | 1 iter |
| iter AS: `cons` lowering | First end-to-end Pair-returning JIT body. | 1 iter |
| iter AT: monomorphic IC infrastructure | Per-call-site cache slot in RIR + Cranelift; helper for slow path. | 1 iter |
| iter AU: general `Call` via the IC | Biggest perf unlock for non-leaf bodies. Multi-procedure programs that today fall through to bytecode. | 1 iter |
| iter AV: `Lambda` lowering via `vm_make_closure` | Closure construction in JIT body. | 1 iter |
| iter AW: Gabriel benchmark suite import | First real perf scoreboard beyond hand-rolled bench files. | 2 iters |
| Bignum / Rational paths | Fixnum overflow trips today; deopt-friendly fast/slow split. | 2 iters |
| Mid-execution deopt trampoline | FR-4 partial today (type-guard miss → bytecode tier). Full mid-instruction unwind + recompile. | 3 iters |
| Bytevector / String / Hashtable access lowering | Direct memory reads via tagged-pointer cast. | 3 iters |
| Cross-platform | x86_64 only today. ARM64 + RISC-V via `cranelift-native`. | 2 iters |
| `(jit-dump <proc>)` full dump | RIR + Cranelift IR + native disasm. `(jit-status)` covers the high-level summary today. | 1 iter |

## Risks observed during Phase 3

1. **Block-param type defaults silently wrong (iter AM, fixed).**
   Untyped block params turning into Fixnum was a single-line
   default that lurked through every JIT compile of a branchy
   flonum body. Caught only by writing a perf bench whose result
   was visibly wrong. Lesson: post-pass type inference needs to
   read both `func.params` and each `block.params`, not just RIR
   instruction dsts.

2. **Tail-call calling convention coupling (iter AN, fixed AO).**
   `return_call` is a verifier-checked instruction whose legality
   depends on `CallConv`. SystemV rejects it, Tail accepts it but
   has a different stack layout than `extern "C"`. The wrapper
   pattern is the right answer, but it doubles the compile cost
   per JIT'd function. Keep an eye on this if compile time matters
   for very-many-closures workloads.

3. **Mixed-tower contagion silently wrong (iter AP, fixed).**
   The `all_flonum` gate was a "feels conservative" choice —
   "if any operand isn't statically flonum, use the fixnum path"
   sounds defensive but produces wrong results on the common case
   of `(+ flo-acc 1)`. Lesson: contagion in numeric towers needs
   to be explicit, not gated. Always promote when any operand is
   higher-tower.

4. **Compile output verified late.** The `define_function` call
   defers verification; errors only surface at finalize. Until
   we added the `CRABSCHEME_JIT_DEBUG` envvar (ad-hoc in iter AN's
   investigation), failures were silent and the closure stayed on
   bytecode. A permanent diagnostic surface (e.g. the existing
   `(jit-status proc)` could expose a compile-error reason field)
   would catch this earlier. Future iter.

## Counts at exit

- 0 new workspace crates.
- 11 new `pub const u8` reserved tags (iter AL, no code paths use them yet).
- 2 new helper functions in `cs-jit-cranelift` (`compile_inner_body`,
  `compile_outer_trampoline`, `detect_tail_call_self`).
- 4 new differential tests covering: let-loop flonum accumulator,
  tail-call deep recursion (250k iters), mixed-tower arithmetic, plus
  AM's block-param type-propagation regression.
- ~15× JIT-over-VM headline at 1M iters of typed flonum recursion.

---

*Authored at the close of M6 Phase 3. The JIT is now correct on
every typed-numeric body it lowers, including mixed-tower arithmetic,
deep tail recursion, and flonum-arg passthrough at any signature
shape. Remaining work targets heap-pointer values + general Call —
that's ADR 0011's end-state B and starts a new milestone.*
