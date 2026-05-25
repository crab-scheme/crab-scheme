# ADR 0024: Eta-elision for typed-derived monomorphic contracts

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

Phase 2B.7 (issue #150 on the queue) shipped an eta-elision
fast path in `lib/contract/contract.scm`: when every dom spec
and the range spec of an `(-> doms… rng)` contract is a plain
predicate (not a sub-contract), `apply-contract` returns a
specialized wrapper (`__apply-contract-fast-fixed` or
`__apply-contract-fast-variadic`) that inlines the predicate
calls and skips the generic `__apply-domain` / `__apply-range`
dispatch.

Detection happens once at construction time
(`__all-simple-preds?` walks the doms + rng); the per-call hot
path then jumps straight into the specialized lambda.

Issue #11 ext-4 asks whether this same optimization fires for
contracts produced by the ext-2 auto-contract pass — typed
library exports whose type lowers to predicates.

## Decision

No new code. The typed-derived contracts already take the fast
path automatically.

### Why it works

The ext-2 pass lowers types via `type_to_contract_datum`:

- `Type::Fixnum` → `integer?`
- `Type::Flonum` → `real?`
- `Type::Boolean` → `boolean?`
- `Type::String` → `string?`
- `Type::Union(ts)` → `(or/c <lowered>...)`
- `Type::Procedure_(p)` → `(-> doms… rng)` or `(->* …)`
- … etc.

The emitted contract is consumed by `apply-contract` (or the
runtime in the case of ext-2's auto-wrap). `apply-contract`
calls `__all-simple-preds?` on the contract's doms and rng:

- For `(-> integer? integer?)` — every spec is a plain
  `procedure?` and NOT a `contract?` → returns `#t` → fast path.
- For `(-> (or/c integer? real?) integer?)` — `(or/c …)` is a
  sub-contract → returns `#f` → slow path.
- For `(-> (-> integer? integer?) integer?)` — the inner
  arrow is a sub-contract → slow path.

So **monomorphic** typed contracts (atomic types throughout,
no unions, no Listof/Vectorof, no higher-order arrows) hit the
fast path; **structured** typed contracts (unions, lists,
vectors, higher-order) fall through to the slow path. Both
yield correct semantics; the perf delta only matters for hot-
path code.

### What this doesn't try to do

- **Inline the predicates at lowering time**. We could in
  principle emit a hand-rolled lambda
  `(lambda (x) (if (integer? x) (let ((r (F x))) (if (integer? r) r raise…)) raise…))`
  instead of `(apply-contract (-> integer? integer?) F 'F)`,
  saving one function call (`apply-contract` itself). The
  contract library's fast path is already a single
  closure-call, so the additional win is small and offset by
  the duplicate-code cost. Deferred.

- **AOT-time monomorphisation**. cs-aot could specialize a
  typed contract to inline machine-code predicate checks.
  Requires the typer-to-AOT hint flow that already drives
  param-type hints (Phase 5++). Deferred to AOT roadmap.

### Alternatives considered

- **Implement a parallel optimisation in the typed lowering**.
  Rejected: duplicates the contract library's existing
  optimization, risks divergence.

- **Mark typed contracts with a hint flag** so the fast-path
  check skips `__all-simple-preds?`. Rejected: micro-
  optimisation, the check is already O(arity) and runs once
  per contract construction.

## Consequences

### Positive
- Zero implementation cost. The optimisation already exists.
- Typed library exports with simple types
  (`(-> Fixnum Fixnum)`, `(-> String Boolean)`, etc.) — the
  most common shape — get the fast path automatically.

### Negative / risks
- None directly. The semantic correctness of the slow path is
  preserved for typed contracts that don't qualify (unions,
  Listof, higher-order).

### Things that *don't* change
- The contract library's `__apply-contract-fast-*` wrappers
  remain unchanged.
- Hand-written `(define/contract … (apply-contract …))` calls
  in user code continue to benefit from the same fast path
  (shared mechanism).

## Follow-ups

- [x] 4 e2e tests in `phase4_eta_elision.rs` exercising the
      fast-path and slow-path lowering for representative
      type shapes (monomorphic atomic, multi-arg monomorphic,
      higher-order, union).
- [ ] Hand-rolled inline predicate lambdas at lowering time
      (further perf win, deferred).
- [ ] AOT-time monomorphisation of typed contracts (separate
      roadmap).

## References

- Issue #11 — typed-boundaries 5 natural extensions.
- Issue #150 — Phase 2B.7 eta-elision (the underlying
  optimisation in `lib/contract/contract.scm`).
- ADR 0022 / 0023 — ext-2 / ext-3 (the lowering pipeline ext-4
  rides on).
- `lib/contract/contract.scm` lines 139-200 — the eta-elision
  fast path implementation.
