# `(crab base)` — base64 / hex encoding

CrabScheme stdlib module wrapping the `base64` and `hex` Rust
crates. Iter 6 of the stdlib-modules spec.

```
(base64-encode bv)        ;-> string  ; standard alphabet, padded
(base64-decode str)       ;-> bytevector
(base64url-encode bv)     ;-> string  ; URL-safe, no padding
(base64url-decode str)    ;-> bytevector
(hex-encode bv)           ;-> string  ; lowercase
(hex-decode str)          ;-> bytevector ; accepts upper or lower
```

## Example

```scheme
(import (crab base))
(import (crab random))
(import (crab hash))   ; iter 7

;; 256-bit nonce, hex-encoded
(display (hex-encode (random-bytes 32))) (newline)

;; base64-encoded payload for an API call
(display (base64-encode (string->utf8 "hello")))
;; aGVsbG8=
```
