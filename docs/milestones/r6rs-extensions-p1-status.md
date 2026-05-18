# R6RS++ Phase 1 — interim status

> Status: **Phase 1 mostly landed — 2 of 4 spec deliverables
> shipped, 2 cs-expand bug fixes that unblock match's natural
> spelling, 1 cs-pkg import-bridge for future library loading.
> Remaining blockers (syntax-case, cross-file library loading)
> are scoped as separate milestones.**
> Branch: `r6rs-extensions`.
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

## cs-expand bugs surfaced during implementation — both FIXED

Two `syntax-rules` bugs surfaced while building the `match`
library. Both were root-caused, fixed in cs-expand, regression-
tested, and the match library was rewritten to use the natural
spellings the fixes enable. Combined diff: ~50 lines of
expander code + 4 new tests.

### #112 — `_` in literals list broke subsequent catch-all rule (FIXED `0f7aa92`)

Bug: `match_pattern` checked the `_`-as-wildcard arm
unconditionally before the literals-list check, so a macro that
opted into literal-`_` semantics still saw `_` patterns match
anything.

Fix: hoist the outer-macro-name pass to the top, reorder
match arms so literals-list takes precedence over the underscore
wildcard, and reject literal-symbol patterns matched against
non-symbol input.

### #111 — dotted-pair patterns + templates not supported (FIXED `4c02b30`)

Bug: both `match_list_pattern` (pattern side) and `instantiate`
(template side) called `collect_proper_list_strict` and bailed
on improper lists, rejecting `(x . y)` and dotted templates
`(a . b)`.

Fix: new `collect_pair_chain` helper returns `(spine, tail)`
where tail is `Null` for proper lists and the dotted atom
otherwise. New `match_dotted_list_pattern` walks the spine
positionally then binds the tail-pattern to the input
remainder. `instantiate` uses the same helper to support
dotted templates. `collect_pattern_vars_into` walks the dotted
tail so its variables are seen by ellipsis logic.

`lib/match/match.scm` was rewritten to use the natural
`(P1 . P2)` and `(P1 P2 P3)` forms as the primary spelling,
with `(cons ...)` and `(list ...)` kept as Racket-style sugar.
21 match tests, all green.

## cs-pkg integration seam — landed (`f121d4a`)

Added `Resolver::resolve_import_spec` that bridges from a Scheme
import-spec datum to a filesystem path. Recognises
`(pkg NAME SEG...)` shape, delegates to the existing version map.

Architectural call: **cs-expand stays agnostic of cs-pkg.** The
seam is the existing `IncludeResolver` callback that callers
(cs-cli, REPL) install — that callback invokes
`resolve_import_spec` on the import datum and routes the
resulting path through the include machinery.

What's still missing for `(import (pkg http server))` to work
end-to-end: cs-expand's cross-file library loading. The
existing code only supports same-Expander libraries (declared
via `(library ...)` in the same source unit). Building real
cross-file loading is its own milestone — see "What's left"
below.

## What's left for full Phase 1

After this sweep, two genuinely large items remain:

| # | Item | Blocked on |
|---|---|---|
| A | Cross-file library loading in cs-expand | New subsystem; multi-day. Today cs-expand only resolves libraries declared in the same Expander session. Needed before `(import (pkg http server))` can splice external bindings into the importer. |
| B | `syntax-case` extension to cs-expand | Multi-day. Unlocks §9 source metadata (`syntax-source` / `syntax-line` / `syntax-column`) AND Phase 2's `syntax-parse`. Requires first-class syntax objects, hygiene-mark tracking through user code, `syntax->datum`/`datum->syntax` primitives. |

Both are properly scoped as their own milestones. They're not
blocking the work already shipped — match works, packages parse
and resolve, the expander bugs are fixed.

## Recommended next iter

Two tractable options:

1. **Start syntax-case**: it's the higher-leverage item — it
   unlocks both §9 source-metadata accessors AND `syntax-parse`
   in Phase 2. Multi-iter, but each iter (datum syntax objects;
   then `syntax-case`-the-form; then `with-syntax`; then
   `syntax->datum`/`datum->syntax`) is independently shippable.
2. **Start cross-file library loading**: smaller scope (no new
   value type, just file I/O + lib registry plumbing), unlocks
   `(import (pkg ...))` actually loading. Useful for ecosystem
   bootstrap once published packages exist.

Order doesn't matter — they don't block each other. Pick by
what we want to demo first.
