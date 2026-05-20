# M01 — Foundations: effect annotations + hot-upgrade form

**Crates touched:** `cs-rir`, `cs-expand`, `cs-opt`, `cs-hotreload`, `cs-runtime`.
**Estimated effort:** 1-2 iters.
**Depends on:** nothing.
**Unblocks:** every other milestone (effect-set checks for workflows + replicated actors, hot upgrade for durable code evolution).

## Goal

Add an `#:effects` annotation form recognized by `cs-expand`, with
effect-set inference and a static checker. Add a top-level
`define-state-migration` form that registers migrations with
`cs-hotreload`.

## Acceptance

- `(define foo #:effects '(net io) (lambda ...) )` parses, the IR carries the declared effect set, and a body containing a forbidden effect (e.g., `random` when declared `'(net)`) is a compile-time error.
- `(define-state-migration v1->v2 (lambda (s) ...))` registers a migration callable via `cs-hotreload::register_migration`.
- `(with-effect 'io (read-file p))` is a form that locally widens the effect set; outside a `(define foo #:effects ...)` declaration that already includes `io`, it's an error.
- New cs-opt pass `effect-infer` runs after typer; new pass `effect-check` runs before lowering.

## Iters

### Iter A — Add `EffectSet` to IR

- Add `EffectSet` enum/type to `cs-rir::CoreExpr` node annotation:
  ```rust
  pub struct EffectSet(BitSet);
  pub enum Effect { Pure=0, Alloc, Io, Net, WallClock, Random, Mutation, Panic, Agent, Audit }
  ```
- Threading: most existing passes only need to propagate the field;
  the IR-to-bytecode lowering can ignore it.

**Acceptance.** `cs-rir` tests pass; `cs-opt::verify` accepts an
EffectSet-annotated IR.

**Code pointers.**
- `crates/cs-rir/src/lib.rs` — current `CoreExpr` shape.
- `crates/cs-opt/src/lib.rs` — pass plugin framework.

### Iter B — Effect-infer pass

- Bottom-up walk: union of operand effect sets, plus any effects
  contributed by the form itself (e.g., `current-time` contributes `wall-clock`).
- Built-in effect table for stdlib primitives.
- Closures capture the effect set of their body.

**Acceptance.** A pure expression has empty effect set; a body
calling `(read-file …)` has `{io}`; a body calling a binding with
declared effects has the union.

### Iter C — `#:effects` annotation form

- Extend `(define …)` parser in `cs-expand` to accept the optional
  `#:effects expr` keyword.
- Same for `(lambda …)` for inline annotations.
- Annotation is parsed as a quoted symbol list, lowered to `EffectSet`.

**Acceptance.** All three forms parse:
- `(define f #:effects '(net) body)`
- `(define f (lambda args #:effects '(net) body))`
- `(with-effect 'io expr)` (block-level widening)

### Iter D — Effect-check pass

- Visit each `(define name #:effects DECLARED body)`.
- Run effect-infer on `body`.
- If `inferred ⊄ declared`, emit a diagnostic with a precise
  source span and the diff.

**Acceptance.** The test
`(define f #:effects '(io) (lambda (p) (http-get p)))` fails with
"effect `net` not declared".

### Iter E — `define-state-migration`

- New top-level form lowered by `cs-expand` into a registration call:
  `(cs_hotreload::register_migration "v1->v2" migration-thunk)`.
- The thunk must have effect set `⊆ {alloc, mutation}` (no IO during migration).

**Acceptance.** Hot-reloading a function with a registered
migration runs the migration on every active actor's state.

**Code pointers.**
- `crates/cs-hotreload/src/lib.rs` — `register_migration` exists; expose it.
- `lib/beam/prelude.scm` — current `define-state-migration` (Scheme-side macro).

## Example

```scheme
;; New: effect-annotated definition.
(define send-email
  #:effects '(net audit)
  (lambda (to subj body)
    (smtp-send to subj body)))

;; Compile error — body has effect `net`, not in declared set.
(define safe-fn
  #:effects '(io)
  (lambda (path)
    (http-get path)))    ; ERROR: effect `net` not declared

;; Block widening.
(define mixed-fn
  #:effects '(io net)
  (lambda (path)
    (with-effect 'net (http-get path))))

;; State migration.
(define-state-migration counter-v1->v2
  (lambda (old-state)
    ;; old: integer; new: (cons integer initial-time)
    (cons old-state (workflow-now))))   ; ERROR if effects include `wall-clock`

(define-state-migration counter-v1->v2
  (lambda (old-state) (cons old-state 0)))   ; OK
```

## External references

- F# computation expression zoo (annotation pattern) — <https://tomasp.net/academic/papers/computation-zoo/computation-zoo.pdf>
- Haskell's type-level effects — <https://hackage.haskell.org/package/effectful>
