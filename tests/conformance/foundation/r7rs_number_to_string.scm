(test-section "R7RS number->string with radix and proper sign handling")

; --- decimal (default) ---
(test-equal "nts-int"      "42"    (number->string 42))
(test-equal "nts-zero"     "0"     (number->string 0))
(test-equal "nts-neg"      "-7"    (number->string -7))

; --- explicit decimal ---
(test-equal "nts-dec-arg"  "100"   (number->string 100 10))

; --- binary ---
(test-equal "nts-bin"      "1010"  (number->string 10 2))
(test-equal "nts-bin-zero" "0"     (number->string 0 2))
(test-equal "nts-bin-1"    "1"     (number->string 1 2))
(test-equal "nts-bin-neg"  "-1010" (number->string -10 2))

; --- octal ---
(test-equal "nts-oct"      "17"    (number->string 15 8))
(test-equal "nts-oct-neg"  "-17"   (number->string -15 8))

; --- hex ---
(test-equal "nts-hex"      "ff"    (number->string 255 16))
(test-equal "nts-hex-neg"  "-ff"   (number->string -255 16))
(test-equal "nts-hex-zero" "0"     (number->string 0 16))
(test-equal "nts-hex-large" "deadbeef" (number->string 3735928559 16))

; --- round-trip with string->number ---
(test-eqv "rt-decimal"  42    (string->number (number->string 42)))
(test-eqv "rt-binary"   42    (string->number (number->string 42 2) 2))
(test-eqv "rt-hex"      255   (string->number (number->string 255 16) 16))
(test-eqv "rt-neg-hex" -255   (string->number (number->string -255 16) 16))

; --- round-trip with prefix ---
(test-eqv "rt-prefix-hex" 255 (string->number (string-append "#x" (number->string 255 16))))
(test-eqv "rt-prefix-bin"  10 (string->number (string-append "#b" (number->string 10 2))))

; --- decimal floats render via Display ---
(test-true "nts-float"
  (string? (number->string 3.14)))

; --- non-integer with non-decimal radix raises ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (number->string 1.5 16))))))
(test-eqv "nts-float-hex" 'caught c1)

; --- unsupported radix raises ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (number->string 42 5))))))
(test-eqv "nts-bad-radix" 'caught c2)

; --- bignum hex / binary ---
(define big (expt 2 80))
(test-equal "nts-bignum-hex"
  "100000000000000000000"
  (number->string big 16))
(test-equal "nts-bignum-bin-len"
  81
  (string-length (number->string big 2)))
