# cs-stdlib-deflate — `(crab deflate)` / gzip + raw deflate

> Iter 17 of the `stdlib-modules` spec — flate2-based codecs
> split out of `cs-stdlib-compress` so the WASM build can ship
> gzip/deflate without needing `zstd-sys`.

## What it does

Four Scheme procedures exposing flate2:

| Procedure | Args | Returns |
|---|---|---|
| `gzip-compress`      | bv [level] | gzip-framed bytevector (level 0–9; default 6) |
| `gzip-decompress`    | bv [max]   | bytevector (raises if output > max; default 64 MB) |
| `deflate-compress`   | bv [level] | raw deflate bytevector (no gzip header) |
| `deflate-decompress` | bv [max]   | bytevector (raises if output > max; default 64 MB) |

`max` is a decompression-bomb mitigation: caller-controlled
compressed input can otherwise expand 1 KB → several GB with no
warning. Pass a larger explicit `max` when processing trusted
bulk archives.

## Why split this out of `cs-stdlib-compress`?

The pre-split crate bundled flate2 + zstd. `zstd-sys`'s C build
script passes `-fzero-call-used-regs` to clang which the
nix-wrapped clang in our dev env rejects when targeting
`wasm32-wasip1`. So the entire crate failed to compile for
WASM, taking gzip/deflate down with it even though flate2 is
pure-Rust and WASM-portable.

Splitting keeps zstd in `cs-stdlib-compress` (for native builds
that want it) and gives WASM users a WASM-safe alternative
through `cs-stdlib-deflate`.

## Scheme-side compatibility

Procedure names are unchanged. A caller that does
`(gzip-compress …)` works as long as either crate registers it,
which means default native builds (both crates enabled) work
exactly as before the split. WASM users enable only
`stdlib-deflate` and lose `(zstd-compress …)` / `(zstd-decompress …)`.
