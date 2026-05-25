# ADR 0023: Intra-library contract elision via rename

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

ADR 0022 (issue #11 ext-2) shipped library-export
auto-contracting: a `(: NAME T)` ascription on a library export
causes the auto-contract pass to wrap the export with
`(apply-contract <contract> NAME (quote NAME))` so untyped
callers hit `&contract-violation` on misuse.

The wrap implementation used a trailing `set!`:

```text
(define f LAMBDA)
(set! f (apply-contract <contract> f 'f))
```

This works at the export boundary, but it's a perf footgun
*inside* the library:

- Self-recursion: `(define f (lambda (n) (if (= n 0) 1 (* n (f (- n 1))))))`.
  After the set! runs, every recursive `(f …)` goes through the
  contract wrap. For an n-element recursion that's n contract
  applications, n predicate calls per arg, n predicate calls
  per return — all redundant because n was already verified at
  the outermost call.

- Cross-binding intra-library calls: `(define g (lambda (x) (f x)))`.
  When external code calls g, g's call to f hits the wrap a
  second time even though g is already operating inside a
  contract-checked context.

Issue #11 ext-3 calls this out as the next perf gate: typed →
typed calls should be zero-overhead, with contracts firing
only at the typed ↔ untyped boundary.

## Decision

We solve the intra-library case with a Datum-level rename +
body rewrite, performed by the same `auto_contract_library_exports`
pass that ext-2 introduced. Cross-library elision is deferred
to a follow-up (it requires the library import/export
machinery to expose both wrapped and unwrapped ports).

### Pattern shift

Old (ext-2):

```text
(define f LAMBDA)
(set! f (apply-contract <contract> f 'f))
```

New (ext-3):

```text
(define f$unwrapped LAMBDA-with-internal-refs-rewritten)
(define f (apply-contract <contract> f$unwrapped 'f))
```

The library body is then walked and every reference to `f`
(including self-recursion inside LAMBDA's body and cross-
binding calls in other defines) is rewritten to point at
`f$unwrapped`. The `(define f …)` line we just synthesized
keeps `f` (the exported name) as the contract-wrapped binding.

### Rewrite scope

`rewrite_refs(datum, rename_map, kws)`:

- Recurses through Pair / Vector / Null structures.
- Substitutes a Symbol when it matches an entry in
  `rename_map` (e.g. `f → f$unwrapped`).
- Skips `(quote …)` forms entirely (the quoted symbol is
  literal data, not a reference; rewriting `(quote f)` to
  `(quote f$unwrapped)` would change the program's observable
  behavior).
- The binding position of `(define NAME …)` /
  `(define (NAME params…) …)` is NOT rewritten by
  `rewrite_refs` itself — `rewrite_library` finds the binder
  via `find_define_name` *before* calling `rewrite_refs` and
  applies a separate `rename_define_binder` pass that swaps
  ONLY the binder slot.

### Hygiene limitation

The rewrite is a textual symbol substitution. A library that
shadows an exported name inside a local lambda — e.g.
`(define use-f (lambda (f) (f 1)))` inside a library exporting
`f` — would have the lambda's bound `f` incorrectly rewritten
to `f$unwrapped`, which is unbound inside the lambda.

This is an unusual shape for typed library exports (users
typically don't bind a local parameter to the same name as a
library-level export they're trying to use). A full hygienic
substitution would duplicate the expander's scope machinery
and is overkill for the typed-library-export use case.
Documented as a known limitation; users who hit it can either
rename the local binding or refactor the lambda.

### Why intra-library, not full typed↔typed

The "full" version of ext-3 would elide the contract at every
typed→typed call site, including cross-library ones. That
requires:

- Per-library "wrapped" + "unwrapped" port. External imports
  pick the version that matches the caller's typing.
- cs-expand awareness of caller type at every call site (the
  Checker's annotation table needs to flow into cs-expand,
  which currently has no cs-typer dep by design).
- A linkage-time decision: when library A imports library B,
  resolve A's references to B's exports against the right
  port.

That's a significant architectural change. The intra-library
case is the high-frequency one in practice (hot loops, self-
recursion, helper calls); it's the right place to start.
Cross-library elision is deferred to a follow-up tracked in
issue #11 (the bullet remains open).

### Alternatives considered

- **Hygienic rewrite using cs-expand's scope frames**. Would
  correctly handle the shadow-by-local-formal case. Rejected
  for now: cs-expand layering doesn't expose its scope
  machinery to cs-typer, and pulling in a partial dependency
  for one edge case isn't worth it.

- **Runtime sentinel + branch on caller**. Each contract-wrap
  checks a thread-local "I'm a typed caller" flag and bypasses
  if set. Rejected: branches in the hot path, perf delta
  ambiguous, action-at-a-distance.

- **Static analysis at the Checker level: flag every
  typed→typed call site and emit a CoreExpr that bypasses the
  wrap**. The cleanest architecture but requires the Checker
  to emit a richer output (rewritten CoreExpr or a side-table
  consumed by cs-vm). Multi-iter scope; deferred.

## Consequences

### Positive
- Self-recursion in typed library exports skips the contract.
  For a 25-deep `(fib 25)`, that's 100+ predicate calls saved
  per outer invocation — measurable on tight recursive loops.
- Cross-binding intra-library calls skip the contract.
- The export-side wrap is preserved; external callers still
  hit `&contract-violation` for type mismatches.
- The `define + rename` pattern is cleaner than the trailing
  `set!` it replaces — fewer moving parts, no transient
  unwrapped-then-wrapped binding window.

### Negative / risks
- Hygiene caveat (above) is documented but real. Users with
  unusual local-shadowing patterns may see runtime errors
  about `NAME$unwrapped` being unbound.
- The `$unwrapped` suffix is now a reserved naming convention.
  Users who write their own `f$unwrapped` would collide;
  unlikely given the `$` is uncommon in Scheme identifiers.

### Things that *don't* change
- ext-1 (static check at expand time) — unaffected; the
  Checker peels `(apply-contract _ inner _)` regardless of
  whether the wrap is from `define/typed` or ext-2/3.
- ext-2 ascription-driven wrap — the high-level
  "ascribed exports auto-wrap" promise is unchanged; the
  implementation pattern is the only thing that shifted from
  `set!` to `define+rename`.
- Untyped libraries — entirely unaffected.

## Follow-ups

- [x] Rename-based intra-library elision (issue #11 ext-3, this
      ADR).
- [x] 3 new e2e tests in `phase4_auto_contract.rs` proving the
      elision works (self-recursion, cross-binding, quote
      preservation).
- [ ] Cross-library typed→typed elision via two-port imports.
      Requires cs-expand changes to track imported-binding
      type info; multi-iter scope; tracked in issue #11
      ext-3's open follow-up.
- [ ] Hygiene improvement: skip rewriting inside lambdas that
      bind a same-named formal. Bounded scope, low priority
      (uncommon pattern).

## References

- Issue #11 — typed-boundaries 5 natural extensions.
- ADR 0021 — `define/typed` static check (ext-1).
- ADR 0022 — library-export auto-contracting (ext-2).
- `docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md` —
  interim status, iter 8 entry added.
