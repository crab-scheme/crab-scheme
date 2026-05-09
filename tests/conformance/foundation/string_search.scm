(test-section "string search & replace ops")

; --- string-index-right ---
(test-eqv "sir-found"  4 (string-index-right "abXcX" #\X))
(test-eqv "sir-first"  0 (string-index-right "Xabc" #\X))
(test-equal "sir-no-match" #f (string-index-right "abc" #\X))

; --- string-contains-right (last substring index) ---
(test-eqv "scr-found"     6 (string-contains-right "foofoofoo" "foo"))
(test-eqv "scr-once"      0 (string-contains-right "foo" "foo"))
(test-equal "scr-no-match" #f (string-contains-right "abc" "xyz"))

; --- string-replace (first occurrence only) ---
(test-equal "sr-first"     "Xfoofoo" (string-replace "foofoofoo" "foo" "X"))
(test-equal "sr-no-match"  "abc"     (string-replace "abc" "z" "Q"))
(test-equal "sr-empty-to"  "bar"     (string-replace "foobar" "foo" ""))

; --- string-replace-all (every occurrence) ---
(test-equal "sra-multi"    "XXX"           (string-replace-all "foofoofoo" "foo" "X"))
(test-equal "sra-no-match" "abc"           (string-replace-all "abc" "z" "Q"))
(test-equal "sra-shrink"   "abc"           (string-replace-all "a-b-c" "-" ""))
(test-equal "sra-grow"     "a/b/c"         (string-replace-all "a-b-c" "-" "/"))

; --- string-count ---
(test-eqv "sc-char"   5 (string-count "abracadabra" #\a))
(test-eqv "sc-str"    2 (string-count "foofoo" "foo"))
(test-eqv "sc-zero"   0 (string-count "abc" #\z))
(test-eqv "sc-overlap" 1 (string-count "aaaa" "aaa"))   ; non-overlapping

; --- error guards ---
(test-true "sr-empty-pattern raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (string-replace "abc" "" "X"))))
(test-true "sra-empty-pattern raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (string-replace-all "abc" "" "X"))))

; --- regression: existing ops still work ---
(test-eqv "string-index forward"  2 (string-index "abXcX" #\X))
(test-eqv "string-contains first" 0 (string-contains "foofoofoo" "foo"))
