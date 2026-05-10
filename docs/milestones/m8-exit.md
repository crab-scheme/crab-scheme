# M8 Exit Report — First-Class Continuations (VM tier)

> Tagged: `m8-vm-complete` at the merge commit of this report.
> Predecessor: M6 (`docs/milestones/m6-exit.md`, Cranelift JIT shipped).
> Predecessor: M5b (`docs/milestones/m5b-exit.md`, Rust FFI shipped).
> Spec: `.spec-workflow/specs/continuations/`.
> ADR: `docs/adr/0010-continuations-design.md`.

## Decision

**Close M8 as the VM-tier first-class continuation milestone.** The VM tier supports the full R6RS first-class call/cc semantics (re-invocation outside the dynamic extent, multiple invocations, continuation-as-data-structure-element). The walker tier remains escape-only and is documented as such.

The remaining spec items — walker-tier first-class, `dynamic-wind` shared-prefix, the Larceny continuation suite, JIT deopt-on-capture — are queued as post-M8 follow-ups. Tagging `m8-vm-complete` (rather than `m8-complete`) signals that the milestone shipped on one of two tiers; a subsequent milestone can pick up the walker-tier work.

This mirrors the M6 close pattern (Phase 1 / Phase 2 deferred items) and is consistent with the project's iter-discipline approach: ship what's solid, document what's deferred, don't tag a clean "complete" without observable evidence.

---

## What shipped

### VM-tier first-class continuations

Per ADR 0010 D-1 / D-4: the VM's `Continuation` value carries a heap-allocated snapshot of frames + value stack, captured at `call/cc` entry. Invocation restores the snapshot and resumes the run loop at the captured top frame. Re-invocation re-clones the inner Vecs so the snapshot is reusable.

Key implementation pieces (all in `crates/cs-vm/src/vm.rs`):

- `Frame` derives `Clone + Debug` so the snapshot can clone the frame Vec cheaply (each frame's `insts` / `spans` / `bc` / `env` are already `Rc`-shared).
- `VmContSnapshot { frames: Rc<Vec<Frame>>, stack: Rc<Vec<Value>> }` — the snapshot itself is Rc-cheap; the per-invocation clone is `(*snap.frames).clone()` and `(*snap.stack).clone()`.
- `VmContinuation { id, snapshot, in_flight: Cell<bool> }` — the in_flight flag is the design's lazy-capture realization (D-7). While call/cc is on the stack, dispatch takes the legacy escape path. After call/cc returns, dispatch routes through snapshot-restore.
- The `call/cc` builtin captures the snapshot and clears `in_flight` on return.
- The `VmContinuation` dispatch in `run_dispatch`'s main fast path checks `in_flight` and either restores the snapshot or falls through to the pending-escape unwind.

### Pre-existing VM compiler bug fix

Iter 4 caught and fixed a separate bug that was blocking the multi-invocation test: `compile_with_globals_and_primops` was folding `Ref` to the captured-at-compile-time global value even when the program rebound the name via top-level `define` (which lowers to `Set`). Repro: `(define count 0) (+ count 1)` errored with "got procedure (#<procedure count>)" because the runtime's `count` builtin shadowed the user's define at compile time.

Fix: pre-scan the `CoreExpr` tree for every `Set` target and add them to a synthetic top scope so `is_locally_bound` reports them as locally bound. The fold suppression is conservative (any name `Set!` anywhere in the program is excluded) but the perf cost is one extra `LoadVar` per such reference.

### Diagnostic improvement

Iter 2 replaced the generic "uncaught escape continuation #N" error with a M8-aware diagnostic: "continuation #N invoked outside its dynamic extent (value: V) — first-class re-invocation is not yet supported (see M8 spec)". This was the user-visible boundary of the M8-iter-1 state; iter 3 then made the diagnostic moot for the VM tier (the runtime now does the right thing rather than erroring). The walker-tier still emits this diagnostic since walker-tier first-class isn't shipped.

### Spec + ADR

- `.spec-workflow/specs/continuations/{requirements,design}.md` — 7 FRs, 4 NFRs, 7-iter plan.
- `docs/adr/0010-continuations-design.md` — 9 ratified decisions, including the lazy-capture (D-7) and walker-tier-via-option-B (D-3) approaches.

---

## Acceptance summary

| Gate | Spec acceptance | Result |
|---|---|---|
| **FR-1.** Re-invocation from outside the dynamic extent | "saved continuation re-invokes; surrounding context re-runs." | **✅ VM tier.** `m8_reinvocation_after_extent_vm` passes. Walker: `m8_reinvocation_after_extent_walker` ignored. |
| **FR-2.** Multiple invocations | "amb-style backtracking enumeration." | **✅ VM tier.** `m8_multiple_invocations_vm` passes (counter pattern returns 3 after three resumes). Walker: ignored. |
| **FR-3.** Generators / coroutines | "canonical generator example produces successive integers." | **⚠️ Indirectly demonstrated.** The multi-invocation test exercises the same machinery; a dedicated generator test isn't yet in the suite. Add when needed. |
| **FR-4.** Interaction with `dynamic-wind` | "an R6RS dynamic-wind crossing-boundary test passes." | **❌ Deferred.** Current `dynamic-wind` doesn't track an Rc-linked frame chain, so continuation crossings don't run before/after thunks at the right times. `m8_dynamic_wind_through_continuation` ignored. |
| **FR-5.** Continuation-aware exception handling | "with-exception-handler tests still pass after M8 lands." | **✅** `eval_continues_after_caught_ffi_error` (FFI-conformance) passes. The `in_flight` gate is critical here — escape-only behavior is preserved while call/cc is in flight, which is the case for `with-exception-handler` patterns. |
| **FR-6.** JIT compatibility | "JIT-compiled fib continues to work when wrapped in (call/cc (lambda (k) (fib 20)))." | **✅ Trivially.** The JIT translator declines anything that calls `call/cc`; calls through call/cc take the bytecode path which has snapshot support. The "deopt on capture" design point is moot because JIT-side capture isn't a real configuration. |
| **FR-7.** Larceny continuation suite | "≥95% pass on the suite." | **❌ Deferred.** Suite not yet imported. |

NFRs:

| NFR | Spec | Result |
|---|---|---|
| **NFR-1.** Performance | "call/cc overhead within 3× of a typical procedure call on the JIT." | **Not measured.** Snapshot capture is O(frame depth) Rc-bumps, invocation is O(frame depth) Vec-clone — both cheap in absolute terms. A criterion bench would land alongside the dynamic-wind work. |
| **NFR-2.** Backward compatibility | "existing escape-only patterns pass without modification." | **✅** All 540 pre-M8 tests still pass; `with-exception-handler`, `guard`, etc. unaffected. |
| **NFR-3.** ADR | written | **✅** `docs/adr/0010-continuations-design.md`. |
| **NFR-4.** Documentation | "explains the implementation to new contributors." | **⚠️ Partial.** Spec + ADR cover the design. A `docs/continuations.md` user-facing piece is deferred. |

---

## Test inventory

| File | Coverage | Tests |
|---|---|---|
| `crates/cs-runtime/tests/m8_baseline.rs` | escape-only baselines (3) + diagnostic shape (1, walker-only now) + VM-tier first-class (5) + walker placeholders (3 ignored) + dynamic-wind placeholder (1 ignored) | 9 active, 4 ignored |
| `crates/cs-runtime/tests/ffi_error_conformance.rs` | `eval_continues_after_caught_ffi_error` exercises call/cc inside `with-exception-handler` | 1 (M8-relevant) |
| `crates/cs-runtime/tests/vm_conformance.rs` and friends | the broader conformance suite uses call/cc heavily for `with-exception-handler` patterns | 100+ (indirect M8 coverage) |
| **M8 total** | | **9 explicit + indirect coverage via the broader suite** |

Workspace at exit: **549 passed, 0 failed** (skipping the pre-existing `memory_baseline_large_list_construction` debug-stack overflow inherited from M5).

---

## Iteration log

| Iter | Commit | Deliverable |
|---|---|---|
| 1 | `c0297a7` | spec + ADR scaffold + baseline tests |
| 2 | `8c24a28` | clearer diagnostic for after-extent invoke |
| 3 | `e4d2147` | VM-tier snapshot capture + resume + in_flight gate |
| 4 | `d785125` | VM compiler global-fold bug fix; multi-invocation flips on |
| 5 | `7a6888b` | VM-tier stress tests (continuation-as-data, nested call/cc, arithmetic context) |
| 6 | this commit | exit report + tag m8-vm-complete |

---

## What's deferred (post-M8 follow-ups)

| Item | Why deferred | Effort estimate |
|---|---|---|
| Walker-tier first-class | Walker uses host-stack recursion in `eval()`. Implementing first-class requires either trampolining `eval()` (option A) or implementing the unwind-and-re-enter pattern from ADR D-3 (option B). Multi-iter project. | 4-6 iters |
| `dynamic-wind` shared-prefix | Current `dynamic-wind` runs before/thunk/after sequentially with no chain tracking. First-class continuations crossing a wind boundary need: an Rc-linked `WindFrame` chain, capture into the snapshot, LCA computation, before/after thunks fired in shared-prefix order. Substantial. | 2-3 iters |
| Larceny continuation suite | Not yet imported. Once dynamic-wind lands, the suite becomes a meaningful conformance test. | 1-2 iters (import + reporting) |
| JIT deopt-on-capture | JIT translator doesn't lower `Call` (and so doesn't lower `call/cc`). Programs that hit call/cc take the bytecode path automatically. Deopt-on-capture as a discrete feature is moot until the JIT covers more bytecode. | 0 iters now; revisit alongside JIT broadening. |
| Generators / coroutines test | Implicitly covered by multi-invocation; a dedicated test would document the pattern. | 1 iter (test only). |
| `(jit-dump <proc>)` for continuations | M6 deferred; carrying forward. | Same as M6 follow-up. |
| User-facing `docs/continuations.md` | Spec + ADR cover the design; user docs are deferred. | 1 iter |
| Performance benchmarks (NFR-1) | call/cc isn't a hot-path operation; benchmarks would document baseline but don't gate any decision. | 1 iter alongside dynamic-wind. |

---

## Risks observed during M8 work

1. **In-extent vs after-extent ambiguity.** Iter 3's first attempt did snapshot resume on every continuation invocation, breaking `with-exception-handler` (which relies on escape unwinding to tear down handler state). The fix — `in_flight: Cell<bool>` — is small but load-bearing; remove it and FFI conformance breaks immediately.
2. **Pre-existing VM compiler bug.** Iter 4 caught the global-fold-vs-rebind bug. The fix is conservative (suppress fold on any `Set!`'d name) but doesn't measurably hurt perf.
3. **Walker-tier physics.** The walker tier's host-stack recursion doesn't admit a low-effort first-class implementation. Two paths forward (trampoline / option-B unwind) both substantial. Calling this out as a deferred item rather than crashing through it.
4. **dynamic-wind through continuation: silent program hangs.** Brief experiment in iter 6 showed dynamic-wind + continuation invocation produces silently-empty output — likely the dynamic-wind Rust impl returning early after the snapshot resume bypasses its `after` thunk. Defer to a dedicated iter that adds the wind chain.

---

## Counts at exit

- 0 new workspace crates (M8 is feature work in existing crates).
- 9 M8-specific tests across the cs-runtime test suite (5 active VM-tier first-class, 3 escape-only baselines, 1 walker-tier diagnostic).
- 549 total passing assertions in the workspace test suite at exit.
- ADR 0010 ratified, M8 spec marked "VM-tier complete".
- Pre-existing VM compiler bug fixed as a side effect.

---

*Authored at the close of M8's VM tier. The walker tier remains escape-only; users wanting full first-class call/cc semantics should target `Runtime::eval_str_via_vm` (or `--tier vm-jit`). The `JitBackend` trait and `Continuation` value design are preserved so a future walker-tier first-class iter can land without API churn.*
