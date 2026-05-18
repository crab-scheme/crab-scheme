# R6RS++ §12 — cs-expand syntax-case extension (#118)

Status: in-progress (Iter A landing first).

## Why this exists

`syntax-rules` covers most macro use cases but stops short of:

* binding-aware identifier comparisons (`bound-identifier=?` / `free-identifier=?`)
* fender expressions that gate a clause on a runtime predicate
* arbitrary template construction from inside arbitrary Scheme (`with-syntax`, `quasisyntax`)
* introspection — `syntax->datum`, `datum->syntax`, source-metadata accessors

Several downstream items want this:

* §9 source-metadata accessors today read from `Pair.source` only; first-class syntax
  objects extend the surface to arbitrary values that flow through macro pattern vars.
* The `match` library (§1) could express richer guards with fender expressions.
* Any Racket-flavoured `syntax-parse`-style extension assumes `syntax-case` exists.

## Foundation already in place

* `Pair.source : Cell<Option<Span>>` and reader-attached spans (`#114`).
* `match_pattern`, `match_dotted_list_pattern`, `collect_pair_chain` in cs-expand
  cover the syntax-rules pattern grammar — syntax-case reuses the same grammar.
* `template-symbol` hygiene marker mechanism on the syntax-rules side.
* `LibraryCache` infra for cross-file expansion (`#116`/`#117`).
* `syntax-source`/`syntax-line`/`syntax-column` builtins exist and degrade gracefully
  for non-Pair inputs.

## Architecture decisions deferred to Iter D/E

* **SyntaxObject representation.** Three options:
  1. New `Value::SyntaxObject` variant (touches every `Value` match — 12+ files).
  2. Wrap as a record type built on Pair (no new variants, but no type safety).
  3. Side-table on `Pair` (extending today's `source` Cell with a `marks` Cell).

  Decision deferred to Iter E (when marks first matter for correctness). Iters
  A–C operate on raw datums + treat identifiers as bare `Value::Symbol`s. This
  is consistent with how `syntax-source` already works.

* **Expand-time Scheme evaluator (for fenders).** Iter D scope. Two sub-options:
  1. Lift the existing walker into a `cs-expand-eval` crate that runs at expand
     time over `CoreExpr` (Chez-style Phase 1 evaluator).
  2. Synthesize a closure for each fender and dispatch through the runtime —
     simpler but couples cs-expand to cs-runtime.

  Likely (1); ADR drafted alongside Iter D.

## Iter breakdown

| Iter | Scope | Status | Ships independently? |
|------|-------|--------|----------------------|
| **A** | identifier?, syntax→datum, datum→syntax, generate-temporaries, bound-identifier=?, free-identifier=? as builtins | **Done** (`08f0e0f`) | Yes — foundational surface, used by downstream Scheme code |
| **B** | `syntax-case` form recognizer + matcher + single-template clause | **Done** | Yes — covers ~80% of real-world syntax-case use without fenders |
| **C** | `with-syntax`, `quasisyntax`/`unsyntax`/`unsyntax-splicing`, ellipsis `…` in patterns/templates | pending | Yes — depends on B |
| **D** | Fender expressions (expand-time Scheme eval) | pending | Largest iter; may slip to Phase 2 |
| **E** | Proper hygiene tracking — mark-aware identifier comparison | pending | Replaces the Iter A symbol-eq stand-ins; needs SyntaxObject decision |

## Iter B — `syntax-case` form

Implemented in `cs-expand` as a desugaring to a `let` + `cond`
chain over the scrutinee. For each clause:

1. The pattern compiles to a boolean test (built up from
   `pair?`/`null?`/`eq?`/`equal?` over `car`/`cdr` chains of the
   key) plus a list of `(pvar, extractor)` bindings.
2. The body is walked for `(syntax T)` forms which are rewritten
   to template-instantiation expressions. Inside the rewrite:
   * a bare symbol that's a pvar → reference the bound let-var
   * a bare non-pvar symbol → `(quote T)`
   * self-quoting atom → emit unchanged
   * pair `(t1 . t2)` → `(cons <T t1> <T t2>)`
3. Clauses chain into a `cond`; if none match, an `error` is
   raised that names `syntax-case` and includes the key.

Standalone `(syntax T)` (outside a syntax-case body) lowers to
`(quote T)` — no pvars exist in that context until Iter C/D
introduce them via `with-syntax` / fenders.

**Deferred to Iter C**: ellipsis (`…`) in patterns or templates;
vector patterns. A 3-element clause `(pat fender tmpl)` is
rejected up front with a pointer to Iter D.

## Iter A — surface builtins

Today's semantics (documented in each builtin's doc comment):

* `(identifier? v)` → `#t` iff `v` is a `Value::Symbol`.
* `(syntax->datum v)` → identity. (Future: strips marks.)
* `(datum->syntax ctxt-id datum)` → returns `datum`; `ctxt-id` ignored.
  (Future: stamps `datum` with `ctxt-id`'s marks.)
* `(generate-temporaries l)` → list of fresh symbols, one per element of `l`.
  Names are `t.<n>` from a thread-local counter.
* `(bound-identifier=? a b)` → `eq?` on symbol names. (Future: name + marks.)
* `(free-identifier=? a b)` → `eq?` on symbol names. (Future: resolves both
  to their binding sites and compares.)

These pin the API. Code can be written today targeting it; the symbol-eq
stand-ins upgrade transparently in Iter E.

## Tests strategy

* Per-iter test file: `cs-runtime/tests/syntax_case_iter_<A|B|...>.rs`.
* Each test asserts ONE behavioural claim so a regression points at exactly
  which surface area broke.
* Iter A tests document the today-vs-future delta (e.g., `bound-identifier=?`
  case where today returns `#t` but should return `#f` once marks land — left
  as `#[ignore = "needs Iter E"]`).
