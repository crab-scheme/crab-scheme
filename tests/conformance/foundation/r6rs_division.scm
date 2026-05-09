(test-section "R6RS Euclidean division: div / mod / div0 / mod0")

; --- div / mod (non-negative remainder) ---
; Sign matrix from the R6RS spec: 0 ≤ x − y·div(x,y) < |y|.
(test-eqv "div-pos-pos"   3 (div 17 5))
(test-eqv "mod-pos-pos"   2 (mod 17 5))
(test-eqv "div-neg-pos" -4 (div -17 5))
(test-eqv "mod-neg-pos"  3 (mod -17 5))
(test-eqv "div-pos-neg" -3 (div 17 -5))
(test-eqv "mod-pos-neg"  2 (mod 17 -5))
(test-eqv "div-neg-neg"  4 (div -17 -5))
(test-eqv "mod-neg-neg"  3 (mod -17 -5))
(test-eqv "div-zero-x"   0 (div 0 7))
(test-eqv "mod-zero-x"   0 (mod 0 7))

; --- div0 / mod0 (centered remainder in [−|y|/2, |y|/2)) ---
(test-eqv "div0-pos-pos"   3 (div0 17 5))
(test-eqv "mod0-pos-pos"   2 (mod0 17 5))
(test-eqv "div0-neg-pos" -3 (div0 -17 5))
(test-eqv "mod0-neg-pos" -2 (mod0 -17 5))
; Tie at exactly |y|/2 — when |y| is even and m == |y|/2, R6RS shifts so
; the result lands in [-|y|/2, |y|/2). With y=10, x=15: m would be 5,
; which equals |y|/2; |y| is even, so we shift to (div0 15 10) = 2,
; (mod0 15 10) = -5. The half-open interval includes -5 but not 5.
(test-eqv "div0-tie-even" 2 (div0 15 10))
(test-eqv "mod0-tie-even" -5 (mod0 15 10))

; --- div-and-mod / div0-and-mod0 return both via values ---
(call-with-values
  (lambda () (div-and-mod 17 5))
  (lambda (d m)
    (test-eqv "dam-d" 3 d)
    (test-eqv "dam-m" 2 m)))

(call-with-values
  (lambda () (div-and-mod -17 5))
  (lambda (d m)
    (test-eqv "dam-neg-d" -4 d)
    (test-eqv "dam-neg-m" 3 m)))

(call-with-values
  (lambda () (div0-and-mod0 -17 5))
  (lambda (d m)
    (test-eqv "d0am0-d" -3 d)
    (test-eqv "d0am0-m" -2 m)))

; --- division by zero raises a proper condition ---
(test-true "div-by-zero"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'div)))
    (lambda () (div 5 0))))
(test-true "mod-by-zero"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (mod 5 0))))

; --- invariant: x = y·div(x,y) + mod(x,y) for both ---
(define (check-div x y)
  (= x (+ (* y (div x y)) (mod x y))))
(test-true "invariant-pos-pos"  (check-div 17 5))
(test-true "invariant-neg-pos"  (check-div -17 5))
(test-true "invariant-pos-neg"  (check-div 17 -5))
(test-true "invariant-neg-neg"  (check-div -17 -5))
(test-true "invariant-misc"     (check-div 100 7))
(test-true "invariant-misc2"    (check-div -100 7))
