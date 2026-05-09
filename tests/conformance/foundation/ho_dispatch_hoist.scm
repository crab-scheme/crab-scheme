(test-section "VM dispatch-hoist coverage: builtin proc passed to fold-right/filter/find/any/every/count/partition")

; --- fold-right with builtin `+` (single list) ---
(test-eqv "fold-right + sum"
  15  (fold-right + 0 (list 1 2 3 4 5)))

; fold-right with builtin `cons` rebuilds the list
(test-equal "fold-right cons identity"
  '(1 2 3) (fold-right cons '() '(1 2 3)))

; multi-list fold-right with builtin (zip-like via list*)
(test-equal "fold-right + 2 lists"
  '(11 22 33)
  (fold-right (lambda (a b acc) (cons (+ a b) acc))
              '()
              '(1 2 3) '(10 20 30)))

; --- filter with builtin pred ---
(test-equal "filter even?" '(2 4 6) (filter even? '(1 2 3 4 5 6)))
(test-equal "filter pair?" '((a) (b))
            (filter pair? (list 1 '(a) 2 '(b) 3)))

; --- find with builtin pred ---
(test-eqv "find even?" 2 (find even? '(1 2 3 4)))
(test-equal "find no-match" #f (find negative? '(1 2 3)))

; --- any / every with builtin pred ---
(test-true  "any even?"  (any even? '(1 3 5 6 7)))
(test-false "any neg?"   (any negative? '(1 2 3)))
(test-true  "every odd?" (every odd? '(1 3 5 7)))
(test-false "every odd?" (every odd? '(1 3 4 5)))

; --- count ---
(test-eqv "count even?"  3 (count even? '(1 2 3 4 5 6)))
(test-eqv "count zero?"  2 (count zero? '(1 0 2 0 3)))

; --- partition ---
(call-with-values
  (lambda () (partition even? '(1 2 3 4 5 6)))
  (lambda (yes no)
    (test-equal "partition yes" '(2 4 6) yes)
    (test-equal "partition no"  '(1 3 5) no)))

; --- mixed: closure (no hoist) still works ---
(test-equal "filter closure"
  '(10 20)
  (filter (lambda (x) (>= x 10)) '(1 5 10 7 20)))
(test-eqv "count closure"
  2
  (count (lambda (x) (> x 10)) '(1 5 10 11 15 8)))

; --- big lists make the hoist meaningful (correctness check) ---
(define (range n)
  (let loop ((i n) (acc '()))
    (if (= i 0) acc (loop (- i 1) (cons i acc)))))
(define big (range 1000))
(test-eqv "big fold-right + len" 500500 (fold-right + 0 big))
(test-eqv "big count even" 500 (count even? big))
(test-eqv "big any > 999" #t (any (lambda (x) (> x 999)) big))
(test-eqv "big every < 1001" #t (every (lambda (x) (< x 1001)) big))
