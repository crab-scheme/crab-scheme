(test-section "SRFI-1/13 higher-order ops (walker tier)")

;; ---- unzip / unzip2 ----
(call-with-values (lambda () (unzip '((1 a) (2 b) (3 c))))
  (lambda (a b)
    (test-equal "unzip-keys"   '(1 2 3)   a)
    (test-equal "unzip-vals"   '(a b c)   b)))

(call-with-values (lambda () (unzip2 '((10 100) (20 200))))
  (lambda (a b)
    (test-equal "unzip2-1" '(10 20)   a)
    (test-equal "unzip2-2" '(100 200) b)))

;; ---- circular-list constructor ----
(let ((c (circular-list 1 2 3)))
  (test-true  "circ-is-circular" (circular-list? c))
  (test-false "circ-not-proper"  (proper-list? c)))

;; ---- find-tail ----
(test-equal "find-tail-mid"     '(5 6 7)  (find-tail odd? '(2 4 5 6 7)))
(test-false "find-tail-none"             (find-tail odd? '(2 4 6 8)))
(test-equal "find-tail-first"   '(1 2 3) (find-tail odd? '(1 2 3)))

;; ---- reduce-right ----
(test-equal "reduce-right-cons" '(1 2 3 . 4) (reduce-right cons '() '(1 2 3 4)))
(test-equal "reduce-right-1"    7            (reduce-right + 0 '(7)))
(test-equal "reduce-right-empty" 99          (reduce-right + 99 '()))

;; ---- pair-fold / pair-fold-right / pair-for-each ----
(test-equal "pair-fold-len"     3 (pair-fold (lambda (p acc) (+ acc 1)) 0 '(a b c)))
(test-equal "pair-for-each-count"
            3
            (let ((n 0))
              (pair-for-each (lambda (p) (set! n (+ n 1))) '(x y z))
              n))

;; ---- string-fold / string-fold-right ----
(test-equal "string-fold"       '(#\c #\b #\a) (string-fold cons '() "abc"))
(test-equal "string-fold-right" '(#\a #\b #\c) (string-fold-right cons '() "abc"))
(test-equal "string-fold-slice" '(#\c #\b)     (string-fold cons '() "abcd" 1 3))

;; ---- string-tabulate ----
(test-equal "string-tabulate"   "ABCDE"
  (string-tabulate (lambda (i) (integer->char (+ 65 i))) 5))
(test-equal "string-tabulate-0" ""
  (string-tabulate (lambda (i) #\x) 0))

;; ---- vector-fold-right ----
(test-equal "vector-fold-right" '(1 2 3 4)
  (vector-fold-right cons '() #(1 2 3 4)))
(test-equal "vector-fold-right-empty" 'init
  (vector-fold-right cons 'init #()))

;; ---- unfold-right ----
(test-equal "unfold-right-squares"
  '(1 4 9 16 25)
  (unfold-right zero? (lambda (x) (* x x)) (lambda (x) (- x 1)) 5))
(test-equal "unfold-right-with-tail"
  '(1 4 9 :end)
  (unfold-right zero? (lambda (x) (* x x)) (lambda (x) (- x 1)) 3 '(:end)))
