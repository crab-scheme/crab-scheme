# `(crab random)` — Random number generation

CrabScheme stdlib module wrapping the `rand` crate.  Iter 5 of the
stdlib-modules spec.

This iter exposes the thread-local default RNG (`rand::thread_rng()`),
which is cryptographically seeded from the OS per Rust's `ThreadRng`
contract — fine for everything from session IDs to nonces. A
seedable `make-random-source` returning a typed handle lands when
the FFI gains an opaque-payload Scheme value.

## Procedures

```
(random-bytes n)          ;-> bytevector  ; cryptographic
(random-integer n)        ;-> fixnum      ; in [0, n); errors if n ≤ 0
(random-flonum)           ;-> flonum      ; in [0.0, 1.0)
(random-choice list)      ;-> value       ; uniform sample; errors on empty
```

## Example

```scheme
(import (crab random))
(import (crab base))      ; once iter 6 ships hex-encode

;; 256-bit nonce as hex
(display (random-bytes 32))     ; raw bytevector for now

;; random pick
(display (random-choice '("alice" "bob" "carol")))
```
