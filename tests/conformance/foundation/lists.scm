(test-section "R6RS §11.9 — pairs and lists")

; cons / car / cdr
(test-eqv   "car-cons"        1          (car (cons 1 2)))
(test-eqv   "cdr-cons"        2          (cdr (cons 1 2)))
(test-equal "cons-list"       '(1 2 3)   (cons 1 (cons 2 (cons 3 '()))))

; list / length
(test-equal "list-of-3"       '(1 2 3)   (list 1 2 3))
(test-eqv   "length-empty"    0          (length '()))
(test-eqv   "length-3"        3          (length '(a b c)))
(test-eqv   "length-7"        7          (length '(1 2 3 4 5 6 7)))

; reverse
(test-equal "reverse-3"       '(3 2 1)   (reverse '(1 2 3)))
(test-equal "reverse-empty"   '()        (reverse '()))

; append
(test-equal "append-2"        '(1 2 3 4) (append '(1 2) '(3 4)))
(test-equal "append-empty"    '(1 2)     (append '() '(1 2)))
(test-equal "append-many"     '(1 2 3 4 5 6) (append '(1) '(2 3) '(4 5 6)))
(test-equal "append-one"      '(a b)     (append '(a b)))

; list-tail / list-ref
(test-equal "list-tail-2"     '(c d e)   (list-tail '(a b c d e) 2))
(test-eqv   "list-ref-0"      'a         (list-ref '(a b c) 0))
(test-eqv   "list-ref-2"      'c         (list-ref '(a b c) 2))

; predicates
(test-true  "pair-of-pair"    (pair? (cons 1 2)))
(test-false "pair-of-null"    (pair? '()))
(test-true  "null-of-empty"   (null? '()))
(test-false "null-of-pair"    (null? (cons 1 2)))

; map / for-each
(test-equal "map-square"      '(1 4 9 16 25)  (map (lambda (x) (* x x)) '(1 2 3 4 5)))
(test-equal "map-2-lists"     '(11 22 33)     (map + '(1 2 3) '(10 20 30)))

; apply
(test-eqv   "apply-list"      15         (apply + '(1 2 3 4 5)))
(test-eqv   "apply-with-prefix" 15       (apply + 1 2 '(3 4 5)))
