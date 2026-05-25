# R6RS++ Phase 4 — typed-boundaries arc, EXIT REPORT

> Status: **5 iters substrate + 4 extensions shipped (static
> check at expand time, library-export auto-contracting,
> intra-library contract elision, eta-elision verification);
> 1 extension formally deferred (ProcType.filter lowering).
> Issue #11 closed.**
> Branch: `r6rs-extensions` (substrate), `feat/issue-11-static-define-typed` (extensions).
> Spec: `docs/research/r6rs_extensions_spec.md` (§12).
> Predecessor: Phase 3 (closed in `c0cca4b`).

Captures what shipped in the typed-boundaries subgoal of Phase 4
and what's natural-but-not-yet-built.

## What this report covers

Phase 4 is "advanced research" per the spec's phased rollout
table — it has four broad deliverables (typed integration,
optimizer plugins, sandboxing, custom readers). The other
three deliverables have their own exit reports (optimizer
plugins, sandboxing L1+L2, custom readers). This report
covers the typed-boundaries subgoal, now closed via the 5-
iter substrate + 4 of 5 extensions tracked in issue #11. The
5th extension (ProcType.filter lowering) is formally deferred
in ADR 0025.

## What shipped

### Iter 1 — Rust type → contract translator
Commit: `c63d341`.

`cs_typer::contract_lowering::type_to_contract(ty: &Type) -> String`.
Lowers every variant of `cs_typer::types::Type` to Scheme contract
source. 17 tests covering atomic types, Any/Never, Union,
Procedure_ arrow, Listof/Vectorof, Forall, nested combinations,
and a paren well-formedness sanity check. ProcType.rest was
initially dropped (closed in iter 3); list-of/c and vector-of/c
were emitted speculatively (added in iter 2).

### Iter 2 — list-of/c + vector-of/c + e2e round-trip
Commit: `0f2991a`.

Two variadic-element predicates added to lib/contract/contract.scm
so iter 1's lowering output actually runs. 10 runtime tests for
the combinators + 7 end-to-end tests in
crates/cs-typer/tests/contract_lowering_e2e.rs that round-trip:
Rust Type → contract source → loaded into Runtime with
lib/contract → applied to a procedure → fires on violation.

### Iter 3 — variadic-tail arrow + lowering update
Commit: `aaef6f3`.

`(->* (mandatory-doms ...) rest-pred rng)` form in
lib/contract/contract.scm. apply-contract now dispatches on
whether the contract record carries a rest slot. The cs-typer
lowering emits `->*` whenever ProcType.rest is Some, otherwise
the existing `->` form. 8 runtime tests + 3 lowering tests
covering zero/non-zero mandatories, type errors on mandatory vs
rest args, range enforcement, malformed-construction errors.

### Iter 4 — define/typed (user-facing API)
Commit: `c08e3eb`.

`(define/typed NAME TYPE-ANN EXPR)` in lib/contract/typed.scm.
TYPE-ANN uses cs-typer's annotation syntax (Fixnum, Flonum,
(-> ...), (U ...), (Listof ...), (->* ...)); EXPR is wrapped
with the lowered contract. `__type->contract` is the Scheme port
of iter 1's Rust translator — two implementations of the same
mapping serving different consumers (Rust for tooling source-text
output; Scheme for runtime use without crossing the Rust↔Scheme
tier per call). Bare unknown type-var symbols lower to `any/c`
(polymorphism-erasure rule). 13 runtime tests.

### Iter 5 — composition with libraries + submodules
Commit: `fa0a3d2`.

Validation tests confirming define/typed drops cleanly into the
existing library and submodule machinery without new wiring: a
typed binding inside a library is exported as the wrapped value;
violations fire at call sites; Phase 3B submodules can `guard`
typed-export violations. 4 tests.

### Iter 6 — static check at definition time (issue #11 ext-1) ✅

`(define/typed N T E)` now fails at expand / `crabscheme check`
time when `E` mismatches `T`, in addition to the existing
runtime contract wrap. Closes the largest user-visible gap in
the substrate — the binding flips from "fail on first call" to
"fail at load" (Typed-Racket semantics).

Two surgical changes:

- **`cs_typer::extract::classify_define_typed`** recognizes
  `(define/typed N T E)` at the Datum level (before
  cs-expand sees it) and synthesizes a `TopLevelAnnotation
  { name: N, type_ann: T }`. The original Datum survives in the
  stripped output so the contract macro still runs at the
  usual expansion time and produces the runtime wrap. An
  explicit `(: N T)` ascription written by the user wins on
  conflict.
- **`cs_typer::checker::Checker::peel_apply_contract`** strips
  a `(apply-contract _ inner _)` wrap so `check_set` checks the
  inner expression against the binding's declared type. Without
  the peel, the wrap call's inferred type `Any` defeats the
  gradual check.

The peel is unconditional — any hand-written
`(apply-contract _ inner _)` against an ascribed binding gets
the same static guarantee, not just macro-emitted ones.

Design call in `docs/adr/0021-define-typed-static-check.md`.

15 new tests:
- `extract::tests::define_typed_*` (6) — Datum-level recognition
- `crates/cs-typer/tests/define_typed_static.rs` (9) — full
  parse → extract → expand → check pipeline, covering literal
  conform / mismatch, Ref-to-helper conform / mismatch, and
  three peel-soundness cases (wrong arity, wrong head symbol,
  non-app value).

### Iter 7 — library-export auto-contracting (issue #11 ext-2) ✅

Library exports whose names are ascribed (via `(: NAME T)`
inside the library body, or at the file's top level above the
library) are now auto-wrapped with a runtime contract derived
from the type. Untyped callers hit `&contract-violation` on
misuse — without users having to write `define/typed` for every
exported binding.

New crate module `cs_typer::auto_contract`:
- **`type_to_contract_datum`** mirrors `type_to_contract` but
  builds a Datum tree directly, avoiding stringify-and-reparse
  in the runtime hot path. Procedure types lower to
  `(-> doms... rng)`; variadic-tail types lower to
  `(->* (list doms...) rest-pred rng)` — the `(list ...)` form
  is important: it tells the contract library's `->*` constructor
  to BUILD a runtime list of dom contracts, instead of treating
  the parenthesized doms as a procedure application.
- **`auto_contract_library_exports`** walks the top-level
  Datums looking for `(library …)` forms. For each, it scans
  the body for `(: NAME T)` and `(define/typed NAME T E)`
  ascriptions (extract_annotations doesn't recurse into library
  bodies — library-local scope), strips the bare `(: …)` forms
  (otherwise the expander would later fail on `:` as an unbound
  reference), and injects
  `(set! NAME (apply-contract <contract-datum> NAME (quote NAME)))`
  immediately after the matching define. Top-level ascriptions
  in the [`AnnotationTable`] are used as a fallback when no
  library-internal ascription is found.

Wiring: `cs_runtime::Runtime::eval_data_in_env` now runs
`extract_annotations` + `auto_contract_library_exports` before
calling the expander. Untyped code pays a per-eval walk that
bails on the first non-typed Datum; typer diagnostics are
dropped on the runtime path (contract-only constructors like
`->*` may not parse against the typer grammar, but the macro
expander handles them — `crabscheme check` remains the place
typer feedback surfaces).

Typer grammar extension: `parse_ann::parse_arrow_star` now
recognizes `(->* (mandatory-doms ...) rest-pred rng)` so the
auto-contract pass can wrap variadic library exports without
falling back to "unknown type constructor".

17 new tests:
- `cs_typer::auto_contract::tests::*` (10) — Datum-level
  rewriting: untyped library passthrough, ascribed export wrap,
  unrelated internal ascription stripped without wrap injection,
  union / Listof / ->* lowering, define/typed fallthrough,
  outside-library ascription fallback, non-library top-level
  passthrough.
- `crates/cs-runtime/tests/phase4_auto_contract.rs` (7) —
  Runtime e2e: ascribed library export rejects bad calls with
  `&contract-violation`, untyped library unchanged,
  mixed ascribed + unascribed exports (only ascribed wraps),
  unrelated internal ascription doesn't drive wrap, outside-the-
  library fallback, `(: x T)` strips cleanly even outside any
  library, variadic-tail export wraps.

### Iter 8 — intra-library contract elision (issue #11 ext-3) ✅

The ext-2 pattern wrapped exports with a trailing `set!` —
correct at the export boundary but perf-hostile inside the
library: self-recursion and cross-binding calls all went
through the wrap a second (or n-th) time, even though they were
already inside a contract-checked context.

ext-3 closes the intra-library half of the typed→typed elision
gap. The auto-contract pass now:

1. Renames the original define from `f` → `f$unwrapped`.
2. Rewrites every reference to `f` inside the library body to
   `f$unwrapped`, recursing through Pair / Vector / Null
   structures, skipping `(quote …)` forms (literal data, not
   references) and the new `(define f …)` wrap's binding slot.
3. Inserts `(define f (apply-contract <contract> f$unwrapped (quote f)))`
   immediately after the renamed define.

External callers (via `(import (lib))` of the exported name)
hit the contract wrap. Internal callers — including self-
recursion — bypass it because their references resolved to
`f$unwrapped` at rewrite time.

The pattern shift replaces ext-2's `set!`-based wrap with a
clean `define + rename`:

```text
;; before (ext-2):
(define f LAMBDA)
(set! f (apply-contract <contract> f 'f))

;; after (ext-3):
(define f$unwrapped LAMBDA-with-internal-refs-rewritten)
(define f (apply-contract <contract> f$unwrapped 'f))
```

Design call in `docs/adr/0023-contract-elision-intra-library.md`.
Includes a hygiene caveat: the rewrite is a textual symbol
substitution, so an exotic shape like
`(define use-f (lambda (f) (f 1)))` inside a library exporting
`f` would incorrectly rewrite the lambda's bound `f`. Full
hygienic substitution requires duplicating cs-expand's scope
machinery; deferred as a known limitation.

3 new e2e tests in `phase4_auto_contract.rs`:
- `self_recursion_inside_typed_export_bypasses_contract` —
  external call returns the right result, external bad-arg call
  fires `&contract-violation` (export wrap intact, internal
  self-recursion elides).
- `cross_binding_intra_library_call_bypasses_contract` — `g`
  calling `f` from inside the same library skips f's wrap;
  external call from untyped code to `g` still hits the export
  boundary.
- `quoted_symbol_matching_export_name_is_preserved` — `(quote
  name-of)` inside a library exporting `name-of` is NOT
  rewritten; the symbol literal survives.

Cross-library typed→typed elision (the "full" version of
ext-3) is deferred: it requires the library import/export
machinery to expose both wrapped and unwrapped ports, and
cs-expand to track caller types at every import boundary —
multi-iter scope. Tracked in issue #11 ext-3's open follow-up.

### Iter 9 — eta-elision verification (issue #11 ext-4) ✅

Phase 2B.7 (issue #150) shipped an eta-elision fast path in
`lib/contract/contract.scm`: when every dom spec and the
range spec of an `(-> doms… rng)` contract is a plain
predicate, `apply-contract` returns a specialized wrapper
(`__apply-contract-fast-fixed` / `__apply-contract-fast-variadic`)
that inlines the predicate calls and skips generic dispatch.

ext-4 asks: do typed-derived contracts (from ext-2's
auto-contract lowering) also take the fast path? Answer:
**yes**, automatically. The typed lowering emits contracts via
the same `->` constructor, with every spec a plain predicate
(`integer?`, `string?`, …). `__all-simple-preds?` returns true,
so the fast path triggers. No new implementation code needed;
the optimisation is shared.

Design call in `docs/adr/0024-eta-elision-typed-contracts.md`.
Documents what the optimisation does (and doesn't do):
- Monomorphic atomic types: fast path.
- Structured types (Union, Listof, Vectorof, higher-order
  arrows): slow path, semantically correct.

4 new e2e tests in `phase4_eta_elision.rs`:
- `monomorphic_typed_export_uses_fast_path_correctly` —
  single-arg fast path, tight loop sanity check.
- `monomorphic_multi_arg_typed_contract_works` —
  multi-domain fixed-arity fast path, arity-mismatch raises
  `&contract-violation`.
- `higher_order_typed_contract_falls_through_to_slow_path` —
  `(-> (-> A B) C)` correctly uses the slow path; still
  semantically correct.
- `union_typed_contract_does_not_use_fast_path_but_still_works` —
  union types lower to `(or/c …)` sub-contract → slow path;
  semantically correct.

Stretch goals (deferred): hand-rolled inline predicate
lambdas at lowering time (further perf win); AOT-time
monomorphisation of typed contracts.

### Iter 10 — ProcType.filter deferral (issue #11 ext-5) ⏸

Per ADR 0025: the semantic mismatch between filter types
(compile-time narrowing propositions per Tobin-Hochstadt +
Felleisen's occurrence typing paper) and contracts (runtime
value predicates) is deep enough that no contract-grammar
extension within reach would close it without changing
procedure semantics. Filter dropped at lowering time;
occurrence typing remains a pure compile-time feature.

Issue #11 ext-5 is formally closed via this deferral; the
filter slot in `ProcType` still drives compile-time narrowing
via `narrow_positive` / `narrow_negative` in `cs_typer::check`.

## Test additions

| Suite                                                    | New tests |
|----------------------------------------------------------|-----------|
| crates/cs-typer/tests/contract_lowering.rs (iter 1)      | 20        |
| crates/cs-typer/tests/contract_lowering_e2e.rs (iter 2)  |  7        |
| crates/cs-runtime/tests/phase4_list_vector_of_c.rs (i2)  | 10        |
| crates/cs-runtime/tests/phase4_arrow_star.rs (iter 3)    |  8        |
| crates/cs-runtime/tests/phase4_define_typed.rs (iter 4)  | 13        |
| crates/cs-runtime/tests/phase4_define_typed_library.rs   |  4        |
| crates/cs-typer/src/extract.rs (define_typed_* iter 6)   |  6        |
| crates/cs-typer/tests/define_typed_static.rs (iter 6)    |  9        |
| crates/cs-typer/src/auto_contract.rs (iter 7+8)          | 10        |
| crates/cs-runtime/tests/phase4_auto_contract.rs (iter 7+8) | 10      |
| crates/cs-runtime/tests/phase4_eta_elision.rs (iter 9)   |  4        |
| **Total**                                                | **101**   |

All green; full workspace test sweep is clean.

Final tallies on `feat/issue-11-static-define-typed`:
- cs-typer: 326 passed, 0 failed
- cs-runtime: 1039 passed, 0 failed

## Issue #11 ext summary

The 5 issue-#11 extensions, all resolved:

1. ~~**Static check at definition time**~~ ✅ shipped iter 6
   (see above). `(define/typed)` now fails at expand time.

2. ~~**Library-export auto-contracting from inferred types**~~ ✅
   shipped iter 7 (see above) for the *ascription-driven*
   variant: any `(: NAME T)` declared inside a library body (or
   above it at file scope) drives an auto-wrap on the exported
   binding. The "pure inference" variant (no annotation
   required, type pulled from the Checker's inference) is a
   stretch goal: it depends on the Checker emitting a per-
   binding type table the runtime can consume — tracked but
   unimplemented in this iter.

3. ~~**Contract elision at typed→typed boundaries**~~ ✅
   shipped iter 8 (see above) for the *intra-library* variant:
   rename + body rewrite makes internal callers (self-recursion,
   cross-binding helper calls) skip the contract wrap. The
   *cross-library* variant (typed library A imports typed
   library B; A's calls into B elide the wrap) requires
   library two-port semantics and cs-expand type tracking —
   tracked but unimplemented.

4. ~~**Eta-elision for monomorphic contracts**~~ ✅ shipped
   iter 9 (see "Iter 9" below). Verification + tests
   confirming Phase 2B.7's fast path automatically covers
   typed-derived contracts whose specs are plain predicates
   (no sub-contracts).

5. ~~**Cover ProcType.filter**~~ ⏸ deferred indefinitely
   (ADR 0025). The semantic mismatch between filter types
   (compile-time narrowing propositions) and contracts
   (runtime value predicates) is deep enough that no contract-
   grammar extension within reach would close it without
   adding non-Scheme behavior. Occurrence typing remains a
   pure compile-time feature.

No 1.0 blocker. The follow-up tasks (pure-inference auto-
contracting, cross-library typed→typed elision) are post-1.0
quality-of-life improvements that don't change the surface.

## Other Phase 4 deliverables — all closed

| Deliverable          | Status                                       |
|----------------------|----------------------------------------------|
| Typed boundaries     | ✅ closed (this report)                      |
| Optimizer plugins    | ✅ closed (ADR 0014, exit report)            |
| Sandboxing L1+L2     | ✅ closed (ADR 0015, exit reports)           |
| Custom readers       | ✅ closed (issue #10, p3-exit update)        |

All four Phase 4 deliverables have shipped. Phase 4 is closed.
