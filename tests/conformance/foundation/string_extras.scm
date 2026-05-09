(test-section "string extras: prefix/suffix, take/drop, pad, foldcase, titlecase")

; --- string-prefix? / string-suffix? ---
(test-true  "prefix-yes"   (string-prefix? "hel" "hello"))
(test-false "prefix-no"    (string-prefix? "world" "hello"))
(test-true  "prefix-empty" (string-prefix? "" "hello"))
(test-true  "prefix-self"  (string-prefix? "hello" "hello"))
(test-false "prefix-longer" (string-prefix? "hello!" "hello"))

(test-true  "suffix-yes"   (string-suffix? "lo" "hello"))
(test-false "suffix-no"    (string-suffix? "world" "hello"))
(test-true  "suffix-empty" (string-suffix? "" "hello"))

; --- string-take / string-drop ---
(test-equal "take-3"  "abc" (string-take "abcdef" 3))
(test-equal "drop-3"  "def" (string-drop "abcdef" 3))
(test-equal "take-0"  ""    (string-take "abc" 0))
(test-equal "drop-all" ""   (string-drop "abc" 3))
(test-equal "take-all" "abc" (string-take "abc" 3))

; --- string-take-right / string-drop-right ---
(test-equal "take-right-2"  "ef"   (string-take-right "abcdef" 2))
(test-equal "drop-right-2"  "abcd" (string-drop-right "abcdef" 2))
(test-equal "take-right-too-many" "abc" (string-take-right "abc" 99))
(test-equal "drop-right-too-many" "" (string-drop-right "abc" 99))

; --- string-pad / string-pad-right ---
(test-equal "pad-default"     "   42" (string-pad "42" 5))
(test-equal "pad-with-zero"   "00042" (string-pad "42" 5 #\0))
(test-equal "pad-no-change"   "abcde" (string-pad "abcde" 5))
; SRFI-13: padding to a smaller width truncates from the left for `pad`,
; from the right for `pad-right`.
(test-equal "pad-truncates-left"  "cdef" (string-pad "abcdef" 4))
(test-equal "pad-right-truncates-right" "abcd" (string-pad-right "abcdef" 4))
(test-equal "pad-right-default"   "42   " (string-pad-right "42" 5))
(test-equal "pad-right-with-dot"  "42..." (string-pad-right "42" 5 #\.))

; --- string-foldcase / string-titlecase ---
(test-equal "foldcase-upper" "hello" (string-foldcase "HELLO"))
(test-equal "foldcase-mixed" "hello" (string-foldcase "HeLLo"))
(test-equal "titlecase-words" "Hello, World!" (string-titlecase "hello, world!"))
(test-equal "titlecase-all-caps" "Hello" (string-titlecase "HELLO"))
(test-equal "titlecase-empty" "" (string-titlecase ""))

; --- error cases ---
(test-true "take-rejects-negative"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'string-take)))
    (lambda () (string-take "abc" -1))))
(test-true "pad-rejects-negative-width"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (string-pad "abc" -1))))
