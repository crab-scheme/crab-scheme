; Conformance test for `(crab regex)` — stdlib-modules iter 4.

(test-section "(crab regex) — match/find")

(test-true  "match? hit"   (regex-match? "h.llo" "hello"))
(test-false "match? miss"  (regex-match? "world" "hello"))

(test-equal "find returns first matched text"
            "say"
            (regex-find "[a-z]+" "say hello there"))
(test-false "find returns #f on no match"
            (regex-find "[0-9]+" "letters only"))

(test-equal "find-all collects every match"
            '("foo" "bar" "baz")
            (regex-find-all "[a-z]+" "foo 1 bar 2 baz"))

(test-section "(crab regex) — replace")

(test-equal "replace first match only"
            "kept: 7"
            (regex-replace "[0-9]+" "1: 7" "kept"))

(test-equal "replace-all replaces every match"
            "X: X X"
            (regex-replace-all "[0-9]+" "1: 7 42" "X"))

(test-equal "replace-all no match no-op"
            "abc"
            (regex-replace-all "[0-9]+" "abc" "X"))

(test-section "(crab regex) — split")

(test-equal "split on whitespace runs"
            '("foo" "bar" "baz")
            (regex-split "\\s+" "foo  bar    baz"))

(test-equal "split keeps empty leading/trailing parts"
            '("" "a" "b" "")
            (regex-split "," ",a,b,"))

(test-section "(crab regex) — errors")

; Invalid regex pattern → host failure raised through the FFI layer.
; We can't `assert-error` here cleanly because the conformance
; harness doesn't ship one; using guard.
(test-true "invalid pattern raises"
           (guard (e (#t #t))
             (regex-match? "(unclosed" "irrelevant")
             #f))
