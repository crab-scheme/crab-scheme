# ADR 0027: Tail-safe continuation marks

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

Continuation marks (R6RS++ §6, Phase 3A) shipped as a *naive*
Scheme library (`lib/cmarks/cmarks.scm`): `with-continuation-mark`
was a `syntax-rules` macro expanding to `parameterize`, which in
turn desugars to `dynamic-wind` over a global parameter holding an
alist.

Two problems, both flowing from that desugaring:

1. **Not tail-safe.** Each `with-continuation-mark` installed a
   `dynamic-wind` whose *after*-thunk must run when the body
   returns. A pending after-thunk is a live frame, so a wcm in a
   tail loop accumulated O(n) frames — defeating tail-call
   elimination outright and growing memory without bound.
2. **Wrong tail semantics.** Even ignoring space, the alist
   *accumulated*: `(wcm k 1 (wcm k 2 …))` recorded both. R7RS /
   Racket semantics are that a wcm in tail position installs its
   mark on the *current continuation frame*, so a same-key wcm
   reached through tail calls **replaces** rather than accumulates.

Issue #36 (internal task #155) asks for the tail-safe, VM-level
implementation.

## Decision

Make `with-continuation-mark` a **core special form** and store
marks **on the continuation frame**, so tail-safety and
replace-semantics fall out of the existing tail-call machinery
rather than being fought against by `dynamic-wind`.

### Surface → IR

`with-continuation-mark` becomes a keyword recognised by the
expander (like `if` / `begin` / `parameterize`), lowering to a new
`CoreExpr::WithContinuationMark { key, val, body, span }`. `body`
is an implicit `begin` and sits in tail position. The old
`syntax-rules` macro is removed; `lib/cmarks/cmarks.scm` is now a
documented no-op shim.

`current-continuation-marks` becomes a primitive that reads the
live mark state (it needs runtime access the macro layer can't
give): a `Higher` (ctx-taking) builtin on the walker, and a
VM-special procedure (`VmCurrentContinuationMarks`, dispatched like
`call/cc`) on the bytecode VM.

### Walker tier — depth-tagged mark stack

`EvalCtx` gains `cont_marks: Vec<(depth, key, val)>`.
`WithContinuationMark` upserts `(ctx.depth, key, val)` (replacing a
same-key entry at that depth), then evaluates `body` in tail
position. The walker's `eval_inner` trampoline keeps `depth`
constant across tail calls (it `continue`s the loop) and bumps it
only for non-tail subexpressions (via `eval`). So:

- a wcm reached through tail calls lands at the same `depth` and
  **replaces** → constant mark-space in a tail loop;
- a non-tail call bumps `depth`, so its marks **accumulate** and
  are cleared when `eval` returns (the `retain(|d| d <= depth)` on
  the gated, normally-empty stack).

### VM tier — per-frame mark slot

The VM `Frame` gains `marks: Option<Vec<(key, val)>>`. A new
`PushMark` opcode (emitted by the compiler for
`WithContinuationMark`: compile key, compile val, `PushMark`,
compile body in the caller's tail position) upserts on the current
frame. `TailCall` already reuses the current frame in place — and
deliberately **preserves** its `marks` slot — so a wcm reached via
a tail call replaces; `Call` pushes a fresh frame (`marks: None`)
so non-tail nesting accumulates. `current-continuation-marks`
walks the frame stack.

A welcome consequence: `VmContSnapshot` clones the frame vector at
`call/cc` capture, so it captures the marks for free — full
re-entrant continuations restore their marks correctly with no
extra code.

### Tier agreement

The two tiers have independent storage (depth-tagged stack vs
per-frame slot) but produce identical observable results, verified
by a walker-vs-VM agreement test over a battery of mark programs.

## Why not keep it in Scheme / use `parameterize`

The `parameterize`/`dynamic-wind` desugaring is the *cause* of both
defects: the after-thunk is a mandatory non-tail frame. No amount
of macro cleverness recovers tail-safety, because the macro layer
cannot see tail position or frame identity. Frame identity —
"is this wcm on the same continuation frame as the last?" — is
exactly what the tail-call machinery already tracks, so the fix
belongs at the frame level.

## Consequences

### Positive
- Tail loops with `with-continuation-mark` run in constant
  mark-space on both tiers (tested at 200k iterations).
- Correct R7RS/Racket tail-mark replace-semantics.
- Marks compose correctly with full `call/cc` on the VM (captured
  in the snapshot).
- Zero cost when unused: the walker stack is gated on non-empty;
  the VM frame slot is `None` until a mark is installed.

### Negative / migration
- **Behavior change:** same-key marks nested in tail position now
  *replace* instead of *accumulate*. Code that relied on the naive
  accumulate behavior sees different results (this is the bug being
  fixed; the prior behavior was non-conformant). One existing test
  was updated to the correct semantics.
- A new `CoreExpr` variant means every exhaustive `CoreExpr` match
  gained a (mostly trivial, pass-through) arm across cs-ir,
  cs-expand, cs-typer (×5 passes), cs-vm, and cs-runtime.

### JIT tier
A function containing `PushMark` declines to JIT-compile and runs
on the bytecode VM (which has full mark support). Native
mark-slot support in the JIT trampoline is deferred — it's a pure
perf refinement; correctness and tail-safety are already provided
by the VM tier.

## Follow-ups
- First-class `continuation-mark-set` values
  (`continuation-mark-set->list`, capturing a mark set independent
  of the current dynamic extent). Still deferred.
- JIT-tier mark slot (perf only).

## References
- Issue #36 / internal task #155.
- `crates/cs-ir/src/lib.rs` — `CoreExpr::WithContinuationMark`.
- `crates/cs-expand/src/lib.rs` — `expand_with_continuation_mark`.
- `crates/cs-runtime/src/eval.rs` — walker depth-tagged stack.
- `crates/cs-vm/src/vm.rs` — `Frame.marks`, `PushMark` dispatch,
  `VmCurrentContinuationMarks`, `build_continuation_marks`.
- `lib/cmarks/cmarks.scm` — now a no-op shim.
- R7RS §`with-continuation-mark`; Racket continuation-marks docs.
