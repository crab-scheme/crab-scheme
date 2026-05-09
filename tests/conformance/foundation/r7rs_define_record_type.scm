(test-section "R7RS define-record-type shape")

; --- minimal: type + constructor + predicate + accessors ---
(define-record-type Point
  (make-point x y)
  point?
  (x point-x)
  (y point-y))

(define p (make-point 3 4))
(test-true  "r7-pt-pred"   (point? p))
(test-eqv   "r7-pt-x"  3   (point-x p))
(test-eqv   "r7-pt-y"  4   (point-y p))
(test-false "r7-pt-not-pair" (point? '(3 . 4)))

; --- with mutator ---
(define-record-type Box
  (make-box val)
  box?
  (val box-val set-box-val!))

(define b (make-box 'hello))
(test-equal "r7-box-val-init" 'hello (box-val b))
(set-box-val! b 'world)
(test-equal "r7-box-val-mutated" 'world (box-val b))

; --- mixed: some fields with mutators, some without ---
(define-record-type Person
  (make-person name age)
  person?
  (name person-name)
  (age  person-age set-person-age!))

(define alice (make-person "Alice" 30))
(test-equal "r7-person-name" "Alice" (person-name alice))
(test-eqv   "r7-person-age"  30      (person-age alice))
(set-person-age! alice 31)
(test-eqv   "r7-person-age-mut" 31   (person-age alice))

; --- empty constructor + no fields ---
(define-record-type Empty
  (make-empty)
  empty?)

(define e (make-empty))
(test-true "r7-empty-pred" (empty? e))
(test-false "r7-empty-rejects-other" (empty? 42))

; --- two records with non-overlapping field names cohabit safely ---
(define-record-type A (make-a a-x) a? (a-x a-x-of))
(define-record-type B (make-b b-x) b? (b-x b-x-of))
(define a (make-a 1))
(define b2 (make-b 2))
(test-eqv "r7-a-x" 1 (a-x-of a))
(test-eqv "r7-b-x" 2 (b-x-of b2))
(test-false "r7-a-not-b" (b? a))
(test-false "r7-b-not-a" (a? b2))

; --- predicate distinguishes from non-records ---
(test-false "r7-pt-not-int"   (point? 5))
(test-false "r7-pt-not-pair"  (point? '(1 . 2)))
(test-false "r7-pt-not-vec"   (point? #(1 2)))
(test-false "r7-pt-not-empty" (point? '()))

; --- error: constructor field name not in field-specs ---
; This is an expand-time error. We can't easily test it from inside
; the runtime without harness support; trust the parser comment.

; --- R6RS-shape DRT still works (regression) ---
(define-record-type (Foo make-foo foo?)
  (fields (immutable val foo-val)))
(define f (make-foo 99))
(test-eqv "r6-still-works" 99 (foo-val f))
