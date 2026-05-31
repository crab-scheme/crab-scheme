# `(crab crypto)` — secure randomness, AEAD encryption, Ed25519 signatures

CrabScheme stdlib module — modern, misuse-resistant cryptography
beyond the digests in `(crab hash)`. The `(crab …)` answer to Go's
`crypto/*`, Python's `secrets` + `cryptography`, and Clojure's
`buddy`.

All primitives are pure-Rust ([RustCrypto] + [dalek]) — no OpenSSL,
no C — so the module cross-compiles to `wasm32-wasip1` (it ships in
the `wasm-stdlib` feature). Cryptographic material (keys, nonces,
signatures) is passed and returned as **bytevectors**; data inputs
(plaintext, message, aad) accept a bytevector or a string.

## Procedures

```
(crypto-random-bytes n)                    ;-> bytevector ; n CSPRNG bytes
(crypto-random-token n)                    ;-> string     ; n bytes, URL-safe base64
(crypto-constant-time=? a b)               ;-> boolean    ; timing-safe compare

(crypto-aead-keygen)                       ;-> bytevector ; 32-byte key
(crypto-aead-nonce)                        ;-> bytevector ; 12-byte nonce
(crypto-aead-encrypt key nonce pt [aad])   ;-> bytevector ; ciphertext ‖ tag
(crypto-aead-decrypt key nonce ct [aad])   ;-> bytevector ; plaintext (raises on failure)

(crypto-ed25519-keypair)                   ;-> #(secret public)
(crypto-ed25519-sign secret message)       ;-> bytevector ; 64-byte signature
(crypto-ed25519-verify public message sig) ;-> boolean
```

## AEAD (ChaCha20-Poly1305)

A 32-byte key and a 12-byte nonce. **A `(key, nonce)` pair must never
be reused** — generate a fresh nonce per message with
`crypto-aead-nonce` and send it alongside the ciphertext (the nonce
is not secret). Optional `aad` is authenticated but not encrypted;
the same `aad` must be supplied to decrypt. Decryption raises on any
tampering, wrong key/nonce, or `aad` mismatch.

```scheme
(import (crab crypto))

(define key   (crypto-aead-keygen))
(define nonce (crypto-aead-nonce))
(define ct    (crypto-aead-encrypt key nonce "secret message"))

(utf8->string (crypto-aead-decrypt key nonce ct))   ; => "secret message"
```

## Ed25519 signatures

```scheme
(define kp  (crypto-ed25519-keypair))
(define sk  (vector-ref kp 0))   ; 32-byte secret
(define pk  (vector-ref kp 1))   ; 32-byte public
(define sig (crypto-ed25519-sign sk "msg"))

(crypto-ed25519-verify pk "msg" sig)        ; => #t
(crypto-ed25519-verify pk "tampered" sig)   ; => #f
```

## Scope

This first cut covers the "secure a message" core: randomness,
authenticated symmetric encryption, and signatures. Key agreement
(X25519 ECDH), key derivation (HKDF), and password hashing (Argon2)
are natural follow-ups.

[RustCrypto]: https://github.com/RustCrypto
[dalek]: https://github.com/dalek-cryptography
