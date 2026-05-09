(test-section "define-record-type with (parent ...) clause")

; Single inheritance: cpoint extends point with one extra field.
(define-record-type point (fields x y))
(define-record-type (cpoint mk-cpoint cpoint?)
  (parent point)
  (fields color))

(define p  (make-point 1 2))
(define cp (mk-cpoint 3 4 'red))

; Parent's own predicate accepts both parent and child instances.
(test-true  "point?-on-point"   (point? p))
(test-true  "point?-on-cpoint"  (point? cp))
; Child predicate rejects bare parent instances.
(test-false "cpoint?-on-point"  (cpoint? p))
(test-true  "cpoint?-on-cpoint" (cpoint? cp))

; Parent accessors still read parent slots in a child instance.
(test-eqv  "point-x-on-cpoint"  3 (point-x cp))
(test-eqv  "point-y-on-cpoint"  4 (point-y cp))
; Child's own accessor.
(test-equal "cpoint-color"      'red (cpoint-color cp))

; Multi-level chain: a -> b -> c
(define-record-type a (fields ax))
(define-record-type b (parent a) (fields by))
(define-record-type c (parent b) (fields cz))
(define ci (make-c 10 20 30))

(test-true  "a?-on-c"           (a? ci))
(test-true  "b?-on-c"           (b? ci))
(test-true  "c?-on-c"           (c? ci))
(test-eqv   "a-ax-on-c"         10 (a-ax ci))
(test-eqv   "b-by-on-c"         20 (b-by ci))
(test-eqv   "c-cz-on-c"         30 (c-cz ci))

; A b-instance is NOT a c.
(define bi (make-b 100 200))
(test-true  "a?-on-b"           (a? bi))
(test-true  "b?-on-b"           (b? bi))
(test-false "c?-on-b"           (c? bi))

; A bare a is not b or c.
(define ai (make-a 1))
(test-true  "a?-on-a"           (a? ai))
(test-false "b?-on-a"           (b? ai))
(test-false "c?-on-a"           (c? ai))

; Mutable field inherited through the chain. Define a parent with a mutable
; field; mutate via parent's mutator on a grandchild instance.
(define-record-type cell (fields (mutable val cell-get cell-set!)))
(define-record-type (named-cell mk-named named?)
  (parent cell)
  (fields name))
(define nc (mk-named 7 'first))
(test-eqv  "cell-get-on-named"  7 (cell-get nc))
(cell-set! nc 99)
(test-eqv  "cell-get-after-set" 99 (cell-get nc))
(test-equal "named-name"        'first (named-cell-name nc))

; Records can carry no own fields — child just narrows the type tag.
(define-record-type animal (fields species))
(define-record-type dog (parent animal))
(define d (make-dog 'canis))
(test-true  "dog?-on-dog"       (dog? d))
(test-true  "animal?-on-dog"    (animal? d))
(test-equal "animal-species-d"  'canis (animal-species d))

; Negative cases: predicate rejects unrelated values.
(test-false "point?-on-other-vec" (point? #(other 1 2)))
(test-false "point?-on-list"      (point? '(1 2 3)))
(test-false "point?-on-num"       (point? 42))
(test-false "point?-on-string"    (point? "hi"))
