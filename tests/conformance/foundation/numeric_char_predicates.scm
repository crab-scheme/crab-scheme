(test-section "fixnum?/flonum?/rational? + char-foldcase/titlecase + digit-value")

; --- numeric type predicates ---
(test-true  "fixnum-int"     (fixnum? 42))
(test-true  "fixnum-zero"    (fixnum? 0))
(test-true  "fixnum-neg"     (fixnum? -100))
(test-false "fixnum-flonum"  (fixnum? 1.5))
(test-false "fixnum-string"  (fixnum? "no"))

(test-true  "flonum-real"    (flonum? 1.5))
(test-true  "flonum-zero"    (flonum? 0.0))
(test-false "flonum-int"     (flonum? 42))
(test-false "flonum-symbol"  (flonum? 'x))

; rational?: exact integers and rationals always; flonums when finite.
(test-true  "rational-int"     (rational? 42))
(test-true  "rational-frac"    (rational? 3/4))
(test-true  "rational-flonum"  (rational? 1.5))
(test-false "rational-string"  (rational? "no"))

; --- numeric? umbrella still works alongside the new predicates ---
(test-true "number-int"  (number? 42))
(test-true "number-float" (number? 1.5))
(test-true "integer-int"  (integer? 42))
(test-false "integer-float" (integer? 1.5))

; --- char-foldcase / char-titlecase ---
(test-equal "foldcase-A"   #\a (char-foldcase #\A))
(test-equal "foldcase-a"   #\a (char-foldcase #\a))
(test-equal "titlecase-b"  #\B (char-titlecase #\b))
(test-equal "titlecase-B"  #\B (char-titlecase #\B))
(test-equal "foldcase-num" #\7 (char-foldcase #\7))   ;; non-letter unchanged

; --- digit-value ---
(test-eqv   "digit-7"    7 (digit-value #\7))
(test-eqv   "digit-0"    0 (digit-value #\0))
(test-eqv   "digit-9"    9 (digit-value #\9))
(test-false "digit-a"      (digit-value #\a))
(test-false "digit-space"  (digit-value #\space))

; --- error cases ---
(test-true "fixnum?-1-arg"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'fixnum?)))
    (lambda () (fixnum? 1 2))))

(test-true "char-foldcase-rejects-num"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'char-foldcase)))
    (lambda () (char-foldcase 42))))

(test-true "digit-value-rejects-num"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (digit-value 42))))
