(test-section "R6RS §11.8 — booleans")

; Self-evaluation
(test-true  "true-evaluates"   #t)
(test-false "false-evaluates"  #f)

; Predicate
(test-true  "boolean-of-true"   (boolean? #t))
(test-true  "boolean-of-false"  (boolean? #f))
(test-false "boolean-of-num"    (boolean? 0))
(test-false "boolean-of-null"   (boolean? '()))
(test-false "boolean-of-sym"    (boolean? 'true))

; not
(test-true  "not-false"         (not #f))
(test-false "not-true"          (not #t))
(test-false "not-zero"          (not 0))    ; 0 is truthy
(test-false "not-empty-list"    (not '()))  ; () is truthy
(test-false "not-symbol"        (not 'x))

; Truthiness in if
(test-eqv   "if-only-#f-is-false" 'yes (if 0 'yes 'no))
(test-eqv   "if-empty-truthy"     'yes (if '() 'yes 'no))
(test-eqv   "if-#f-false"         'no  (if #f 'yes 'no))
