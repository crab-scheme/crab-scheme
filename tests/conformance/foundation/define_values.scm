(test-section "define-values (R6RS multiple-value top-level binding)")

; Fixed-arity binding from a multi-value producer.
(define-values (a b c) (values 1 2 3))
(test-eqv "dv-a" 1 a)
(test-eqv "dv-b" 2 b)
(test-eqv "dv-c" 3 c)

; Single-value binding still works.
(define-values (only) 42)
(test-eqv "dv-single" 42 only)

; Rest formals collect remaining values into a list.
(define-values (head . tail) (values 10 20 30 40))
(test-eqv   "dv-rest-head" 10 head)
(test-equal "dv-rest-tail" '(20 30 40) tail)

; Bare-symbol formals captures all values as one list.
(define-values everything (values 'x 'y 'z))
(test-equal "dv-bare-rest" '(x y z) everything)

; Empty-rest case (consumer of zero values).
(define-values nothing (values))
(test-equal "dv-empty-rest" '() nothing)

; The bound names are mutable in the usual top-level sense.
(define-values (m n) (values 100 200))
(set! m 999)
(test-eqv "dv-mutable-m" 999 m)
(test-eqv "dv-mutable-n" 200 n)

; Producer expression can be any thunk-yielding-multiple-values.
(define-values (x y)
  (call-with-values
    (lambda () (values 7 8))
    (lambda (a b) (values (* a 2) (* b 2)))))
(test-eqv "dv-producer-cwv-x" 14 x)
(test-eqv "dv-producer-cwv-y" 16 y)

; Single value passed where formals are a fixed list of one. (R6RS:
; expression yielding one value matches a one-name formals.)
(define-values (one-only) (* 3 7))
(test-eqv "dv-from-single-expr" 21 one-only)
