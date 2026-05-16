# Bug: JIT'd loop with cross-lambda Fixnum-returning call

> Discovered 2026-05-15 during Phase 5 perf investigation. Pre-dates Phase 5
> work — verified on `4673d76`.

## Symptom

A pattern like

```scheme
(define (inner n) (+ n 1))
(define (loop count acc)
  (if (= count 0) acc (loop (- count 1) (inner acc))))
(display (loop 5000 0))
```

run on `--tier vm-jit` produces garbage like `-140707035149248` instead of
the expected `5000`. Works correctly on `--tier vm` and `--tier walker`.

The corrupted value, decoded as bits, is an `NB_TAG_GC_VALUE`-tagged
NanboxValue — i.e., a wrapped Gc<Value> pointer rather than an
inline-tagged Fixnum.

## Triggering pattern

- A JIT'd loop body (named-`let` or top-level recursive function)
- That calls another lambda via `CallGeneral`
- Whose return value becomes the next iteration's accumulator

## Does NOT trigger on

- Any of the 8 microbench cases (fib, tak, ack, nqueens, mandelbrot,
  spectral-norm, binary-trees, alloc-stress).
- nbody (flonum-heavy named-let loops with cross-lambda calls).
- The full conformance suite (883/0 tests passing).

Mandelbrot uses a similar pattern but `mandelbrot-pixel` returns a Boolean
that flows into an `if` (where NB-truthiness check is correct regardless
of inline-immediate vs Gc<Value> encoding). The Fixnum-accumulator
specifically is what miscompiles.

## Root cause hypothesis (unverified)

The loop body's RIR contains a `BoxTyped` instruction (the translator
boxes the typed-lane `acc` before passing to `CallGeneral`, since the
generic-call ABI takes Any-shaped args). `BoxTyped` is not in the
`compile_uniform_nb` eligibility list (`lowering.rs:4860`) — so loop
falls back to `compile_pure_fixnum` (specialized tier).

In the specialized tier, the return type is inferred from all return
paths. The base case returns `acc` (typed Fixnum). The recursive case
returns CallSelf result with arg 1 being the CallGeneral dst (Type::Any).
The merge widens to Type::Any, so `jit_return_type` becomes `JIT_RT_ANY`.

The body's actual returned i64 is the Gc<Value> wrap (NB_TAG_GC_VALUE),
but the value INSIDE the wrap is a Fixnum. The outer dispatcher
re-encodes via `value_to_gc_i64(decode_jit_return(JIT_RT_ANY, raw))` —
which DECODES the Gc<Value> wrap, extracts the inner Value::Number(Fixnum(n)),
and re-encodes as NB-inline-Fixnum. So the *return path* should be
correct.

The corruption likely happens INSIDE the loop body during the recursive
call setup, where the CallSelf arg 1 (Type::Any, NB-encoded as Gc wrap)
gets passed as the next loop iter's `acc`, then on the next iteration
when `(= acc 0)` or `(inner acc)` is computed, the wrong shape is read.

Or possibly: in the recursive call, the BoxTyped of the Fixnum `(- count 1)`
emits a Gc<Value> wrap, the IC slot population stamps cached_param_types
based on whatever was passed first, and subsequent calls hit the IC fast
path with the wrong unbox lane.

Needs deeper investigation. Out of scope for Phase 5 perf engineering
since none of the canonical benchmarks trip it.

## Triage priority

Medium. Doesn't affect canonical benchmarks or production code. But
indicates a soundness gap that could surface elsewhere as the JIT covers
more patterns. Worth filing as a tracking item for Phase 5+ or M10
(AOT) — the AOT path will inherit the same lowering and could expose
this more broadly.

## Repro file

`/tmp/jit_loop_cross_bug.scm` (recreate by copy-paste of the example
above) — not checked in to avoid bloating the bench/ dir with broken
fixtures. When this bug is fixed, the repro should become a
`tests/conformance/foundation/jit_loop_cross_lambda.scm` regression test.
