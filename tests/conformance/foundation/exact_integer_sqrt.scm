(test-section "R6RS exact-integer-sqrt: multi-value + bignum")

; --- small integers, perfect square ---
(call-with-values (lambda () (exact-integer-sqrt 0))
  (lambda (s r)
    (test-eqv "eis-0-s" 0 s)
    (test-eqv "eis-0-r" 0 r)))
(call-with-values (lambda () (exact-integer-sqrt 1))
  (lambda (s r)
    (test-eqv "eis-1-s" 1 s)
    (test-eqv "eis-1-r" 0 r)))
(call-with-values (lambda () (exact-integer-sqrt 81))
  (lambda (s r)
    (test-eqv "eis-81-s" 9 s)
    (test-eqv "eis-81-r" 0 r)))

; --- non-square ---
(call-with-values (lambda () (exact-integer-sqrt 50))
  (lambda (s r)
    (test-eqv "eis-50-s" 7 s)   ; 7² = 49 ≤ 50 < 64 = 8²
    (test-eqv "eis-50-r" 1 r)))

; --- identity: n = s² + r ---
(define (eis-identity n)
  (call-with-values (lambda () (exact-integer-sqrt n))
    (lambda (s r) (= n (+ (* s s) r)))))
(test-true "eis-id 0"     (eis-identity 0))
(test-true "eis-id 1"     (eis-identity 1))
(test-true "eis-id 50"    (eis-identity 50))
(test-true "eis-id 12345" (eis-identity 12345))
(test-true "eis-id 99999" (eis-identity 99999))

; --- bignum input: 2^100 ---
(define big2-100 (expt 2 100))
(call-with-values (lambda () (exact-integer-sqrt big2-100))
  (lambda (s r)
    ; s = 2^50, r = 0 (perfect square)
    (test-equal "eis-2^100-s" (expt 2 50) s)
    (test-eqv "eis-2^100-r" 0 r)))

; --- bignum non-square: 2^100 + 1 ---
(call-with-values (lambda () (exact-integer-sqrt (+ big2-100 1)))
  (lambda (s r)
    (test-equal "eis-bignum-non-sq-s" (expt 2 50) s)
    (test-eqv "eis-bignum-non-sq-r" 1 r)))

; identity holds for bignum too
(test-true "eis-id big" (eis-identity big2-100))
(test-true "eis-id big+1" (eis-identity (+ big2-100 1)))
(test-true "eis-id big+big" (eis-identity (+ big2-100 big2-100)))

; --- negative input raises ---
(test-true "eis-negative-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (exact-integer-sqrt -1))))
(test-true "eis-negative-bignum-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (exact-integer-sqrt (- big2-100)))))
