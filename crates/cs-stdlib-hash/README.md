# `(crab hash)` — Cryptographic hashes and HMAC

CrabScheme stdlib module wrapping `sha2` / `sha1` / `md-5` /
`blake3` / `hmac`. Iter 7 of the stdlib-modules spec. Supersedes
the `cs-ffi-sha2` example crate (which stays as the FFI-plugin
teaching example).

## Procedures

```
(hash-sha256 input)   ;-> bytevector  ; 32 bytes
(hash-sha512 input)   ;-> bytevector  ; 64 bytes
(hash-sha1 input)     ;-> bytevector  ; 20 bytes (legacy)
(hash-md5 input)      ;-> bytevector  ; 16 bytes (legacy; checksum/etag only)
(hash-blake3 input)   ;-> bytevector  ; 32 bytes

(hmac-sha256 key msg) ;-> bytevector  ; 32 bytes
```

`input`, `key`, and `msg` accept either a string (hashed as its
UTF-8 byte sequence) or a bytevector. Pair with `(crab base)` for
hex / base64 rendering.

## Example

```scheme
(import (crab hash))
(import (crab base))

(display (hex-encode (hash-sha256 "hello")))
;; 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824

(define tag (hmac-sha256 "shared-secret" "payload"))
(display (base64-encode tag)) (newline)
```
