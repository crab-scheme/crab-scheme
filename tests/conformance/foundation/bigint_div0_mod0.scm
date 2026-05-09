(test-section "div0/mod0 on bignum operands (centered division)")

; Reference: 2^100 = 1267650600228229401496703205376
(define big (expt 2 100))

; div0/mod0 produces a remainder in [-|y|/2, |y|/2).
; For positive dividend big and small positive divisor:
;   div0(big, 3): same as div(big, 3) since remainder 1 is in [-1, 1)? Actually
;   |y|/2 = 1.5 so the centered window is [-1.5, 1.5); m=1 is inside, so d0=d.
(test-equal "div0-big-by-3"
  422550200076076467165567735125    ; same as div big 3
  (div0 big 3))
(test-eqv "mod0-big-by-3" 1 (mod0 big 3))

; div0(big, 7): remainder 2 is in [-3.5, 3.5), so d0=d, m0=2.
(test-equal "div0-big-by-7"
  181092942889747057356671886482
  (div0 big 7))
(test-eqv "mod0-big-by-7" 2 (mod0 big 7))

; (modulo big 5) = 1, but for div0 with divisor 5, |y|/2 = 2.5 so 1 is in
; [-2.5, 2.5) — d0=d.
(test-eqv "mod0-big-by-5" 1 (mod0 big 5))

; A case where centering shifts: choose y so the Euclidean remainder
; is > |y|/2. (mod big 4) = 0, so try (div0 (+ big 3) 4):
;   euclid-mod (big+3) 4 = 3, |y|/2 = 2 → 3 >= 2, shift up by 1.
(test-equal "div0-shifts-up"
  (+ (div (+ big 3) 4) 1)
  (div0 (+ big 3) 4))
(test-eqv "mod0-shifts-up" -1 (mod0 (+ big 3) 4))

; Negative dividend: (- big) mod 7 (Euclidean) = 5; |y|/2=3.5 → 5 ≥ 3.5, shift up
;   div(-big, 7) is rounded toward -∞: floor(-big/7).
;   div0(-big, 7) shifts that by +1 (since y > 0).
(test-equal "div0-neg-big-shift"
  (+ (div (- big) 7) 1)
  (div0 (- big) 7))
(test-eqv "mod0-neg-big" -2 (mod0 (- big) 7))

; div0-and-mod0 returns both values for bignum operands.
(call-with-values
  (lambda () (div0-and-mod0 big 7))
  (lambda (d m)
    (test-equal "dam0-d" 181092942889747057356671886482 d)
    (test-eqv   "dam0-m" 2 m)))

; Identity: x = d0*y + m0
(define (d0m0-identity x y)
  (call-with-values
    (lambda () (div0-and-mod0 x y))
    (lambda (d m) (= x (+ m (* y d))))))
(test-true "d0m0-id-big-pos"  (d0m0-identity big 7))
(test-true "d0m0-id-big-neg"  (d0m0-identity (- big) 7))
(test-true "d0m0-id-fix"      (d0m0-identity 17 5))

; Bound check: |m0| <= |y|/2 always.
(define (mod0-in-bounds x y)
  (let ((m (mod0 x y)))
    (let ((abs-m (if (< m 0) (- m) m))
          (half  (quotient (if (< y 0) (- y) y) 2)))
      (or (<= abs-m half)
          ; if |y|/2 is non-integral (odd y), strict bound is m*2 < |y|
          (< (* abs-m 2) (if (< y 0) (- y) y))))))
(test-true "mod0-bound-big-3" (mod0-in-bounds big 3))
(test-true "mod0-bound-big-7" (mod0-in-bounds big 7))
(test-true "mod0-bound-big-4" (mod0-in-bounds (+ big 3) 4))

; Small values still behave.
(test-eqv "small-div0-7-5" 1 (div0 7 5))     ; rem 2, |y|/2=2.5, no shift
(test-eqv "small-mod0-7-5" 2 (mod0 7 5))
(test-eqv "small-div0-8-5" 2 (div0 8 5))     ; rem 3, shift to 2 with rem -2
(test-eqv "small-mod0-8-5" -2 (mod0 8 5))

; Division by zero still raises.
(test-true "div0-big-zero"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (div0 big 0))))
