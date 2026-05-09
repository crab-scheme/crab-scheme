(test-section "IEEE-754 literals: +inf.0 / -inf.0 / +nan.0 / -nan.0")

; --- parse + display roundtrip ---
(test-equal "inf-pos-display"  "+inf.0"
  (let ((p (open-string-output-port))) (display +inf.0 p) (get-output-string p)))
(test-equal "inf-neg-display"  "-inf.0"
  (let ((p (open-string-output-port))) (display -inf.0 p) (get-output-string p)))
(test-equal "nan-display"       "+nan.0"
  (let ((p (open-string-output-port))) (display +nan.0 p) (get-output-string p)))

; --- type predicates ---
(test-true  "inf-is-flonum"     (flonum? +inf.0))
(test-true  "inf-is-number"     (number? +inf.0))
(test-false "inf-is-fixnum"     (fixnum? +inf.0))
(test-false "inf-is-rational"   (rational? +inf.0))
(test-false "nan-is-rational"   (rational? +nan.0))

; --- arithmetic ---
(test-equal "inf+1"   +inf.0 (+ +inf.0 1))
(test-equal "inf*-1"  -inf.0 (* +inf.0 -1))
; nan compares unequal to itself per IEEE-754
(test-false "nan-eq-nan" (= +nan.0 +nan.0))
; Float divide-by-zero produces IEEE-754 infinities / NaN per R6RS.
(test-equal "1.0/0.0"  +inf.0 (/ 1.0 0.0))
(test-equal "-1.0/0.0" -inf.0 (/ -1.0 0.0))
; 0.0 / 0.0 produces a NaN; compare via display roundtrip.
(test-equal "0.0/0.0"
  "+nan.0"
  (let ((p (open-string-output-port))) (display (/ 0.0 0.0) p) (get-output-string p)))
; Exact division by exact zero still raises (catchable).
(test-true "1/0-exact-raises"
  (with-exception-handler (lambda (c) (error? c)) (lambda () (/ 1 0))))

; --- bare + and - still parse as identifiers ---
(test-true "plus-is-procedure"  (procedure? +))
(test-true "minus-is-procedure" (procedure? -))

; --- identifiers starting with + or - that aren't inf/nan stay identifiers ---
(define +mark 42)
(test-eqv "user-plus-prefix" 42 +mark)
(define -count 99)
(test-eqv "user-minus-prefix" 99 -count)

; --- arithmetic preserving inf ---
(test-equal "inf-minus-inf-is-nan"
  "+nan.0"
  (let ((r (- +inf.0 +inf.0)))
    ;; Use display roundtrip since (= +nan.0 +nan.0) is #f per IEEE.
    (let ((p (open-string-output-port))) (display r p) (get-output-string p))))
