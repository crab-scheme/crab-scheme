(test-section "exactness extras: numerator/denominator, exact-integer?, finite?, etc.")

; --- numerator / denominator on integers ---
(test-eqv "num-int"  7 (numerator 7))
(test-eqv "den-int"  1 (denominator 7))
(test-eqv "num-neg" -3 (numerator -3))
(test-eqv "den-neg"  1 (denominator -3))

; --- on rationals ---
(test-eqv "num-rat" 3 (numerator 3/7))
(test-eqv "den-rat" 7 (denominator 3/7))
(test-eqv "num-neg-rat" -2 (numerator -2/5))
(test-eqv "den-neg-rat"  5 (denominator -2/5))

; --- on flonums (returns flonum integer) ---
(test-equal "num-flo-int"  3.0 (numerator 3.0))
(test-equal "den-flo-int"  1.0 (denominator 3.0))
; 0.5 = 1/2 exactly in IEEE-754 → numerator 1.0, denominator 2.0
(test-equal "num-flo-half" 1.0 (numerator 0.5))
(test-equal "den-flo-half" 2.0 (denominator 0.5))

; --- inexact->exact / exact->inexact aliases ---
(test-eqv "exact-of-fixnum"   7   (exact 7))
(test-eqv "exact-of-3.0"      3   (exact 3.0))
(test-eqv "inexact->exact-3"  3   (inexact->exact 3.0))
(test-equal "exact->inexact-7" 7.0 (exact->inexact 7))
(test-equal "exact->inexact-rat" 0.5 (exact->inexact 1/2))

; exact of non-integral flonum → rational (dyadic)
(test-eqv "exact-of-half"  1/2 (exact 0.5))
; 0.25 = 1/4 exactly
(test-eqv "exact-of-quarter" 1/4 (exact 0.25))
; (exact 0.1) is the exact dyadic rational represented by the IEEE-754
; bit pattern of 0.1 — not 1/10.
(test-true "exact-of-0.1-not-tenth" (not (= (exact 0.1) 1/10)))

; exact of non-finite raises
(test-true "exact-of-inf-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (exact +inf.0))))

; --- exact-integer? predicate ---
(test-true  "ei?-fix"   (exact-integer? 7))
(test-true  "ei?-neg"   (exact-integer? -7))
(test-false "ei?-flo"   (exact-integer? 3.0))
(test-false "ei?-rat"   (exact-integer? 3/7))
(test-false "ei?-non"   (exact-integer? "foo"))

; --- exact-nonnegative-integer? ---
(test-true  "eni?-pos"  (exact-nonnegative-integer? 7))
(test-true  "eni?-zero" (exact-nonnegative-integer? 0))
(test-false "eni?-neg"  (exact-nonnegative-integer? -1))
(test-false "eni?-flo"  (exact-nonnegative-integer? 3.0))

; --- exact-rational? ---
(test-true  "er?-fix"  (exact-rational? 7))
(test-true  "er?-rat"  (exact-rational? 3/7))
(test-false "er?-flo"  (exact-rational? 3.0))

; --- nan? / finite? / infinite? ---
(test-true  "nan?-nan"     (nan? +nan.0))
(test-false "nan?-3.0"     (nan? 3.0))
(test-false "nan?-fix"     (nan? 3))
(test-true  "finite?-3.0"  (finite? 3.0))
(test-true  "finite?-fix"  (finite? 3))
(test-false "finite?-inf"  (finite? +inf.0))
(test-false "finite?-nan"  (finite? +nan.0))
(test-true  "infinite?-inf" (infinite? +inf.0))
(test-true  "infinite?--inf" (infinite? -inf.0))
(test-false "infinite?-3.0" (infinite? 3.0))
(test-false "infinite?-fix" (infinite? 3))
