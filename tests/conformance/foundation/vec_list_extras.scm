(test-section "vector-append, subvector, make-list, list-copy")

; --- vector-append ---
(test-equal "vec-append empty" #() (vector-append))
(test-equal "vec-append single" #(1 2 3) (vector-append #(1 2 3)))
(test-equal "vec-append two" #(1 2 3 4 5)
  (vector-append #(1 2) #(3 4 5)))
(test-equal "vec-append many" #(a b c d e f)
  (vector-append #(a) #(b c) #(d) #() #(e f)))

; not-a-vector raises
(test-true "vec-append rejects list"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (vector-append #(1 2) '(3 4)))))

; --- subvector ---
(test-equal "subvec mid" #(2 3) (subvector #(1 2 3 4) 1 3))
(test-equal "subvec full" #(1 2 3 4) (subvector #(1 2 3 4) 0 4))
(test-equal "subvec empty" #() (subvector #(1 2 3 4) 2 2))

; out-of-range raises
(test-true "subvec start>end raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (subvector #(1 2 3 4) 3 1))))
(test-true "subvec end>len raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (subvector #(1 2 3 4) 0 5))))

; --- make-list ---
(test-equal "make-list 3" '(#f #f #f) (make-list 3 #f))
(test-equal "make-list 0" '() (make-list 0))
(test-equal "make-list zero with fill" '() (make-list 0 'x))
(test-equal "make-list 5 sym" '(a a a a a) (make-list 5 'a))

(test-true "make-list neg raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (make-list -1))))

; --- list-copy ---
(define lst '(1 2 3))
(define cp  (list-copy lst))
(test-equal "list-copy equal"  '(1 2 3) cp)
(test-false "list-copy not eq" (eq? lst cp))
(test-true  "list-copy pairs differ"
  (not (eq? (cdr lst) (cdr cp))))

; mutation of copy doesn't affect original
(set-car! cp 99)
(test-eqv "original unchanged" 1 (car lst))

; copy of '() is '()
(test-equal "list-copy empty" '() (list-copy '()))

; improper list copy preserves the dotted tail
(define improper (cons 1 (cons 2 'tail)))
(define icp (list-copy improper))
(test-eqv "improper car" 1 (car icp))
(test-eqv "improper cdr" 2 (car (cdr icp)))
(test-equal "improper tail" 'tail (cdr (cdr icp)))
