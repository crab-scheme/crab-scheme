(test-section "SRFI-1 extras: cons* / take-while / span / filter-map / list-tabulate")

; --- cons* / list* (alias) ---
(test-equal "cons*-3-args" '(1 2 3 4 5) (cons* 1 2 3 (list 4 5)))
(test-equal "cons*-1-arg"  42 (cons* 42))
(test-equal "list*-alias"  '(a b c) (list* 'a 'b (list 'c)))

; cons* with empty tail returns the prefix as a proper list.
(test-equal "cons*-empty-tail" '(1 2) (cons* 1 2 '()))

; --- alist-copy ---
(define src (list (cons 1 'a) (cons 2 'b)))
(define dst (alist-copy src))
(test-equal "alist-copy-eq"  src dst)
(set-cdr! (car dst) 'mutated)
; Original is untouched (independent cons cells).
(test-equal "alist-copy-independent" '(1 . a) (car src))

; --- take-while / drop-while ---
(test-equal "take-while" '(1 3 5) (take-while odd? '(1 3 5 4 7)))
(test-equal "drop-while" '(4 7)   (drop-while odd? '(1 3 5 4 7)))
(test-equal "take-while-all"  '(1 3 5) (take-while odd? '(1 3 5)))
(test-equal "take-while-none" '()      (take-while odd? '(2 4 6)))
(test-equal "take-while-empty" '()     (take-while odd? '()))

; --- span / break (return values) ---
(call-with-values
  (lambda () (span odd? '(1 3 4 5 6)))
  (lambda (a b)
    (test-equal "span-prefix" '(1 3) a)
    (test-equal "span-rest"   '(4 5 6) b)))

(call-with-values
  (lambda () (break odd? '(2 4 5 6)))
  (lambda (a b)
    (test-equal "break-prefix" '(2 4) a)
    (test-equal "break-rest"   '(5 6) b)))

; --- list-index ---
(test-eqv "list-index-found" 2 (list-index even? '(1 3 4 7)))
(test-false "list-index-none" (list-index even? '(1 3 5 7)))
(test-eqv "list-index-multi-list" 1
  (list-index (lambda (a b) (= a b)) '(1 2 3) '(4 2 6)))

; --- filter-map ---
(test-equal "filter-map-squares-odd"
  '(1 9 25)
  (filter-map (lambda (x) (if (odd? x) (* x x) #f)) '(1 2 3 4 5)))

; --- append-map ---
(test-equal "append-map-double"
  '(a a b b c c)
  (append-map (lambda (x) (list x x)) '(a b c)))

; --- list-tabulate ---
(test-equal "list-tabulate-squares"
  '(0 1 4 9 16)
  (list-tabulate 5 (lambda (i) (* i i))))
(test-equal "list-tabulate-zero" '() (list-tabulate 0 (lambda (i) i)))

; --- error cases ---
(test-true "list-tabulate-rejects-negative"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'list-tabulate)))
    (lambda () (list-tabulate -1 (lambda (i) i)))))
