# `(crab compress)` — zstd

CrabScheme stdlib module wrapping `zstd`. After the iter-17 split,
gzip + raw deflate live in `cs-stdlib-deflate` so the WASM build
can ship those without needing the zstd C toolchain.

Native default builds keep enabling both crates so the surface
`(gzip-compress …)`, `(deflate-compress …)`, `(zstd-compress …)`
keeps working without Scheme-side changes.

## Procedures (this crate)

```
(zstd-compress bv [level])           ;-> bytevector  ; level 1–22, default 3
(zstd-decompress bv [max-output])    ;-> bytevector  ; max default 64 MB
```

`max-output` is a decompression-bomb mitigation — caller-supplied
compressed input can expand 1 KB → several GB. Pass a larger cap
explicitly when processing trusted bulk data.

## Example

```scheme
(import (crab compress))
(import (crab fs))

(define raw (read-file-bytes "log.txt"))
(define zst (zstd-compress raw 9))
(write-file-bytes "log.txt.zst" zst)

(display "ratio: ")
(display (* 100.0 (/ (bytevector-length zst)
                     (bytevector-length raw))))
(display "%") (newline)
```

## WASM

This crate **does not build for `wasm32-wasip1`** in the default
dev env because `zstd-sys`' build script passes
`-fzero-call-used-regs` to clang which the nix-wrapped clang
rejects for the wasm target. For WASM use
`cs-stdlib-deflate` (gzip + raw deflate) instead.
