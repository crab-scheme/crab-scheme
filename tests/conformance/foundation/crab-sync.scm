; Conformance test for `(crab sync)` — atoms.

(test-section "(crab sync) — atoms")
(define a (make-atom 0))
(test-true "make-atom builds an atom" (atom? a))
(test-false "a number is not an atom" (atom? 5))
(test-equal "atom-deref reads the initial value" 0 (atom-deref a))

(atom-set! a 42)
(test-equal "atom-set! updates the value" 42 (atom-deref a))

(test-equal "atom-swap! returns the new value" 43 (atom-swap! a (lambda (x) (+ x 1))))
(test-equal "atom-swap! persists" 43 (atom-deref a))
(test-equal "atom-swap! threads extra args" 50 (atom-swap! a + 7))

(test-true "atom-cas! succeeds when current matches" (atom-cas! a 50 100))
(test-equal "atom-cas! stored the new value" 100 (atom-deref a))
(test-false "atom-cas! fails on mismatch" (atom-cas! a 999 0))
(test-equal "atom-cas! leaves the value unchanged after a failed swap" 100 (atom-deref a))
