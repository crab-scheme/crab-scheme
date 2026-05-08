(test-section "R6RS §11 — control flow")

; if
(test-eqv   "if-true"         1     (if #t 1 2))
(test-eqv   "if-false"        2     (if #f 1 2))
(test-eqv   "if-truthy-num"   1     (if 0 1 2))   ; 0 is truthy in Scheme!
(test-eqv   "if-falsy-only-#f" 2    (if #f 1 2))

; cond
(test-eqv   "cond-first"      'a    (cond (#t 'a) (#t 'b)))
(test-eqv   "cond-second"     'b    (cond (#f 'a) (#t 'b)))
(test-eqv   "cond-else"       'c    (cond (#f 'a) (#f 'b) (else 'c)))

; case-style branching via cond
(define (classify n)
  (cond ((< n 0) 'negative)
        ((= n 0) 'zero)
        (else    'positive)))
(test-eqv "classify-neg"  'negative (classify -5))
(test-eqv "classify-zero" 'zero     (classify 0))
(test-eqv "classify-pos"  'positive (classify 7))

; and / or
(test-true  "and-empty"       (and))
(test-eqv   "and-last"        3     (and 1 2 3))
(test-false "and-short"       (and 1 #f 3))
(test-false "or-empty"        (or))
(test-eqv   "or-first-truthy" 2     (or #f 2 3))
(test-false "or-all-false"    (or #f #f))

; when / unless
(test-eqv   "when-true-body"  10    (when #t 10))
(test-eqv   "unless-false-body" 20  (unless #f 20))

; let / let* / letrec
(test-eqv   "let-binding"     5     (let ((x 2) (y 3)) (+ x y)))
(test-eqv   "let-nested"      10    (let ((x 2)) (let ((y (* x 5))) y)))
(test-eqv   "let*-sequential" 6     (let* ((x 2) (y (* x 3))) y))

; recursion + tail-call
(define (count-down n acc)
  (if (= n 0) acc (count-down (- n 1) (+ acc 1))))
(test-eqv "tail-call-bounded"  10000  (count-down 10000 0))

; closures
(define (make-adder n) (lambda (x) (+ x n)))
(define add10 (make-adder 10))
(test-eqv "closure-capture-1"  15  (add10 5))
(test-eqv "closure-capture-2"  17  (add10 7))
