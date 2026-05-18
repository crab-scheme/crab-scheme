# R6RS++ Phase 3 — exit report

> Status: **Phase 3 complete — 3 subphases shipped, 2 follow-ups
> tracked for the post-1.0 sweep, 30 new tests, full workspace
> sweep clean.**
> Branch: `r6rs-extensions`.
> Spec: `docs/research/r6rs_extensions_spec.md` (§4 submodules, §5
> `#lang` reader, §6 continuation marks).
> Predecessor: Phase 2 (closed in `8cdef4d`).

Captures what shipped in the Phase 3 sweep and what's tracked for
later iteration.

## What shipped

### 3B — submodules
Commit: `2af10e6`.

`(submodule NAME body...)` inside a library is lifted to a sibling
library named `(parent... NAME)`. The lifted library is expanded
and registered alongside its parent; its body runs after the
parent's body, so submodules see all of the parent's defines (the
flat-namespace milestone makes this transparent). Optional leading
`(export ...)` / `(import ...)` clauses are honored.

This matches the spec's example:
```scheme
(library (web server)
  (export start)
  (import (rnrs))
  (define (start port) ...)
  (submodule tests
    (test-equal (start 8080) 'ok)))
```
Compiled as siblings `(web server)` + `(web server tests)`.

9 tests in `phase3_submodules.rs` cover the lifting mechanics,
multiple submodules in order, registration under the extended
name, optional export clauses, and the parent-visible-state
pattern.

### 3A — continuation marks (naive impl)
Commit: `d35910f`.

Surface in `lib/cmarks/cmarks.scm`:
```scheme
(with-continuation-mark key val body ...)
(current-continuation-marks)
(current-continuation-marks key)
```
Layered using an internal parameter to model the dynamic mark
chain. `parameterize` handles scoping; nested
`with-continuation-mark` forms collect innermost-first when read
back.

NAIVE IMPL caveats (documented in the lib header):
1. **Not tail-safe.** Each `with-continuation-mark` in a
   tail-recursive loop grows the chain rather than replacing the
   caller frame's mark. The spec's VM-level implementation
   (`Option<Arc<MarkChain>>` slot per frame; tail calls share
   the slot) is tracked as #155.
2. **No first-class continuation-mark-set.** Only direct chain
   readout; `continuation-mark-set->list` etc. are deferred.

The surface API is Racket-compatible at the call shape level.
User code written against this layer will migrate unchanged when
the tail-safe impl lands.

12 tests in `phase3_continuation_marks.rs`.

### 3C — `#!lang` reader protocol (MVP)
Commit: `53e3b91`.

A leading `#!lang NAME` (or `#lang NAME`) header is rewritten to
`(import (lang NAME))` before parsing. The rewrite happens
line-1-in-place so line count is preserved and source-span line
numbers stay accurate for diagnostics.

Surface contract: the `(lang NAME)` library is whatever you make
it — a typical lang library exports the constants / forms users
want available under that file's header.

MVP SCOPE: only the header → import rewrite. The spec's full
reader protocol (loading the lang lib at parse time, invoking its
exported `reader` proc on the file body) is tracked as #156 — it
depends on parse-time eval which we don't have yet.

9 tests in `phase3_hashlang.rs` cover the rewrite mechanics (both
`#!lang` and `#lang`), library loading observable through
imports, source-line preservation (verified via the new
`Runtime::sources_line_col` accessor), and the no-header /
header-only edge cases.

## What's deferred (tracked post-1.0)

| ID    | Title                                                         |
|-------|---------------------------------------------------------------|
| #155  | Phase 3A.tail-safe — VM-level marks with tail-call replacement|
| #156  | Phase 3C.full — `#lang` parse-time custom reader protocol     |

Both are non-blocking for 1.0: the surface API lands now; the
"real" implementation can replace the MVP without surface
changes.

## Test additions

| Suite                            | New tests |
|----------------------------------|-----------|
| phase3_submodules.rs (3B)        |  9        |
| phase3_continuation_marks.rs (3A)| 12        |
| phase3_hashlang.rs (3C)          |  9        |
| **Total Phase 3**                | **30**    |

All green; full workspace test sweep is clean.

## What's next

The post-1.0 R6RS++ queue:
- #155 / #156 (Phase 3 follow-ups described above)
- #142 / #143 / #144 (Phase 2A deferred sub-iters)
- #147 (cs-runtime map raise-propagation bug)
- #150 (Phase 2B.7 eta-elision perf)

Per the spec's "Phased rollout" table, Phase 4 is "advanced
research" (typed layer integration, optimizer plugins, sandboxing,
custom readers). None of that is necessary for 1.0; tackle when
specific use cases motivate.
