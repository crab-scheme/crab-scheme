# `(crab crypto)` — randomness, AEAD, Ed25519/X25519, HKDF, Argon2

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

(crypto-x25519-keypair)                    ;-> #(secret public)
(crypto-x25519-shared secret their-public) ;-> bytevector ; 32-byte ECDH secret
(crypto-hkdf-sha256 ikm salt info length)  ;-> bytevector ; derived key
(crypto-password-hash password)            ;-> string     ; Argon2id PHC string
(crypto-password-verify password phc)      ;-> boolean
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

## Key agreement, derivation, and passwords

X25519 ECDH gives both peers the same shared secret; run it through
HKDF before using it as a symmetric key (the raw DH output isn't
uniform). Argon2id hashes user passwords for storage.

```scheme
;; ECDH → HKDF → AEAD key
(define a (crypto-x25519-keypair))
(define b (crypto-x25519-keypair))
(define shared (crypto-x25519-shared (vector-ref a 0) (vector-ref b 1)))
(define key (crypto-hkdf-sha256 shared "" "chat-v1" 32))

;; Passwords
(define h (crypto-password-hash "hunter2"))
(crypto-password-verify "hunter2" h)        ; => #t
(crypto-password-verify "wrong" h)          ; => #f
```

[RustCrypto]: https://github.com/RustCrypto
[dalek]: https://github.com/dalek-cryptography
