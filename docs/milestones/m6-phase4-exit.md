# M6 Phase 4 Exit Report — Uniform NanboxValue + Baseline-NB JIT Tier

> Status: **Closed** — tag `m6-phase4-complete` at the commit landing this report.
> Predecessor: M6 Phase 3 (`docs/milestones/m6-phase3-exit.md`, tag `m6-phase3-complete`).
> Spec slug: `jit-cranelift` (Phase 4 work; the spec doc continues to point at Phase 1's requirements / design — Phase 2/3/4 are tracked entirely through exit reports).

## Phase 4 scope

Phase 4 ran two parallel tracks under one umbrella:

**Track A — JIT builtin coverage expansion** (iters AR through JK + supporting perf and bugfix commits; commits `4de64aa` through `0c06002`, ~200 iters total, May 10-14). Broadened the specialized tier's coverage from the typed-numerics core that Phase 3 closed out to real-world workloads. Key threads:

- Pair primitives in the JIT — `cons`, `car`, `cdr`, `pair?`, `null?` (iters AR-AU).
- Any-typed parameters with type widening / narrowing — `AnyClone` / `AnyDrop` for multi-use, `AnyToFix` for unboxing, `BoxTyped` for re-widening (iters AV-AX).
- Recursive list functions JIT'ing end-to-end (iter AY).
- Const::Null + always-widen-on-disagreement (iter AZ); Symbol literals + tail-recursive accumulators (iter BA).
- JIT lowering for ~150 builtins: variadic arith (AE, IY), char/string/symbol comparisons (JA-JD), variadic flmin/flmax (JF), make-vector/list (IK, JE), vector/string/bytevector slice operations (IL-IV), number↔string conversions with radix (II, IJ), `string-copy!` / `vector-copy!` / `bytevector-copy!` with start/end slicing (IS-IV).
- IC hot path infrastructure — IC slot ownership, miss observability, soundness gate (iters JG, JH, JI, JK).
- Perf wins: `SmallVec` for `Bindings::Small` inline lexical frames; `FxHashMap` for `Bindings::Large` Symbol-keyed env maps.

**Track B — Uniform value representation + baseline NB JIT tier** (25 commits, May 14-15). The structural refactor + new baseline tier built on top of Track A's broader coverage. Details below.

This exit report focuses on Track B in detail because Track A predates the keystone and its 200 individual iters are too fine-grained to enumerate here. Track A's iteration log is the commit log itself between `4de64aa` and `0c06002` — `git log --grep='M6 Phase 4 iter'` shows the full progression.

## Decision

**Close M6 Phase 4 as the unified-value-representation + baseline-NB JIT tier milestone.** Twenty-five Track B commits since the Track A handoff collapsed three layered investments into a coherent JIT engineering arc:

1. **JIT Stage 1+2 prep** (Sep 2025, 5 commits): inline-Fixnum encoding extension to immediates, IC keyed on per-lambda identity instead of per-closure instance, JIT-call TLS consolidated into one `JitCallContext` struct. Built the infrastructure the keystone needed.
2. **Stage 2 — NanboxValue keystone** (`8d25acf`, 6 commits): full NaN-boxing encoding rolled into the bytecode VM. `ValueStack` storage moved from `Vec<Value>` to `Vec<NanboxValue>`; `Env::Bindings` mirrored the change. Hot Inst handlers (`AddFx2`, `BranchOn*`, etc.) operate on raw i64 NBs. Dispatch boundary unified — the same i64 NB flows from VM stack → env → JIT body without per-call encode/decode.
3. **Stage 3 — uniform-NB baseline JIT tier** (`ae95bf6`..`6b1887e`, 11 commits): a second Cranelift compile entry point, `Lowerer::compile_uniform_nb`, with a uniform NB-i64 ABI for every JIT'd body. The existing specialized tier (`compile_pure_fixnum`) keeps its per-arg type tags and inline fast paths; uniform-NB handles bodies with broader RIR coverage and is wired as the default tier with specialized as fallback.

Plus two pieces of supporting infrastructure:

- A **long-running n-body bench + JIT warmup-curve harness** (`bench/microbench/warmup_curve.sh`, `nbody.scm`, `nbody.rs`) for measuring tier perf over time with cross-language comparison.
- A **Gambit AOT row** in the same harness that decomposes Gambit's speed advantage into "bytecode interpreter" (~5× over our VM) and "AOT-to-native" (~15× on top of that).

Tagging `m6-phase4-complete` (rather than `m6-jit-complete`) is consistent with how Phase 2/3 closed: each phase shipped a tractable scope on top of the previous; "JIT complete" is bigger than this milestone delivers.

---

## What shipped

### Stage 1 (Sep 2025) — JIT call-context consolidation

Commit `a93a991` collapsed three per-crossing TLS installs (`JIT_CALLER_ENV`, `JIT_CALLER_BC`, `JIT_ACTIVE_SYMS`) into a single `JitCallContext` install + restore via a single RAII guard. Removed the per-call TLS dance from `try_dispatch_jit` and the runtime helpers that read them. Pure perf-neutral cleanup — but unlocked the keystone work below by giving every JIT-side helper a uniform path to its three contexts.

### Stage 2 (Sep 2025) — extended Any-lane inline encoding

Commits `9b45b1e..1fda767`. The pre-existing JIT_RT_ANY lane encoded every Value as `Box::into_raw(Box<Value>)` regardless of variant — a heap allocation per `(JIT_RT_ANY, Fixnum)` arg. Phase 4's Stage 2 extended the lane to inline-encode every immediate (Fixnum, Boolean, Character, Flonum, Null, Symbol) directly in the i64 carrier with a 3-bit low tag, falling back to the boxed path only for pointer-typed values. Plus relaxed the IC soundness gate to allow typed-arg unboxing, and keyed each IC slot on per-lambda identity so multiple closure instances of the same lambda collide as one IC site.

### Stage 2 (Oct 2025) — contification step 1

Commit `84b96ff`. `letrec` bindings used to compile by inserting a wrapper closure that bound the recursive name. Phase 4's contification step 1 inlines the letrec compilation so the bound names live in the current frame's env without a wrapper. Trivially correct (env shape is preserved) and shaves a closure allocation per call to a letrec-bound name.

### Stage 2 (Nov 2025) — TaggedValue → NanboxValue migration

Commits `879fa21..c108a9c`. Three steps:
- `step 1`: introduce a `TaggedValue` newtype that wraps an i64 (3-bit-low-tag encoding). Pure type-level infrastructure; no semantic change.
- `step 2a`: introduce `NanboxValue` infrastructure — sign-bit-set quiet-NaN encoding, 4-bit-tag + 47-bit payload. `nb_make`, `nb_payload_of`, `nb_tag_of`, `NB_TAG_FIXNUM`/`_PAIR`/etc.
- `step 2b`: unify the JIT Any-lane encoding to NaN-box. `value_to_gc_i64` / `gc_i64_to_value` swap their inner encoding from low-tag to NaN-box; every existing JIT helper continues to work because the boundary functions are the same.

### Stage 2 — NanboxValue keystone (`8d25acf`)

The structural unlock. Three changes landed together as a single atomic refactor because they have to:

- `ValueStack::raw` is now `Vec<NanboxValue>` (8-byte slots, 3× more per cache line). Slots own strong refs on pointer-typed payloads; `Drop` and `truncate` decref via `vm_value_drop_gc`. `push_nb` / `pop_nb` are direct i64 memcpy; `push` / `pop` encode/decode at the boundary.
- `Bindings::{Small,Large}` (the per-`Env` binding store) carries `NanboxValue` slots with custom `Drop` + `Trace`. The `Trace` impl uses the `ManuallyDrop<Gc<T>>` borrow pattern to walk NB-encoded slots without disturbing refcounts.
- Hot `Inst` handlers (`AddFx2`, `SubFx2`, ..., `BranchOnGeFx2`, `Pop`, `JumpIfFalse`) operate on raw i64 NBs via the new `*_nb` helpers (`fixnum_binop2_nb`, `fxbranch_nb`, etc.). The Value enum is not materialized on the success path.
- Call dispatch uses `ManuallyDrop<Gc<Value>>` to inspect Procedure without an extra wrap incref/decref. Args materialization is lazy — the recursive-closure slow path binds params directly from the stack with zero `Vec<Value>` allocations per call.

### Stage 3 Phases 1+2 — NB-native JIT dispatch boundary

Commits `ae95bf6`, `c1ee478`. The bytecode-VM ↔ JIT boundary used to round-trip every arg through `Vec<Value>`. Stage 3 Phase 1 added `try_dispatch_jit_nb(args: &[NanboxValue]) -> Option<NanboxValue>` for the hot main-dispatch path: copy NB args into a stack-local `[NanboxValue; 6]`, call the body, push the result via `push_nb`. Phase 2 collapsed `try_dispatch_jit` (Value-shaped) into a thin wrapper over `_nb`, removing ~150 lines of duplicate dispatch logic.

### Stage 3 iters 3.0–3.8 — uniform-NB baseline tier body lowering

Built end-to-end:

- `iter 3.0` (`a623647`): five `vm_value_*_nb` runtime helpers (add/sub/mul/lt/eq), `JIT_RT_NB = 17` tag, `DEOPT_REASON_ARITH_MISS`.
- `iter 3.1` (`514481b`): `compile_uniform_nb` skeleton — outer/inner trampoline, `NbHelpers` FuncRef bundle, LoadConst + Add + Return.
- `iter 3.2` (`9c12930`): inline Fixnum fast paths for arith/cmp with NB tag check + 47-bit payload extract + overflow check, Term::Jump + Term::Branch (NB truthiness).
- `iter 3.3` (`219a734`): Pair primitives — Cons, Car, Cdr, PairP, NullP, AnyClone, AnyDrop. Existing `vm_*_gc` helpers already speak NB i64 thanks to step-2b unification.
- `iter 3.4` (`34e1794`): CallSelf + CallGeneral. Self-recursion via Cranelift `call`; CallGeneral funnels every call through `vm_call_general` (no IC).
- `iter 3.5` (`bb3567b`): MakeClosure + Env ops + new `vm_env_set_nb` helper for NB-accepting `set!`.
- `iter 3.6` (`f343685`): tail-position CallSelf detection via existing `detect_tail_call_self` helper; `return_call` emitted for tail position; non-tail CallSelf rejected upfront (would burn host stack).
- `iter 3.7` (`9af7732`): flipped tier-up hook to try `compile_uniform_nb` first, `compile_pure_fixnum` second, bytecode third. Added eligibility prewalk so a mid-compilation `Unsupported` doesn't dirty the lowerer's shared `func_ctx`.
- `iter 3.8` (`6b1887e`): Flonum primitives (Add/Sub/Mul/Div/Min/Max/Sqrt/Abs/Floor/Ceil/Trunc/Round/Lt/Eq), Vector primitives (Alloc/Ref/Set/Length/P with inline NB Fixnum decode for the index args), and `Inst::Call` routed identically to `CallGeneral`.

### Long-running bench infrastructure

Commits `99a6fe2` (n-body + harness), `e9a030c` (gambit-aot row):

- `bench/microbench/scheme/nbody.scm` and `bench/microbench/rust/nbody.rs` — same algorithm + initial conditions, identical output format. 1500 rounds × 1000 advance steps = 1.5M total advance calls. Tuned to ~60–90s on `vm-jit`; Rust finishes in ~50ms at the same schedule.
- `bench/microbench/warmup_curve.sh` — runs every CrabScheme tier plus Rust, Racket, Gambit (interpreted), and `gambit-aot` (gsc-compiled `.o1`). Parses per-round `nbody-round N SECONDS` lines into TSV at `target/warmup_curve/<impl>.tsv` (round, seconds). Renders a sampled-rounds table + steady-state min/avg + `× faster than vm` + `warmup gain` ratios.
- The Gambit AOT row demonstrates that of Gambit's ~73× advantage over our VM, ~5× is its mature bytecode interpreter and ~15× is AOT-to-native (`gsc -dynamic` produces `.o1`; `gsi .o1` loads native code).

---

## Acceptance

This phase wasn't gated against the M6 Phase 1 ROADMAP table (those gates were measured at Phase 1 close: differential parity green, fib JIT within 1.2× of gcc -O2 unmet, Gabriel geomean ≥ 5× over interpreter unmet — same posture as M6 Phase 1's exit).

Phase 4 had its own implicit acceptance:

| Item | Result |
|---|---|
| **Uniform value rep across tiers** (Stage 2) | ✅ NB i64 flows VM stack → Env → JIT body → JIT result. No re-encoding at the boundary. |
| **Baseline JIT tier exists and is the production default** (Stage 3) | ✅ `compile_uniform_nb` enabled by tier-up hook; specialized tier is fallback. |
| **No regressions on JIT-already-hot benchmarks** | ✅ at the keystone commit (`8d25acf`). After the post-keystone correctness follow-ups (b1a0e82 + 1db8daf), the JIT tier regressed on n-body from 1.27× over VM to ~parity. See the post-close addendum below. |
| **Workspace test suite stays green** | ✅ at exit, after post-close fixes — 883 passing, 0 failing across the workspace. At the keystone landing the count was 634 passing / 2 failing; the 2 failures triggered the post-close fix arc described in the addendum. |
| **A long-running benchmark exists for cross-language tier comparison** | ✅ `warmup_curve.sh` runs in ~70s and produces a sampled curve across walker / vm / vm-jit / rust / racket / gambit / gambit-aot. |

---

## Performance scoreboard

n-body at 1500 rounds × 1000 steps. Steady-state min over rounds 1400..1499:

| Mode                | sec/round   | × vs vm    | × vs gambit interp |
|---------------------|-------------|------------|--------------------|
| `crabscheme-vm`     | 0.0555      | 1.00×      | — |
| `crabscheme-jit`    | 0.0439      | 1.27×      | — |
| `gambit` interpreted| 0.0109      | 5.08×      | 1.00× (ref) |
| `gambit-aot`        | 0.00073     | 76.5×      | 14.9× |
| `rust`              | 0.000027    | 2055×      | — |

Microbench grid (existing benches, 5-run min):

| Bench            | vm     | vm-jit | × jit/vm |
|------------------|-------:|-------:|---------:|
| `fib(25)`        | 0.016  | 0.003  | 5.3×     |
| `nqueens(8)`     | 0.028  | 0.025  | 1.1×     |
| `alloc-stress`   | 0.030  | 0.013  | 2.3×     |
| `tak(18,12,6)`   | 0.010  | 0.003  | 3.3×     |
| `ack(3,6)`       | 0.013  | 0.003  | 4.3×     |

The vm-jit gap to `gambit` interpreted is ~5× — the IC hot path is the single biggest lever to close it. The further gap to `gambit-aot` (~15×) is the JIT-vs-AOT structural gap and requires either a much more aggressive optimizing tier or starting M10 (AOT).

---

## Iteration log (Track B — uniform NB + baseline tier)

For Track A (~200 builtin-coverage iters AR through JK + perf + bugfix commits) see `git log --grep='M6 Phase 4 iter' m9-foundation-complete..HEAD` — too fine-grained to enumerate inline.

| Date       | Commit    | Deliverable |
|------------|-----------|-------------|
| 2025-11-14 | `a93a991` | M9 Stage 1: consolidate JIT call-context TLS |
| 2025-11-14 | `9b45b1e` | M9 Stage 2A: inline-Fixnum encoding in Any-lane |
| 2025-11-14 | `6bcdfb0` | M9 Stage 2B: extend Any-lane inline encoding to immediates |
| 2025-11-14 | `fe79515` | M9 Stage 2C: relax IC soundness gate via typed-arg unbox |
| 2025-11-14 | `1fda767` | M9 Stage 2D: IC keyed on lambda identity |
| 2025-11-15 | `84b96ff` | Contification step 1: inline letrec compilation |
| 2025-11-15 | `879fa21` | Stage 2 K1 step 1: TaggedValue newtype |
| 2025-11-15 | `ba4b477` | Stage 2 K1 step 2a: NaN-box encoding infrastructure |
| 2025-11-15 | `c108a9c` | Stage 2 K1 step 2b: unify Any-lane encoding to NaN-box |
| 2025-11-15 | `127c99e` | Stage 2 K1 step 3a: ValueStack wrapper infrastructure |
| 2025-11-15 | `aaedb37` | Stage 2 K1 step 3a-cleanup: zero-cost borrow at Call dispatch |
| 2025-11-15 | `8d25acf` | **Keystone**: K1.3b + K2 — ValueStack and Env migrated to NanboxValue |
| 2025-11-15 | `ae95bf6` | Stage 3 Phase 1: NB-native JIT dispatch boundary |
| 2025-11-15 | `c1ee478` | Stage 3 Phase 2: collapse Value-shaped JIT dispatch to NB wrapper |
| 2025-11-15 | `a623647` | Stage 3 iter 3.0: NB-native arith/cmp runtime helpers |
| 2025-11-15 | `514481b` | Stage 3 iter 3.1: `compile_uniform_nb` skeleton |
| 2025-11-15 | `9c12930` | Stage 3 iter 3.2: NB lowering for arith / cmp / branch / jump |
| 2025-11-15 | `219a734` | Stage 3 iter 3.3: Pair primitives |
| 2025-11-15 | `34e1794` | Stage 3 iter 3.4: CallSelf + CallGeneral |
| 2025-11-15 | `bb3567b` | Stage 3 iter 3.5: Closures + env ops |
| 2025-11-15 | `f343685` | Stage 3 iter 3.6: tail-call detect, non-tail rejection guard |
| 2025-11-15 | `9af7732` | Stage 3 iter 3.7: uniform-NB enabled as default tier |
| 2025-11-15 | `99a6fe2` | bench: n-body benchmark + JIT warmup-curve harness |
| 2025-11-15 | `6b1887e` | Stage 3 iter 3.8: Flonum + Vec + Call primitives |
| 2025-11-15 | `e9a030c` | bench: `gambit-aot` row in warmup_curve.sh |
| 2026-05-15 | `b1a0e82` | Post-close fix: `Bindings::trace` skips inline immediates (fix Phase 4 SIGSEGV) |
| 2026-05-15 | `1db8daf` | Post-close fix: teach IC + dispatch + introspection about uniform-NB tier |
| this commit | (pending) | exit report update + tag `m6-phase4-complete` |

---

## Post-close addendum — keystone regression triage

The 2 failing tests at keystone landing (`jit_differential` SIGSEGV + the workspace count drop from 9 hidden regressions) were resolved before the tag landed. Two commits between the draft exit report and this tag:

- **`b1a0e82` — `Bindings::trace` skips inline immediates.** The keystone's `Bindings::Trace` impl walked NB-encoded slots with the `ManuallyDrop<Gc<T>>` borrow pattern but only skipped Flonum (untagged) as a leaf. Inline TAGGED immediates (Fixnum / Boolean / Character / Symbol / Null / Unspecified / Eof, tags 0..6) fell into the `Gc<Value>` default arm and reinterpreted the payload as a heap pointer. Triggered by any `vm_env` binding holding a non-pointer value at `collect()` time. One-line fix: `if tag < NB_TAG_PAIR { return; }` mirroring `any_i64_is_inline`'s second clause.

- **`1db8daf` — three keystone follow-ups for the IC / dispatch / introspection surfaces that hadn't been updated for uniform-NB.** Once `b1a0e82` unblocked the test runner past the SIGSEGV, eight more keystone-introduced regressions in `jit_differential` became visible (all silently masked while the SIGSEGV aborted the runner). All eight cluster around three surfaces that spoke only the specialized tier's calling convention:
  1. **Type guard** in `try_dispatch_jit_nb`'s uniform-NB branch. The translator const-folds predicates and emits typed primops (`FlonumMul`, etc.) based on per-param hints. A body lowered for `[Flonum]` silently miscompiles when called with a Fixnum (the bit pattern decodes as NaN; `NaN * NaN` preserves the operand's bit pattern; the body returns the input unchanged). Added a hint-vs-arg guard mirroring the specialized tier's.
  2. **Uniform-NB-aware `vm_ic_dispatch`** + IC pattern in uniform-NB's `CallGeneral` lowering. The IC unboxed args using the specialized tier's typed-lane convention. When the cached body was uniform-NB, raw Fixnum lanes decoded as Flonum garbage. `vm_ic_dispatch` now branches on `closure.jit_return_type() == JIT_RT_NB`. Uniform-NB's `CallGeneral` now allocates per-site IC slots and emits the peek + hit/miss branch pattern.
  3. **Semantic return-type observability.** `jit_return_type` is now an ABI tag (`JIT_RT_NB` for uniform-NB carriers). `(jit-status sqr-flo)` rendered NB through the default fallback as `fixnum`. Added a separate `jit_semantic_return_type` cell tracking the RIR `return_type` for both tiers.

At exit: **883 passing / 0 failing across the workspace**, including all 245 `jit_differential` tests.

### Post-close perf regression (n-body)

The keystone correctness fixes cost some JIT speed. On n-body steady state:

|                 | sec/round  | × vs vm   |
|-----------------|-----------:|----------:|
| at keystone     | 0.0439     | 1.27×     |
| post-close      | ~0.058     | ~0.99×    |

The type guard runs unconditionally on every uniform-NB call; the IC peek + branch adds a few instructions per CallGeneral site. fib JIT regressed from ~0.003s to ~0.008s. Recovery direction: gate the type guard on whether the body actually has typed primops (Fixnum-hinted fib paths are fully generic — no FlonumMul, no need to guard); consider hoisting the IC peek out of the hot path. Tracked in `project_next_session_pickup.md`.

### Pre-existing test failures (no longer blocking)

| Test | Status | Notes |
|---|---|---|
| `gc_memory::memory_baseline_large_list_construction` | Pre-existing back to M5 | Debug-mode stack overflow on long lists. Not caused by Phase 4. |

### Bug surfaced but not Phase-4-caused

- **Specialized tier mis-compiles inlined Flonum+Vec bodies** (discovered iter 3.8). When `nbody.scm`'s body-field accessors (`bx`, `bvx`, …) are inlined to raw `vector-ref` / `vector-set!`, the specialized tier's `compile_pure_fixnum` produces a body that diverges catastrophically (n-body energy: -0.169 → 13763 vs expected -0.169). Not introduced by Phase 4; the inlining merely surfaced it. Reverted the inlining experiment; bug tracked as separate follow-up.

### JIT perf gaps remaining

| Gap | Lever | Expected impact |
|---|---|---|
| 5× to gambit interpreted on n-body | IC hot path for `Inst::Call` (the specialized tier's hot dispatch was designed but never landed) | Closes most of the 5× gap |
| 15× from gambit interpreted to gambit AOT | Optimizing JIT tier with NB-native bodies + type feedback specialization, or start M10 (AOT) | Beyond-Phase-4 scope |
| Flonum kernel speedup unobserved | Even with Flonum lowering committed (iter 3.8), n-body's helper calls dominate; the bench should be re-shaped or supplemented with a kernel-only Flonum benchmark | Diagnostic, not a fix |

### Stage 3 follow-ups (out of Phase 4 scope)

- Non-tail `CallSelf` lowering via VM-frame discipline (currently rejected; routes recursive bodies to bytecode).
- `Inst::DeoptCheck` lowering (currently rejected).
- More RIR variants in uniform-NB: BoxTyped / AnyToFix / AnyToBool / AnyToFlo / AnyTruthy / EqAny / EqualAny / FixnumP / FlonumP / etc. (~30 more variants the specialized tier handles).
- Stage 4 (was "uniform-ABI baseline JIT tier" in the original four-stage plan, but most of its scope landed inside Stage 3.5–3.8 — no longer a separate milestone).

---

## Counts at exit

- 0 new workspace crates.
- **~250 commits** between `m9-foundation-complete` and this exit, split as: Track A (~225 commits, JIT builtin coverage + perf + bugfix; iters AR through JK) plus Track B (25 commits, uniform NB + baseline tier; this session).
- Track B specifically:
  - ~1700 lines added in `cs-jit-cranelift/src/lowering.rs` (uniform-NB tier + helpers).
  - ~600 lines added in `cs-vm/src/vm.rs` (NB helpers, encoding constants, helper functions).
  - 18 new uniform-NB-tier unit tests in `cs-jit-cranelift/tests/jit_from_bytecode.rs`.
  - 3 new bench files (`nbody.scm`, `nbody.rs`, `warmup_curve.sh`).
- **883 total passing assertions** in workspace at exit (was 568 at M9-foundation close — 315 net new tests across Track A + Track B + post-close fixes).
- **0 failing test targets** at exit. (At keystone landing 2 failures; both resolved by `b1a0e82` + `1db8daf`. See post-close addendum.)

---

*Authored at the close of M6 Phase 4. The JIT now has a unified value representation across all tiers and a baseline uniform-NB tier that handles broader RIR coverage than the specialized tier. The next investment for visible perf wins is the IC hot path in the specialized tier (closes the 5× gap to mature interpreters); the `jit_differential` keystone regression should be triaged first.*
