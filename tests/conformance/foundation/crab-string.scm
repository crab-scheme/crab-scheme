; Conformance test for `(crab string)` — stdlib-modules iter 4.

(test-section "(crab string) — split/join")

(test-equal "split by space"
            '("foo" "bar" "baz")
            (string-split "foo bar baz" " "))
(test-equal "split by comma"
            '("a" "b" "c")
            (string-split "a,b,c" ","))
(test-equal "split empty sep yields chars"
            '("a" "b" "c")
            (string-split "abc" ""))

(test-equal "join two"
            "foo,bar"
            (string-join '("foo" "bar") ","))
(test-equal "join three"
            "a-b-c"
            (string-join '("a" "b" "c") "-"))
(test-equal "join empty list"
            ""
            (string-join '() ","))

(test-section "(crab string) — trim")

(test-equal "trim both sides"     "hello" (string-trim       "  hello  "))
(test-equal "trim left only"      "hello  " (string-trim-left  "  hello  "))
(test-equal "trim right only"     "  hello" (string-trim-right "  hello  "))

(test-section "(crab string) — search/replace")

(test-equal "replace one"
            "hi world" (string-replace "hello world" "hello" "hi"))
(test-equal "replace many"
            "x-x-x"    (string-replace "a-a-a" "a" "x"))
(test-equal "replace empty no-op"
            "hello"    (string-replace "hello" "xyz" "abc"))

(test-true  "contains hit"   (string-contains? "hello world" "world"))
(test-false "contains miss"  (string-contains? "hello world" "xyz"))

(test-true  "starts hit"     (string-starts-with? "hello world" "hello"))
(test-false "starts miss"    (string-starts-with? "hello world" "world"))

(test-true  "ends hit"       (string-ends-with? "hello world" "world"))
(test-false "ends miss"      (string-ends-with? "hello world" "hello"))

(test-section "(crab string) — pad/repeat")

(test-equal "pad-left default fill"
            "   abc" (string-pad-left  "abc" 6))
(test-equal "pad-left no-op when wider"
            "abcdef" (string-pad-left  "abcdef" 3))

(test-equal "pad-right default fill"
            "abc   " (string-pad-right "abc" 6))

(test-equal "string-repeat 3x"
            "abcabcabc" (string-repeat "abc" 3))
(test-equal "string-repeat 0x"
            ""          (string-repeat "abc" 0))
