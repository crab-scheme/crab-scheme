; Conformance test for `(crab walk)` — tree transformation.

(define (double-nums x) (if (number? x) (* x 2) x))

(test-section "(crab walk) — postwalk / prewalk")
(test-equal "postwalk doubles nested numbers"
            '(2 (4 6) 8)
            (postwalk double-nums '(1 (2 3) 4)))
(test-equal "postwalk descends into vectors"
            #(2 4)
            (postwalk double-nums #(1 2)))
(test-equal "prewalk reaches the same leaves"
            '(2 (4 6) 8)
            (prewalk double-nums '(1 (2 3) 4)))
(test-equal "a bare atom passes through" 10 (postwalk double-nums 5))

(test-section "(crab walk) — replace")
(test-equal "postwalk-replace swaps matching nodes"
            '(x (y x))
            (postwalk-replace '((a . x) (b . y)) '(a (b a))))
(test-equal "prewalk-replace swaps matching nodes"
            '(1 (2 1))
            (prewalk-replace '((a . 1) (b . 2)) '(a (b a))))
