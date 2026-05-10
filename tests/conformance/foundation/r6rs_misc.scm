(test-section "R6RS small wins: list filters, fx bit ops, fixnum bounds, flexpt")

;; ---- remp / remv / list-head ----
(test-equal "remp-odd"           '(2 4)         (remp odd? '(1 2 3 4 5)))
(test-equal "remp-empty"         '()            (remp (lambda (x) #t) '(1 2 3)))
(test-equal "remp-no-match"      '(1 2 3)       (remp (lambda (x) #f) '(1 2 3)))
(test-equal "remv-3"             '(1 2 4 2 1)   (remv 3 '(1 2 3 4 3 2 1)))
(test-equal "remv-uses-eqv"      '(1.0 2.0)     (remv 3.0 '(1.0 2.0 3.0)))
(test-equal "list-head-3"        '(10 20 30)    (list-head '(10 20 30 40 50) 3))
(test-equal "list-head-0"        '()            (list-head '(a b c) 0))
(test-equal "list-head-full"     '(a b c)       (list-head '(a b c) 3))

(test-true "list-head-rejects-overflow"
  (guard (c (#t #t))
    (list-head '(a b) 10)
    #f))

;; ---- fxlength / fxbit-count / fxfirst-bit-set / fxbit-set? ----
(test-equal "fxlength-7"         3   (fxlength 7))
(test-equal "fxlength-neg-8"     3   (fxlength -8))
(test-equal "fxlength-0"         0   (fxlength 0))
(test-equal "fxlength-1"         1   (fxlength 1))
(test-equal "fxlength-256"       9   (fxlength 256))

(test-equal "fxbit-count-7"      3   (fxbit-count 7))
(test-equal "fxbit-count-neg1"   -1  (fxbit-count -1))
(test-equal "fxbit-count-0"      0   (fxbit-count 0))
(test-equal "fxbit-count-15"     4   (fxbit-count 15))

(test-equal "fxfirst-12"         2   (fxfirst-bit-set 12))
(test-equal "fxfirst-0"          -1  (fxfirst-bit-set 0))
(test-equal "fxfirst-1"          0   (fxfirst-bit-set 1))
(test-equal "fxfirst-8"          3   (fxfirst-bit-set 8))

(test-true  "fxbit-set?-5-0"     (fxbit-set? 5 0))
(test-false "fxbit-set?-5-1"     (fxbit-set? 5 1))
(test-true  "fxbit-set?-5-2"     (fxbit-set? 5 2))
(test-false "fxbit-set?-0-3"     (fxbit-set? 0 3))

;; ---- fixnum bounds ----
(test-equal "fixnum-width-64"    64                     (fixnum-width))
(test-equal "least-fixnum"       -9223372036854775808   (least-fixnum))
(test-equal "greatest-fixnum"    9223372036854775807    (greatest-fixnum))

;; ---- flexpt ----
(test-equal "flexpt-2-10"        1024.0                 (flexpt 2.0 10.0))
(test-equal "flexpt-0.5-4"       0.0625                 (flexpt 0.5 4.0))
(test-equal "flexpt-1-anything"  1.0                    (flexpt 1.0 1234.5))
(test-equal "flexpt-anything-0"  1.0                    (flexpt 7.5 0.0))

;; ---- *-valued? predicates ----
(test-true  "real-valued-int"    (real-valued? 3))
(test-true  "real-valued-flo"    (real-valued? 3.0))
(test-false "real-valued-str"    (real-valued? "x"))
(test-false "rational-valued-inf" (rational-valued? +inf.0))
(test-true  "rational-valued-3.5" (rational-valued? 3.5))
(test-true  "integer-valued-int" (integer-valued? 5))
(test-true  "integer-valued-flo" (integer-valued? 5.0))
(test-false "integer-valued-half" (integer-valued? 5.5))
(test-true  "integer-valued-neg-flo" (integer-valued? -3.0))

;; ---- real->flonum ----
(test-equal "real->flo-int"      3.0  (real->flonum 3))
(test-equal "real->flo-flo"      3.5  (real->flonum 3.5))

;; ---- rationalize ----
(test-equal "rationalize-int"    5    (rationalize 5 0.1))
(test-equal "rationalize-zero"   0    (rationalize 0.0 0.01))

;; ---- symbol-append ----
(test-equal "symbol-append"      'foo-bar (symbol-append 'foo '- 'bar))
(test-equal "symbol-append-1"    'a       (symbol-append 'a))
(test-equal "symbol-append-0"    '||      (symbol-append))

