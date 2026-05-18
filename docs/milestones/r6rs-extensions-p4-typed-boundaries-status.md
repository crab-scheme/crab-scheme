# R6RS++ Phase 4 — typed-boundaries arc, interim status

> Status: **5 iters shipped; substrate complete for user-facing
> typed defines. Phase 4 as a whole remains open research per the
> spec — this report covers only the typed-layer subgoal (§12).**
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

## Test additions

| Suite                                                  | New tests |
|--------------------------------------------------------|-----------|
| crates/cs-typer/tests/contract_lowering.rs             | 20        |
| crates/cs-typer/tests/contract_lowering_e2e.rs         |  7        |
| crates/cs-runtime/tests/phase4_list_vector_of_c.rs     | 10        |
| crates/cs-runtime/tests/phase4_arrow_star.rs           |  8        |
| crates/cs-runtime/tests/phase4_define_typed.rs         | 13        |
| crates/cs-runtime/tests/phase4_define_typed_library.rs |  4        |
| **Total**                                              | **62**    |

All green; full workspace test sweep is clean.

## What's natural but not yet built

The typed-boundaries arc could keep iterating along several axes:

1. **Static check at definition time**: today `define/typed` only
   wraps EXPR with a contract; cs-typer could ALSO check EXPR
   against the annotated type at expansion time, catching errors
   that the dynamic wrap would otherwise only catch on first
   misuse. Bigger scope — requires cs-expand→cs-typer plumbing.

2. **Library-export auto-contracting from inferred types**: if
   cs-typer infers a type for an exported binding (no annotation
   required), the library expander would auto-wrap the export with
   the corresponding contract. Removes the need for explicit
   `(define/typed ...)` for fully-inferable code.

3. **Contract elision at typed→typed boundaries**: when both sides
   of a call are statically type-checked, the contract is
   redundant; drop it for zero-overhead. Requires call-site
   typing information at link / load time.

4. **Eta-elision for monomorphic contracts** (Phase 2B.7 task
   #150 already on the queue): the same optimization applied to
   typed-derived contracts.

5. **Cover ProcType.filter** (predicate-narrowing types): the
   iter-1 lowering currently ignores `filter` because contracts
   don't express occurrence typing. Could lower to a contract +
   side-effect that narrows the runtime type for downstream
   readers, but the semantic mismatch is deep.

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
