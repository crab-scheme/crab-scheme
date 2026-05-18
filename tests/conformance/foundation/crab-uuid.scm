; Conformance test for `(crab uuid)` — stdlib-modules iter 5.

(test-section "(crab uuid) — generation")

(define __v4__ (uuid-v4))
(test-true  "uuid-v4 returns a string"     (string? __v4__))
(test-eqv   "uuid-v4 has 36 chars"          36 (string-length __v4__))
(test-true  "uuid-v4 is valid"             (uuid-valid? __v4__))
(test-eqv   "uuid-v4 has version 4"        4  (uuid-version __v4__))

(define __v7__ (uuid-v7))
(test-true  "uuid-v7 returns a string"     (string? __v7__))
(test-eqv   "uuid-v7 has 36 chars"          36 (string-length __v7__))
(test-true  "uuid-v7 is valid"             (uuid-valid? __v7__))
(test-eqv   "uuid-v7 has version 7"        7  (uuid-version __v7__))

; Two v4 draws collide with probability ≈ 2^-122.
(test-false "two uuid-v4 draws differ"
            (equal? __v4__ (uuid-v4)))

(test-section "(crab uuid) — parsing")

(test-true  "valid? on a known good string"
            (uuid-valid? "550e8400-e29b-41d4-a716-446655440000"))
(test-false "valid? on garbage"             (uuid-valid? "not-a-uuid"))
(test-false "valid? on partial"             (uuid-valid? "550e8400"))

(test-eqv "version of a known v4"
          4 (uuid-version "550e8400-e29b-41d4-a716-446655440000"))
(test-false "version of garbage returns #f" (uuid-version "garbage"))
