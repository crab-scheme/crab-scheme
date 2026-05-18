# `(crab archive)` — tar / zip read + extract

CrabScheme stdlib module wrapping the `tar` and `zip` Rust crates.
Iter 7 of the stdlib-modules spec.

This iter's surface is intentionally narrow: list contents and
extract to disk. Programmatic archive *creation* needs a richer
Value model (per-entry metadata, streaming writes) and lands in a
follow-up iter alongside `Value::Opaque`. For now use `(crab
process)` with the host `tar` / `zip` binaries to construct
archives.

## Procedures

```
(tar-list path)                ;-> list of strings   ; entry paths
(tar-extract path dest-dir)    ;-> unspec            ; unpack into dest-dir

(tar-gz-list path)             ;-> list of strings   ; .tar.gz / .tgz
(tar-gz-extract path dest-dir) ;-> unspec

(zip-list path)                ;-> list of strings
(zip-extract path dest-dir)    ;-> unspec
```

## Example

```scheme
(import (crab archive))
(import (crab fs))

(directory-create-all "/tmp/release")
(tar-gz-extract "/tmp/release-1.2.3.tar.gz" "/tmp/release")

(display "extracted ")
(display (length (tar-gz-list "/tmp/release-1.2.3.tar.gz")))
(display " entries") (newline)
```
