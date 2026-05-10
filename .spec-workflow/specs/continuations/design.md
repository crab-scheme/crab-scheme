# M8 First-Class Continuations — Design

> Status: **Draft** (M8 iter 1)
> Companion: `requirements.md`
> ADR: `docs/adr/0010-continuations-design.md`

## Overview

Capture the live evaluation context — frame stack, env chain, dynamic-wind chain, pending raise/escape state — into a heap-allocated `Continuation` value. Invoking it restores that state and resumes execution at the `call/cc` boundary.

The current foundation `Continuation` is escape-only: invocation throws an `EvalError::Escape` that the matching `call/cc` catches via id. M8 reuses the id-tagged Continuation procedure but extends:

1. The captured state — full frame chain, not just an id.
2. The invocation mechanism — restore + resume, not just unwind.
3. The dynamic-wind shared-prefix algorithm.

## Components

### `Continuation` value

```rust
#[derive(Debug)]
pub struct Continuation {
    /// Unique id (existing).
    pub id: u64,

    /// Captured frame stack: a snapshot of `frames: Vec<Frame>` at
    /// capture time. Frames are immutable once captured (Rc'd
    /// internally so re-invocation doesn't deep-clone).
    frames: Rc<Vec<Frame>>,

    /// Captured value stack at capture time (typically empty for
    /// the call/cc-as-tail-position case but not always).
    stack: Rc<Vec<Value>>,

    /// Captured dynamic-wind frame at capture time.
    wind_frame: Rc<DynamicWindFrame>,

    /// Captured environment chain.
    env: Rc<Frame>,

    /// One-shot flag — set when the continuation is invoked; on a
    /// second invocation, error or warn (R6RS §11.15 leaves this
    /// implementation-defined).
    invoked: Cell<bool>,
}
```

The walker tier's `Frame` type (cs-runtime/src/eval.rs) already exists; M8 promotes the frames Vec to be reachable from a Continuation via Rc.

### Capture (call/cc)

```rust
fn b_call_cc(args, ctx) -> Result<Value, String> {
    let id = next_continuation_id();
    let k = make_continuation(Continuation {
        id,
        frames: Rc::new(ctx.frames.clone()),
        stack: Rc::new(ctx.stack.clone()),
        wind_frame: ctx.dynamic_wind_chain.clone(),
        env: ctx.top.clone(),
        invoked: Cell::new(false),
    });
    apply_procedure(&args[0], &[k], ctx)
}
```

The capture cost is O(frame count); each frame's contents (insts, env) are already Rc-shared so the deep copy is just a Vec-of-Rc clone.

### Invocation

```rust
// In apply_procedure, when callee is a Continuation:
fn invoke_continuation(k: &Continuation, vals: &[Value], ctx: &mut EvalCtx)
    -> Result<Value, EvalError>
{
    // Run dynamic-wind unwind to LCA.
    let lca = lowest_common_ancestor(&ctx.dynamic_wind_chain, &k.wind_frame);
    run_after_thunks(&ctx.dynamic_wind_chain, &lca, ctx);

    // Restore captured state.
    ctx.frames = (*k.frames).clone();
    ctx.stack = (*k.stack).clone();
    ctx.dynamic_wind_chain = k.wind_frame.clone();

    // Run dynamic-wind rewind from LCA into captured.
    run_before_thunks(&lca, &k.wind_frame, ctx);

    // Push the invocation argument as the result of the original
    // call/cc, then resume the eval loop.
    ctx.stack.push(if vals.len() == 1 { vals[0].clone() } else { make_values(vals) });
    Ok(/* resume */)
}
```

### Resume mechanics (the hard part)

The walker tier currently uses the host call stack — recursion in `eval()` mirrors recursion in the Scheme program. To resume from a continuation, we'd need to re-enter `eval` with the captured frame stack, but the existing host stack is in the way.

Two options:

**A. Trampoline the eval loop.** Refactor `eval()` so the top-level driver is a loop that pulls work off `ctx.frames` rather than recursive calls. This is invasive — every recursive call site in eval becomes "push a frame and continue".

**B. Use the EvalError unwind machinery.** Tag continuation invocation as a special unwind that (a) walks up to the topmost driver, (b) restores the captured state, (c) the driver re-enters eval. The driver is `Runtime::eval_str`'s outer loop; we add a "resume continuation" branch that the unwind targets.

Option B is less invasive — it builds on the existing escape unwind. The cost: every call/cc invocation pays a full unwind. Acceptable for a non-hot-path operation.

We'll go with **option B for the walker tier**. The VM tier already has explicit frames in `Vec<Frame>`; capture/restore is more direct (mutate `frames` + `stack`).

### VM tier

The VM's frames are explicit. Capture: `frames.clone() + stack.clone()`. Invoke: replace `frames` and `stack` with the captured snapshots, push the new arg as the value, continue the run loop. No host-stack issue.

### Dynamic-wind shared-prefix

`DynamicWindFrame` is a linked list (Rc-shared). LCA: walk both chains' parent pointers, find the deepest shared `Rc::ptr_eq`. Unwind: walk current chain to LCA, run each `after` thunk. Rewind: walk captured chain to LCA in reverse, run each `before`.

### JIT integration (FR-6)

When `(call/cc proc)` runs, the runtime knows it's about to capture. If the current call site is a JITted procedure, we deopt to the VM before capture so the captured frames are all VM frames.

Implementation: `try_dispatch_jit` already falls through on type mismatch. Add a "deopt on capture" path: if `proc` is `call/cc` (recognizable by its bound symbol), don't dispatch through JIT; route to the VM body. Cheap: call/cc is a known builtin, not a user closure.

## Plan order

1. **Iter 1** (this iter): scaffold spec + ADR. Existing escape-only call/cc stays untouched.
2. **Iter 2**: refactor `Continuation` to carry a frame snapshot. VM tier capture+invoke (no walker change yet).
3. **Iter 3**: walker tier capture+invoke via Option-B unwind.
4. **Iter 4**: dynamic-wind shared-prefix algorithm + tests.
5. **Iter 5**: JIT deopt-on-capture.
6. **Iter 6**: Larceny continuation suite import + pass-rate measurement.
7. **Iter 7**: M8 exit report.

Each iter ships tests; no flag-day landing.

## Risks

1. **Walker tier refactor**: Option B's "unwind to driver, re-enter" needs the driver to know how to resume. The current driver is `Runtime::eval_str` which is one-shot. Either we add a re-entry loop, or we model continuations as "call eval recursively with captured state" (which works but pays a host-stack cost per invocation — acceptable for non-hot-path).
2. **Capture cost**: Rc-cloning a Vec<Frame> at every call/cc is fast but not zero. For exception-handler-heavy code (tight loops with `guard`), this could matter. Mitigation: keep the existing escape-only fast-path for `EvalError::Escape` use; only allocate full Continuation when invocation outside the dynamic extent is observed (lazy capture).
3. **Stack-stored mutable state**: any state that lives on the host stack and isn't captured will be lost on re-invocation. Audit `ctx` for fields beyond what we list; pending_raise / pending_escape / pending_values / current_input_port etc. Capture all of them.
4. **JIT-side complexity**: deopt-on-capture is the M8 plan; future iters might want true stack-walking capture from native frames, which is a much larger project.
