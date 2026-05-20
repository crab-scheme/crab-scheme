# Language Layer — effects, hot upgrade, codebase DB

Covers milestones **M01** (effects + hot-upgrade form) and **M12**
(content-addressed codebase DB). Task lists in `tasks/M01-foundations.md`
and `tasks/M12-codebase-db.md`.

## Effect annotations (M01)

Effect sets are sets of symbols carried on every IR node. They flow
bottom-up: a function's effect set is the union of its body's
effect-bearing operations.

### The canonical effect set

| Effect | Examples | Forbidden inside |
|--------|----------|------------------|
| `pure` | arithmetic, list ops, contract checks | (nothing — pure is the universal allow) |
| `alloc` | `cons`, `make-vector`, closures | (compile-time-pure only — rare) |
| `io` | `read-line`, `display`, file ops | workflows, replicated state machines |
| `net` | HTTP, gRPC, sockets | workflows, replicated state machines |
| `wall-clock` | `current-time`, `current-seconds` | workflows, replicated state machines |
| `random` | `(random)`, system RNG | workflows, replicated state machines |
| `mutation` | `set!` of free vars, `vector-set!` on closed-over | replicated state machines |
| `panic` | `error`, `assert`, raise | (allowed) |
| `agent` | model calls, tool dispatch | workflows (must go through activity) |
| `audit` | tool call recordable for compliance | (allowed; mandatory for security tools) |

User code annotates either:

```scheme
;; per-binding:
(define foo #:effects '(net audit)
  (lambda (req) (http-post req)))

;; per-form:
(let ((data (with-effect 'io (read-file path))))
  ...)
```

The compiler infers effects bottom-up; explicit annotations serve
as *upper bounds* (deny the body any effect not in the declared
set) and as *documentation*.

### Effect-gated forms

The expander refuses certain (form, body-effect-set) combinations:

- `(define-workflow ... body)` — body must not contain `net`, `io`,
  `wall-clock`, `random`.
- `(define-replicated-actor ... #:state-machine body)` — same.
- `(define-tool 'name #:effects required handler)` — handler is
  required to have effects ⊆ declared `#:effects`; if it has more,
  compile error.

### Code pointers

- `crates/cs-rir/src/lib.rs` — IR node shape; add `EffectSet` field.
- `crates/cs-expand/src/lib.rs` — `expand_define`, `expand_define_workflow`; add effect-set checking.
- `crates/cs-opt/src/lib.rs` — pass plugin framework; effect inference is a new pass.

## Hot upgrade forms

`cs-hotreload` already ships two-version dispatch + state migration.
M01 adds the missing syntactic sugar:

```scheme
;; Already in main, exposed via lib/beam/prelude.scm:
(define-state-migration counter-v1->v2
  (lambda (old-state) (cons old-state 'extra-field-default)))

;; New in M01: explicit version markers on functions, lowered to
;; hotreload table entries.
(define foo
  #:version "v2"
  #:migrates-from "v1"
  #:migration counter-v1->v2
  (lambda (...) ...))
```

When hot-reloading a function, the runtime:

1. Looks up the new version.
2. For every active actor whose state was produced by the old version,
   applies the registered migration.
3. Atomically swaps dispatch.

Reference: existing `crates/cs-hotreload/src/lib.rs`.

## M12 — Content-addressed codebase DB (cs-codebase)

### The big idea

Every top-level binding hashes its canonical AST. Names are
metadata; identity is hash.

```scheme
(hash-of foo)         ;⇒ #blake3:abc123…
(name->hash 'foo)     ;⇒ #blake3:abc123…  (current "foo")
(hash->ast hash)      ;⇒ <CoreExpr>
(deps-of hash)        ;⇒ (list #blake3:def456 ...)
```

Hot reload introduces a new hash for the new definition; old
workflows pinned to the old hash continue running on the old AST.

### Hash function

BLAKE3 over a canonicalized form of the `CoreExpr`:

- All `Identifier` references resolved to their target's hash (so
  renames don't change the hash, only definition changes do).
- Whitespace + comments stripped.
- Macro expansion already done (we hash post-expand IR).
- Deterministic field ordering.

Two ASTs with the same hash are *operationally equivalent* in the
language semantics; one cached compile result serves both.

### Storage

Pluggable behind a `CodebaseStore` trait:

| Store | Use case |
|-------|----------|
| `sled` (embedded) | default |
| `rocksdb` | high-write self-hosted |
| `postgres` | shared across multi-process deployments |
| `in-memory` | tests |

### Namespaces

```scheme
;; A namespace is a name → hash mapping with optional version pins.
(namespace 'production)
(namespace-bind! 'production 'foo (hash-of foo))
(namespace-bind! 'production 'foo+v2 (hash-of foo-impl-v2))
```

Workflows record the namespace they're pinned to in their history;
hot reload bumps the namespace, but the running workflow still
resolves names through the snapshot it took at start.

### Dependency graph

```scheme
(deps-of hash)               ;⇒ (list hash...)  ; direct deps
(transitive-deps hash)       ;⇒ list of all deps
(reverse-deps hash)          ;⇒ who depends on this hash
```

Used by:
- M08 workflow replay safety (refuse if any pinned dep is unreachable).
- The optimizer — invalidate cached compilations when deps change.
- Audit — "what was running 3 weeks ago when this incident happened?"

### Code transfer

`cs-distrib` (M02) fetches code by hash over the `bulk` channel.
Receiving node checks if hash is in local store; if missing,
requests the closure (the hash + transitive deps) from the sender.

### v1 minimum

- BLAKE3 hash function + canonicalization.
- Sled-backed store (rocksdb / postgres later).
- `hash-of`, `name->hash`, `hash->ast`, `deps-of` primops.
- Namespace API.
- Workflow integration (workflow start records pinned hash; replay refuses if hash unreachable).
- Defer: full migration graph, dependency-invalidation in the optimizer, distributed code fetch (M02 dependency).

### Code pointers

- `crates/cs-rir/src/lib.rs` — `CoreExpr` shape (the hashable thing).
- `crates/cs-pkg/src/lib.rs` — manifest + lockfile (similar storage shape).
- `crates/cs-hotreload/src/lib.rs` — existing version tracker (extend with hash awareness).

### External references

- Unison Code Mappings — <https://www.unison-lang.org/docs/the-big-idea/>
- IPFS content addressing — <https://docs.ipfs.tech/concepts/content-addressing/>
- BLAKE3 — <https://github.com/BLAKE3-team/BLAKE3>
