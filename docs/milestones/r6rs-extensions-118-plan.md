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
| **C** | `with-syntax`, `quasisyntax`/`unsyntax`/`unsyntax-splicing`; pvar stack on Expander | **Done** | Yes — depends on B |
| **C2** | Minimal ellipsis `…` (single-pvar form only) | **Done** | Yes — covers `(prefix… pvar …)` / `(prefix… pvar …)` splice |
| **C3** | Compound sub-patterns under `…`, multi-pvar zip-map templates | **Done** | Yes — unblocks `let`-style macros |
| **C4** | Literals + wildcards + dotted tail inside compound sub | **Done** | Yes — unblocks `cond`/`case-lambda`/`=>`-bearing macros |
| **C5** | Nested compound sub `((a (b c)) …)` (recursive sub-pattern walker) | **Done** | Yes — handles arbitrarily deep nesting under one ellipsis level |
| **C6** | Minimal nested ellipsis `((p …) …)` with bare-pvar inner | **Done** | Yes — covers the canonical "list of lists" shape |
| **C7** | Nested ellipsis with compound/prefixed inner: `((kw p …) …)` / `(((a b) …) …)` | pending | Last ellipsis grammar gap |
| **D** | Fender expressions (expand-time Scheme eval) | pending | Largest iter; may slip to Phase 2 |
| **E** | Proper hygiene tracking — mark-aware identifier comparison | pending | Replaces the Iter A symbol-eq stand-ins; needs SyntaxObject decision |

## Iter C6 — Minimal nested ellipsis

Pattern `((p …) …)` where `p` is a single bare pvar binds `p` at
**depth 2** — a list-of-lists. Because the inner `(p …)`
trivially binds `p` to the entire inner element, the outer
depth-2 `p` value is just `walking-key` itself (with a structural
check that every outer element is a proper list).

Template machinery: each ellipsis layer drops one depth level
for referenced pvars. `(syntax ((p …) …))` with `p` at depth 2:
* Outer `(… …)` rebinds `p` to depth 1 in the inner template.
* Inner `(p …)` with `p` at depth 1 splices the inner list
  (already implemented by Iter C2).

The fix that made this work cleanly: in `compile_syntax_template`'s
zip-map case, drop `(depth, depth - 1)` for matched pvars rather
than reset to 0 — that lets deeper-depth pvars survive one
ellipsis layer.

**What lands:**
* `((p …) …)` pattern + `(syntax p)` / `(syntax ((p …) …))` /
  `(syntax ((wrap p …) …))` templates
* Empty-outer / empty-inner-lists handled

**Deferred to Iter C7 (last grammar piece):**
* Compound inner: `(((a b) …) …)` — needs the recursive walker
  to handle nested ellipsis recursively, with depth bookkeeping
* Prefixed inner: `((kw p …) …)` — needs the inner ellipsis to
  consume a prefix per outer element

## Iter C5 — Nested compound sub-patterns (recursive walker)

Replaces Iter C3/C4's flat `classify_compound_sub` with a
recursive `walk_sub_pattern`. Given a sub-pattern Datum and an
accessor expression referencing one outer-list element, it
accumulates:

* `constraints: Vec<Datum>` — structural predicates AND-conjoined
  in the shape lambda body (`pair?`, `null?`, `eq?` for literals,
  `equal?` for self-quoting atoms).
* `pvars: Vec<(Symbol, Datum)>` — pvar name + accessor
  expression describing how to extract its value from the element.

For each compound layer the walker emits `(pair? <acc>)` and
recurses on `(car <acc>)` and `(cdr <acc>)`. Atomic cases
(literal, pvar, wildcard, null, self-quoting) terminate.
Nested ellipsis (`(p …)` inside the sub) returns `Err` and the
caller surfaces a "future iter" pointer.

**What lands:**
* `((a (b c)) …)` — pvar + nested compound
* `((a (b . c)) …)` — dotted nested
* `((kw (a b)) …)` — literal kw + nested compound
* `((a (_ c)) …)` — wildcards at any depth
* `((a (b (c d))) …)` — arbitrary nesting depth
* `define-record-type`-style field-list zip-maps

**Deferred to Iter C6 (the last grammar gap):**
* Nested ellipsis `((p …) …)` — needs per-element matcher loops
  to handle variable-length inner sections producing depth-2
  pvars.

## Iter C4 — Literals + wildcards + dotted tail in compound sub

Extends Iter C3's compound sub-pattern handling. Each spine slot
is classified as one of:

* `Pvar(s)` — bare-symbol pvar (Iter C3 case)
* `Literal(s)` — name from the `literals` list, `eq?`-checked
* `Wildcard` — `_`, accepts anything, no binding

The compound sub may also have a dotted tail of pvar/wildcard
form: `((a . b) …)`, `((x y . rest) …)`.

`classify_compound_sub` returns `(spine_slots, tail_slot)`; the
shape lambda emits `pair?` for each spine position plus the
slot-specific check (`eq?` for literals, nothing for pvars/
wildcards), and either `null?` for proper-list sub or no
constraint for dotted-tail sub.

**Unblocks:** `cond` macros (`((test => proc) …)` with `=>`
literal), `case-lambda` rewrites (`((args body) …)` and
`((args . body) …)` shapes), and any other macro that wants to
discriminate its compound clauses by a keyword.

**Deferred to Iter C5:** nested ellipsis (`((p …) …)`), nested
compound sub-patterns (`((a (b c)) …)`). Both still rejected
with the same "future iter" pointer.

## Iter C3 — Compound + zip-map ellipsis

Patterns of shape `(prefix… (p1 p2 … pK) …)` where each `pi` is
a bare-symbol pvar. The sub-pattern (a proper-list of pvars)
binds each pvar at depth 1, capturing the per-element value of
its slot across the whole ellipsis section. Test code generated:

* `(every (lambda (e) (and (pair? e) (pair? (cdr e)) … (null? (cdr^K e)))) walking-key)`
* `(list? walking-key)`

Extraction: each `pi` binds to `(map (lambda (e) (car (cdr^i e))) walking-key)`.

Templates of matching shape `(prefix… sub …)` where `sub`'s
referenced pvars are all depth-1 zip-map: inner sub-template
runs with those pvars re-bound at depth 0; outer call becomes
`(map (lambda (p1 p2 … pK) <inner>) p1-list … pK-list)`.

**Architectural change**: `syntax_pvars` upgraded from
`Vec<Symbol>` to `Vec<(Symbol, u32)>` to track each pvar's
ellipsis depth. `compile_sc_pattern` returns
`Vec<(Symbol, u32, Datum)>`; `compile_syntax_template` takes
`&[(Symbol, u32)]` to decide scalar-substitution vs. zip-map.

**What lands**: `let`/`cond`/`case`-style macros that bind
`((var val) …)` and emit `(lambda (var …) …) val …` etc.
Probably 90% of real-world syntax-case usage when combined with
Iter C2's single-pvar shape.

**Deferred to Iter C4**: nested ellipsis (`((p …) …)`),
literals inside compound sub-patterns, dotted-tail sub-patterns.

## Iter C2 — Minimal ellipsis

Patterns of shape `(prefix… pvar …)` where `pvar` is a single
bare symbol: the pvar binds to the *list* of remaining
subject elements (after consuming the prefix). The subject must
be a proper list of length ≥ prefix-length.

Templates of matching shape `(prefix… pvar …)` splice the bound
list into the rebuilt structure: emitted as
`(cons prefix1 (cons prefix2 … (cons prefixN pvar)))`.

**What lands here:**
* `(args …)` / `(args …)` — common args-pattern macro shape
* `(name args body)` + ellipsis-rich templates like
  `(define name (lambda args body))` (no ellipsis required)
* `with-syntax` patterns of the same shape

**Explicitly rejected, with pointer to Iter C3:**
* Compound sub-patterns: `((a b) …)` — needs per-element matcher loop
  with pvar accumulators
* Multiple pvars under one ellipsis position: `((a b) …)` template-side
* Nested ellipsis: `((p …) …)`

This is the 80/20 cut: covers the canonical "args-list" macro
shape without the considerable complexity of multi-pvar zip-maps.

## Iter C — `with-syntax`, `quasisyntax`, pvar stack

`with-syntax` desugars to a nest of single-clause `syntax-case`
forms. `quasisyntax` is implemented by rewriting the template
(`quasisyntax`/`unsyntax`/`unsyntax-splicing` → `quasiquote`/
`unquote`/`unquote-splicing`) and delegating to the existing
`expand_quasiquote` engine — with today's syntax-object-as-datum
model the semantics match exactly.

**Architectural change:** Iter B's "eager pre-pass walker"
(`rewrite_syntax_forms`) was removed in favor of an
Expander-level `syntax_pvars: Vec<Symbol>` stack. Every
syntax-binding form pushes its clause-local pvars before
expanding its body and pops after. `expand_syntax_form` consults
the stack at expansion time. This means:

* Nested with-syntax / syntax-case forms correctly inherit
  outer pvars (Iter B's pre-pass scoping was broken for the
  nested case).
* Standalone `(syntax X)` outside any binding form sees an
  empty stack and lowers to literal — same as Iter B.
* Single source of truth for what's a pvar; no more risk of the
  walker and expander disagreeing.

16 Iter C tests cover with-syntax single+multi+destructuring
bindings, quasisyntax+unsyntax+unsyntax-splicing,
`unsyntax`-outside-`quasisyntax` rejection, and the
`syntax-case`+`with-syntax`+`quasisyntax` composition pipeline.

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
