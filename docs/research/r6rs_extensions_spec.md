# R6RS++ — Racket-inspired extensions on the R6RS foundation

> Status: **Research / Proposed** as of 2026-05-18. Predecessor:
> 1.0-rc4 (`beam-runtime` merged in `58dc1af`).
> Spec slug: `r6rs-extensions`.
> Estimated duration: phased over 12-18 months; each phase
> independently shippable. See "Phased rollout" for the breakdown.
>
> **Target outcome:** extend the R6RS foundation with the parts of
> Racket that make it a *language platform* — pattern matching,
> contracts, syntax/parse-style macro UX, submodules, `#lang`,
> packages, continuation marks, parameters — without forking the
> core or sacrificing R6RS conformance.

## Why this matters

CrabScheme today is a strict, conformant R6RS implementation with
multi-tier execution (walker / VM / JIT / AOT), per-actor isolation
(BEAM v1, `58dc1af`), and an emerging optional type system (`cs-typer`).
The R6RS foundation already gives us things that classic Scheme
implementations lacked:

| Capability | R6RS status |
|---|---|
| Hygienic macros (`syntax-case`) | ✓ |
| Library system | ✓ |
| Exception system + conditions | ✓ |
| Record system | ✓ |
| Unicode + standardized ports | ✓ |
| Phase-aware expansion foundations | partial |
| Bytevectors | ✓ |

So the prior framing — "bring Scheme up to Racket-level infrastructure" —
is *wrong* for this codebase. We don't need to replace the R6RS
expander, the library system, or the record machinery. The
real goal is more focused: **add the language-platform features that
distinguish Racket from "a good Scheme implementation"**, layered on
top of R6RS rather than replacing it.

The strongest single insight: **R6RS + layered Racket extensions
gives us a smaller, cleaner, standards-compatible language platform
than either R6RS alone or a Racket clone.** We keep R6RS
authoritative; extensions live as libraries and compiler plugins;
the core evaluator changes are minimal.

## TL;DR architecture

```
┌──────────────────────────────────────────────────┐
│ Racket-inspired extensions                       │
│  - contracts                                     │
│  - #lang reader protocol                         │
│  - pattern matching (match)                      │
│  - syntax-parse                                  │
│  - submodules                                    │
│  - continuation marks                            │
│  - parameters                                    │
│  - package manager                               │
├──────────────────────────────────────────────────┤
│ Enhanced R6RS library layer                      │
│  - package metadata                              │
│  - source-tracked syntax objects                 │
│  - incremental compilation cache                 │
├──────────────────────────────────────────────────┤
│ R6RS macro expander (syntax-case) ← unchanged    │
├──────────────────────────────────────────────────┤
│ R6RS runtime + VM ← unchanged at the core        │
│  + minimal hooks for continuation marks          │
└──────────────────────────────────────────────────┘
```

**Key architectural calls:**

- **R6RS remains authoritative.** `(import (rnrs))` is canonical.
  Existing R6RS code keeps working unchanged. We never break import
  semantics.
- **Extensions are layered.** Pattern matching, contracts,
  syntax-parse, parameters, and submodules are *libraries*
  implemented over `syntax-case` — zero VM changes.
- **The VM changes once.** Continuation marks require frame metadata.
  Everything else is a library or a build-system addition.
- **The reader becomes pluggable.** `#lang` is a header-driven
  reader-dispatch mechanism, *sandboxed* per file — no global
  reader mutation (unlike Racket).

## Goals

1. **R6RS conformance unchanged.** All existing conformance suites
   keep passing; `(import (rnrs))` programs run bit-for-bit
   identically.
2. **Modern developer ergonomics.** Pattern matching, syntax-parse,
   structured errors with source tracking, REPL-introspectable
   syntax objects.
3. **Ecosystem viability.** A real package system with semver,
   lockfiles, and reproducible builds — the single highest-ROI
   missing piece.
4. **Language extensibility.** `#lang` makes it possible to ship
   domain-specific surface languages (typed, lazy, untyped) on the
   same runtime.
5. **Composability with existing CrabScheme work.** Pattern matching
   integrates with cs-typer's type annotations; contracts route
   through the R6RS condition hierarchy; continuation marks expose
   profiler/debugger hooks the BEAM runtime can use.

## Non-goals (this spec)

- **Not a Racket clone.** We don't ship `racket/base`, `racket/list`,
  or any Racket library. The naming, semantics, and surface differ
  where the R6RS analog already exists.
- **Not replacing R6RS macros.** `syntax-case` stays as the macro
  primitive; `syntax-parse` is a higher-level wrapper, not a
  replacement.
- **Not global reader mutation.** Languages are file-scoped via
  `#lang`. No `(read-table)` shenanigans that leak across files.
- **Not gradual typing in v1.** `cs-typer` already exists as a
  separate track (`typer-plan.md`); contracts here are the
  *runtime* boundary mechanism, complementary to but not
  superseding the static typer.
- **Not a new dialect.** The output is "R6RS plus modern
  ergonomics", not a fork.

## What NOT to reimplement

R6RS already gives us infrastructure we shouldn't re-do. Explicit
**non-actions**:

- **Don't build another macro core.** `syntax-case` is the
  primitive. `syntax-parse` lives on top of it.
- **Don't replace the library system.** Add package metadata,
  versioning, dependency resolution — don't replace `(library …)`.
- **Don't break R6RS import semantics.** Immutability and explicit
  imports are valuable; preserve them.
- **Don't add a second condition hierarchy.** Contracts emit
  R6RS conditions tagged `&contract`.
- **Don't fragment the record machinery.** Racket-style record
  shorthands expand to `define-record-type`.

## What ships

### 1. Pattern matching (Phase 1 — first priority)

R6RS surprisingly lacks built-in modern pattern matching. This is
the most asked-for feature and the lowest-risk to implement.

**Surface:**

```scheme
(match expr
  [('add x y)   (+ x y)]
  [('sub x y)   (- x y)]
  [(? number? n) n]
  [(vector a b c) (list a b c)]
  [(rec user name age) (format "~a/~a" name age)]
  [_ (error 'match "no pattern matched" expr)])
```

**Patterns supported:** literals, identifiers (bind), wildcards
(`_`), constructor patterns for pairs / vectors / bytevectors /
records, predicate patterns (`?`), guard clauses (`when`),
ellipsis patterns (`(x ...)`), quasiquote patterns.

**Implementation:** entirely as `(library (core match) …)` using
`syntax-case`. No VM changes. Expands into nested `if` /
`cond` / record-accessor calls.

### 2. Contracts (Phase 2)

Boundary contracts with blame tracking — Racket's strongest
ecosystem advantage and a natural fit for the R6RS condition
system.

**Surface:**

```scheme
(library (math vec)
  (export length normalize)
  (import (rnrs) (core contract))
  (provide/contract
    [length    (-> vec? non-negative-real?)]
    [normalize (-> vec? vec?)])
  …)
```

**Blame objects:**

```scheme
#<blame source-library: (math vec)
       target-library: (graphics ray)
       contract: (-> vec? non-negative-real?)
       value: <faulty-input>
       stack: <continuation-marks>>
```

**R6RS integration:** blame is raised as a `&contract` condition
extending `&error`, so existing `with-exception-handler` /
`guard` code catches it. Stack traces use continuation marks
(see §6).

**Tail call discipline:** monomorphic contracts on
non-higher-order values are eta-elided; contracts on procedures
wrap them via `case-lambda` preserving the existing tail-call
ABI.

### 3. syntax-parse (Phase 2)

R6RS macros are powerful but the UX is poor — error messages
point at the macro expansion site, not the user's typo.
`syntax-parse` is Racket's answer; we lift the design unchanged.

**Surface:**

```scheme
(define-syntax-parser define-config
  [(_ name:id value:expr)
   #'(define name value)]
  [(_ name:id value:expr #:when guard:expr)
   #'(define name (if guard value (error 'config "guard failed")))])
```

**Syntax classes:** `id`, `expr`, `number`, `string`, plus
user-defined via `define-syntax-class`. Class violations produce
the kind of pinpoint error messages Racket users expect.

**Implementation:** library on top of `syntax-case`. No expander
changes. Source-tracking from §9 makes the diagnostics work.

### 4. Submodules (Phase 3)

R6RS libraries are flat — there's no way to colocate tests with
implementation. Submodules fix that.

**Surface:**

```scheme
(library (web server)
  (export start)
  (import (rnrs))
  (define (start port) …)

  (submodule tests
    (import (test framework))
    (test-equal (start 8080) 'ok))

  (submodule benchmark
    (import (bench harness))
    (bench (start 8080))))
```

**Compilation:** internally, submodules become sibling libraries
named `(web server) (web server tests) (web server benchmark)`.
The import system needs zero changes — submodules expand into
new library declarations during pre-processing.

### 5. `#lang` reader protocol (Phase 3 — biggest architectural feature)

Per-file language declaration. **The single new capability that
turns CrabScheme into a language platform.**

**Surface (file header):**

```scheme
#!lang typed
(define (square [n : Integer]) : Integer (* n n))
```

**Language module contract:** a `#lang foo` file desugars to
`(import (lang foo))`, where `(lang foo)` exports a
`reader` procedure, an `expander`, and a base runtime
environment. The reader is invoked on the rest of the file;
its output is expanded by the language's expander against its
base env.

**Sandboxing constraint** (key difference from Racket): readers
are *scoped to the file declaring `#lang`*. There is no global
reader-table mutation. Cross-file effects go through the library
system, not the reader. This keeps the security and reasoning
story clean.

### 6. Continuation marks (Phase 3 — the VM change)

The one feature requiring runtime modification. Frame metadata
that propagates through tail calls + a primitive to read it back.

**Surface:**

```scheme
(with-continuation-mark 'request-id "abc-123"
  (handle-request req))

(current-continuation-marks 'request-id)  ; => '("abc-123")
```

**Why they matter:** stack annotations, profilers, debuggers,
tracing libraries, dynamic context propagation. Racket's most
underrated innovation.

**VM impact:** each frame gains an `Option<Arc<MarkChain>>`
slot. Tail calls *push to the chain*, not replace it; non-tail
calls inherit the parent chain. Read is `O(frame depth)` worst
case — acceptable for the tooling use cases.

**Integration with BEAM runtime:** the actor system's tokio-thread
context can stamp continuation marks at spawn so per-actor
profiling falls out for free.

### 7. Parameters (Phase 2)

Dynamic configuration that integrates with `dynamic-wind`.

**Surface:**

```scheme
(define current-output-style (make-parameter 'pretty))

(parameterize ([current-output-style 'compact])
  (pretty-print value))
```

**Implementation:** pure R6RS library on top of `dynamic-wind` +
hashtables keyed by parameter identity. No VM changes.

### 8. Record system extensions (Phase 2)

R6RS records work but are verbose. Add ergonomic surface
without breaking the underlying machinery.

**Pattern integration:**

```scheme
(match rec
  [(rec user name age) (format "~a/~a" name age)])
```

**Constructor shorthand:**

```scheme
(define-record user
  (name string?)
  (age  integer?))
```

— expands directly into `define-record-type` with the predicate
arguments wired through contracts (Phase 2 dep).

### 9. Better exception experience (Phase 2)

R6RS exceptions exist but the developer experience is poor:
opaque positions, sparse stack traces. The fixes are all
infrastructure:

- **Stack traces:** standardized structured traces (`&stack`
  condition) using continuation marks once §6 lands.
- **Source mapping:** every syntax object carries `(file, line,
  column, expansion-origin)`. Foundation for `syntax-parse`
  diagnostics + IDE go-to-definition.
- **Condition hierarchy:** new standard condition types
  `&contract`, `&syntax`, `&type`, `&module` extending the
  R6RS root condition type.

### 10. Package manager (Phase 1 — highest ecosystem ROI)

R6RS libraries lack distribution semantics. We add semver,
lockfiles, and reproducible builds — the single highest-ROI
feature for ecosystem viability.

**Package metadata:**

```scheme
(package
  (name      "http")
  (version   "1.2.0")
  (dependencies
    [(json  ">=1.0")
     (match ">=0.5")]))
```

**Capabilities:** reproducible builds, semantic versioning,
lockfiles, native dependency resolution, compiled-artifact
caching (integrates with §11 incremental compilation).

**Import integration:** `(import (pkg http server))` — the
package resolver maps `(pkg <name> <module>)` to the local
library path under the package's resolved version.

### 11. Tooling architecture (cross-cutting, Phase 1-3)

- **Expansion introspection:** `(expand expr)`,
  `(expand-library lib)` — for IDEs and macro debuggers.
- **Syntax object APIs:** `(syntax-source stx)`,
  `(syntax-line stx)`, `(syntax-column stx)`. Foundation for
  every other tooling feature.
- **Incremental compilation:** cache macro expansions, library
  IR, and syntax metadata. R6RS library boundaries make this
  easier than R5RS — each `(library …)` is a natural cache unit.

### 12. Typed layer (deferred to existing `typer-plan.md`)

The `cs-typer` track already covers this. The R6RS++ contribution
is making *contracts the runtime mechanism that typed module
boundaries lower to* — Typed Racket's strategy unchanged. No
new spec work here; this is integration with an in-flight track.

## Integration with existing CrabScheme

### Library boundaries

R6RS libraries are already CrabScheme's compilation unit. The
incremental-compilation cache, the package resolver, and the
submodule pre-processor all key on library identity.

### cs-expand changes

- **Source metadata:** every `Datum` already has a `Span`; what's
  missing is propagating that through expansion so `syntax-parse`
  diagnostics work. Iter 1 of this spec.
- **Phase-aware expansion:** R6RS has the foundation; what's
  missing is exposing it to user code via `define-syntax-class`.
- **No new macro engine.** `syntax-parse` is a `syntax-case`
  macro that generates `syntax-case` patterns under the hood.

### cs-runtime changes

- **Continuation marks:** new field on the eval frame, new
  primops `(with-continuation-mark)` and
  `(current-continuation-marks)`.
- **Parameters:** registered via the existing condition /
  dynamic-wind machinery; no Runtime-level type changes.
- **Contract boundaries:** new `&contract` condition type
  registered alongside `&error`.

### cs-vm changes

- **Continuation marks ABI:** mark-chain is a field on the
  bytecode frame; lowered to a stack slot in JIT-compiled code.
  Adds ~one word per frame; profile to confirm no regression
  on the bench/realworld microbenches.

### cs-jit-cranelift changes

- **Mark-chain in JIT frames:** the JIT lowers mark reads to
  inline loads; mark writes call into a runtime helper
  (uncommon enough that we don't need direct codegen).

### docs/research + docs/adr

- This doc → `docs/research/r6rs_extensions_spec.md` (you're
  reading it).
- An ADR per major design call:
  - ADR-XX: pattern matching as a library, not a primitive.
  - ADR-XX+1: contracts route through R6RS conditions (not a
    parallel error system).
  - ADR-XX+2: `#lang` reader is file-scoped (no global mutation).
  - ADR-XX+3: continuation marks ABI — frame slot vs. stack
    map vs. side table.

## Phased rollout

| Phase | Theme | Deliverables | Acceptance |
|---|---|---|---|
| **1** | Ecosystem foundation | package manager (§10), pattern matching (§1), syntax source metadata (§9 partial), incremental compilation cache (§11 partial) | A real package can be authored, published locally, depended on, and incrementally rebuilt. `(match …)` ships as a library. |
| **2** | Developer experience | `syntax-parse` (§3), better exceptions (§9 full), contracts (§2), parameters (§7), record extensions (§8) | All in-tree macros migrated to `syntax-parse` (their error messages now point at the user). A medium-sized library uses contracts at every export. |
| **3** | Language platform | submodules (§4), `#lang` (§5), continuation marks (§6) | A `typed` `#lang` ships as a third-party language module. The BEAM runtime uses continuation marks for per-actor tracing. |
| **4** | Advanced research | typed layer integration (§12), optimizer plugins, sandboxing, custom readers | `(import …)` from a sandboxed environment cannot escape; a custom `#lang` can install a domain-specific optimizer pass. |

Each phase is ~2-4 months of focused work; ~12-18 months total
across all four. Phases 1 and 2 are independently shippable and
each delivers immediate ergonomic wins. Phase 3 is the
architectural leap; Phase 4 is open research.

## What we measure

**Conformance:**

- All existing R6RS conformance tests keep passing.
- `(import (rnrs))` programs run bit-for-bit identical wall-clock
  on the bench/realworld suite (no contract overhead unless opted
  in).

**Ergonomics (Phase 1-2):**

- `syntax-parse` error messages reference the user's source span,
  not the macro template's.
- `match` covers ≥90% of in-tree pattern-style `cond`/`case` uses
  after migration; net code shorter.
- Contract-instrumented modules catch deliberately-introduced
  type bugs at the boundary, not deep in a call chain.

**Ecosystem (Phase 1):**

- One non-trivial package (target: a JSON parser, a CLI library,
  or a markdown renderer) authored against the package manager
  end-to-end. Reproducible build from lockfile on a clean
  machine.

**Language platform (Phase 3):**

- Continuation marks added to a frame add < 1% wall-clock
  overhead on the existing bench/realworld microbenches when
  unused; reads are O(depth) but < 5 µs for ≤ 100-frame stacks.
- `#lang typed` runs the `cs-typer` test suite through the
  reader / expander dispatch.

## Open questions

1. **Pattern compiler:** decision-tree (DT) vs. backtracking-tree
   (BT) for `match` expansion? DT generates faster code but
   larger; BT is simpler. Racket's `racket/match` uses BT. For
   v1, BT — revisit if profiles show match-heavy code is hot.

2. **Contract per-call overhead budget:** how slow are contracts
   allowed to be on the fast path? Racket's are ~5-30% on
   contract-heavy code. We can do better with cs-typer's static
   info (contracts on already-typed args become no-ops). Target:
   ≤ 5% on a contract-instrumented JSON parser microbench.

3. **`#lang` and AOT:** AOT-emitted code is build-time-frozen.
   Does AOT support `#lang` files? Recommendation: yes for
   languages whose reader+expander are themselves AOT-compileable
   (the common case); reject AOT for languages with runtime
   reader hooks.

4. **Package manager scope:** vendored deps only, or remote
   fetch? v1 — local + git URL, no central registry. v2 —
   registry support if community grows. Prefer "boring +
   reproducible" over "convenient + opaque".

5. **Submodule cycles:** is `(submodule tests)` allowed to
   import from `(submodule benchmark)`? Recommendation: no —
   submodules are *siblings*, only the parent library is
   importable from outside. Forces clean dependency structure.

6. **Continuation-mark interaction with `call/cc`:** captured
   continuations carry their mark chain. Re-invocation
   *restores* the captured chain, not the current one. Matches
   Racket; document loudly because it surprises people new to
   marks.

7. **R6RS macro hygiene + `#lang` reader hooks:** can a custom
   `#lang` produce syntax that's already-tagged with hygiene
   marks, or must it produce raw datums and let the expander
   tag? Recommendation: latter — keeps the hygiene story uniform.

8. **Conditional compilation:** does the spec need `cond-expand`
   for environment-aware code (`(cond-expand (chez …) (crab
   …))`)? Probably yes for Phase 1 package portability. Adopt
   the SRFI-0 form unchanged.

9. **Versioning + R6RS library version refs:** R6RS allows
   `(library (foo (1 0)) …)` version annotations that almost
   nobody uses. The package manager versioning supersedes them.
   Recommendation: keep parsing R6RS version refs but treat
   them as unconstrained; require packages to carry their
   semver in the package metadata.

## Strategic positioning

The strongest framing — and the one this spec is built on:

> CrabScheme is **R6RS++**, not a Racket clone.

The advantage of staying R6RS-centric:

- **Smaller surface** — we ship the parts of Racket that
  matter, not the whole `racket/base` weight.
- **Cleaner semantics** — every extension is a library or a
  small VM hook, not a rewrite of the substrate.
- **Standards-compatible** — existing R6RS code keeps working
  and the conformance story stays defensible.
- **Modern ergonomics** — pattern matching, contracts,
  syntax-parse, continuation marks, packages, `#lang`. The
  features that move users.
- **Composable with our existing tracks** — BEAM runtime uses
  continuation marks; cs-typer uses contracts at module
  boundaries; AOT uses incremental-compile artifacts; realworld
  benches measure conformance unchanged.

The bet: an R6RS implementation with first-class Racket
ergonomics is more attractive to both communities than either
"yet another Scheme" or "the 38th Racket dialect".

## References

### R6RS

- R6RS standard (Sperber et al., 2007) — https://r6rs.org/
- SRFI-0 (cond-expand) — https://srfi.schemers.org/srfi-0/
- SRFI-200 / R7RS-large pattern matching design — for v1
  reference

### Racket

- Felleisen, Findler, Flatt, Krishnamurthi, "The Racket Manifesto"
  (SNAPL 2015) — https://felleisen.org/matthias/Thoughts/Racket_Manifesto.html
- `syntax-parse` — Culpepper, "Fortifying Macros" (JFP 2012)
- `racket/match` — Wright & Cartwright, "A Practical Soft Type
  System for Scheme" — for pattern compilation strategies
- Continuation marks — Clements et al., "A Tail-Recursive
  Machine with Stack Inspection" (TOPLAS 2004)
- Racket package manager design — https://docs.racket-lang.org/pkg/
- `#lang` reader protocol — Tobin-Hochstadt & Felleisen,
  "Languages as Libraries" (PLDI 2011)

### Internal

- `docs/research/beam_runtime_spec.md` — the predecessor track
  that motivated the continuation-mark integration story.
- `docs/milestones/typer-plan.md` — `cs-typer` parallel track;
  this spec coordinates with it on the contracts-at-boundary
  story.
- `docs/milestones/beam-v1-exit.md` — recent exit report
  illustrating the project's per-iter shipping cadence.
- `docs/adr/` — see ADRs 0006-0013 for prior architectural
  decision records.

## What the work-rate looks like

If this spec is greenlit, ~12-18 months of focused engineering
to land all four phases. The biggest risks:

1. **Package manager scope creep.** Easy to grow this into a
   registry + sandbox + dependency-solver project of its own.
   Hold the line at "vendored + git URL" for v1.

2. **`#lang` invariants.** The sandboxing constraint matters more
   than it looks — Racket has had subtle reader-state bugs for
   years. Get the spec right on day 1 and the implementation
   stays clean.

3. **Continuation marks vs. tail calls.** The ABI question
   (where in the frame the chain lives, who's responsible for
   propagating on tail call) needs an ADR before any code lands.
   Get this wrong and we either lose tail-call discipline or
   take a per-call hit on everything.

4. **Contract perf.** Naïve contracts are ~5-30% overhead.
   Acceptable for development; unacceptable for shipped libraries.
   Integration with cs-typer to *elide statically-provable
   contracts* is the way out, but coordination cost across two
   in-flight tracks.

Compared to the BEAM-runtime work (also ~6 months across 8
phases), R6RS++ is more **library** and less **system**:
Phases 1, 2, and most of 3 are libraries on top of R6RS. Only
continuation marks (Phase 3) touches the VM. Lower risk per
iter, higher cumulative reward.

The recommendation: **proceed with Phase 1 immediately** after
this spec is reviewed. Pattern matching alone closes the
single largest ergonomic gap, and the package manager
unblocks ecosystem growth in a way nothing else does.
