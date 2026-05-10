# M6 Phase 2 Exit Report — JIT Coverage Expansion (Typed Numerics)

> Tagged: `m6-phase2-complete` at the merge commit of this report.
> Predecessor: M6 Phase 1 (`docs/milestones/m6-exit.md`, Cranelift JIT shipped with i64-only ABI).
> Spec slug: `jit` (Phase 2 deferred items from M6 Phase 1).

## Decision

**Close M6 Phase 2 as the typed-numerics JIT expansion milestone.** Sixteen
iters (W through AJ, plus the earlier A–J Phase 2 work) extended the JIT
from "pure-fixnum hot loops only" to a coherent four-tag immediate-value
pipeline (Fixnum / Boolean / Character / Flonum) covering arithmetic,
comparison, branches, predicates, and return-type decoding. Both the
walker and the dispatcher accept all four types as args and decode all
four as return values.

The remaining JIT work splits cleanly into two later milestones:

- **End-state B** ("real workloads") — needs the boxed-Value ABI design
  decision before more iters can land. Items 5–10 of `docs/jit-detailed-plan.md`.
- **End-state C** ("production-grade") — deopt trampoline, feedback-
  driven recompile beyond the current type-guard miss path, etc.

Tagging `m6-phase2-complete` (rather than `m6-jit-complete`) signals that
Phase 2's scope was the expansion within the i64-only ABI; the next big
ABI decision is queued separately.

## What shipped

### Four-tag immediate-value pipeline (iters W / X / Z)

The JIT signature now carries a **return-type tag** and **per-param-type
tags** that the dispatcher consults to box and unbox values correctly:

| Tag       | Encoding in i64 carrier  | Iter |
|-----------|--------------------------|------|
| Fixnum    | direct i64               | (existing) |
| Boolean   | 0/1                      | W |
| Character | u32 codepoint (low bits) | X |
| Flonum    | f64::to_bits             | Z |

Pipeline:

- `cs-rir::Function` gains `return_type: Type`, populated by a translator
  post-pass (`infer_return_type`) that classifies each block's terminator
  return value based on which RIR ops produced it.
- `cs-vm::VmClosure` carries `jit_return_type: Cell<u8>` and
  `jit_param_types: Cell<u32>` (4 bits per arg, 8 args max).
- `try_dispatch_jit` reads the per-param tags, type-checks each arg, and
  decodes the i64 return via the matching tag.
- The runtime jit hook receives args (signature change to
  `VmTierUpHook = fn(&VmClosure, &[Value])`) and bakes the observed types
  into the closure's signature.

### Per-Value type tracking in the translator (iter AA)

`bytecode_to_rir` maintains a `value_types: HashMap<RirValue, Type>` table
populated as instructions emit. Lets `emit_arith_binop` / `emit_cmp_binop`
choose `Add` vs `FlonumAdd`, `Lt` vs `FlonumLt`, etc., based on operand
types. Without this, every arithmetic op was unconditionally fixnum,
silently producing wrong results on flonum operands.

### Flonum arithmetic / comparison / branches (iters AA / AB / AC)

12 new RIR ops + Cranelift lowering:

- `FlonumAdd` / `Sub` / `Mul` / `Div` (AA) — `fadd`/`fsub`/`fmul`/`fdiv`
  with bitcast i64↔f64 around them.
- `FlonumLt` / `FlonumEq` (AB) — IEEE-754 ordered compares
  (`fcmp LessThan`, `fcmp Equal`).
- `FlonumSqrt` / `FlonumAbs` / `FlonumMax` / `FlonumMin` (AD).
- `FlonumFloor` / `FlonumCeil` / `FlonumTrunc` / `FlonumRound` (AG) —
  with `nearest` for banker's rounding to match R6RS `round`.

Branch terminators (`BranchOn{Lt,Le,Gt,Ge,Ne}Fx2`) dispatch through new
`emit_typed_lt` / `emit_typed_eq` helpers (AC) so flonum-typed brif
operands use the correct compare flavor.

### Variadic arithmetic (iter AE)

`+ / - / *` with arities 0, 1, and 3+ now JIT through chained binary
ops. Without this, `(+ a b c)` reached the JIT as a generic `Call` and
deopted. `(/)` still falls through (R6RS rationals don't fit the i64 ABI).

### Predicate lowering (iter Y)

`eq?` / `eqv?` / `boolean=?` / `char=?` / `symbol=?` lower to `Eq` (i64
compare on the tagged carriers). `boolean?` / `char?` / `pair?` / `null?`
/ `symbol?` / `string?` / `vector?` / `procedure?` / `port?` /
`eof-object?` lower to `LoadConst(false)` since the JIT body only runs
on Fixnum (or other immediate) args. `number?` / `integer?` / etc. fixed
to use `Const::Boolean(true)` instead of leaking `Number(1)`.

### `(command-line)` programs surface (iter K — counted in M9 but
relevant here as it touched the runtime's `Value::list` path that JIT
dispatch returns to).

### Arg-side type feedback at tier-up (iter AF)

The hook signature (`VmTierUpHook`) now receives the args that triggered
tier-up. The runtime hook classifies each arg's type and threads the
observed signature through `bytecode_to_rir_with_hints`. Bodies like
`(define (sqr-flo n) (* n n))` called with `(sqr-flo 3.5)` now JIT with
flonum-typed param and emit `FlonumMul` directly — no `real->flonum`
conversion needed in the source.

### Feedback-driven recompile (iter AH)

Type-guard misses bump a per-closure deopt counter. When it crosses
`JIT_DEOPT_RECOMPILE_THRESHOLD` (256), the closure clears its JIT
pointer and primes the tier counter for re-fire on the next call. The
new compile uses the freshly-observed signature. Closures stuck on the
wrong arg-type signature now self-heal.

`Tier::reset_for_recompile` re-primes the counter to threshold − 1 and
bumps the deopt-budget counter so a pathological program can't loop
forever.

### Latent bug fix — multi-compile (iter AI)

`compile_pure_fixnum` was passing `rir.name = "anon-jit"` to
`module.declare_function` for every compile. Cranelift rejects duplicate
names, so the **second** JIT-eligible closure in any session silently
failed to compile. Existed since Phase 1 iter 4b. Tests didn't catch it
because each test creates a fresh `Runtime`+`Lowerer`. Fix appends
`fresh_id` to the module name.

### JIT introspection (iters AI / AJ)

Three Scheme-visible builtins:

- `(jit-installed?)` → `#t`/`#f`
- `(jit-stats)` → `(tier-ups jit-calls deopts)`
- `(jit-status proc)` →
  `'not-a-closure` | `'jit-off` | `(jit-on <ret> (<param>...) calls N deopts M)`

Per-closure call counter (`jit_call_count`) and deopt counter
(`jit_deopt_count`) in `VmClosure` for postmortem of "is this closure
actually dispatching natively?".

## Acceptance summary

| Gate | Spec acceptance | Result |
|------|-----------------|--------|
| **Pure-fixnum hot loops at ≥3× VM** | M6 Phase 1 NFR-3 | **✅** ~150× on fib(30) (M6 Phase 1). Phase 2 flonum bench: 2.2× over VM, 3.5× over walker on env+builtin mix (commit 63f765d). |
| **Flonum hot loops** | Phase 2 implicit | **✅** Bodies that lift fixnum args via `real->flonum` and operate on flonums JIT end-to-end. After AF, bodies whose params are *natively* flonum (called with flonum literals) JIT directly. |
| **Multi-arg arithmetic** | Phase 2 implicit | **✅** `+`/`-`/`*` for all arities 0/1/3+. `/` deferred. |
| **Type-feedback-driven recompile** | Roadmap item 12 | **⚠️ Partial.** Type-guard misses trigger recompile; full mid-execution deopt trampoline is item 11 of `docs/jit-detailed-plan.md` and remains deferred. |
| **JIT introspection** | Roadmap item 15 | **⚠️ Partial.** `(jit-status proc)` returns the signature + counters. Full `(jit-dump <proc>)` (RIR + Cranelift IR + native disasm) is deferred. |
| **End-state B (real workloads)** | Boxed-Value ABI + general Call | **❌ Deferred.** Gates the next milestone. |

## Test inventory

| Suite | Tests | Notes |
|-------|-------|-------|
| `crates/cs-runtime/tests/jit_differential.rs` | 21 | Walker == VM-no-JIT == VM-JIT agreement on every iter's lowering. |
| `crates/cs-runtime/tests/jit_conformance.rs` | 6 | Conformance fixtures driven through the JIT runtime. |
| `crates/cs-runtime/tests/jit_runtime.rs` | 5 | Tier-up + JIT dispatch wiring. |
| `crates/cs-runtime/tests/jit_tier_up.rs` | 4 | Threshold crossing + hook signature. |

Workspace at exit: **581 passed, 0 failed** (skipping
`memory_baseline_large_list_construction` per the M5b carry-over).

## Iteration log (W–AJ + earlier A–J)

| Iter | Commit | Deliverable |
|------|--------|-------------|
| A | `d1c1bf4` | translator small wins — Pop + clearer diagnostics |
| B | `f234651` | JIT free-var env access via `Inst::EnvLookup` |
| C | `2358f84` | JIT free-var `set!` via `Inst::EnvSet` |
| D | `b33c8c9` | differential coverage for env access |
| E | `d96dc5e` | `LeFx2` / `GtFx2` / `GeFx2` in the translator |
| F | `4eb98ad` | fixnum-only builtin calls (`quotient`, etc.) |
| G | `a7b9ab4` | `abs` / `max` / `min` for fixnums |
| H | `8a5eb27` | 1-arg fixnum predicates |
| I | `1a034d4` | numeric type predicates |
| J | `1b23880` | fixnum-identity rounding ops + `square` |
| W | `8c03a65` | return-type pipeline — Boolean decoding |
| X | `535323f` | Character return + `integer->char` lowering |
| Y | `6cb04aa` | more JIT builtins (`char->integer`, `eq?`, predicates) |
| Z | `673c2c5` | Flonum return — `real->flonum` / `exact->inexact` |
| AA | `09d0657` | flonum arithmetic — `FlonumAdd`/`Sub`/`Mul` |
| AB | `3f0a3cc` | flonum comparison — `FlonumLt` / `FlonumEq` |
| AC | `e03927a` | flonum branches — `BranchOn*Fx2` use `FlonumLt`/`Eq` |
| AD | `df26ba8` | more flonum builtins — `flsqrt`, `flabs`, `flmax`, `flmin` |
| AE | `9c34b44` | variadic `+`/`-`/`*` (0, 1, 3+ args) |
| AF | `a368053` | arg-side flonum passthrough via tier-up signature |
| AG | `0c35426` | flonum rounding — `floor`/`ceil`/`trunc`/`round` |
| AH | `eace7b6` | feedback-driven JIT recompile on type-guard miss |
| AI | `8269008` | JIT introspection builtins + multi-compile fix |
| AJ | `cd03698` | per-closure JIT call/deopt counts in `jit-status` |
| AK | this commit | exit report + tag `m6-phase2-complete` |

## What's deferred

These items remain on the road to a "production-grade" JIT but are out
of scope for Phase 2. See `docs/jit-detailed-plan.md` for the full
breakdown.

| Item | Why deferred | Effort |
|------|--------------|--------|
| Boxed-Value ABI design (ADR) | Gates every end-state B item; should be ratified before more code lands. | 1 iter (ADR only) |
| General `Call` lowering w/ monomorphic IC | Biggest perf unlock for non-leaf bodies. Currently only `CallSelf` and BuiltinRef calls JIT. | 3 iters |
| `apply` lowering | Variadic dispatch; reuses general Call infra. | 1 iter |
| Tail call optimization | Cranelift `tail_call` for self-recursion + IC-monomorphic tail calls. | 2 iters |
| Heap-pointer ABI generalization | Boxed `Value` returns + args via tagged pointer (Rc-pinned). The load-bearing decision. | 4 iters |
| Allocation lowering (`cons` / `list` / `vector`) | Depends on heap ABI. | 2 iters |
| Lambda / closure creation in JIT body | Depends on heap ABI. | 2 iters |
| Mid-execution deopt trampoline | Save JIT register state at failing instruction, reconstruct VM frame, resume at equivalent bytecode op. | 3 iters |
| Bignum / Rational paths | Fixnum overflow trips today; would deopt on the rare overflow side, keep i64 result on the no-overflow branch. | 2 iters |
| `(jit-dump <proc>)` introspection | RIR + Cranelift IR + native disasm. `(jit-status)` covers the high-level summary today. | 1 iter |
| Bytevector / String / Hashtable access lowering | Direct memory reads via tagged-pointer cast; bounds-checked indexing. | 3 iters |
| Cross-platform | x86_64 only today. Cranelift supports ARM64 / RISC-V; needs the `cranelift-native` ISA selection tested + ABI tweaks. | 2 iters |
| `call/cc` interaction (deopt on capture) | JIT translator declines anything calling `call/cc`. Per M8 design, the path is "deopt on capture" — capture forces re-entry through the VM. | 2 iters |
| Gabriel benchmark suite import | Real perf scoreboard. Was deferred from Phase 1; still deferred. | 2 iters |

## Risks observed during Phase 2

1. **`anon-jit` name collision (latent since Phase 1).** The
   `compile_pure_fixnum` path reused the same module-level name across
   compiles, causing the second JIT-eligible closure in any session to
   silently fail. Existing tests didn't catch it because each test
   creates a fresh `Runtime` + `Lowerer`. Bug + fix landed in iter AI.
2. **Bytecode-VM sees flonum where translator assumes fixnum.** The
   per-Value type tracking in iter AA is *static*; if the source has
   `(* a b)` where `a` and `b` are dynamically-typed (came from a
   non-immediate Value), the translator picks fixnum semantics by
   default. The dispatcher type-guard catches the mismatch at runtime
   and falls back to bytecode. This is conservative but correct.
3. **Tier-counter races.** The `cs_jit::Tier` uses atomic ops
   (`AtomicU32`); the counter is process-wide-ish (per-closure). Multi-
   threaded use isn't currently exercised; if multiple threads enter
   the same closure simultaneously, the tier-up hook could fire twice.
   Phase 2 is single-threaded by construction; multi-threaded JIT would
   need a CAS guard around the hook fire site.
4. **Recompile thrash.** Iter AH's threshold-based recompile could
   theoretically loop if a closure is repeatedly called with
   alternating type signatures every 256 calls. Bounded by the
   `MAX_DEOPT_RETRIES` budget on `Tier::deopt_count`, after which the
   closure is permanently bytecode'd.
5. **Test-helpers feature wasn't declared.** Compiler warned for
   several iters; fixed in AK by declaring the feature in
   `cs-jit-cranelift/Cargo.toml`.

## Counts at exit

- 0 new workspace crates (Phase 2 is feature work in cs-rir, cs-vm,
  cs-jit-cranelift, cs-runtime).
- 16 new RIR ops shipped across W–AJ:
  - 1 conversion (`FixToFlo`)
  - 4 arithmetic (`FlonumAdd`/`Sub`/`Mul`/`Div`)
  - 2 comparison (`FlonumLt`/`Eq`)
  - 4 unary/binary (`FlonumSqrt`/`Abs`/`Max`/`Min`)
  - 4 rounding (`FlonumFloor`/`Ceil`/`Trunc`/`Round`)
  - 1 retag (`IntCharBitcast`)
- 4 new tag constants (`JIT_RT_FIXNUM`/`BOOLEAN`/`CHARACTER`/`FLONUM`).
- 3 new Scheme builtins (`jit-installed?` / `jit-stats` / `jit-status`).
- 1 latent bug fix (iter AI, multi-compile name collision).
- 581 total passing assertions in the workspace test suite at exit
  (was 549 at M8 close; ~32 added by JIT work and concurrent M9 stdlib
  iters).

---

*Authored at the close of M6 Phase 2's typed-numerics expansion. The
JIT covers every immediate value type (fixnum, boolean, character,
flonum) for arg, body, and return. Remaining work targets heap-pointer
values + general procedure call — that's the boxed-Value ABI design
decision, which deserves its own ADR before code lands.*
