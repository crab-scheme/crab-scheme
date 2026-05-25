# ADR 0021: `define/typed` static type checking at expand time

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

R6RS++ Phase 4 §12 ("typed boundaries") shipped a 5-iter substrate
that brought `(define/typed NAME TYPE EXPR)` to user code:

- Rust `cs_typer::contract_lowering::type_to_contract` translates
  every `Type` to a Scheme contract.
- Scheme `__type->contract` is the same mapping for runtime use
  (avoids crossing the Rust ↔ Scheme tier on every call).
- The `define/typed` macro itself wraps `EXPR` with
  `(apply-contract (__type->contract 'TYPE) EXPR 'NAME)` so each
  call goes through a contract that fires
  `&contract-violation` on misuse.

The substrate handed users a *dynamic* type check: a typed
binding called with a malformed argument fails on the first call,
not at program load. The interim status report (issue #11) lists
"static check at definition time" as the biggest user-visible
upgrade — Typed-Racket-style "fail at load" semantics for users
who reach for `define/typed`.

The blocker has been: the contract macro and cs-typer don't talk
to each other. cs-expand expands the macro into the apply-contract
wrap and moves on; the Checker (the only piece that can produce
a static type error) doesn't know `(define/typed)` exists and
sees the wrap call as `apply-contract → Any` — the gradual escape
defeats the static guarantee.

## Decision

We thread `(define/typed)` through `cs_typer::extract_annotations`
and peel `(apply-contract _ inner _)` in `Checker::check_set`.
Together these flip the binding from "fail on first call" to
"fail at expansion / `crabscheme check`":

1. **`extract_annotations` recognizes `(define/typed N T E)`.**
   The pre-pass synthesizes a `TopLevelAnnotation { N, T }` and
   pushes it into the table just like an explicit `(: N T)` would.
   Critically, the Datum *also survives in the stripped output* —
   the contract library's macro still runs at the usual expansion
   time and produces the runtime wrap unchanged.

2. **`Checker::check_set` peels apply-contract.** Before checking
   a binding's value against its declared type, the Checker
   looks for the shape `(apply-contract _ inner _)` (3-arg call,
   head Ref to the exact symbol `apply-contract`) and recurses
   on `inner` instead. Without the peel, the wrap call's
   inferred type `Any` defeats the gradual check at the
   binding's type slot; with it, the inner expression's type is
   verified directly.

The result: `(define/typed N T E)` raises a typer diagnostic
when `E` mismatches `T` under `crabscheme check`. The runtime
contract wrap is preserved so untyped callers still get the
dynamic guarantee.

### Alternatives considered

- **Strip the wrap at Datum level and re-emit `(set! N (apply-contract …))`**
  after the bare define. Static check would run on `(define N E)`
  (clean), runtime contract would still wrap via the trailing
  `set!`. Rejected: two Datums for one declaration is awkward
  for downstream tooling, and the brief window between define
  and set! where `N` is unwrapped is a subtle footgun.

- **Special-case `define/typed` as a CoreExpr-level form** in
  cs-expand. Skips the macro and the wrap; cs-expand emits a
  `Set` plus the ascription, cs-typer checks normally, runtime
  re-wraps via a post-pass. Rejected: invades cs-expand with
  contract-library knowledge, breaking the layering where
  cs-expand is contract-agnostic.

- **Have the Checker special-case the *macro name*** by
  walking back from the post-expansion form. Rejected: macro
  expansion erases the surface form; the Checker would have to
  pattern-match on the macro's emitted shape, which is exactly
  what the apply-contract peel already does — the peel just
  doesn't require a name-specific rule.

## Consequences

### Positive
- `(define/typed N T E)` now fails at load when `E` mismatches `T`.
  The macro's dynamic contract continues to fire on untyped
  callers, so the binding is robust in both directions.
- The peel is unconditional — any hand-written
  `(apply-contract _ inner _)` call against a typed binding gets
  the same static guarantee. Hand-rolled contract code benefits
  for free.
- No new crate dependencies, no new public APIs. The change is
  self-contained in `cs_typer::extract` + `cs_typer::checker`.

### Negative / risks
- Static checking now runs on the body's *inferred* shape.
  Inline lambdas without parameter annotations degrade to all-Any
  under gradual typing — `(define/typed f (-> Fixnum String) (lambda (x) 42))`
  passes statically (the lambda infers to `(-> Any Any)` which
  is `<:` any procedure type) even though the body is a Fixnum.
  Catching this requires inline lambda annotations, which is a
  Phase 1 grammar extension separate from this ADR (deferred to
  the same scope as `(lambda ([p : T]) : R …)` recognition).
- The peel matches on the Symbol literal `apply-contract`. Under
  cs-expand's current flat-namespace semantics this is fine —
  the symbol always resolves to the contract library's binding.
  A future hygienic-rename pass that gives the binding a fresh
  identity per import would need to update the peel rule to
  follow.

### Things that *don't* change
- Untyped code remains entirely unaffected. `extract_annotations`
  on a program without `(define/typed)` produces no new
  annotations, the Checker's peel doesn't fire when no
  ascription is present.
- The contract macro `define/typed` in `lib/contract/typed.scm`
  is unchanged. Existing tests in `phase4_define_typed.rs` and
  `phase4_define_typed_library.rs` continue to pass.
- The interim status doc (`r6rs-extensions-p4-typed-boundaries-status.md`)
  remains the source of truth for the typed-boundaries arc; this
  ADR is referenced as the design call for extension 1.

## Follow-ups

- [x] `extract_annotations` recognizes `(define/typed N T E)` and
      synthesizes the ascription (issue #11 ext-1).
- [x] `Checker::check_set` peels `(apply-contract _ inner _)`
      (issue #11 ext-1).
- [ ] Extensions 2-4 from issue #11 (auto-contracting library
      exports, typed↔typed elision, eta-elision for monomorphic
      contracts).
- [ ] Inline lambda parameter annotations (separate scope; would
      let `(define/typed f (-> Fixnum String) (lambda ([x : Fixnum]) : String 42))`
      fail statically).

## References

- Issue #11 — typed-boundaries 5 natural extensions.
- `docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md` —
  interim status (now updated with the ext-1 entry).
- `docs/research/r6rs_extensions_spec.md` §12 — typed layer.
- Tobin-Hochstadt + Felleisen, "The Design and Implementation of
  Typed Scheme" (POPL 2008) — the bidirectional + gradual basis
  cs-typer follows.
- ADR 0019 — Phase 2 contracts substrate (the runtime side).
