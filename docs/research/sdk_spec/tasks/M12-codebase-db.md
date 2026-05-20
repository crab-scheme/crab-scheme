# M12 — Content-addressed codebase DB (cs-codebase)

**Crates created:** `cs-codebase`.
**Effort:** 3-4 iters.
**Depends on:** M01 (effect-aware IR — hashing needs canonical post-expand AST).
**Numbered out-of-order:** Runs immediately after M01 because workflows (M08) depend on hash pinning.

## Goal

BLAKE3 over canonical `CoreExpr`; namespaces; dependency graph;
storage adapter (sled / rocksdb / postgres). Workflow runs pin
their code hashes so durable replay across hot upgrades is safe.

## Acceptance

- `(hash-of foo)` returns a stable hash that doesn't change with
  whitespace, comments, or renames of unreferenced callees.
- `(deps-of hash)` returns direct deps; `(transitive-deps hash)` does the closure.
- Hot-reloading `foo` produces a new hash without invalidating the old.
- Workflows pin the hash on start; replay refuses if hash is unreachable.
- cs-distrib (M02) `spawn-remote` fetches missing closures over the `bulk` channel.

## Iters

### A — BLAKE3 hash + canonicalization

- Resolve every `Identifier` to its target's hash (rename-resilient).
- Strip whitespace + comments; deterministic field order.
- **Code:** `crates/cs-codebase/src/hash.rs`. Reuse `cs-rir::CoreExpr`.

### B — Storage abstraction + sled backend

- `trait CodebaseStore { put, get, deps }`.
- `sled` for embedded; `rocksdb` and `postgres` behind features.

### C — Namespace API + `name → hash` mapping

- `(namespace name)`, `(namespace-bind! ns name hash)`.
- Workflows record namespace snapshot on start.

### D — Dependency graph + workflow integration

- `(deps-of h)`, `(reverse-deps h)`, `(transitive-deps h)`.
- M08 workflow start: record current hash + namespace; replay loads pinned.
- M02 spawn-remote: lookup closure hash; fetch missing if needed.

## Example

```scheme
(define foo (lambda (x) (* x 2)))
(hash-of foo)             ; ⇒ #blake3:abc123...
(deps-of (hash-of foo))   ; ⇒ '()

(define bar (lambda (x) (foo (+ x 1))))
(deps-of (hash-of bar))   ; ⇒ (list (hash-of foo))

;; Hot reload doesn't break old workflows:
(hot-reload! 'foo (lambda (x) (* x 3)))   ; new hash for foo
(hash-of foo)             ; ⇒ #blake3:def456...

;; A workflow started with the old hash still resolves through it.

;; Distributed code fetch — invisible:
(spawn-remote 'node-b@cluster bar)        ; node-b fetches bar's
                                          ; closure (bar + foo) over the
                                          ; bulk channel if not already cached
```

## External refs

- Unison "The Big Idea" — <https://www.unison-lang.org/docs/the-big-idea/>
- IPFS content addressing — <https://docs.ipfs.tech/concepts/content-addressing/>
- BLAKE3 — <https://github.com/BLAKE3-team/BLAKE3>
- sled — <https://github.com/spacejam/sled>

## Code pointers

- `crates/cs-rir/src/lib.rs` — `CoreExpr` (the hashable thing).
- `crates/cs-pkg/src/lib.rs` — manifest + lockfile (similar storage shape; possible code reuse).
- `crates/cs-hotreload/src/lib.rs` — existing version tracker (extend to cs-codebase awareness).
- `crates/cs-distrib/src/` (M02) — `bulk` channel for code fetch.
- `crates/cs-workflow/src/` (M08) — hash pinning on workflow start.
