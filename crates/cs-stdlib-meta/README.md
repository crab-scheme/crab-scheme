# cs-stdlib-meta — `(crab)` meta / introspection

> Iter 14 of the `stdlib-modules` spec — the unprefixed `(crab)`
> library, exposing which `(crab …)` modules the running runtime
> has compiled in.

## What it does

Two Scheme procedures, both purely introspective (no I/O, no
state):

| Procedure | Args | Returns |
|---|---|---|
| `(crab-list-modules)` | — | sorted list of module-name strings, e.g. `("base" "collection" "fs" …)` |
| `(crab-module-procedures name)` | string | list of procedure-name strings the module registered, or `#f` if the module isn't compiled in |

The list is derived at runtime from each enabled `cs-stdlib-*`
crate's own `procs()` result. There is no hand-maintained name
table to drift out of sync.

## Why not Scheme-side?

A pure-Scheme version (`define-list-of-modules` macro) would have
to know what's compiled in. The Rust side already does — the
build system enables `meta-<name>` features for whichever
sibling crates are also enabled, and the Rust macro inside
`manifest()` collapses each enabled crate to a single
`(name, procs)` entry via `cfg(feature = …)`. Subset embeds
(WASM, embedded) see exactly what they shipped.

## Wiring

Each `cs-runtime/stdlib-<name>` feature implies
`cs-stdlib-meta?/meta-<name>` via the `?` optional-dep syntax.
This means:

- An embed that enables only `stdlib-fs` (no umbrella) gets
  `cs-stdlib-meta` pulled in *only* if it also enables
  `stdlib-meta`; if it does, `(crab-list-modules)` returns
  `("fs")` — accurate to what's actually registered.
- The default `stdlib` umbrella turns on `stdlib-meta` plus all
  26 functional modules, so `(crab-list-modules)` returns the
  full set.

## Example

```scheme
(import (crab))

(for-each
  (lambda (name)
    (display name) (display " (")
    (display (length (crab-module-procedures name)))
    (display " procs)\n"))
  (crab-list-modules))
```

Output (on the default native build):

```
archive (4 procs)
base (5 procs)
collection (10 procs)
…
websocket (5 procs)
```

## Error model

- `(crab-list-modules)` with any arg → `ArityError`.
- `(crab-module-procedures)` with non-string → `TypeMismatch`.
- `(crab-module-procedures)` with unknown module → `#f` (not
  an error — lets callers probe with `if`).

## Feature flags

The crate's own `meta-<name>` features simply forward to the
sibling crate. Cs-runtime is the source of truth for the
combined wiring (see `crates/cs-runtime/Cargo.toml`).
