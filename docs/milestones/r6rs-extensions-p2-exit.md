# R6RS++ Phase 2 — exit report

> Status: **Phase 2 complete — 7 subphases shipped, 3 deferred
> with documented rationale, 1 spillover bug filed.**
> _Post-exit: 2A.3 (syntax-parse combinators) shipped — see the
> Deferred section below; issue #31._
> Branch: `r6rs-extensions`.
> Spec: `docs/research/r6rs_extensions_spec.md` (§2 contracts, §3
> records, §4 conditions, §5 parameters, §6 macro cache, §12
> syntax-parse).
> Predecessor: Phase 1.5 SyntaxObject migration (closed in
> b9b45fb).

Captures what shipped in the Phase 2 sweep, what's deferred, and
why.

## What shipped

### 2A.1 — `define-syntax-parser` + basic syntax classes
Commit: `fd48be1` (#140).

A Racket-style `define-syntax-parser` that wraps `define-syntax` /
`syntax-rules` with built-in class annotations:
`pvar:id`, `pvar:expr`, `pvar:number`, `pvar:string`. The expander
desugars each class to an `if` that checks the predicate and
raises a "expected <class>" error on mismatch. 12 tests in
`phase2_syntax_parser.rs`.

### 2A.2 — `define-syntax-class` (user-defined)
Commit: `9f11312` (#141).

`(define-syntax-class name predicate-symbol)` registers a class
whose check calls the named predicate. User classes compose with
built-in classes in the same parser. 9 tests in
`phase2_syntax_class.rs`. The expander tracks classes in
`syntax_classes: HashMap<Symbol, Symbol>` (class → predicate).

### 2B.2 — contracts library + `(-> dom rng)`
Commit: `b4a0734` (#145).

Initial contracts substrate in `lib/contract/contract.scm`:
contracts as record values `(vector '__contract__ doms rng)`, a
single-domain `(-> dom rng)` constructor, `apply-contract`
wrapper, and blame-carrying violations that fire
`&contract` (registered in 2D). 10 tests in `phase2_contracts.rs`.

### 2B.4 — per-arg + higher-order contracts
Commit: `de15829`.

`(-> dom1 dom2 ... rng)` enforces fixed arity with per-position
checks. A domain or range spec may itself be a contract; in that
case the matching arg/result must be a procedure and gets wrapped
via `apply-contract` (blame transfers to the inner wrapper). 12
tests in `phase2_contracts_higher_order.rs`.

Side effect: filed #147 — `(raise ...)` inside the callback of
`map` doesn't propagate to outer `guard`. Workaround in the
contract library uses explicit `let-loop` instead of `map`.

### 2B.5 — contract combinators
Commit: `3c70548`.

`or/c`, `and/c`, `list/c`, `any/c`, `none/c` — predicate-builders
that drop into the existing `(-> dom rng)` form without grammar
changes (e.g. `(-> (or/c number? string?) (and/c number? pos?))`).
Empty `(or/c)` rejects all; empty `(and/c)` accepts all; empty
`(list/c)` matches `'()` only. 14 tests in
`phase2_contracts_combinators.rs`.

### 2B.6 — `define/contract` + `provide/contract`
Commit: `8c3d961`.

- `(define/contract name contract expr)` — one-step contracted
  define.
- `(provide/contract (name1 c1) ...)` — rebind already-defined
  names to wrapped versions.

Because the bound name IS the wrapped procedure, an enclosing
library's `(export name ...)` re-exports the contract-protected
closure transparently — no library-boundary plumbing needed. 9
tests in `phase2_provide_contract.rs`.

Required two cs-expand fixes that unlock macros-yielding-defines
at top level generally:
1. `expand_top` now expands user macros and recurses through
   `expand_top`, so `(define ...)` produced by a macro is
   recognized rather than erroring as "define in expression
   position".
2. Top-level `(begin ...)` splices its children through
   `expand_top` (R7RS top-level begin semantics), so a multi-
   define expansion like `provide/contract`'s `(begin (define
   ...) ...)` classifies each child as a top-level form.

### 2C — `define-record` / `define-record-mutable` shorthand
Commit: `5d44168`.

Two new macros in `lib/record/record.scm` that wrap
`define-record-type` for the common "auto-named accessors"
case. The mutable form needed an expander-level addition: the
R6RS field-decl parser now accepts a two-element `(mutable
FIELD)` clause that auto-generates `NAME-FIELD` accessor and
`set-NAME-FIELD!` mutator. Existing four-element `(mutable FIELD
ACCESSOR MUTATOR)` form continues to work. 9 tests in
`phase2_record_shorthand.rs`.

### 2D — `&contract` / `&type` / `&module` condition types
Commit: `4a5ad4b` (#137).

Three new R6RS condition subtypes registered as children of
`&error`. Constructors `make-contract-violation`,
`make-type-error`, `make-module-error` plus accessors. Used by
the contracts library (2B.2+) for raising blame-carrying
violations. 12 tests in `phase2_condition_types.rs`.

### 2E — parameters audit + `parameter?`
Commit: `86ecc32` (#138).

Tightened `make-parameter` to type-check the optional converter
arg (was previously silently accepting non-procedures). Added
`parameter?` predicate. 15 tests in `phase2_parameters_audit.rs`.

### 2F — library cache dep-closure invalidation
Commit: `bc20af0` (#139).

Library cache keys changed from
`LibraryCacheKey = (Vec<Symbol>, u64)` to `(Vec<String>, u64)` so
cached entries survive across `SymbolTable` resets (Symbol IDs
are per-session). Each cache entry now stores its deps as
`Vec<(Vec<String>, u64)>`; on lookup, the validator re-interns
each dep name and checks the hash to detect stale entries. Tests
in `phase2_cache_dep_closure.rs`.

## Deferred

### 2A.3 — syntax-parse combinators (`~or`, `~optional`, `~once`) — ✅ SHIPPED (issue #31)
**Original deferral rationale:** combinators need multi-clause
expansion semantics and extra pattern-matching machinery that
doesn't fit on top of the current syntax-rules infrastructure. Best
layered after we have a fuller pattern compiler. Task: #142.

**Resolution:** shipped that fuller matcher — a small backtracking
pattern matcher in `cs-expand/src/syntax_parse.rs` that produces the
same `MatchBinding` map as the syntax-rules engine (so `instantiate`
is reused). Only combinator-using parsers route through it (the
`Macro::parser` flag); plain syntax-rules macros are untouched. Full
Racket-style semantics: ordered `~or` alternation with backtracking,
`~optional` with `#:defaults`, and ellipsis-head `~once`/`~optional`
with cardinality accumulated across the repetition (order-free
keyword parsing). Limitations (proper-list clauses, depth-1 nesting,
conflicting per-`~or`-alternative `:class` annotations) documented in
the module header. Tests: `phase2a3_syntax_parse_combinators.rs` (15).

### 2A.4 — expand-time error pinpoint
**Why:** needs procedural macros so the parser can inspect
intermediate parses and emit precise source-spanned errors.
Current syntax-rules driver only produces a single "no rule
matches" error. Task: #144.

### 2A.5 — migrate in-tree macros to `define-syntax-parser`
**Why:** no good migration candidate in the current tree —
existing macros are either already simple enough that
`syntax-rules` is fine, or use `syntax-case` machinery that
hasn't been wrapped by define-syntax-parser yet. Task: #143.

### 2B.7 — eta-elision for monomorphic contracts
**Why:** a perf optimization, not a correctness or surface-area
extension. Should land after a wider perf pass and benchmark
baseline; deferred to post-1.0. Task: #150.

## Test additions

| Suite                                  | New tests |
|----------------------------------------|-----------|
| phase2_condition_types.rs (2D)         | 12        |
| phase2_parameters_audit.rs (2E)        | 15        |
| phase2_syntax_parser.rs (2A.1)         | 12        |
| phase2_syntax_class.rs (2A.2)          |  9        |
| phase2_contracts.rs (2B.2)             | 10        |
| phase2_contracts_higher_order.rs (2B.4)| 12        |
| phase2_contracts_combinators.rs (2B.5) | 14        |
| phase2_provide_contract.rs (2B.6)      |  9        |
| phase2_record_shorthand.rs (2C)        |  9        |
| **Total Phase 2**                      | **102**   |

All green; full workspace test sweep is clean.

## Spillover bugs

| ID    | Title                                                          |
|-------|----------------------------------------------------------------|
| #147  | cs-runtime: raise inside callback doesn't propagate through map|
| #150  | Phase 2B.7 (perf) deferred to post-1.0                         |

## What's next

Phase 3:
- 3A: continuation marks
- 3B: submodules
- 3C: `#lang` directive

None started yet; pre-1.0 only if scope demands. The post-1.0
plan continues with M11 (AOT long-tail) and the deferred R6RS++
items.
