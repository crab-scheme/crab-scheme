(test-section "R7RS division aliases: truncate-, floor-, truncate/, floor/")

; --- truncate-quotient = R5RS quotient (truncated toward 0) ---
(test-eqv "tq 17 5"   3 (truncate-quotient 17 5))
(test-eqv "tq -17 5" -3 (truncate-quotient -17 5))   ; truncated toward 0
(test-eqv "tq 17 -5" -3 (truncate-quotient 17 -5))
(test-eqv "tq -17 -5" 3 (truncate-quotient -17 -5))

; --- truncate-remainder = R5RS remainder (sign of x) ---
(test-eqv "tr 17 5"   2 (truncate-remainder 17 5))
(test-eqv "tr -17 5" -2 (truncate-remainder -17 5))
(test-eqv "tr 17 -5"  2 (truncate-remainder 17 -5))
(test-eqv "tr -17 -5" -2 (truncate-remainder -17 -5))

; --- floor-quotient = floor(x/y) (toward -∞) ---
(test-eqv "fq 17 5"    3 (floor-quotient 17 5))
(test-eqv "fq -17 5"  -4 (floor-quotient -17 5))   ; floored
(test-eqv "fq 17 -5"  -4 (floor-quotient 17 -5))
(test-eqv "fq -17 -5"  3 (floor-quotient -17 -5))

; --- floor-remainder = R5RS modulo (sign of y) ---
(test-eqv "frem 17 5"   2 (floor-remainder 17 5))
(test-eqv "frem -17 5"  3 (floor-remainder -17 5))
(test-eqv "frem 17 -5" -3 (floor-remainder 17 -5))
(test-eqv "frem -17 -5" -2 (floor-remainder -17 -5))

; --- identity: x = y · floor-q + floor-r ---
(define (frem-identity x y)
  (= x (+ (floor-remainder x y) (* y (floor-quotient x y)))))
(test-true "frem-id 17 5"   (frem-identity 17 5))
(test-true "frem-id -17 5"  (frem-identity -17 5))
(test-true "frem-id 17 -5"  (frem-identity 17 -5))
(test-true "frem-id -17 -5" (frem-identity -17 -5))

; --- truncate/ multi-value ---
(call-with-values
  (lambda () (truncate/ 17 5))
  (lambda (q r)
    (test-eqv "trunc/ q" 3 q)
    (test-eqv "trunc/ r" 2 r)))
(call-with-values
  (lambda () (truncate/ -17 5))
  (lambda (q r)
    (test-eqv "trunc/-q" -3 q)
    (test-eqv "trunc/-r" -2 r)))

; --- floor/ multi-value ---
(call-with-values
  (lambda () (floor/ 17 5))
  (lambda (q r)
    (test-eqv "floor/ q" 3 q)
    (test-eqv "floor/ r" 2 r)))
(call-with-values
  (lambda () (floor/ -17 5))
  (lambda (q r)
    (test-eqv "floor/-q" -4 q)
    (test-eqv "floor/-r" 3 r)))

; --- bignum operands round-trip ---
(define big (expt 2 100))
(test-equal "fq big 7"
  (floor-quotient big 7)
  (quotient big 7))   ; positive operands → floor and truncate match
; Negative big with positive divisor: floor-q < trunc-q.
(test-equal "fq-neg-big differs"
  (- (quotient (- big) 7) 1)
  (floor-quotient (- big) 7))

; --- division by zero raises ---
(test-true "tq zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (truncate-quotient 5 0))))
(test-true "fq zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (floor-quotient 5 0))))
