# ADR 0022: Library-export auto-contracting from ascriptions

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

R6RS++ Phase 4 §12 ("typed boundaries") gave us
`(define/typed NAME TYPE EXPR)` as a one-liner for binding a
name to a value with both static (cs-typer ascription) and
dynamic (contract wrap) guarantees. For library code, this
works but is verbose: every exported binding needs the
`define/typed` macro to opt into the contract wrap.

Issue #11 ext-2 framed the next iteration: a library's exports
should auto-contract from the ascription alone — users write
`(: NAME T)` + `(define NAME …)` (or even just `(define NAME …)`
with a cs-typer-inferred type) and the wrap happens for free at
the library boundary.

ADR 0021 (ext-1) plumbed `(define/typed)` recognition into
`extract_annotations` and added a `(apply-contract _ inner _)`
peel to `Checker::check_set`. That work made cs-typer aware of
typed defines at the Datum level. This ADR continues that arc
by turning library bodies into a target for the auto-wrap pass.

## Decision

We add a new Datum-level transform
`cs_typer::auto_contract_library_exports` and wire it into both
`crabscheme check` and `cs_runtime::Runtime::eval_data_in_env`
(via a new cs-runtime → cs-typer dependency).

### Pass design

1. Top-level Datum walk; non-`(library ...)` forms pass
   through unchanged.

2. For each library, scan the body for `(: NAME T)` and
   `(define/typed NAME T E)` forms. The scan parses each T via
   `parse_datum_as_type` against an empty alias context
   (library-internal aliases are out of scope for ext-2;
   `define-type` inside a library would be a separate
   extension).

3. Outside-library ascriptions (the `AnnotationTable` produced
   by `extract_annotations`) serve as fallback for exports
   without library-local ascriptions, so users who declare
   `(: f T) (library (foo) (export f) … (define (f x) …))` at
   the top level still get the wrap.

4. Rewrite the library body:
   - Drop bare `(: NAME T)` forms (the expander would
     otherwise try to evaluate `:` at runtime as an unbound
     reference).
   - Keep every other form unchanged.
   - For each exported `NAME` that has a parsable type `T`,
     inject `(set! NAME (apply-contract <contract-datum> NAME (quote NAME)))`
     immediately after the matching `(define NAME …)` form.
     `<contract-datum>` is built by `type_to_contract_datum`,
     a Datum-tree analog of `type_to_contract`.

5. If the scan finds no ascribed export but the body still
   contains stripped `(: …)` forms (e.g. an ascription on a
   non-exported helper), the rewrite still runs to perform the
   strip — otherwise runtime expansion fails on `:`.

### Variadic-tail wraps need `(list …)`

The contract library's `->*` constructor expects a runtime LIST
of mandatory-dom contracts, not a literal Scheme list. Without
the wrap-time `(list …)` form, the lowered
`(->* (integer?) integer? integer?)` would parse as
"call `->*` with `(integer?)` evaluated as a 0-arg call to
`integer?`" → arity error. We emit `(->* (list integer?) …)`
so the Scheme evaluator builds the dom list at runtime.

### Typer-grammar extension

`parse_ann::parse_arrow_star` now recognizes
`(->* (mandatory-doms …) rest-pred rng)`. The existing
`Type::Procedure_` ProcType variant already carries a `rest`
slot, so the parser maps `->*` to the same internal Type as
`(-> doms … rest-pred ... ret)`. Required so the auto-contract
pass can wrap variadic library exports.

### Runtime integration

`Runtime::eval_data_in_env` now runs
`extract_annotations` + `auto_contract_library_exports` before
calling the expander. Diagnostics from the typer pre-passes are
DROPPED on the runtime path — the macro expander handles
contract-only constructors (like the existing `(-> A ... R)`
rest-arrow form before parse_arrow_star landed), and
`crabscheme check` remains the place typer feedback surfaces.
Without this drop, a perfectly valid `define/typed` program
whose type uses a contract-only constructor would fail at eval
purely because the typer can't represent that constructor.

cs-runtime → cs-typer is a new crate dependency. cs-typer's
own deps (cs-core, cs-diag, cs-ir, cs-parse, cs-rir) are all
already in cs-runtime, so the new edge adds no transitive
crates. Untyped code pays a per-eval walk that bails on the
first non-typed Datum.

### Alternatives considered

- **Per-export macro `(export/typed NAME T)`**. Users would
  write a different syntax for typed exports. Rejected: the
  whole point of ext-2 is to remove that boilerplate — using a
  separate form would be a parallel `define/typed`-style burden.

- **Wrap at the importer side rather than the exporter side**.
  When another library imports the binding, lookup its type
  and inject the wrap. Rejected: the contract should fire on
  the EXPORTING library's runtime boundary, not at every
  import site, to keep the cost amortized.

- **Make extract_annotations recurse into library bodies**.
  Then library-internal ascriptions land in the global
  `AnnotationTable` and the existing top-level wrap path
  applies. Rejected: ascription scope semantics — a library-
  internal `(: f T)` should scope the type to the library's
  `f`, not any same-named top-level `f`. Recursive extraction
  would conflate the two scopes.

- **Pure-inference auto-contract** (no ascription required).
  Rejected for ext-2: depends on cs-typer's Checker exposing a
  per-binding inferred-type table the runtime can consume —
  tracked but deferred. The ascription-driven variant in this
  iter covers the most useful case.

## Consequences

### Positive
- Library exports become automatically typed-safe by writing a
  single ascription. The verbose `define/typed` ceremony stays
  available but isn't needed for the export case.
- `(: …)` is now a valid form at runtime, not just under
  `crabscheme check` — it just doesn't *do* anything for
  non-library scopes (correctly, since static-only typed
  defines outside libraries are still a documented option).
- `cs_typer::parse_arrow_star` lets the typer represent the
  full variadic-tail surface, closing a gap noted in
  `phase4_arrow_star.rs` tests.

### Negative / risks
- The injected wrap requires the user library to
  `(import (crab contract))` (or whatever path exposes
  `apply-contract`, `->`, `or/c`, etc.). A library that
  ascribes its exports but doesn't import the contract
  combinators will fail at load with `unbound apply-contract`.
  We could auto-inject the import; rejected for now because it
  silently grows every typed library's dependency surface.
- cs-runtime → cs-typer is a new crate edge. cs-typer is
  small (~8K LOC across 16 files), no transitive crates, and
  the per-eval cost on untyped code is a single walk that
  bails on the first non-`(:)`/non-`(library)` Datum. Worth
  the user-facing payoff.
- Diagnostics emitted by `extract_annotations` are silenced
  on the runtime path. Users who want typer feedback run
  `crabscheme check`; runtime is permissive by design (typer
  is optional).

### Things that *don't* change
- `define/typed` still emits its macro-level contract wrap. Its
  combination with auto-contract on an export DOES double-wrap
  (the macro's wrap inside the auto-wrap), which is harmless
  because `apply-contract` is idempotent on values that pass.
- Untyped libraries are entirely unaffected.
- The runtime contract wrap is exactly the same shape
  `define/typed` already produces; no new contract semantics.

## Follow-ups

- [x] `cs_typer::auto_contract::auto_contract_library_exports`
      function + 10 unit tests (issue #11 ext-2).
- [x] `cs_typer::auto_contract::type_to_contract_datum` Datum-
      tree contract builder.
- [x] `cs_typer::parse_arrow_star` grammar extension for `->*`.
- [x] cs-runtime → cs-typer plumbing in `eval_data_in_env`.
- [x] 7 e2e tests in `phase4_auto_contract.rs`.
- [ ] Pure-inference variant: pull binding types from the
      Checker's inference instead of requiring an ascription.
      Depends on the Checker exposing a per-binding type
      table.
- [ ] Library-internal `define-type` aliases. Currently the
      scan parses against an empty alias context; library-
      local aliases would need a small extension to
      `scan_local_ascriptions`.

## References

- Issue #11 — typed-boundaries 5 natural extensions.
- ADR 0021 — `define/typed` static type checking at expand
  time (the layer this ADR builds on).
- `docs/milestones/r6rs-extensions-p4-typed-boundaries-status.md` —
  interim status (now reflecting iter 7).
- `docs/research/r6rs_extensions_spec.md` §12 — typed layer.
