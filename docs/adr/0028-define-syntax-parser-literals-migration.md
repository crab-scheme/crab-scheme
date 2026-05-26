# ADR 0028: `#:literals` for `define-syntax-parser` + in-tree macro migration

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

R6RS++ Phase 2A introduced `define-syntax-parser` as the blessed
macro-definition surface: a `syntax-rules`-shaped form that adds
`pvar:class` annotations (`id` / `number` / `string` / `expr`) for
argument validation and, via #31, the `~or` / `~optional` / `~once`
backtracking combinators. The in-tree libraries (`match`, contracts,
records, typed defines, the beam runtime's `receive` / `select` /
gen-server / web macros) still defined their macros with raw
`syntax-rules`. Issue #32 (internal task #143) asks to migrate them
for consistency.

Two facts shaped the work:

1. **`define-syntax-parser` could not express a literals list.** It
   always desugared to `(syntax-rules () …)` — `literals: Vec::new()`
   hardcoded on both the desugar and the combinator path. Most of the
   interesting in-tree macros are *keyword-driven*: `match`/`when`,
   `select`/`recv send! after else`, `define-behavior`/`init handle-call
   …`, `with-validated-request`/`#:param #:header #:body`. Migrating
   them without a literals list would silently bind those keywords as
   pattern variables — a behavior change, not a migration.
2. **`:class` validation is incompatible with definition-producing
   macros.** The `:class` desugar wraps the template body in
   `(if (pred …) <body> (error …))`, forcing `<body>` into expression
   position. Every name-taking macro worth validating
   (`define/contract`, `define/typed`, `define-record`) expands to a
   `define` / `define-record-type`, which is then rejected with
   "define not allowed in expression position". So there is no in-tree
   macro that is both a validation candidate (has an identifier
   argument) and an expression body.

## Decision

### Add an optional `#:literals` clause

`(define-syntax-parser name #:literals (lit ...) clause ...)`, the
Racket spelling. The listed identifiers match by name in patterns
instead of binding pattern variables. The parsed literals thread into
both expansion paths: the `(syntax-rules (lit ...) …)` desugar and the
combinator `parser`-macro registration. `#:literals` lexes as an
ordinary keyword identifier (same path as `#:effects`), and the
matcher already supported a literals list — only the
`define-syntax-parser` front door was dropping it.

### Migrate every top-level in-tree macro (behavior-preserving)

All 27 top-level `(define-syntax NAME (syntax-rules (LITS) …))` forms
across the eight libraries become `(define-syntax-parser NAME
[#:literals (LITS)] …)`. **No `:class` annotations were added** — per
the incompatibility above, and because "migrate everything
mechanically" is a consistency pass, not a semantics change. Since the
form desugars to the identical `(syntax-rules (LITS) …)`, matching
behavior is unchanged by construction.

Two things are deliberately *not* migrated:

- The `syntax-rules` macros bound by `let-syntax` inside
  `with-request` (web-contracts.scm) — there is no local
  `define-syntax-parser` form; only top-level definitions migrate.
- The libraries' helper functions, untouched.

## Why not add `:class` validation while we're here

It was the original motivation ("better error messages"), but it does
not fit any in-tree macro: the validation candidates all expand to
definitions, and the `:class` `if`-wrap demotes their bodies to
expression position. Delivering validation would require expand-time
class checking (the Phase 2A.4 pinpointing path is wired for the
combinator `parser` macros, not the `syntax-rules` `:class` desugar) —
a larger change than #32, tracked as a follow-up. The migration still
delivers the consistency half of #32 and lands `#:literals`, which the
combinator path can use for richer diagnostics later.

## Consequences

### Positive
- One macro-definition surface across the stdlib.
- `define-syntax-parser` can now express keyword-driven macros — a
  real capability gap closed, independently useful for future macros.
- The beam libraries' macros are now exercised from disk for the first
  time (see Testing).

### Neutral / no-op
- Runtime macro-matching behavior is unchanged: the desugar emits the
  same `syntax-rules` form the libraries had before.

### Negative / limitations
- No new validation / error-message improvement on the migrated macros
  (see above); `:class` on a definition-bodied macro still errors.
- `web-server.scm` / `web-contracts.scm` still cannot be `eval_str`-ed
  wholesale: their helper functions use the dotted-rest shape
  `(define (f a . rest) …)`, which cs-expand's define-shape parser
  rejects. This is pre-existing and unrelated to #32 (contract.scm
  already documents the same limitation and works around it with
  `(lambda args …)`); it only surfaced here because no prior test
  loaded these libs from disk.

## Testing

- `phase2a5_syntax_parser_literals` (5): `#:literals` discriminates
  clauses, keyword literals (`#:tag`), composition with `:class`,
  no-literals regression, recursive keyword macro.
- `phase2a5_beam_lib_migration` (3): extracts the
  `define-syntax-parser` forms from `channels.scm` /
  `web-server.scm` / `web-contracts.scm` and expands every keyword
  macro (closing the "never loaded from disk" gap).
- Existing suites unchanged and green: `match_basic`,
  `phase2_record_shorthand`, `phase2_provide_contract`,
  `phase4_define_typed{,_library}`, `phase4_auto_contract`,
  `beam_prelude_macros`, `channel_builtins`, `web_builtins`, plus the
  prior `define-syntax-parser` suites.

## Follow-ups
- Expand-time `:class` checking for `syntax-rules`-path parser macros,
  so definition-bodied macros (`define/contract`, `define/typed`,
  `define-record`) can validate their name argument — the "better
  error messages" half of #32.
- A `(define (f a . rest) …)` define-shape in cs-expand would let the
  web libs (and any dotted-rest helper) load from disk.

## References
- Issue #32 / internal task #143.
- `crates/cs-expand/src/lib.rs` — `expand_define_syntax_parser`
  (`#:literals` parsing + threading).
- `lib/{match,record,contract}/…`, `lib/beam/…` — migrated macros.
- ADR 0021 (static `define/typed`), #31 (syntax-parse combinators).
