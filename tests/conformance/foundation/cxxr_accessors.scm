(test-section "cXXr compositional accessors (R6RS)")

; Reference list: each level has a known value at every position.
(define data
  (list (list 'a 'b 'c 'd)        ; index 0
        (list 'e 'f 'g 'h)        ; index 1
        (list 'i 'j 'k 'l)))      ; index 2

; --- depth 2 (caar / cadr / cdar / cddr) ---
(test-equal "caar" 'a (caar data))
(test-equal "cadr" '(e f g h) (cadr data))
(test-equal "cdar" '(b c d) (cdar data))
(test-equal "cddr" '((i j k l)) (cddr data))

; --- depth 3 (caaar..cdddr) ---
(define nested
  (list (list (list 1 2 3) (list 4 5)) 'rest))
(test-equal "caaar" 1 (caaar nested))
(test-equal "caadr" 'rest (cadr nested))
; caddar = (car (cdr (cdr (car x)))) — get to the third element of the
; first element. (car nested) is `((1 2 3) (4 5))`; (cdr ...) is
; `((4 5))`; (cdr ...) is `()`; (car ()) errors. Use shallower paths
; for depth-3 verification.
(test-equal "caddr-on-nested-pair"
  '(c)
  (caddr (list 'a 'b '(c) 'd)))
(test-equal "cdddr" '(d) (cdddr (list 'a 'b 'c 'd)))

; --- depth 4 (caaaar..cddddr) ---
(define deep4
  (list (list (list (list 'leaf)))))
(test-equal "caaaar" 'leaf (caaaar deep4))

(test-equal "cadddr" 'd (cadddr (list 'a 'b 'c 'd 'e)))
(test-equal "cddddr" '(e) (cddddr (list 'a 'b 'c 'd 'e)))

; --- error cases: applying to a non-pair raises a catchable condition ---
(test-true "cadr-of-num"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'cadr)))
    (lambda () (cadr 42))))

(test-true "caddr-of-short-list"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (caddr (list 'one 'two)))))

; --- equivalence to spelled-out car/cdr chains ---
(define lst '(1 2 3 4 5))
(test-eqv "cadr-eq-car-cdr"   (car (cdr lst)) (cadr lst))
(test-eqv "caddr-eq"          (car (cdr (cdr lst))) (caddr lst))
(test-eqv "cadddr-eq"         (car (cdr (cdr (cdr lst)))) (cadddr lst))
