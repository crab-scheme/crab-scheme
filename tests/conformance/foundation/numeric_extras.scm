(test-section "Numeric extras: gcd/lcm/floor/etc")

; gcd / lcm
(test-eqv "gcd-12-18"     6   (gcd 12 18))
(test-eqv "gcd-100-75"    25  (gcd 100 75))
(test-eqv "gcd-empty"     0   (gcd))
(test-eqv "gcd-one"       7   (gcd 7))
(test-eqv "gcd-negatives" 6   (gcd -12 -18))
(test-eqv "lcm-4-6"       12  (lcm 4 6))
(test-eqv "lcm-empty"     1   (lcm))
(test-eqv "lcm-with-zero" 0   (lcm 0 5))

; floor / ceiling / truncate / round
(test-eqv "floor-flonum"      3.0   (floor 3.7))
(test-eqv "floor-neg-flonum"  -4.0  (floor -3.2))
(test-eqv "floor-int"         5     (floor 5))
(test-eqv "ceiling-flonum"    4.0   (ceiling 3.2))
(test-eqv "ceiling-neg"       -3.0  (ceiling -3.7))
(test-eqv "truncate-pos"      3.0   (truncate 3.7))
(test-eqv "truncate-neg"      -3.0  (truncate -3.7))
(test-eqv "round-pos"         4.0   (round 3.5))    ; banker's rounding: round-half-to-even, but 3.5→4
(test-eqv "round-neg-half"    -4.0  (round -3.5))
(test-eqv "round-up"          4.0   (round 3.6))
(test-eqv "round-down"        3.0   (round 3.4))

; even? / odd?
(test-true  "even-of-4"   (even? 4))
(test-false "even-of-5"   (even? 5))
(test-true  "odd-of-7"    (odd? 7))
(test-false "odd-of-8"    (odd? 8))
(test-true  "even-of-0"   (even? 0))

; square
(test-eqv "square-3"    9    (square 3))
(test-eqv "square-7"    49   (square 7))
(test-eqv "square-neg"  16   (square -4))
(test-eqv "square-0"    0    (square 0))
