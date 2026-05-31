; Conformance test for `(crab iter)` — itertools + lazy streams.

(test-section "(crab iter) — ranges")
(test-equal "range end" '(0 1 2 3) (range 4))
(test-equal "range start/end" '(2 3 4) (range 2 5))
(test-equal "range step" '(0 2 4 6 8) (range 0 10 2))
(test-equal "range negative step" '(5 4 3 2 1) (range 5 0 -1))

(test-section "(crab iter) — grouping & counting")
(test-equal "distinct dedups (equal?)" '(1 2 3) (distinct '(1 1 2 3 3 3)))
(test-equal "distinct works on strings" '("a" "b") (distinct '("a" "b" "a")))
(test-equal "group-by, first-seen key order"
            '((#t 2 4) (#f 1 3))
            (group-by even? '(2 4 1 3)))
(test-equal "frequencies counts occurrences" '((a . 2) (b . 1)) (frequencies '(a b a)))
(test-equal "count-if" 2 (count-if even? '(1 2 3 4 5)))

(test-section "(crab iter) — reshaping")
(test-equal "chunk into groups of n" '((1 2) (3 4) (5)) (chunk '(1 2 3 4 5) 2))
(test-equal "interleave stops at shortest" '(1 a 2 b) (interleave '(1 2 3) '(a b)))
(test-equal "flatten deeply" '(1 2 3 4) (flatten '(1 (2 (3)) 4)))
(test-equal "iterate n times" '(1 2 4 8) (iterate (lambda (x) (* x 2)) 1 4))

(test-section "(crab iter) — lazy streams")
(test-equal "naturals" '(0 1 2 3 4) (stream-take naturals 5))
(test-equal "stream-iterate" '(1 2 4 8)
            (stream-take (stream-iterate (lambda (x) (* x 2)) 1) 4))
(test-equal "stream-map over an infinite stream" '(0 2 4 6)
            (stream-take (stream-map (lambda (x) (* x 2)) naturals) 4))
(test-equal "stream-filter over an infinite stream" '(0 2 4 6)
            (stream-take (stream-filter even? naturals) 4))
(test-equal "stream-repeat" '(7 7 7) (stream-take (stream-repeat 7) 3))
