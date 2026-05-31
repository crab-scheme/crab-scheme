; Conformance test for `(crab crypto)` — randomness, AEAD, signatures.

(test-section "(crab crypto) — secure randomness")

(define rb (crypto-random-bytes 16))
(test-true "random-bytes is a bytevector" (bytevector? rb))
(test-equal "random-bytes length" 16 (bytevector-length rb))
(test-equal "random-bytes 0 is empty" 0 (bytevector-length (crypto-random-bytes 0)))
(test-true "two random draws differ"
           (not (equal? (crypto-random-bytes 16) (crypto-random-bytes 16))))

(define tok (crypto-random-token 24))
(test-true "random-token is a string" (string? tok))
(test-true "random-token is non-empty" (> (string-length tok) 0))

(test-section "(crab crypto) — constant-time compare")

(test-true "equal byte strings compare true"
           (crypto-constant-time=? "abc" "abc"))
(test-false "differing byte strings compare false"
            (crypto-constant-time=? "abc" "abd"))
(test-false "differing lengths compare false"
            (crypto-constant-time=? "abc" "abcd"))

(test-section "(crab crypto) — AEAD (ChaCha20-Poly1305)")

(define key (crypto-aead-keygen))
(define nonce (crypto-aead-nonce))
(test-equal "keygen is 32 bytes" 32 (bytevector-length key))
(test-equal "nonce is 12 bytes" 12 (bytevector-length nonce))

(define ct (crypto-aead-encrypt key nonce "secret message"))
(test-true "ciphertext is longer than plaintext (tag appended)"
           (> (bytevector-length ct)
              (bytevector-length (string->utf8 "secret message"))))
(test-equal "round-trip recovers the plaintext"
            "secret message"
            (utf8->string (crypto-aead-decrypt key nonce ct)))

(test-true "decrypt with a wrong key fails"
           (guard (e (#t #t))
             (crypto-aead-decrypt (crypto-aead-keygen) nonce ct)
             #f))

(test-true "decrypt of tampered ciphertext fails"
           (guard (e (#t #t))
             (let ((bad (bytevector-copy ct)))
               (bytevector-u8-set! bad 0 (modulo (+ 1 (bytevector-u8-ref bad 0)) 256))
               (crypto-aead-decrypt key nonce bad))
             #f))

(test-true "short key is rejected"
           (guard (e (#t #t))
             (crypto-aead-encrypt (crypto-random-bytes 16) nonce "x")
             #f))

; Associated data is authenticated but not encrypted.
(define ct-aad (crypto-aead-encrypt key nonce "msg" "context-v1"))
(test-equal "aad round-trip with matching aad"
            "msg"
            (utf8->string (crypto-aead-decrypt key nonce ct-aad "context-v1")))
(test-true "aad mismatch fails authentication"
           (guard (e (#t #t))
             (crypto-aead-decrypt key nonce ct-aad "context-v2")
             #f))

(test-section "(crab crypto) — Ed25519 signatures")

(define kp (crypto-ed25519-keypair))
(test-true "keypair is a 2-vector" (and (vector? kp) (= (vector-length kp) 2)))
(define sk (vector-ref kp 0))
(define pk (vector-ref kp 1))
(test-equal "secret key is 32 bytes" 32 (bytevector-length sk))
(test-equal "public key is 32 bytes" 32 (bytevector-length pk))

(define sig (crypto-ed25519-sign sk "the message"))
(test-equal "signature is 64 bytes" 64 (bytevector-length sig))
(test-true "verify accepts a valid signature"
           (crypto-ed25519-verify pk "the message" sig))
(test-false "verify rejects a tampered message"
            (crypto-ed25519-verify pk "the messagE" sig))
(test-false "verify rejects a foreign public key"
            (crypto-ed25519-verify (vector-ref (crypto-ed25519-keypair) 1)
                                   "the message" sig))
