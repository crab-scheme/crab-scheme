# R6RS++ Phase 4 — typed-boundaries arc, interim status

> Status: **5 iters substrate + 2 extensions shipped (static check
> at expand time, library-export auto-contracting at runtime);
> 2 extensions tracked in issue #11.**
> Branch: `r6rs-extensions`.
> Spec: `docs/research/r6rs_extensions_spec.md` (§12).
> Predecessor: Phase 3 (closed in `c0cca4b`).

Captures what shipped in the typed-boundaries subgoal of Phase 4
and what's natural-but-not-yet-built.

## Why interim, not exit

Phase 4 is "advanced research" per the spec's phased rollout
table — it has four broad deliverables (typed integration,
optimizer plugins, sandboxing, custom readers) and the spec is
deliberately sparse on three of them. The typed-boundaries arc is
the one with concrete prior art (cs-typer + Phase 2 contracts);
the others are open design questions. This report covers ONLY
the typed-boundaries subgoal so the writeup matches what's
actually shipped.

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

## Test additions

| Suite                                                  | New tests |
|--------------------------------------------------------|-----------|
| crates/cs-typer/tests/contract_lowering.rs             | 20        |
| crates/cs-typer/tests/contract_lowering_e2e.rs         |  7        |
| crates/cs-runtime/tests/phase4_list_vector_of_c.rs     | 10        |
| crates/cs-runtime/tests/phase4_arrow_star.rs           |  8        |
| crates/cs-runtime/tests/phase4_define_typed.rs         | 13        |
| crates/cs-runtime/tests/phase4_define_typed_library.rs |  4        |
| crates/cs-typer/src/extract.rs (define_typed_* iter 6) |  6        |
| crates/cs-typer/tests/define_typed_static.rs (iter 6)  |  9        |
| **Total**                                              | **77**    |

All green; full workspace test sweep is clean.

## What's natural but not yet built

The typed-boundaries arc could keep iterating along several axes
(tracked in issue #11):

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

3. **Contract elision at typed→typed boundaries**: when both sides
   of a call are statically type-checked, the contract is
   redundant; drop it for zero-overhead. Requires call-site
   typing information at link / load time. Issue #11 ext-3.

4. **Eta-elision for monomorphic contracts** (Phase 2B.7 task
   #150 already on the queue): the same optimization applied to
   typed-derived contracts. Issue #11 ext-4.

5. **Cover ProcType.filter** (predicate-narrowing types): the
   iter-1 lowering currently ignores `filter` because contracts
   don't express occurrence typing. Could lower to a contract +
   side-effect that narrows the runtime type for downstream
   readers, but the semantic mismatch is deep. Issue #11 ext-5
   (deferred indefinitely per the issue body).

None of these block 1.0. Each is a single iter on its own once
the design question is answered.

## Other Phase 4 deliverables (still untouched)

| Deliverable          | Status                                       |
|----------------------|----------------------------------------------|
| Optimizer plugins    | no design                                    |
| Sandboxing           | no design                                    |
| Custom readers       | tracked as #156 (Phase 3C.full)              |

Each requires a design ADR before implementation. None are 1.0-
blocking. Pick up when specific use cases motivate.
