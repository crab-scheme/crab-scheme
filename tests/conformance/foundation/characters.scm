(test-section "R6RS §11.11 — characters")

; Predicates
(test-true  "char-of-letter"     (char? #\a))
(test-true  "char-of-space"      (char? #\space))
(test-false "char-of-string"     (char? "a"))
(test-false "char-of-number"     (char? 65))

; Equality
(test-true  "char-eq"            (char=? #\a #\a))
(test-false "char-eq-no"         (char=? #\a #\b))
(test-true  "char-eq-multi"      (char=? #\x #\x #\x))

; Order
(test-true  "char-lt"            (char<? #\a #\b))
(test-false "char-lt-eq"         (char<? #\a #\a))
(test-true  "char-lt-chain"      (char<? #\a #\b #\c #\d))

; Conversion
(test-eqv "char->integer-A"      65    (char->integer #\A))
(test-eqv "char->integer-z"      122   (char->integer #\z))
(test-eqv "char->integer-space"  32    (char->integer #\space))
(test-eqv "integer->char-A"      #\A   (integer->char 65))
(test-eqv "integer->char-space"  #\space (integer->char 32))

; Roundtrip
(test-eqv "char-roundtrip-a"     #\a   (integer->char (char->integer #\a)))
(test-eqv "char-roundtrip-Z"     #\Z   (integer->char (char->integer #\Z)))
