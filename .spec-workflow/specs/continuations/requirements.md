# M8 First-Class Continuations — Requirements

> Status: **Draft** (M8 iter 1)
> Spec slug: `continuations`
> Roadmap slot: M8
> Predecessors: M6 (Cranelift JIT), foundation work on call/cc (escape-only)

The runtime currently implements `call/cc` as an *escape-only* continuation: invoking the captured continuation always unwinds upward to the matching `call/cc` frame, and re-invocation after escape is undefined. R6RS mandates *first-class* continuations: the captured continuation is a regular procedure that can be called any number of times, from anywhere in the program, including outside the dynamic extent of the originating `call/cc`.

M8 closes the gap.

---

## Functional requirements

### FR-1. Re-invocation from outside the dynamic extent

```scheme
(define saved-k #f)
(+ 1 (call/cc
       (lambda (k)
         (set! saved-k k)
         10)))   ;; first call returns 1 + 10 = 11
(saved-k 100)    ;; re-enters; should produce 1 + 100 = 101
```

The captured continuation `saved-k` must be invocable *after* the surrounding expression has already completed and returned. This forbids stack-frame-pointer-based representations: by the time `(saved-k 100)` runs, the C stack frame for the original `call/cc` is gone.

Acceptance: a test that captures a continuation, escapes the original call, then invokes it from a fresh top-level form and observes the surrounding context re-run.

### FR-2. Multiple invocations

Invoking the same captured continuation N times re-runs the surrounding dynamic context N times. Each invocation appears to "resume" at the call/cc boundary as if it had just returned the new value.

Acceptance: a fold that uses call/cc to backtrack through a search space (the canonical *amb* example) terminates with the correct enumeration.

### FR-3. Generators / coroutines pattern

```scheme
(define gen
  (let ((k #f))
    (lambda ()
      (call/cc
        (lambda (return)
          (let loop ((i 0))
            (call/cc (lambda (resume)
                       (set! k resume)
                       (return i)))
            (loop (+ i 1))))))))

(gen) ;; -> 0
(gen) ;; -> 1
(gen) ;; -> 2
```

A symmetric coroutine implemented via two continuations. Each `gen` call resumes the saved coroutine, advances by one, and returns.

Acceptance: the canonical generator example produces successive integers.

### FR-4. Interaction with `dynamic-wind`

When a continuation invocation crosses a `dynamic-wind` boundary, the appropriate `before`/`after` thunks run in the right order:

- Invoking a continuation **outward** through a `dynamic-wind`: run `after` thunks for every frame being unwound.
- Invoking a continuation **inward** into a `dynamic-wind`: run `before` thunks for every frame being entered.
- The R6RS rule is "shared prefix"; thunks above the LCA run once.

Acceptance: an R6RS `dynamic-wind` test (e.g., the Larceny suite's coroutine-with-finalizer test) passes.

### FR-5. Continuation-aware exception handling

`with-exception-handler` and `guard` already use the escape-only call/cc internally. With FR-1's re-entry support, the existing handler machinery must continue to work — re-invoking a captured continuation from inside an exception handler must not corrupt the handler stack.

Acceptance: existing `tests/conformance/foundation/exceptions.scm` passes after M8 lands.

### FR-6. JIT compatibility

JIT-compiled procedures may have arbitrary native stack frames mid-execution. When call/cc captures inside or beneath a JIT frame:

- The capture must record enough of the JIT state that re-invocation can resume correctly.
- The simplest correct implementation: deopt to the VM at every call/cc capture so the captured continuation has a fully-VM representation. The performance hit is acceptable since call/cc-heavy code isn't the JIT's target workload.
- A future iter can investigate true stack-walking capture from native frames.

Acceptance: a fib that has been JITted continues to produce the right value when wrapped in `(call/cc (lambda (k) (fib 20)))`.

### FR-7. Larceny continuation suite

The Larceny test corpus has a continuation-stress section. Run it through CrabScheme and aim for ≥95% pass.

Acceptance: scripted test run produces a markdown table in `tests/conformance/continuations/results.md` showing pass/fail per test.

---

## Non-functional requirements

### NFR-1. Performance

`call/cc` overhead within 3× of a typical procedure call on the JIT (per the ROADMAP). Capture is allowed to be O(stack-depth) in the worst case; invocation is O(1).

### NFR-2. Backward compatibility

Existing escape-only patterns (`with-exception-handler`, `guard`, the foundation tests in `tests/conformance/foundation/call_cc.scm`) continue to pass without modification.

### NFR-3. ADR

`docs/adr/0010-continuations-design.md` ratifies:

- Heap-allocated frames vs stack-copy capture
- One-shot vs general continuation distinction (and whether to detect one-shot)
- Deopt-on-capture vs native stack-walking for JIT integration
- `dynamic-wind` shared-prefix algorithm

### NFR-4. Documentation

`docs/continuations.md` (or equivalent) explains the implementation to new contributors: the heap-stack representation, capture/invoke flow, and the dynamic-wind interaction.

---

## Out of scope (deferred)

| Item | Where it lives |
|---|---|
| Delimited continuations (`shift`/`reset`, `prompt`) | Post-M8 |
| Continuation marks (Racket-style) | Post-M8 |
| Native-stack-walking JIT capture | Post-M8 (FR-6 deopt-on-capture is the M8 design) |
| Multi-threading + continuations | Out-of-scope (we're single-threaded) |
