(test-section "R6RS §11.7 — arithmetic")

; Basic addition
(test-eqv "add-fixnums"          5     (+ 2 3))
(test-eqv "add-many"             15    (+ 1 2 3 4 5))
(test-eqv "add-empty"            0     (+))
(test-eqv "add-one"              42    (+ 42))

; Subtraction
(test-eqv "sub-binary"           7     (- 10 3))
(test-eqv "sub-many"             -8    (- 1 2 3 4))
(test-eqv "sub-unary-negate"     -5    (- 5))
(test-eqv "sub-of-zero"          0     (- 5 5))

; Multiplication
(test-eqv "mul-fixnums"          6     (* 2 3))
(test-eqv "mul-empty"            1     (*))
(test-eqv "mul-one"              42    (* 42))
(test-eqv "mul-many"             24    (* 1 2 3 4))
(test-eqv "mul-by-zero"          0     (* 100 0))

; Quotient / remainder / modulo (R6RS §11.7.3 specifies sign behavior)
(test-eqv "quotient-pos-pos"     3     (quotient 13 4))
(test-eqv "quotient-neg-pos"     -3    (quotient -13 4))
(test-eqv "quotient-pos-neg"     -3    (quotient 13 -4))
(test-eqv "remainder-pos-pos"    1     (remainder 13 4))
(test-eqv "remainder-neg-pos"    -1    (remainder -13 4))
(test-eqv "modulo-pos-pos"       1     (modulo 13 4))
(test-eqv "modulo-neg-pos"       3     (modulo -13 4))
(test-eqv "modulo-pos-neg"       -3    (modulo 13 -4))

; Comparisons
(test-true  "lt-true"       (< 1 2))
(test-false "lt-false"      (< 2 1))
(test-true  "lt-chain"      (< 1 2 3 4))
(test-false "lt-chain-fail" (< 1 2 2 3))
(test-true  "le-equal"      (<= 1 1 2))
(test-true  "gt-chain"      (> 4 3 2 1))
(test-true  "eq-numbers"    (= 1 1 1))
(test-false "eq-different"  (= 1 1 2))

; min / max / abs
(test-eqv "min-many"  1  (min 3 1 4 1 5 9 2 6))
(test-eqv "max-many"  9  (max 3 1 4 1 5 9 2 6))
(test-eqv "abs-pos"   5  (abs 5))
(test-eqv "abs-neg"   5  (abs -5))
(test-eqv "abs-zero"  0  (abs 0))

; expt
(test-eqv "expt-2-10"  1024 (expt 2 10))
(test-eqv "expt-3-3"   27   (expt 3 3))
(test-eqv "expt-anything-0" 1 (expt 99 0))

; Predicates
(test-true  "zero-of-0"      (zero? 0))
(test-false "zero-of-1"      (zero? 1))
(test-true  "positive-1"     (positive? 1))
(test-false "positive-neg"   (positive? -1))
(test-true  "negative-neg"   (negative? -1))
(test-false "negative-zero"  (negative? 0))
