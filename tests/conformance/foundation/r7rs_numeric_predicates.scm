(test-section "R7RS numeric type predicates: complex?, real?")

; --- complex? returns #t for any number (R7RS: all numbers are complex) ---
(test-true  "complex-int"     (complex? 42))
(test-true  "complex-neg"     (complex? -7))
(test-true  "complex-zero"    (complex? 0))
(test-true  "complex-float"   (complex? 3.14))
(test-true  "complex-rat"     (complex? 1/2))
(test-true  "complex-bigint"  (complex? (expt 2 80)))

; --- complex? returns #f for non-numbers ---
(test-false "complex-string"  (complex? "5"))
(test-false "complex-symbol"  (complex? 'a))
(test-false "complex-list"    (complex? '(1 2 3)))
(test-false "complex-bool"    (complex? #t))
(test-false "complex-char"    (complex? #\x))
(test-false "complex-null"    (complex? '()))

; --- real? same as complex? in our system (no complex numbers) ---
(test-true  "real-int"        (real? 42))
(test-true  "real-zero"       (real? 0))
(test-true  "real-float"      (real? 3.14))
(test-true  "real-rat"        (real? 1/2))
(test-true  "real-bigint"     (real? (expt 2 80)))

(test-false "real-string"     (real? "5"))
(test-false "real-symbol"     (real? 'a))
(test-false "real-bool"       (real? #f))

; --- numeric tower: integer? ⊆ rational? ⊆ real? ⊆ complex? ⊆ number? ---
(test-true "tower-int-rational"  (rational? 42))
(test-true "tower-int-real"      (real? 42))
(test-true "tower-int-complex"   (complex? 42))
(test-true "tower-rat-real"      (real? 1/2))
(test-true "tower-rat-complex"   (complex? 1/2))
(test-true "tower-real-complex"  (complex? 3.14))

; --- non-integer rational is not integer ---
(test-false "rational-non-integer-isnt-integer" (integer? 1/2))

; --- complex?, real? agree with number? on every numeric value ---
(define (matches-number? v)
  (and (eq? (number? v) (complex? v))
       (eq? (number? v) (real? v))))

(test-true "matches-int"     (matches-number? 42))
(test-true "matches-float"   (matches-number? 3.14))
(test-true "matches-rat"     (matches-number? 1/2))
(test-true "matches-string"  (matches-number? "x"))  ; both false
(test-true "matches-list"    (matches-number? '(1 2)))  ; both false

; --- arity errors ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (complex?))))))
(test-eqv "complex-arity-0" 'caught c1)

(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (real? 1 2))))))
(test-eqv "real-arity-2" 'caught c2)
