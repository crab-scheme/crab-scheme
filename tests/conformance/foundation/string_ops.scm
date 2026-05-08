(test-section "Extended string operations")

; case
(test-equal "string-upcase"       "HELLO"     (string-upcase "hello"))
(test-equal "string-upcase-mixed" "ABC123"    (string-upcase "AbC123"))
(test-equal "string-downcase"     "world"     (string-downcase "WORLD"))
(test-equal "string-downcase-mixed" "abc123"  (string-downcase "AbC123"))

; ordering
(test-true  "string-lt"            (string<? "abc" "abd"))
(test-false "string-lt-equal"      (string<? "abc" "abc"))
(test-true  "string-le-equal"      (string<=? "abc" "abc"))
(test-true  "string-le-lt"         (string<=? "abc" "abd"))
(test-true  "string-gt"            (string>? "z" "a"))
(test-true  "string-ge-equal"      (string>=? "x" "x"))

; ordering chain
(test-true  "string-lt-chain"      (string<? "a" "b" "c"))
(test-false "string-lt-chain-bad"  (string<? "a" "c" "b"))
