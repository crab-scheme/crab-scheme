# R6RS++ Phase 1 — interim status

> Status: **Phase 1 partial (2 of 4 deliverables shipped; 2 blocked on
> cs-expand work).** Branch: `r6rs-extensions`.
> Spec: `docs/research/r6rs_extensions_spec.md` (§1, §9, §10, §11).
> Predecessor: 1.0-rc4 (`beam-runtime` merged in `58dc1af`).

Captures what shipped in the first execution sweep of Phase 1 and
the precise reasons two of the four deliverables can't usefully
land before the cs-expand prerequisites are met.

## What shipped

### `lib/match/match.scm` — pattern matching (Phase 1, §1)

`(match expr (pat body) … (pat (when guard) body) …)` over
syntax-rules. Supported pattern forms:

- `_` (conventional unused binding) — wildcard
- identifier — binds the subject
- `'lit` / `(quote lit)` — literal via `equal?`
- `()` — empty list
- `(? pred)` and `(? pred name)` — predicate test, optional bind
- `(cons p1 p2)` — pair pattern
- `(list p ...)` up to 8 elements
- `(vector p1 ... pn)` — exact-length vector
- guards via `(pat (when guard) body)` clause shape

**Tests:** 16 integration tests in
`crates/cs-runtime/tests/match_basic.rs` cover every shape + the
tag-dispatch idiom + subject-evaluates-once + fall-through.
All green.

### `crates/cs-pkg` — package manifest + lockfile + resolver (Phase 1, §10)

Three building blocks for the spec's package manager:

- `PackageManifest::parse` — parses the
  `(package (name …) (version …) (dependencies (dep req) …))`
  s-expression form using cs-parse.
- `Lockfile::parse` / `Lockfile::to_string` — round-trip the
  `(lock (pkg name version hash) …)` form.
- `Resolver` — maps `(pkg <name> <module-path>)` requests to
  filesystem paths under a vendored tree.
- `Version` / `VersionReq` — strict semver with Cargo-style
  `=`/`>=`/`^`/`~` constraints.

**Tests:** 17 unit tests in `crates/cs-pkg/src/lib.rs` cover
version parsing, all four match shapes, manifest happy + error
paths, lockfile round-trip, resolver lookup + error.
All green.

**NOT yet wired** into cs-expand's import path —
`(import (pkg http server))` doesn't actually consult the
resolver. That integration is a follow-up; the public surfaces
above stay stable so the wiring is local.

## What's blocked (and why)

### Source metadata accessors (Phase 1, §9) — needs syntax-case

Spec calls for `(syntax-source stx)`, `(syntax-line stx)`,
`(syntax-column stx)`. Useful only with first-class **syntax
objects**, which the expander gets through `syntax-case`.
cs-expand currently only supports `syntax-rules` — pattern
variables in templates are not user-accessible as data.

The underlying data is there: `Datum` already carries a `Span`;
some `Value` variants do too. What's missing is the Scheme-level
accessor surface, which lives behind a `syntax-case` extension
to cs-expand. Tracked as #114.

### Incremental compilation cache (Phase 1, §11) — needs cs-expand seam

A standalone cache crate without a consumer would just be a
generic Rust LRU (a hundred exist on crates.io). The valuable
design decision is the **library-level cache key schema**
(library name + content hash + dependency closure) and the
**integration points in the expander**. Both require cs-expand
work to be meaningful. The content-hash machinery already lives
in `cs-pkg::Lockfile`. Tracked as #116.

## cs-expand limitations surfaced during implementation

Two real bugs in cs-expand's `syntax-rules` surfaced building
the `match` library. Both have documented workarounds in
`lib/match/match.scm` and are tracked separately so other
macro work can avoid them.

### #111 — dotted-pair patterns not supported in `syntax-rules`

```scheme
(define-syntax car-of
  (syntax-rules ()
    ((_ (x . y)) x)))     ;; no matching rule for input (1 2 3)
```

A standard R6RS `(x . y)` pattern matches lists `(1 2 3)` with
`x=1, y=(2 3)`. cs-expand rejects it. Workaround: use
`(cons a b)` and `(list a b c)` explicit forms instead.

### #112 — `_` in literals list breaks subsequent catch-all rule

```scheme
(define-syntax try-bind
  (syntax-rules (_)
    ((_ subj _ body)         body)
    ((_ subj var body)       (let ((var subj)) body))))
(try-bind 99 x (+ x 1))     ;; undefined variable: x
```

When `_` is a literal, the second-rule pattern variable `var`
fails to substitute into the template. Workaround: don't list
`_` as a literal; treat it as a regular unused-identifier
binding.

Both bugs land on `syntax-rules`-only consumers. Other
codepaths in the project use `define-syntax`/`syntax-rules`
sparingly enough that this is the first time they've surfaced.

## Phase 1 follow-up work

Reordered after this sweep:

| # | Item | Blocked on |
|---|---|---|
| 1 | `(match …)` ✓ | — (shipped) |
| 2 | `cs-pkg` ✓ | — (shipped) |
| 3 | Wire `cs-pkg::Resolver` into cs-expand's import path | cs-expand surgery; small |
| 4 | Fix #111 + #112 in cs-expand, restore bare-list patterns + `_` wildcard in match | cs-expand work; medium |
| 5 | `syntax-case` extension to cs-expand | medium-to-large; unlocks §9 and §3 |
| 6 | Syntax-source / syntax-line / syntax-column primitives | #5 above |
| 7 | Library-level incremental cache | #3 + cache-key schema |

Items 3 + 4 are the smallest cs-expand changes and unlock the
most surface area. Item 5 is the architectural prerequisite for
Phase 2's `syntax-parse`.

## Recommended next iter

Tackle #4 (cs-expand fixes for `syntax-rules` dotted patterns +
`_` literal) before continuing to Phase 2. The two bugs aren't
load-bearing on Phase 1's shipped work, but they will block
every richer macro that future contributors write, and `match`'s
in-library workarounds become unnecessary cruft once they're
fixed.

After that, item 5 (`syntax-case`) unlocks both §9 source
metadata and Phase 2's `syntax-parse` — the biggest cumulative
ergonomic win still available.
