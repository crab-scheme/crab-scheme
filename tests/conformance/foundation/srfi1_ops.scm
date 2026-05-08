(test-section "SRFI-1 / R6RS list-extras")

; iota
(test-equal "iota-5"          '(0 1 2 3 4)     (iota 5))
(test-equal "iota-from"       '(10 11 12)      (iota 3 10))
(test-equal "iota-step"       '(0 2 4 6 8)     (iota 5 0 2))
(test-equal "iota-empty"      '()              (iota 0))

; last / last-pair
(test-eqv   "last-3"          3                (last '(1 2 3)))
(test-eqv   "last-single"     'x               (last '(x)))
(test-equal "last-pair-of-3"  '(3)             (last-pair '(1 2 3)))

; take / drop
(test-equal "take-3"          '(1 2 3)         (take '(1 2 3 4 5) 3))
(test-equal "take-0"          '()              (take '(1 2 3) 0))
(test-equal "drop-2"          '(3 4 5)         (drop '(1 2 3 4 5) 2))
(test-equal "drop-all"        '()              (drop '(1 2 3) 3))

; zip
(test-equal "zip-2-lists"
  '((1 a) (2 b) (3 c))
  (zip '(1 2 3) '(a b c)))
(test-equal "zip-uneven"      '((1 a) (2 b))   (zip '(1 2 3) '(a b)))

; filter
(test-equal "filter-even"     '(2 4 6)         (filter even? '(1 2 3 4 5 6)))
(test-equal "filter-empty"    '()              (filter even? '(1 3 5)))
(test-equal "filter-all"      '(1 2 3)         (filter (lambda (x) #t) '(1 2 3)))

; fold-left
(test-eqv   "fold-left-sum"   15               (fold-left + 0 '(1 2 3 4 5)))
(test-eqv   "fold-left-prod"  120              (fold-left * 1 '(1 2 3 4 5)))
(test-equal "fold-left-rev"   '(3 2 1)         (fold-left (lambda (acc x) (cons x acc)) '() '(1 2 3)))

; fold-right
(test-equal "fold-right-cons" '(1 2 3)         (fold-right cons '() '(1 2 3)))
(test-eqv   "fold-right-sum"  15               (fold-right + 0 '(1 2 3 4 5)))

; reduce
(test-eqv   "reduce-sum"      15               (reduce + 0 '(1 2 3 4 5)))
(test-eqv   "reduce-empty"    99               (reduce + 99 '()))
(test-eqv   "reduce-single"   42               (reduce + 0 '(42)))
(test-eqv   "reduce-max"      9                (reduce max 0 '(3 1 4 1 5 9 2 6)))

; find
(test-eqv   "find-first"      4                (find (lambda (x) (> x 3)) '(1 2 3 4 5)))
(test-eqv   "find-missing"    #f               (find (lambda (x) (> x 100)) '(1 2 3)))

; count
(test-eqv   "count-evens"     3                (count even? '(1 2 3 4 5 6)))
(test-eqv   "count-empty"     0                (count even? '()))

; any / every
(test-true  "any-found"       (any (lambda (x) (> x 3)) '(1 2 4)))
(test-false "any-none"        (any (lambda (x) (> x 100)) '(1 2 3)))
(test-true  "every-all"       (every positive? '(1 2 3)))
(test-false "every-not-all"   (every positive? '(1 -1 3)))

; for-all (R6RS alias for every)
(test-true  "for-all"         (for-all (lambda (x) (> x 0)) '(1 2 3)))

; partition
(test-equal "partition-list"
  '((2 4 6) (1 3 5))
  (call-with-values
    (lambda () (partition even? '(1 2 3 4 5 6)))
    list))
