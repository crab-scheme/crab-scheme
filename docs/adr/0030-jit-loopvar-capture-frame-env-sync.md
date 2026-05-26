# ADR 0030: JIT — sync loop-updated vars into the frame env on the self-call back-edge

> Status: Accepted
> Date: 2026-05-26
> Authors: crab-scheme contributors

## Context

A JIT correctness bug, surfaced by a perf run: `spectral-norm` produced
a *wrong* floating-point result under `--tier vm-jit` (e.g. `1.0300…`
instead of `1.2742…`) while the walker and bytecode VM were correct. An
output-equality check against `rustc -O` caught it.

Root cause (reduced to an all-integer minimal repro — it is **not**
float-specific):

```scheme
(define (f n)
  (let il ((i 0) (acc 0))                 ; self-recursive outer loop
    (if (< i n)
        (il (+ i 1)
            (+ acc (let jl ((j 0) (s 0))  ; inner loop closes over i
                     (if (< j n) (jl (+ j 1) (+ s i)) s))))
        acc)))
```

The outer named-let `il` is a self-recursive procedure. The JIT
**loop-converts** the self-tail-call: `il`'s parameters become Cranelift
block-params advanced in registers across iterations. Because `il`'s
body builds an inner closure (`jl`), `Function::builds_closures()` is
true, so the dispatcher materializes a per-call **frame env** binding the
params (`vm_ic_dispatch` / `try_dispatch_jit`, ADR 0012 D-1) — and
`MakeClosure` (`vm_make_closure`) snapshots that env for the new closure.

But the frame env binds the params **once, at loop entry**. The
loop-converted back-edge updates the induction variable in the
*register* only; it never writes the new value back into the frame env.
So every iteration's `MakeClosure` captured a frame env whose param slot
was **frozen at the loop-entry value** (specifically the value at the
1024-call tier-up threshold). The inner closure then read that stale
value via `EnvLookup`.

Symptoms, all confirmed: deterministic; appears exactly when the outer
loop crosses the 1024-call tier-up threshold (`DEFAULT_TIER_THRESHOLD`);
the error grows with the post-tier-up iteration count; capturing a
loop-**invariant** variable (e.g. a never-reassigned parameter) is fine
(its frozen value is correct); passing the variable as a call **argument**
instead of capturing it is fine.

## Decision

On the self-recursive tail-call (the loop back-edge), when the body
builds closures, **write each updated parameter back into the frame env**
before continuing the loop. In `cs-vm/src/jit_translate.rs`, the
`StackEntry::SelfRef` arm of `Inst::Call`/`Inst::TailCall` now emits, when
`body_has_makeclosure`, an `EnvSet(param, new_value)` for each parameter
ahead of `CallSelf`:

```rust
if body_has_makeclosure {
    for (p, a) in lambda.params.iter().zip(args.iter()) {
        insts.push(RirInst::EnvSet(p.0, *a));
    }
}
```

This mirrors the existing `SetVar` arm ("always `EnvSet` so a captured
closure sees the new value") and reuses the same `EnvSet` lowering, so
the raw/NB value encoding is handled by existing, shared code. The next
iteration's `MakeClosure` therefore captures current parameter values.

### Alternatives considered

- **Don't loop-convert** self-recursive bodies that build closures
  (force real re-dispatch per iteration, which rebinds the frame env).
  Correct, but discards the loop optimization for an entire class of
  bodies — including the common, *correct* loop-invariant-capture case.
- **Decline JIT** for such bodies (fall back to the VM). Correct but a
  larger perf regression (e.g. spectral-norm's inner loops).

The chosen fix preserves the JIT (measured: spectral-norm `--tier vm-jit`
still ~15× the VM and now bit-identical to `rustc -O`) and is gated on
`body_has_makeclosure`, so non-capturing loops keep the bare register
back-edge.

## Consequences

- **Correctness:** the three tiers (walker / VM / JIT) agree again on the
  repro, on `spectral-norm`, and on loop-varying captures of any
  parameter. Silent wrong answers (int and float) are eliminated for this
  pattern.
- **Performance:** a small per-iteration `EnvSet` for capturing loops
  only; negligible in measurement and far cheaper than the `MakeClosure`
  the iteration already performs. Non-capturing loops are unaffected.
- **Scope:** purely a JIT-translation change; the walker and VM were
  never affected.

## Testing

`crates/cs-runtime/tests/jit_differential.rs` (three-tier agreement):
- `diff_inner_closure_captures_outer_loop_var` (+ exact value 1686375000),
- `diff_inner_closure_captures_loop_accumulator` (captures the *other*
  loop var),
- `diff_inner_closure_captures_outer_loop_var_float` (the spectral-norm
  float shape).

Full suites green: cs-runtime 1078/0, cs-jit-cranelift + cs-vm green,
clippy clean on the touched code.

## References
- ADR 0012 (D-1 closure-env capture; the frame-env mechanism extended here).
- `cs-vm/src/jit_translate.rs` (`SelfRef` arm), `cs-rir/src/lib.rs`
  (`builds_closures`), `cs-vm/src/vm.rs` (frame-env dispatch,
  `vm_make_closure`).
- Minimal repro: `/tmp/jit-capture-loopvar-bug.scm`.
