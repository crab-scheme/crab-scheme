(test-section "expt: exact integer exponents preserve exactness")

; Powers of 2 — easy to verify by hand at small exponents.
(test-eqv "2^0"   1     (expt 2 0))
(test-eqv "2^1"   2     (expt 2 1))
(test-eqv "2^10"  1024  (expt 2 10))
(test-eqv "2^30"  1073741824 (expt 2 30))

; 2^63 fits in i64; 2^64 doesn't — we should still get an exact bignum.
(test-equal "2^63"
  9223372036854775808
  (expt 2 63))
(test-equal "2^64"
  18446744073709551616
  (expt 2 64))

; Big bignum: 2^100. This is the canonical sanity check that the
; integer path doesn't fall back to f64.
(test-equal "2^100"
  1267650600228229401496703205376
  (expt 2 100))

; Powers of 10
(test-equal "10^20"
  100000000000000000000
  (expt 10 20))

; Negative base preserves exactness too.
(test-equal "(-3)^4"  81  (expt -3 4))
(test-equal "(-3)^5"  -243 (expt -3 5))

; Negative exponent on integer base falls back to flonum (R6RS spec
; allows either rational or flonum; we use flonum).
(test-equal "2^-1"  0.5  (expt 2 -1))
(test-equal "2^-3"  0.125 (expt 2 -3))

; Flonum operands stay flonum.
(test-equal "2.5^2" 6.25 (expt 2.5 2))

; expt with exponent 0 is always 1, regardless of base.
(test-eqv "0^0" 1 (expt 0 0))
(test-eqv "5^0" 1 (expt 5 0))

; Idempotence: (expt n 1) = n
(test-eqv "n^1-fixnum"  42 (expt 42 1))
(test-equal "n^1-bignum"
  9999999999999999999999999999
  (expt 9999999999999999999999999999 1))
