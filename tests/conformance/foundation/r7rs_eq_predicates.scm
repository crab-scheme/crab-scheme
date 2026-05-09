(test-section "R7RS variadic equality predicates: boolean=?, symbol=?")

; --- boolean=? two-arg ---
(test-true  "bool-eq-tt"  (boolean=? #t #t))
(test-true  "bool-eq-ff"  (boolean=? #f #f))
(test-false "bool-neq-tf" (boolean=? #t #f))
(test-false "bool-neq-ft" (boolean=? #f #t))

; --- boolean=? three-arg, all true / all same ---
(test-true  "bool-eq-3-true"  (boolean=? #t #t #t))
(test-true  "bool-eq-3-false" (boolean=? #f #f #f))
(test-false "bool-neq-3-mix"  (boolean=? #t #t #f))

; --- boolean=? variadic ---
(test-true  "bool-eq-5"     (boolean=? #t #t #t #t #t))
(test-false "bool-neq-5"    (boolean=? #t #t #t #f #t))

; --- boolean=? error on non-boolean ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (boolean=? #t 1))))))
(test-eqv "bool-eq-non-bool" 'caught c1)

; --- boolean=? arity error (< 2 args) ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (boolean=? #t))))))
(test-eqv "bool-eq-arity" 'caught c2)

; --- symbol=? two-arg ---
(test-true  "sym-eq-aa"   (symbol=? 'a 'a))
(test-false "sym-neq-ab"  (symbol=? 'a 'b))

; --- symbol=? three-arg ---
(test-true  "sym-eq-3"    (symbol=? 'foo 'foo 'foo))
(test-false "sym-neq-3"   (symbol=? 'foo 'foo 'bar))

; --- symbol=? variadic ---
(test-true  "sym-eq-5"    (symbol=? 'x 'x 'x 'x 'x))
(test-false "sym-neq-5"   (symbol=? 'x 'x 'x 'y 'x))

; --- symbol=? error on non-symbol ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (symbol=? 'a 42))))))
(test-eqv "sym-eq-non-sym" 'caught c3)

; --- symbol=? identity matches string->symbol ---
(test-true "sym-eq-interned"
  (symbol=? 'hello (string->symbol "hello")))

; --- symbol=? distinct interned vs constructed ---
(test-false "sym-neq-interned"
  (symbol=? 'foo (string->symbol "bar")))

(test-section "R7RS list-set!")

; --- list-set! at index 0 ---
(define lst1 (list 1 2 3 4 5))
(list-set! lst1 0 'A)
(test-equal "list-set-0" '(A 2 3 4 5) lst1)

; --- list-set! at middle ---
(define lst2 (list 'a 'b 'c 'd 'e))
(list-set! lst2 2 'C)
(test-equal "list-set-2" '(a b C d e) lst2)

; --- list-set! at last ---
(define lst3 (list 10 20 30))
(list-set! lst3 2 99)
(test-equal "list-set-last" '(10 20 99) lst3)

; --- list-set! returns unspecified, but should not error ---
(define lst4 (list 1 2 3))
(define r (list-set! lst4 1 'mid))
; r is unspecified — just verify the mutation happened
(test-equal "list-set-mutates" '(1 mid 3) lst4)

; --- list-set! out-of-range ---
(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (list-set! (list 1 2 3) 10 'x))))))
(test-eqv "list-set-out-of-range" 'caught c4)
