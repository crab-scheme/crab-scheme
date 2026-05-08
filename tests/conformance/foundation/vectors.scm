(test-section "R6RS §11.13 — vectors")

; Construction
(test-equal "vector-empty"   #()       (vector))
(test-equal "vector-3"       #(1 2 3)  (vector 1 2 3))

; make-vector
(test-equal "make-vector-fill" #(0 0 0)  (make-vector 3 0))
(test-eqv   "make-vector-len"   5        (vector-length (make-vector 5 'x)))

; Length
(test-eqv "vector-length-0"   0   (vector-length (vector)))
(test-eqv "vector-length-3"   3   (vector-length (vector 1 2 3)))

; Indexing
(test-eqv   "vector-ref-0"   1   (vector-ref (vector 1 2 3) 0))
(test-eqv   "vector-ref-2"   3   (vector-ref (vector 1 2 3) 2))

; Mutation
(define v (vector 1 2 3))
(vector-set! v 1 99)
(test-eqv   "vector-set"      99           (vector-ref v 1))
(test-equal "vector-after-set" #(1 99 3)   v)

; Fill
(define vf (vector 1 2 3 4 5))
(vector-fill! vf 0)
(test-equal "vector-fill"    #(0 0 0 0 0)  vf)

; Conversion
(test-equal "vector->list"   '(1 2 3)      (vector->list (vector 1 2 3)))
(test-equal "list->vector"   #(1 2 3)      (list->vector '(1 2 3)))
(test-equal "vector-roundtrip" #(a b c)    (list->vector (vector->list (vector 'a 'b 'c))))

; Predicate
(test-true  "vector-of-vec"  (vector? (vector)))
(test-false "vector-of-list" (vector? '(1 2 3)))
