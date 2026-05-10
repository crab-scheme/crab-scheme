# M9 R6RS Stdlib Completion — Requirements

> Status: **Draft** (M9 iter 1)
> Spec slug: `stdlib`
> Roadmap slot: M9
> Predecessor: M8 (`docs/milestones/m8-exit.md`, VM-tier first-class call/cc)

The foundation milestone shipped enough of R6RS to make CrabScheme self-hostingly useful: arithmetic, lists, strings, hashtables, records, bytevectors, basic ports, conditions. M9 fills in the remaining R6RS standard library so a downstream Scheme program can rely on the published spec rather than learning CrabScheme's "subset" personally.

This is largely a *coverage* milestone — the architecture is in place; we're working through the R6RS chapters and turning gaps into tests, then tests into builtins.

---

## Scope

R6RS divides the standard library across these chapters:

| § | Library | Foundation status | M9 target |
|---|---|---|---|
| 1 | `(rnrs base)` (lists, arithmetic, strings, etc.) | ✅ broad coverage | Audit gaps; close the obvious ones. |
| 2 | `(rnrs unicode)` | ⚠️ partial | Full `string-upcase` / `string-downcase` / `string-titlecase` semantics. |
| 3 | `(rnrs bytevectors)` | ✅ data type + most ops | Audit; add any missing `bytevector-*` accessors / setters. |
| 4 | `(rnrs lists)` (member, find, etc.) | ✅ | Done. |
| 5 | `(rnrs sorting)` | ✅ `list-sort` / `vector-sort` work | Done. |
| 6 | `(rnrs control)` (when, unless, do, case-lambda) | ✅ | Audit. |
| 7 | `(rnrs io simple)` / `(rnrs io ports)` | ⚠️ partial | Full transcoder API; binary vs text ports; custom ports. |
| 8 | `(rnrs files)` | ⚠️ partial | `file-exists?` / `delete-file`. |
| 9 | `(rnrs programs)` (`exit`, `command-line`) | ⚠️ partial | `command-line` returning the actual argv. |
| 10 | `(rnrs arithmetic *)` (fixnums, flonums, bitwise) | ✅ broad | Audit. |
| 11 | `(rnrs syntax-case)` | ❌ | Defer — substantial. |
| 12 | `(rnrs hashtables)` | ✅ | Done. |
| 13 | `(rnrs enums)` | ❌ | **Iter 2 target.** No existing implementation; cleanest start point. |
| 14 | `(rnrs eval)` | ⚠️ partial | `eval` accepts a 2nd arg (environment). |
| 15 | `(rnrs records)` (procedural + syntactic) | ✅ syntactic | Add procedural API where the syntactic one wraps it. |

(The chapter numbers above are paraphrased; the exact R6RS structure has more nuance. The status column is M9-iter-1 best-guess from `tests/conformance/foundation/*.scm` coverage.)

---

## Functional requirements

### FR-1. Enumerations (`(rnrs enums)`)

R6RS §13 — type-safe finite sets of symbols. Public API:

- `make-enumeration <list-of-symbols>` → `<enum-set>` (the universe).
- `enum-set?`, `enum-set-universe`, `enum-set-indexer`, `enum-set-constructor`.
- `enum-set->list`, `enum-set-member?`.
- `enum-set-subset?`, `enum-set=?`, `enum-set-union`, `enum-set-intersection`, `enum-set-difference`, `enum-set-complement`, `enum-set-projection`.
- `define-enumeration <type-name> (<symbols>) <constructor-name>`.

Acceptance: `tests/conformance/foundation/enumerations.scm` covers each procedure with one positive + one negative case; passes on both walker and VM tiers.

### FR-2. Procedural records API (`(rnrs records procedural)`)

Foundation has `define-record-type` (the syntactic API). M9 exposes the procedural primitives R6RS §15.2:

- `make-record-type-descriptor <name> <parent> <uid> <sealed?> <opaque?> <fields>` → `rtd`.
- `make-record-constructor-descriptor`, `record-type-descriptor?`, `record-predicate`, `record-accessor`, `record-mutator`.

Internally the syntactic `define-record-type` already builds these constructs; M9 surfaces them as user-facing builtins.

Acceptance: `records_procedural.scm` in conformance — one full RTD construction + accessor/mutator round-trip.

### FR-3. Conditions hierarchy completion

Foundation has the `&error` / `&warning` / `&message` / `&irritants` / `&who` / `&serious` / `&violation` / `&condition` simples. R6RS adds `&assertion`, `&non-continuable`, `&implementation-restriction`, `&undefined`, `&i/o`, `&i/o-read`, `&i/o-write`, `&i/o-port`, `&lexical`, `&syntax`. Audit which are present and add any missing.

Acceptance: every R6RS condition type has a `condition-type-rtd` and a `<type>-condition?` predicate; tests exercise each.

### FR-4. Library form (`(rnrs)` import / export)

Foundation has `(library ...)` forms recognized in conformance test files but not all R6RS semantics: no version handling, partial import phase (run/expand) tracking, no `for syntax`. M9 fills in the missing pieces.

Acceptance: a multi-file library example in `tests/conformance/m9_library/` imports a library defining a procedure, then uses it; passes both tiers.

### FR-5. Full R6RS port API

`(rnrs io ports)`: text vs binary distinction, `transcoder`, `make-transcoder`, `native-transcoder`, `latin-1-codec`, `utf-8-codec`, `utf-16-codec`, `eol-style`, `error-handling-mode`, `transcoded-port`, `port-transcoder`, `binary-port?` / `textual-port?`, `port-eof?`, `flush-output-port`, `close-port`, `make-custom-binary-input-port` and friends.

Acceptance: a custom-port example reads/writes through a user-supplied callback.

### FR-6. R6RS programs (`exit`, `command-line`)

`(rnrs programs)`: `exit [code]` terminates the process; `command-line` returns a list of strings (program path + argv).

Acceptance: a 2-line script that prints `(command-line)` produces `("script.scm" "arg1" "arg2")` when invoked as `crabscheme run script.scm arg1 arg2`.

---

## Non-functional requirements

### NFR-1. R6RS conformance over Racket / Chez extras.

When R6RS specifies a behavior that disagrees with Racket or Chez extensions, follow R6RS. The spec is the source of truth.

### NFR-2. Per-subsystem ADR (when invasive).

Subsystems whose implementation crosses crate boundaries (e.g., custom ports, syntax-case if attempted) get a dedicated ADR. Bounded subsystems (enums, programs) don't need one.

### NFR-3. Tests precede implementation.

For each subsystem, write the conformance test file first (failing or `#[ignore]`d), then implement, then flip the ignore. This documents the target shape and prevents drift.

### NFR-4. Don't regress existing passes.

The baseline at M8 close was 549 workspace tests. M9 work must not flip any of those red.

---

## Out of scope (deferred)

| Item | Where it lives |
|---|---|
| `(rnrs syntax-case)` macros | Post-M9 (separate milestone — substantial) |
| Library version compatibility | Post-M9 |
| Tail-call optimization for direct-style continuations | Post-M9 (orthogonal to M8) |
| Threading / concurrency primitives | Out-of-scope (single-threaded runtime) |
| Foreign function interface beyond cs-ffi | Out-of-scope (M5b shipped that surface) |

---

## Plan order

1. **Iter 1** (this iter): spec + ADR scaffold; first conformance test (enumerations).
2. **Iter 2**: implement `(rnrs enums)` — bounded, tractable.
3. **Iter 3**: procedural records API (FR-2).
4. **Iter 4**: condition-hierarchy audit (FR-3).
5. **Iter 5**: library-form gaps (FR-4).
6. **Iter 6**: R6RS programs (FR-6) — small.
7. **Iter 7+**: port API (FR-5) — large; may split.
8. **Iter N**: M9 exit report.
