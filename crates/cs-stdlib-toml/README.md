# `(crab toml)` — TOML read/write

CrabScheme stdlib module wrapping the `toml` Rust crate. Iter 6
of the stdlib-modules spec. Mapping mirrors `(crab json)` — alists
for tables, lists for arrays.

```
(toml-parse str)         ;-> scheme value  ; errors on malformed input
(toml-stringify val)     ;-> string        ; canonical form
```

Datetimes round-trip as RFC-3339 strings until `Value::Opaque`
lands. Non-table top-level values are wrapped in a single-key
table named `value` so `toml-stringify` is total over any
TOML-encodable input.

## Example

```scheme
(import (crab toml))
(import (crab fs))

(define cfg (toml-parse (read-file-string "Cargo.toml")))
(define pkg (cdr (assoc "package" cfg)))
(display (cdr (assoc "name" pkg))) (newline)
```
