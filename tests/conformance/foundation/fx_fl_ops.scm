(test-section "R6RS (rnrs arithmetic fixnums) typed ops")

; --- fx+ / fx- / fx* ---
(test-eqv "fx+"          5 (fx+ 2 3))
(test-eqv "fx- binary"  -1 (fx- 2 3))
(test-eqv "fx- unary"   -7 (fx- 7))
(test-eqv "fx*"         12 (fx* 3 4))

; Type errors: non-fixnum operands raise.
(test-true "fx+-rejects-flonum"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (fx+ 1 2.0))))
(test-true "fx*-rejects-rational"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (fx* 1/2 4))))

; --- fxdiv / fxmod / fxdiv0 / fxmod0 (Euclidean / centered) ---
(test-eqv "fxdiv"  3 (fxdiv 17 5))
(test-eqv "fxmod"  2 (fxmod 17 5))
(test-eqv "fxdiv-neg"  -4 (fxdiv -17 5))   ; floor toward -inf
(test-eqv "fxmod-neg"   3 (fxmod -17 5))
(test-eqv "fxdiv0-shift" 4 (fxdiv0 18 5))
(test-eqv "fxmod0-shift" -2 (fxmod0 18 5))

; div by zero raises.
(test-true "fxdiv-zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (fxdiv 5 0))))

; --- comparisons ---
(test-true  "fx=? chain" (fx=? 1 1 1))
(test-false "fx=? differs" (fx=? 1 2 1))
(test-true  "fx<? chain" (fx<? 1 2 3))
(test-false "fx<? plateau" (fx<? 1 2 2))
(test-true  "fx>? chain" (fx>? 3 2 1))
(test-true  "fx<=?" (fx<=? 1 1 2))
(test-true  "fx>=?" (fx>=? 3 3 2))

; --- predicates ---
(test-true  "fxzero? 0"   (fxzero? 0))
(test-false "fxzero? 1"   (fxzero? 1))
(test-true  "fxpositive?" (fxpositive? 1))
(test-false "fxpositive? 0" (fxpositive? 0))
(test-true  "fxnegative?" (fxnegative? -1))
(test-true  "fxodd? 3"    (fxodd? 3))
(test-false "fxodd? 4"    (fxodd? 4))
(test-true  "fxeven? -4"  (fxeven? -4))

; --- min/max ---
(test-eqv "fxmax" 7 (fxmax 1 7 3 -2))
(test-eqv "fxmin" -2 (fxmin 1 7 3 -2))

; --- bitwise ---
(test-eqv "fxnot 0"   -1 (fxnot 0))
(test-eqv "fxnot -1"   0 (fxnot -1))
(test-eqv "fxand"      4 (fxand 12 5))   ; 1100 & 0101 = 0100
(test-eqv "fxior"     13 (fxior 12 5))   ; 1100 | 0101 = 1101
(test-eqv "fxxor"      9 (fxxor 12 5))   ; 1100 ^ 0101 = 1001

; --- arithmetic shifts ---
(test-eqv "fx-shift-left"  16 (fxarithmetic-shift 1 4))
(test-eqv "fx-shift-right"  1 (fxarithmetic-shift 16 -4))
(test-eqv "fxasl"          16 (fxarithmetic-shift-left 1 4))
(test-eqv "fxasr"           1 (fxarithmetic-shift-right 16 4))

; -----------------------------------------------------------------
(test-section "R6RS (rnrs arithmetic flonums) typed ops")

; --- fl+ / fl- / fl* / fl/ ---
(test-equal "fl+"        5.0 (fl+ 2.0 3.0))
(test-equal "fl- binary" -1.0 (fl- 2.0 3.0))
(test-equal "fl- unary"  -7.5 (fl- 7.5))
(test-equal "fl*"        7.5  (fl* 2.5 3.0))
(test-equal "fl/"        0.5  (fl/ 1.0 2.0))
(test-equal "fl/ unary"  0.25 (fl/ 4.0))

; Type errors: non-flonum raises.
(test-true "fl+-rejects-fixnum"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (fl+ 1 2.0))))

; --- comparisons ---
(test-true  "fl=?" (fl=? 1.0 1.0 1.0))
(test-false "fl=? differs" (fl=? 1.0 2.0))
(test-true  "fl<?" (fl<? 1.0 2.0 3.0))
(test-true  "fl>?" (fl>? 3.0 2.0 1.0))
(test-true  "fl<=?" (fl<=? 1.0 1.0 2.0))
(test-true  "fl>=?" (fl>=? 3.0 3.0 2.0))

; --- predicates ---
(test-true  "flzero? 0.0"     (flzero? 0.0))
(test-false "flzero? 1.0"     (flzero? 1.0))
(test-true  "flpositive?"     (flpositive? 1.0))
(test-true  "flnegative?"     (flnegative? -1.0))
(test-true  "flnan?"          (flnan? +nan.0))
(test-false "flnan? on 1.0"   (flnan? 1.0))
(test-true  "flfinite?"       (flfinite? 3.14))
(test-false "flfinite? inf"   (flfinite? +inf.0))
(test-true  "flinfinite?"     (flinfinite? +inf.0))
(test-true  "flinteger?"      (flinteger? 3.0))
(test-false "flinteger?"      (flinteger? 3.5))
(test-true  "fleven?"         (fleven? 4.0))
(test-true  "flodd?"          (flodd? 3.0))

; --- min/max ---
(test-equal "flmax" 7.0 (flmax 1.0 7.0 3.0 -2.0))
(test-equal "flmin" -2.0 (flmin 1.0 7.0 3.0 -2.0))

; --- unary math ---
(test-equal "flabs"      3.0 (flabs -3.0))
(test-equal "flfloor"    3.0 (flfloor 3.7))
(test-equal "flceiling"  4.0 (flceiling 3.2))
(test-equal "fltruncate" 3.0 (fltruncate 3.7))
(test-equal "fltrunc-neg" -3.0 (fltruncate -3.7))
(test-equal "flround halfeven" 2.0 (flround 2.5))   ; banker's
(test-equal "flround halfeven up" 4.0 (flround 3.5))
(test-equal "flsqrt"     3.0 (flsqrt 9.0))

; Transcendentals (verify monotonicity / known values to a delta).
(define (close? a b) (< (flabs (fl- a b)) 1e-10))
(test-true "flexp 0"   (close? (flexp 0.0) 1.0))
(test-true "fllog 1"   (close? (fllog 1.0) 0.0))
(test-true "flsin 0"   (close? (flsin 0.0) 0.0))
(test-true "flcos 0"   (close? (flcos 0.0) 1.0))
(test-true "flexp+log" (close? (fllog (flexp 2.0)) 2.0))

; fllog with base
(test-true "fllog base-10 of 100"
  (close? (fllog 100.0 10.0) 2.0))

; --- fixnum->flonum coercion ---
(test-equal "fixnum->flonum 7" 7.0 (fixnum->flonum 7))
(test-true "fixnum->flonum-rejects-flonum"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (fixnum->flonum 1.0))))
