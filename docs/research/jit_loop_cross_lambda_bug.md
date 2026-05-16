# Bug: JIT'd loop with cross-lambda Fixnum-returning call

> **Status: FIXED in Phase 5 iter3 (commit `53207f2`).** Regression test
> lives at `tests/conformance/foundation/jit_cross_lambda_loop.scm` and
> is exercised by `cs-runtime::jit_conformance_cross_lambda_loop`.

## Original symptom (pre-iter3)

A pattern like

```scheme
(define (inner n) (+ n 1))
(define (loop count acc)
  (if (= count 0) acc (loop (- count 1) (inner acc))))
(display (loop 5000 0))
```

run on `--tier vm-jit` produced garbage like `-140707035149248` instead
of the expected `5000`. Worked correctly on `--tier vm` and `--tier walker`.

The corrupted value, decoded as bits, was an `NB_TAG_GC_VALUE`-tagged
NanboxValue — i.e., a wrapped Gc<Value> pointer rather than an
inline-tagged Fixnum.

## Triggering pattern (now safe)

- A JIT'd loop body (named-`let` or top-level recursive function)
- That calls another lambda via `CallGeneral`
- Whose return value becomes the next iteration's accumulator

## Root cause (validated post-fix)

The loop body's RIR contained a `BoxTyped` instruction (the translator
boxes the typed-lane `acc` before passing to `CallGeneral`, since the
generic-call ABI takes Any-shaped args). Pre-iter3, `BoxTyped` was not
in the `compile_uniform_nb` eligibility list — so the loop fell back to
`compile_pure_fixnum` (specialized tier).

In the specialized tier, the return type was inferred from all return
paths. The base case returned `acc` (typed Fixnum). The recursive case
returned a CallSelf whose result type widened to Type::Any (because the
inner `(inner acc)` CallGeneral produces Any). The merge gave the
function `jit_return_type = JIT_RT_ANY`.

The recursive arm was structured so that the JIT body actually returned
an NB-tagged Gc<Value> wrap of the (correct) Fixnum value. The outer
dispatcher decoded via `decode_jit_return(JIT_RT_ANY, raw)` which
extracted the inner Value::Number(Fixnum) and re-encoded — so the
*outermost* return was correct. But the intermediate recursive arm's
type tagging caused downstream consumers (the next iteration's
`acc` accumulator) to read the wrong shape.

## How iter3 fixed it

Phase 5 iter3 (`53207f2`) added BoxTyped as an identity in the
uniform-NB tier — the typed-lane src is already an NB carrier with its
proper tag, and downstream consumers via `gc_i64_to_value` handle every
NB tag uniformly. With BoxTyped supported, the loop body now compiles
under uniform-NB instead of falling back to specialized, and the
uniform-NB return type is always `JIT_RT_NB` (a uniform NB carrier) —
no Any-widening, no decode mismatch.

The fix was incidental — iter3 was targeting Phase 5 perf, not this
correctness bug — but it cleanly removed the offending code path.

## Regression test

`tests/conformance/foundation/jit_cross_lambda_loop.scm` exercises:
- Bytecode-only N (100, below tier-up threshold)
- Tier-up boundary N (5000)
- Well-past-tier-up N (50000), stable JIT
- A second pattern with `inner-mul2` producing a different value shape

Verified by `cs-runtime::jit_conformance_cross_lambda_loop` which runs
the file on all three tiers (walker, vm-no-jit, vm-jit) and asserts
matching pass counts.
