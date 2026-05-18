# `(crab compress)` — gzip / deflate / zstd

CrabScheme stdlib module wrapping `flate2` (gzip, deflate) and
`zstd`. Iter 7 of the stdlib-modules spec.

## Procedures

```
(gzip-compress bv [level])      ;-> bytevector   ; level 0–9, default 6
(gzip-decompress bv)            ;-> bytevector
(deflate-compress bv [level])   ;-> bytevector   ; raw deflate (no gzip header)
(deflate-decompress bv)         ;-> bytevector
(zstd-compress bv [level])      ;-> bytevector   ; level 1–22, default 3
(zstd-decompress bv)            ;-> bytevector
```

All procedures slurp the full input into memory and emit a single
output bytevector. Port-wrapping streaming variants land with
`Value::Opaque`.

## Example

```scheme
(import (crab compress))
(import (crab fs))

(define raw (read-file-bytes "log.txt"))
(define gz (gzip-compress raw 9))
(write-file-bytes "log.txt.gz" gz)

(display "ratio: ")
(display (* 100.0 (/ (bytevector-length gz)
                     (bytevector-length raw))))
(display "%") (newline)
```
