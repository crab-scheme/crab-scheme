# ADR 0010 — First-class continuations design

**Status:** Proposed (M8 iter 1)
**Date:** 2026-05-10
**Predecessor:** Foundation milestone (escape-only call/cc); `docs/adr/0007-jit-design.md` (JIT design); `docs/milestones/m6-exit.md`.
**Spec:** `.spec-workflow/specs/continuations/`.

## Decisions

### D-1. Capture: heap-allocated frame snapshot, not stack copy.

The captured continuation holds an `Rc<Vec<Frame>>` (the frame stack), an `Rc<Vec<Value>>` (the value stack), an `Rc<DynamicWindFrame>` (the wind chain), and the env chain. All cloned by Rc-bumping; no deep memcpy.

This rules out "save the C stack and longjmp back" approaches (e.g., libcoroutine, generic-purpose stack-copy). Those are faster but tie the implementation to the host's stack discipline; CrabScheme's VM already keeps frames in an explicit `Vec<Frame>`, so the heap-snapshot path is natural.

The walker tier doesn't have explicit frames — it recurses through the host stack inside `eval()`. For walker-tier continuations we use the eval-loop unwind machinery (D-3 below) rather than capturing host frames.

### D-2. One-shot vs general: don't distinguish at capture.

R6RS allows continuations to be invoked any number of times. A common optimization detects "one-shot" invocations and uses a faster path (no value-stack clone). M8 doesn't bother: most call/cc usage in idiomatic Scheme is single-shot anyway, and the Rc-snapshot cost is already low. A future iter can add the optimization if profiling motivates it.

The captured `Continuation` does carry an `invoked: Cell<bool>` flag for diagnostics; multiple invocations succeed but the flag is observable for testing.

### D-3. Walker-tier resume: unwind-to-driver + re-enter.

The walker `eval()` recurses through the host stack. To resume a captured continuation we'd need to be at the topmost driver, with the captured frames re-pushed. Two options:

- **A. Refactor eval to a trampoline.** Every recursive call becomes "push a frame and loop". Invasive: touches every site in eval that recurses.
- **B. Unwind to the driver via a sentinel error, restore state, re-enter `eval()`.** Less invasive: builds on the existing `EvalErrorKind::Escape` unwinding. The driver (the outer caller of `eval()`) gets a "resume continuation" sentinel and re-enters eval with the captured frame chain.

We pick **B**. The cost: every continuation invocation pays a full unwind from current depth to the driver, plus one re-entry. For non-hot-path code (call/cc isn't typical in inner loops), this is acceptable.

### D-4. VM-tier resume: direct frame-stack swap.

The VM has explicit `frames: Vec<Frame>`. Capture: clone the Vec + value stack. Invoke: replace `frames` and `stack` with the captured snapshots, push the new value, continue the run loop. No unwind needed.

This is why M8 lands more cleanly on the VM tier — fewer architectural reshapes.

### D-5. dynamic-wind shared-prefix on a linked-list wind frame.

The dynamic-wind chain is a parent-linked list of `(before, after)` thunk pairs. On continuation invocation, find the lowest common ancestor between the current and captured chains via `Rc::ptr_eq` walking, run `after` thunks unwinding to LCA, then `before` thunks rewinding into the captured chain.

R6RS §11.15 specifies this exact behavior. The implementation is straightforward once the chain is Rc-linked.

### D-6. JIT interaction: deopt on capture.

When a JITted procedure transitively calls `call/cc`, the captured frames need to be VM-representable for re-invocation. The simplest correct solution: deopt to the VM at the moment of capture — `call/cc` runs on the VM, captures VM frames, and any subsequent JITted calls resume on the VM (re-entering the JIT only after the original capture completes).

In M6, `try_dispatch_jit` already falls through on argument-type mismatch. M8 extends the fallthrough: if the procedure being called *is* `call/cc` (recognized by its bound symbol or a flag on the builtin), skip JIT for the entire dynamic extent of the capture. Cheap to implement; pessimistic but correct.

Future iter (post-M8) can investigate true native-stack-walking capture from JIT frames.

### D-7. Reuse the existing escape-only fast-path.

Many idiomatic uses of `call/cc` are escape-only: `with-exception-handler`, `guard`, early-exit from a fold. The current foundation `EvalErrorKind::Escape(id, value)` unwinding is fast — no frame snapshot, just an id check at each `call/cc` boundary.

M8 keeps this fast-path for the common case and only pays the heap-snapshot cost when the runtime observes the captured continuation being called *after* the originating `call/cc` has already returned (a "non-escape invocation"). Detection: the `Continuation` struct carries a flag set when its containing `call/cc` returns normally. If the continuation is invoked while the flag is unset, escape semantics; otherwise, full re-entry semantics with the captured frame snapshot.

This is "lazy capture" — most call/cc users pay nothing extra, and only the small minority that needs real first-class behavior pays the snapshot cost.

### D-8. Error-handling state propagates through capture/invoke.

`pending_raise`, `pending_escape`, `pending_values`, and the dynamic ports (`current_input_port`, `current_output_port`, `current_error_port`) are all part of the captured state. On invocation they're restored alongside `frames` and `stack`.

This means a continuation captured inside a `with-exception-handler` body, when invoked from outside, doesn't accidentally drop the active handler. R6RS expects this; tests will exercise it.

### D-9. R6RS §6.4 conformance over Racket extensions.

M8 ships standard `call/cc` and the R6RS interaction with `dynamic-wind`. Racket-style continuation marks, delimited continuations (`prompt`/`reset`/`shift`), and partial continuations are explicitly post-M8. They have value but they're a substantially different design surface.

## Considered alternatives

### A. Stack-copy capture (libcoroutine / makecontext).

Faster on capture (one memcpy) and faster on invocation (one swapcontext). But ties CrabScheme to host-stack semantics, fragile across platforms (different layouts on x86 vs aarch64 vs WASM), and incompatible with the future WASM target. Rejected.

### B. CPS transformation in the compiler.

Compile every Scheme expression into continuation-passing style ahead of time so call/cc is just `(λ k. ...)`. Excellent for performance but a fundamental rewrite of cs-vm and cs-jit-cranelift. Out of scope for M8; could be a separate milestone if perf demands it.

### C. Don't implement first-class continuations; ship as escape-only forever.

Tempting given the implementation cost. Rejected because:
- R6RS mandates first-class call/cc.
- The Larceny conformance suite has substantial coverage that we want to claim.
- Generators/coroutines are valuable user features.
- The lazy-capture optimization (D-7) means typical code pays no cost.

## Consequences

- The walker tier gets a small refactor (eval-loop driver re-entry seam).
- The VM tier gains explicit capture/invoke around the existing `frames` / `stack` fields.
- The JIT silently declines `call/cc` capture (deopt-on-capture).
- The R6RS Continuation procedure value gains internal capture state; the public surface is unchanged.
- `dynamic-wind` becomes a real Rc-linked list rather than a Vec; existing handler logic is updated to match.

## References

- R6RS §6.4 (Control features), §11.15 (dynamic-wind).
- Larceny continuation tests — corpus to lift.
- `docs/adr/0007-jit-design.md` — JIT design.
- `crates/cs-runtime/src/builtins/mod.rs` `b_call_cc` — current escape-only impl.
- `crates/cs-runtime/src/eval.rs` — walker tier eval loop.
- `crates/cs-vm/src/vm.rs` — VM tier explicit frames.
