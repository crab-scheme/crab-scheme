; Conformance test for `(crab functional)` — combinators.

(define inc (lambda (x) (+ x 1)))
(define dbl (lambda (x) (* x 2)))

(test-section "(crab functional) — composition")
(test-equal "compose is right-to-left" 11 ((compose inc dbl) 5))
(test-equal "pipe is left-to-right" 12 ((pipe inc dbl) 5))
(test-equal "compose of nothing is identity" 7 ((compose) 7))
(test-equal "identity returns its argument" 42 (identity 42))

(test-section "(crab functional) — application")
(test-equal "partial prepends arguments" 6 ((partial + 1 2) 3))
(test-equal "constantly ignores arguments" 9 ((constantly 9) 1 2 3))
(test-equal "complement negates a predicate" #t ((complement even?) 3))
(test-equal "flip swaps a binary procedure" 1 ((flip -) 2 3))
(test-equal "juxt collects results" '(5 6) ((juxt + *) 2 3))

(test-section "(crab functional) — fnil + memoize")
(define safe-inc (fnil inc 0))
(test-equal "fnil replaces #f with the default" 1 (safe-inc #f))
(test-equal "fnil passes a non-#f argument through" 6 (safe-inc 5))

(define *calls* 0)
(define counted (lambda (x) (set! *calls* (+ *calls* 1)) (* x x)))
(define m (memoize counted))
(test-equal "memoize returns the value" 25 (m 5))
(test-equal "memoize returns the cached value" 25 (m 5))
(test-equal "memoize computed only once" 1 *calls*)
