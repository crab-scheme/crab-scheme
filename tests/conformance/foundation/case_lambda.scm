(test-section "case-lambda — arity-dispatched procedures")

; Basic 1-clause: same as lambda.
(define id (case-lambda ((x) x)))
(test-eqv "case-lambda-1clause" 42 (id 42))

; Multi-arity: pick by argument count.
(define area
  (case-lambda
    (() 0)                              ; nullary
    ((r) (* 3 r r))                     ; 1-arg circle approximation
    ((w h) (* w h))                     ; 2-arg rectangle
    ((w h d) (* w h d))))               ; 3-arg box
(test-eqv "case-lambda-0arg"     0    (area))
(test-eqv "case-lambda-1arg-r2"  12   (area 2))
(test-eqv "case-lambda-2arg"     20   (area 4 5))
(test-eqv "case-lambda-3arg"     60   (area 3 4 5))

; Rest pattern: matches any arity ≥ N fixed.
(define sum-rest
  (case-lambda
    (() 0)
    ((x . rest) (apply + x rest))))
(test-eqv "case-lambda-rest-empty" 0  (sum-rest))
(test-eqv "case-lambda-rest-one"   1  (sum-rest 1))
(test-eqv "case-lambda-rest-many" 15  (sum-rest 1 2 3 4 5))

; Rest pattern in a middle-arity clause.
(define dispatch
  (case-lambda
    ((x) (list 'one x))
    ((x y) (list 'two x y))
    (args (list 'many args))))
(test-equal "case-lambda-one"  '(one 7)        (dispatch 7))
(test-equal "case-lambda-two"  '(two 7 8)      (dispatch 7 8))
(test-equal "case-lambda-many" '(many (7 8 9)) (dispatch 7 8 9))

; First matching clause wins.
(define order-test
  (case-lambda
    ((x) 'first)
    ((x) 'second)))                     ; never reached
(test-eqv "case-lambda-first-wins" 'first (order-test 1))

; Closes over enclosing scope correctly.
(define (make-counter)
  (let ((n 0))
    (case-lambda
      (() n)
      ((d) (set! n (+ n d)) n))))
(define c (make-counter))
(test-eqv "case-lambda-counter-init"     0  (c))
(c 3)
(c 4)
(test-eqv "case-lambda-counter-after"    7  (c))
