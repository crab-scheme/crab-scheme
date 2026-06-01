; Conformance for the (crab crypto) follow-ups: X25519 ECDH, HKDF,
; Argon2 password hashing. (Randomness / AEAD / Ed25519 are in
; crab-crypto.scm.)

(test-section "(crab crypto) — X25519 key agreement")
(define a (crypto-x25519-keypair))
(define b (crypto-x25519-keypair))
(define a-sk (vector-ref a 0))
(define a-pk (vector-ref a 1))
(define b-sk (vector-ref b 0))
(define b-pk (vector-ref b 1))
(test-equal "x25519 secret key is 32 bytes" 32 (bytevector-length a-sk))
(test-equal "x25519 public key is 32 bytes" 32 (bytevector-length a-pk))
; The defining property of Diffie-Hellman: both peers derive the same secret.
(test-equal "shared secrets agree on both sides"
            (crypto-x25519-shared a-sk b-pk)
            (crypto-x25519-shared b-sk a-pk))
(test-equal "shared secret is 32 bytes" 32 (bytevector-length (crypto-x25519-shared a-sk b-pk)))
(test-false "a different peer yields a different shared secret"
            (equal? (crypto-x25519-shared a-sk b-pk)
                    (crypto-x25519-shared a-sk (vector-ref (crypto-x25519-keypair) 1))))

(test-section "(crab crypto) — HKDF-SHA256")
(define k1 (crypto-hkdf-sha256 "ikm" "salt" "info" 32))
(test-equal "hkdf produces the requested length" 32 (bytevector-length k1))
(test-equal "hkdf is deterministic" k1 (crypto-hkdf-sha256 "ikm" "salt" "info" 32))
(test-false "hkdf varies with the info parameter"
            (equal? k1 (crypto-hkdf-sha256 "ikm" "salt" "other" 32)))
(test-equal "hkdf honors a larger length" 64 (bytevector-length (crypto-hkdf-sha256 "ikm" "salt" "info" 64)))

(test-section "(crab crypto) — Argon2 password hashing")
(define ph (crypto-password-hash "correct horse"))
(test-true "password-hash returns a (PHC) string" (string? ph))
(test-true "verify accepts the correct password" (crypto-password-verify "correct horse" ph))
(test-false "verify rejects a wrong password" (crypto-password-verify "wrong" ph))
(test-false "verify rejects a malformed hash" (crypto-password-verify "pw" "not-a-phc-string"))
(test-false "a random salt makes each hash unique"
            (string=? ph (crypto-password-hash "correct horse")))
