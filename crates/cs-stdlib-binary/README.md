# `(crab binary)` — struct-style binary pack/unpack

CrabScheme stdlib module — the `(crab …)` answer to Python's `struct`
and Go's `encoding/binary`. R6RS bytevectors already give the
per-element primitives; this is the convenient format-string layer on
top, for wire protocols and file headers. Pure Rust, no dependencies,
wasm-portable.

## Format string

One-character type codes, optionally preceded by an endianness marker
that stays in effect until changed. Spaces are ignored.

- Endianness: `>` / `!` big-endian (default, network order), `<` little-endian
- Integers: `b`/`B` 8-bit, `h`/`H` 16-bit, `i`/`I` 32-bit, `q`/`Q` 64-bit (lower = signed)
- Floats: `f` 32-bit, `d` 64-bit

## Procedures

```
(binary-pack fmt value …)   ;-> bytevector  ; one value per code
(binary-unpack fmt bv)      ;-> list         ; values in format order
(binary-size fmt)           ;-> fixnum       ; packed byte length
```

## Example

```scheme
(import (crab binary))

(define hdr (binary-pack ">ihb" 1000000 -5 65))   ; 4 + 2 + 1 = 7 bytes
(binary-unpack ">ihb" hdr)                         ; => (1000000 -5 65)
(binary-size ">ihb")                               ; => 7

(binary-pack "<H" 258)   ; little-endian: bytes 02 01
```

Out-of-range values, a value-count mismatch, or a too-short buffer all
raise an error.
