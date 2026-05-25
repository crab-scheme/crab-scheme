# ADR 0025: ProcType.filter lowering — deferred indefinitely

> Status: Accepted (deferral)
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

`cs_typer::types::ProcType` carries an optional `filter`
field — a predicate-narrowing proposition Typed Racket uses
for occurrence typing. When the typer infers
`(: number? (-> Any Boolean : Fixnum))`, the `filter` slot
records "if this procedure returns #t, narrow the argument to
Fixnum in the then-branch; if #f, narrow to (NOT Fixnum) in
the else-branch".

This is the type-system half of occurrence typing — what gives
`(if (number? x) (+ x 1) "not a number")` its Fixnum-narrowed
arm.

Phase 4 §12's typed-boundaries arc shipped a 5-iter substrate
that lowered `ProcType` to a runtime contract via
`cs_typer::contract_lowering::type_to_contract`. The lowering
honored `params`, `rest`, and `return_type` — but **dropped
`filter`** with a comment:

```rust
// Procedure_({p, r, ...}) → (-> ...lowered_p lowered_r)
```

Issue #11 ext-5 asks: can we lower the filter slot to
something runtime-checkable, so the contract enforces the
predicate-narrowing semantics?

## Decision

**Defer indefinitely.**

The semantic mismatch between filter types (compile-time
narrowing propositions) and contracts (runtime value
predicates) is deep enough that no contract-grammar extension
within reach would close it without adding non-Scheme
behavior.

### The mismatch in detail

A `filter: Fixnum` on `(-> Any Boolean)` says:
- Compile-time: "in the then-branch of any `(if (FOO x) … …)`,
  the typer narrows x's type to Fixnum".
- Runtime: the procedure returns `#t` or `#f` based on whether
  its arg passes the Fixnum check.

A naive contract lowering would emit something like:

```scheme
(-> any/c (lambda (b)
           (if b
               (begin (assert (Fixnum? arg)) #t)
               #t)))
```

But this:
1. Requires the contract to retain a reference to the arg
   across the call (the contract library's current `->` doesn't).
2. Asserts the narrowing at *every* call site, including ones
   that don't branch on the result.
3. Doesn't actually accomplish narrowing — the post-call assert
   is a check, not a type refinement; cs-typer's static
   reasoning isn't fed by it.

Each of these requires a grammar extension to either:

- **The contract library**: add a "filter contract" form that
  ties an arg's runtime check to a returned-boolean
  condition. Implementable but adds runtime cost for a
  feature that's primarily a static-typing aid.
- **The typed-contract lowering**: emit BOTH a regular
  contract (for runtime checks) AND a side-channel filter
  annotation that cs-typer keeps for downstream narrowing.
  But the runtime contract doesn't know about static
  typing — it'd be a no-op channel.
- **Cross-tier compilation**: make occurrence typing a
  compile-time-only feature, with no runtime contract
  emission for filters. This is what we already do
  implicitly by dropping filter.

The simplest and most honest decision is the last: drop the
filter slot at lowering time, document the limitation, and
keep occurrence typing as a pure compile-time feature.

### Alternatives considered

- **Emit a filter contract that asserts at every call**.
  Rejected: changes the procedure's runtime semantics
  (assertions fire even when caller didn't branch); high
  runtime cost; doesn't actually feed back into static
  narrowing.

- **Extend the contract grammar with a `(filter-of T)` form**
  that captures the narrowing intent. Rejected as scope
  creep — designing a runtime form that retains arg references
  across return is a deep change to the contract library.

- **Lower filter to `any/c`** (effectively what we do today).
  ✅ This is the accepted approach.

## Consequences

### Positive
- No engineering work needed. The lowering already does the
  right thing.
- Occurrence typing remains a pure compile-time feature with
  clear semantics. Users who write
  `(: number? (-> Any Boolean : Fixnum))` get static narrowing
  via `cs_typer::narrow_positive` /
  `cs_typer::narrow_negative`; the runtime contract on the
  exported binding (if any) checks `(-> any/c boolean?)` and
  ignores the filter.

### Negative / risks
- A user who exports a custom predicate from a typed library
  and expects the importer's narrowing to "just work" at
  runtime will be disappointed. The static narrowing only
  fires inside the typed code that calls the predicate.
- The filter slot in `ProcType` exists but contributes
  nothing to the lowering. It's a static-only annotation.

### Things that *don't* change
- cs-typer's `narrow_positive` / `narrow_negative` continue
  to consume the filter slot for compile-time narrowing.
- The runtime contract on a typed predicate still verifies
  the arg type AND the boolean return — just not the
  narrowing implication.

## Follow-ups

- Issue #11 ext-5 is now formally deferred. Reopen if a
  concrete use case motivates the work (e.g. cs-typer
  generates filter slots that would benefit from runtime
  enforcement in a specific application domain).

## References

- Issue #11 — typed-boundaries 5 natural extensions.
- Tobin-Hochstadt + Felleisen, "Logical Types for Untyped
  Languages" (ICFP 2010) — the original occurrence-typing
  paper that defines the filter type.
- `crates/cs-typer/src/check.rs` `narrow_positive` /
  `narrow_negative` — the compile-time consumers of the
  filter slot.
- `crates/cs-typer/src/contract_lowering.rs` line 92-96 —
  the existing comment acknowledging the drop.
