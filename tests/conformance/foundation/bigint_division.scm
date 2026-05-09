(test-section "quotient/remainder/modulo + div/mod on bignum operands")

; Reference: 2^100 mod small primes (verifiable by hand or with another impl).
(define big (expt 2 100))   ; 1267650600228229401496703205376

; --- quotient on bignum ---
(test-equal "quot-big-by-3"
  422550200076076467165567735125  ; 2^100 / 3
  (quotient big 3))
(test-equal "quot-big-by-7"
  181092942889747057356671886482  ; 2^100 / 7
  (quotient big 7))

; --- remainder on bignum ---
(test-eqv "rem-big-by-3" 1 (remainder big 3))
(test-eqv "rem-big-by-7" 2 (remainder big 7))
(test-eqv "rem-big-by-13" 3 (remainder big 13))

; --- modulo on bignum (matches remainder for positive operands) ---
(test-eqv "mod-big-by-3" 1 (modulo big 3))
(test-eqv "mod-big-by-7" 2 (modulo big 7))

; Negative dividend: modulo result has sign of divisor.
(test-eqv "mod-neg-big" 5 (modulo (- big) 7))
(test-eqv "rem-neg-big" -2 (remainder (- big) 7))

; --- R6RS div / mod (Euclidean — non-negative remainder) ---
(test-equal "div-big-by-3"
  422550200076076467165567735125
  (div big 3))
(test-eqv "mod-r6-big-by-3" 1 (mod big 3))

; Negative dividend with R6RS div: rounded toward -∞ vs truncated.
(test-equal "div-neg-big"
  -422550200076076467165567735126   ; truncated -...725 then -1
  (div (- big) 3))
(test-eqv "mod-r6-neg-big" 2 (mod (- big) 3))

; --- div-and-mod returns both via values ---
(call-with-values
  (lambda () (div-and-mod big 7))
  (lambda (d m)
    (test-equal "dam-d" 181092942889747057356671886482 d)
    (test-eqv   "dam-m" 2 m)))

; --- mixed bignum / fixnum ---
(test-equal "quot-fix-by-big"
  0   ; 5 / 2^100 = 0
  (quotient 5 big))
(test-eqv "rem-fix-by-big" 5 (remainder 5 big))

; --- division by zero still raises ---
(test-true "quot-big-zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (quotient big 0))))
(test-true "div-big-zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (div big 0))))

; --- regression: small values still work ---
(test-eqv "small-quot-17-5" 3 (quotient 17 5))
(test-eqv "small-rem-17-5"  2 (remainder 17 5))
(test-eqv "small-mod-neg-5" 3 (modulo -17 5))
(test-eqv "small-div-17-5"  3 (div 17 5))
(test-eqv "small-r6-mod"    3 (mod -17 5))

; --- (modulo x y) + (quotient x y) * y = x  identity, both for big and small ---
(define (q-r-identity x y)
  (= x (+ (remainder x y) (* y (quotient x y)))))
(test-true "qr-identity-big-pos"  (q-r-identity big 3))
(test-true "qr-identity-big-neg"  (q-r-identity (- big) 3))
(test-true "qr-identity-fix"      (q-r-identity 17 5))
