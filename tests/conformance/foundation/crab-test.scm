; Conformance test for `(crab test)` — the bundled test framework.
; (run-tests) prints a line per test; the conformance harness reads its
; own __test-summary__, so that output is harmless noise here. We assert
; on run-tests' return value: (list passed failed).

(test-section "(crab test) — pass/fail tally")
(clear-tests!)
(deftest t-pass (assert-equal 4 (+ 2 2)))
(deftest t-fail (assert-equal 5 (+ 2 2)))
(test-equal "one pass, one fail" '(1 1) (run-tests))

(test-section "(crab test) — assertions")
(clear-tests!)
(deftest t-true (assert-true (> 3 2)))
(deftest t-false (assert-false (> 2 3)))
(deftest t-eqv (assert-eqv 'a 'a))
(deftest t-equal (assert-equal '(1 2) (list 1 2)))
(deftest t-raises-ok (assert-raises (error "boom")))
(deftest t-raises-bad (assert-raises (+ 1 2)))  ; no error raised -> fails
(test-equal "five assertions pass, assert-raises-without-error fails"
            '(5 1) (run-tests))

(test-section "(crab test) — empty suite")
(clear-tests!)
(test-equal "no tests registered" '(0 0) (run-tests))
