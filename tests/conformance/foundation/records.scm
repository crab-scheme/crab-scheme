(test-section "R6RS records: define-record-type")

; Basic form
(define-record-type point (fields x y))
(define p (make-point 3 4))

(test-true  "point-pred-true"   (point? p))
(test-false "point-pred-num"    (point? 42))
(test-false "point-pred-list"   (point? '(1 2)))
(test-eqv   "point-x"           3 (point-x p))
(test-eqv   "point-y"           4 (point-y p))

; Extended form: explicit constructor + predicate names
(define-record-type (vec3 mk-vec3 vec3?) (fields x y z))
(define v (mk-vec3 1 2 3))
(test-true  "vec3-pred"  (vec3? v))
(test-eqv   "vec3-x"     1 (vec3-x v))
(test-eqv   "vec3-y"     2 (vec3-y v))
(test-eqv   "vec3-z"     3 (vec3-z v))
(test-false "vec3-not-point" (point? v))
(test-false "point-not-vec3" (vec3? p))

; Mutable fields
(define-record-type box
  (fields (mutable contents box-get box-set!)))
(define b (make-box 42))
(test-eqv "box-initial"   42 (box-get b))
(box-set! b 99)
(test-eqv "box-after-set" 99 (box-get b))

; Mixed mutable / immutable fields
(define-record-type counter
  (fields (immutable name counter-name)
          (mutable count counter-count counter-set-count!)))
(define c (make-counter "ticks" 0))
(test-equal "counter-name"    "ticks" (counter-name c))
(test-eqv   "counter-initial" 0       (counter-count c))
(counter-set-count! c 10)
(test-eqv   "counter-after"   10      (counter-count c))

; Records can be stored in lists, hashtables, etc.
(define points (list (make-point 1 2) (make-point 3 4) (make-point 5 6)))
(test-eqv "points-len"     3  (length points))
(test-eqv "points-first-x" 1  (point-x (car points)))
(test-equal "points-xs"
  '(1 3 5)
  (map point-x points))

; Filter records
(test-equal "points-filtered"
  '(3 4)
  (let ((p (find (lambda (pt) (> (point-x pt) 2)) points)))
    (list (point-x p) (point-y p))))
