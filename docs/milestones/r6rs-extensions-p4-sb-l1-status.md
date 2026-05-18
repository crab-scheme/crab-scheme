# R6RS++ Phase 4 sandboxing — L1 sub-iters complete

> Status: **All 4 L1 sub-iters (L1.1–L1.4) shipped on
> r6rs-extensions. L1 namespace-restricted in-process eval is the
> foundation layer of the sandboxing arc; L2 (WASM-instance
> isolation) is the next major piece.**
> Branch: `r6rs-extensions`.
> ADR: `docs/adr/0015-sandboxing.md` (Accepted 2026-05-18).
> Predecessor: Phase 4 optimizer-plugins arc (closed in
> `1efeeca`).

Captures the L1 work — namespace restriction — and what's next.

## What L1 is

ADR 0015 §"Decision" layer 1 (L1): replace the eval-ignores-env
stub with real per-environment binding filtering. Foundation R6RS
surface; NOT a security boundary against adversarial code, but a
real ergonomic win for plugin authors who want to constrain what
their guests can see.

L1 alone protects against accidental scope leak. The L2 layer
(WASM-instance sandbox, future iter) is the security-boundary
piece that protects against malicious guests.

## What shipped

### L1.1 — `(environment ...)` immutable snapshot
Commit: `4bba51e`.

`(environment <import-spec> ...)` returns an R6RS-strict
immutable snapshot environment encoded as a Vector record
`#('__environment__ <alist> #f)`. `eval` consults the env arg;
builds a `Frame::immutable_root` from the snapshot bindings.
`(eval '(set! x ...) env)` raises a compound `&assertion`
condition (with `&who`/`&message` simples).

Implementation:
- New `Frame::immutable_root(bindings)` constructor + `immutable`
  flag + `is_immutable_definition(name)` chain walker
- `CoreExpr::Set` in eval.rs checks the chain before mutation
- `RNRS_BASE_EXPORTS` hardcoded list (~100 names — arithmetic,
  list ops, equality, strings, chars, vectors, symbols, I/O,
  eval/environment for recursive use)
- `b_eval` routes raised conditions through `pending_raise`
  side-channel so host `guard` catches them as conditions, not
  stringified errors

13 tests covering construction, restricted eval, unbound
identifiers, immutability via set!, snapshot semantics
(post-construction host redefines don't bleed through), error
cases, and recursive eval-inside-eval.

### L1.2 — `(make-namespace ...)` mutable namespace
Commit: `992d681`.

Racket-style mutable namespace constructor alongside the L1.1
snapshot env. Same vector record shape with `mutable?` = `#t`.
Two mutation builtins: `namespace-set-variable-value!` and
`namespace-undefine-variable!`. `eval` against a mutable
namespace allows `set!` inside the eval'd expression.

Implementation:
- New `Frame::mutable_root(bindings)` constructor
- `namespace_update` shared helper: decode alist → mutate Vec →
  re-encode (avoids the pair-splicing aliasing bugs)
- `b_eval` branches on the mutable flag

15 tests covering construction (all 3 library aliases),
mutation via builtins, visibility across multiple evals,
undefine-then-redefine, immutable env rejects namespace
mutation, L1.1 immutability preserved alongside the new
machinery, error cases.

### L1.3 — composite library construction
Commit: `da97ede`.

Split `RNRS_BASE_EXPORTS` from `RNRS_LISTS_EXPORTS` so
`(environment '(rnrs base) '(rnrs lists))` resolves to a proper
UNION of distinct export sets, not the same alias. Previously
all three known library specs aliased to the same list; the
composite property was trivial.

`(rnrs lists)` exports now: `find`, `for-all`, `exists`,
`filter`, `partition`, `fold-left`, `fold-right`, `remove`,
`remp`, `remv`, `remq`, `cons*`. R6RS §3.

9 tests covering: `(rnrs base)` alone does NOT include
`for-all`/`fold-left` (verifies the split, not just the union);
`(rnrs lists)` alone does include them; composite construction
makes both visible; spec order doesn't matter; repeated specs
are idempotent; `make-namespace` accepts composites too.

### L1.4 — first-class environment passthrough
Commit: `3f1e071`.

Environments returned from `(environment ...)` and
`(make-namespace ...)` are ordinary first-class values: bind,
pass to procedures, store in collections, compare. The L1.1/L1.2/
L1.3 substrate already supports this — this iter documents and
tests the property without implementation changes.

10 verification tests covering bind-and-reuse, passthrough to
user procs (direct + curried), storage in list/vector,
`eq?` semantics (two `environment` calls aren't `eq?`; same
env bound twice is), snapshot immutability survives
passthrough, shared mutable namespace between closures sees
writes from either side.

## Test additions

| Suite                                  | New tests |
|----------------------------------------|-----------|
| phase4_sb_l1_environment.rs (L1.1)     | 13        |
| phase4_sb_l1_namespace.rs (L1.2)       | 15        |
| phase4_sb_l1_composite.rs (L1.3)       |  9        |
| phase4_sb_l1_first_class.rs (L1.4)     | 10        |
| **Total L1**                           | **47**    |

All green; full workspace test sweep clean throughout.

## What didn't ship

**Set!-inside-eval write-back to mutable namespace.** Today,
`(eval '(set! x 1) ns)` against a mutable namespace mutates the
per-eval frame but the namespace storage doesn't see it. Explicit
`namespace-set-variable-value!` is the primary write path. The
write-back semantics would be useful for REPL-feel use cases but
nothing currently asks for it; defer until a concrete REPL
embedder requests it.

**Per-library binding metadata at builtin-registration time.**
The two hardcoded `RNRS_*_EXPORTS` lists work but don't scale to
arbitrary libraries. Long-term: each builtin registration carries
a `library: Option<&str>` annotation; `resolve_import_spec`
filters the global registry by it. Mechanical refactor; the L1.3
hardcoded-split is enough until library count grows past 3.

## What's next for the sandboxing arc

L1 is the foundation; L2 is the security boundary. Per ADR 0015's
action items (iter numbering uses ADR's labels, not these L1
sub-iter labels):

- **Phase-4-sb iter 1**: new `cs-sandbox-wasm` crate. Pins
  `wasmtime = "36"`. Ships `SandboxConfig` with the three
  presets (`hygiene`, `plugin`, `adversarial`),
  `SandboxInstance`, `SandboxError`. Implements the stdin/stdout
  text protocol; supports basic value round-trip.
- **Phase-4-sb iter 4**: `(make-wasm-sandbox preset ...)`,
  `(sandbox-eval s expr)`, `(sandbox-config s)`,
  `(reset-sandbox s)`. cs-runtime adds `cs-sandbox-wasm` as a
  feature-gated dep.
- **Phase-4-sb iter 5**: L1 inside L2 — defense in depth.
  Guest's eval defaults to the SandboxConfig.imports
  environment (using L1.1's `environment`).

The L1 work shipped here is what iter 5's defense-in-depth
layer composes with. The cs-sandbox-wasm crate (iter 1) is the
biggest remaining piece — wasmtime integration, the text
protocol, the three preset configs.

## Cross-cutting

- ADR 0013 perf gates: not affected. L1 only triggers when the
  env arg to `eval` is an Environment record; non-`eval` code
  is untouched.
- cs-typer / cs-opt: orthogonal. L1 is a runtime-eval surface
  change; doesn't interact with the JIT/AOT pipeline.
- Phase 3 `#!lang`: orthogonal at the L1 layer; iter 5 (L1
  inside L2) is the natural composition.

## Cross-reference

- [[project_r6rspp_phase4_typed_arc]] — typed-boundaries arc
  (sibling Phase 4 work)
- [[project_r6rspp_phase4_optimizer_arc]] — optimizer-plugins
  arc (closed)
- ADR 0015 `docs/adr/0015-sandboxing.md` — the design this
  implements
