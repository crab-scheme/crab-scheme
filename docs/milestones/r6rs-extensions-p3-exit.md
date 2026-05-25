# R6RS++ Phase 3 — exit report

> Status: **Phase 3 complete — 3 subphases shipped, 2 follow-ups
> tracked for the post-1.0 sweep, 30 new tests, full workspace
> sweep clean.**
> _Post-exit: 3C.full (parse-time custom reader protocol) shipped
> — issue #10. See "3C update" below._
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

#### 3C update — full reader protocol shipped (issue #10) ✅

The parse-time reader hook called out in the MVP-scope note now
ships. If `(lang NAME)` was declared with `(export reader ...)`,
`Runtime::eval_str` calls that procedure on the file body and
feeds its returned datum list to the expander in place of the
host reader. Lang libraries that don't export `reader` keep the
3C MVP behavior unchanged (header line padded with whitespace so
diagnostic spans stay aligned, body parses with the host reader).

Key pieces:
- `crates/cs-runtime/src/lang_reader.rs` — the value→datum bridge
  and `parse_lang_header` helper.
- `Runtime::library_exports` — mirror of each per-call
  `Expander::libraries()`, populated as a side effect of
  `eval_data_in_file`. Lets the pipeline consult declared exports
  across `eval_str` invocations.
- `Runtime::eval_with_lang_header` — dispatches to the custom-
  reader path or the MVP rewrite path based on the
  `library_exports` query.
- The reader proc itself runs through `eval::apply_procedure`
  (walker-tier), not `vm_call_sync` — readers may be user-defined
  closures which the VM-only path can't dispatch.

Limitations rolling forward:
- ~~Datum spans synthesized by a custom reader collapse to the
  first byte of the body file~~ — opt-in span threading via
  `(syntax-datum d start end)` shipped as issue #72. See
  "Follow-up — `syntax-datum`" below.
- The optional `expander` export (per-lang expansion) isn't yet
  honored — tracked as #71.
- The cs-expand "all bindings global" milestone means two langs
  that both export `reader` collide on the binding — the later
  declaration wins. Reflected in
  `lang_switch_between_eval_str_calls`; a future namespace-
  isolation pass will refine this.

##### Follow-up — `base-env` export honored (issue #70) ✅

When `(lang NAME)` exports `base-env` and binds it to an
environment value (built via `environment` or `make-namespace`
from ADR 0015 L1), the file body now evaluates against that env
instead of the runtime's global top env. The reader itself still
runs against the full env (it's meta-level code the lang author
controls); only the body is restricted.

Path: `Runtime::eval_data_in_env(data, Option<Rc<Frame>>)` is the
new core entry that takes an explicit eval-time root frame.
`eval_with_lang_header` resolves the optional env via
`resolve_lang_base_env` (which calls
`crate::builtins::decode_environment` to validate the export
shape — non-env values produce a typed
`base-env for (lang NAME) must be an environment` diagnostic).
Both the custom-reader path and the host-reader fallback honor
`base-env`. Mutable namespaces (from `make-namespace`) allow the
body to `define` and `set!`; immutable envs (from
`environment`) do not.

7 new tests in `phase4_lang_base_env.rs` cover: restricted
visibility (only `(rnrs base)` resolves), out-of-env bindings
blocked, mutable namespace allows define, no-`base-env` falls
through, non-env value diagnostic, reader output runs against
restricted env, restrictive env blocks reader-emitted body forms.

##### Follow-up — `syntax-datum` span threading (issue #72) ✅

A reader proc can now opt into source-accurate diagnostics by
wrapping returned forms with the new `syntax-datum` constructor:

```text
(syntax-datum DATUM START END)
```

where `START` and `END` are non-negative byte offsets into the
body string the reader was invoked with. The bridge in
`crate::lang_reader::value_to_datum` recognises the resulting
tagged-vector record (sentinel `__syntax-datum__`) and stamps
the inner datum with `Span::new(body_file, start, end)` instead
of the coarse zero anchor. Wrappers nest naturally — an inner
wrap re-anchors only its own subtree.

Plain (unwrapped) datums keep today's collapsed-to-byte-zero
anchor, so this is purely additive and backward compatible.

Builtins added:
- `(syntax-datum d start end)` — constructor; rejects non-int
  offsets, negatives, or `end < start`.
- `(syntax-datum? v)` — predicate.

5 new tests in `phase4_reader_spans.rs` cover the round-trip
(constructor + predicate), arity / type / order validation, a
reader emitting a wrapped form (diagnostic line resolves to the
wrapped offset, not line 1), an unwrapped reader (legacy anchor
preserved), and the nested-wrap re-anchor case.

10 new tests in `phase4_custom_reader.rs` plus 5 unit tests in
`lang_reader::tests` cover the reader-invoked path, the body-
sees-other-exports composition, the no-reader fallback (Phase 3C
behaviour preserved), pre-existing user `reader` not mistaken,
lang-reader-overrides-user-reader, and three error paths
(non-list return, improper-list return, reader-raise).

Example lang library: `lib/lang/passthrough-reader.scm`. A
minimal lang that defers entirely to the host reader using
`open-input-string`+`read` in a loop — useful as a starting
point for transformer-style readers.

## What's deferred (tracked post-1.0)

| ID    | Title                                                         |
|-------|---------------------------------------------------------------|
| #155  | Phase 3A.tail-safe — VM-level marks with tail-call replacement|
| ~~#156~~ | ~~Phase 3C.full — `#lang` parse-time custom reader protocol~~ ✅ shipped as issue #10 (see "3C update") |

#155 remains non-blocking for 1.0: the surface API lands now; the
"real" implementation can replace the MVP without surface
changes.

## Test additions

| Suite                                                  | New tests |
|--------------------------------------------------------|-----------|
| phase3_submodules.rs (3B)                              |  9        |
| phase3_continuation_marks.rs (3A)                      | 12        |
| phase3_hashlang.rs (3C MVP)                            |  9        |
| phase4_custom_reader.rs (3C.full / issue #10)          | 12        |
| lang_reader::tests (3C.full unit, in `cs-runtime/src`) |  5        |
| **Total Phase 3 + 3C.full follow-up**                  | **47**    |

All green; full workspace test sweep is clean (1018 cs-runtime
tests post-#10).

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
