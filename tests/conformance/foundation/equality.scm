(test-section "R6RS §11.5 — equivalence predicates")

; eq?
(test-true  "eq-symbols"      (eq? 'foo 'foo))
(test-true  "eq-empty"        (eq? '() '()))
(test-true  "eq-true"         (eq? #t #t))
(test-false "eq-different-syms" (eq? 'foo 'bar))

; eqv?
(test-true  "eqv-fixnums"     (eqv? 5 5))
(test-true  "eqv-same-bool"   (eqv? #f #f))
(test-false "eqv-mixed"       (eqv? 1 #t))

; equal?
(test-true  "equal-lists"     (equal? '(1 2 3) '(1 2 3)))
(test-false "equal-lists-no"  (equal? '(1 2 3) '(1 2 4)))
(test-true  "equal-strings"   (equal? "hello" "hello"))
(test-true  "equal-empty"     (equal? '() '()))
(test-true  "equal-nested"    (equal? '((1) (2 3)) '((1) (2 3))))
(test-false "equal-nested-no" (equal? '((1) (2 3)) '((1) (2 4))))
