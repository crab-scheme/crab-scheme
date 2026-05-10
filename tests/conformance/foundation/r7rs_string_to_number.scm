(test-section "R7RS string->number with radix prefixes and exactness")

; --- basic decimal ---
(test-eqv "stn-int"     42      (string->number "42"))
(test-eqv "stn-neg"     -7      (string->number "-7"))
(test-eqv "stn-pos"     5       (string->number "+5"))

; --- decimal float ---
(test-eqv "stn-float"   3.14    (string->number "3.14"))
(test-eqv "stn-exp"     1000.0  (string->number "1e3"))

; --- explicit radix arg ---
(test-eqv "stn-bin-arg" 10      (string->number "1010" 2))
(test-eqv "stn-hex-arg" 255     (string->number "ff" 16))
(test-eqv "stn-oct-arg" 15      (string->number "17" 8))

; --- R7RS prefix #x #b #o #d ---
(test-eqv "stn-hex-prefix"   255 (string->number "#xff"))
(test-eqv "stn-bin-prefix"   10  (string->number "#b1010"))
(test-eqv "stn-oct-prefix"   15  (string->number "#o17"))
(test-eqv "stn-dec-prefix"   42  (string->number "#d42"))

; --- prefix + sign ---
(test-eqv "stn-hex-neg"     -255 (string->number "#x-ff"))
(test-eqv "stn-bin-pos"      10  (string->number "#b+1010"))

; --- rationals ---
(test-equal "stn-rational" 1/2 (string->number "1/2"))
(test-equal "stn-rational-3-4" 3/4 (string->number "3/4"))
(test-equal "stn-rational-int" 2 (string->number "10/5"))  ; reduces to 2

; --- exactness prefix #e ---
(test-eqv "stn-exact-int"   3 (string->number "#e3"))
(test-true "stn-exact-int-is-exact" (exact? (string->number "#e3")))

; --- exactness prefix #i ---
(test-true "stn-inexact-int-is-flonum"
  (flonum? (string->number "#i5")))

; --- combining radix + exactness (either order) ---
(test-eqv "stn-ex-hex" 255 (string->number "#e#xff"))
(test-eqv "stn-hex-ex" 255 (string->number "#x#eff"))
(test-true "stn-ix-hex-flonum"
  (flonum? (string->number "#i#x10")))

; --- special tokens ---
(test-true "stn-inf"      (infinite? (string->number "+inf.0")))
(test-true "stn-neg-inf"  (infinite? (string->number "-inf.0")))
(test-true "stn-nan"      (nan? (string->number "+nan.0")))
(test-true "stn-neg-nan"  (nan? (string->number "-nan.0")))

; --- malformed returns #f ---
(test-false "stn-bad"      (string->number "abc"))
(test-false "stn-bad-hex"  (string->number "#xZZZ"))
(test-false "stn-empty"    (string->number ""))
(test-false "stn-trailing" (string->number "42abc"))

; --- weird whitespace -> #f (R7RS doesn't trim) ---
(test-false "stn-spaces"   (string->number " 42 "))

; --- sign-only -> #f ---
(test-false "stn-just-sign"   (string->number "+"))

; --- parse 0 in all radices ---
(test-eqv "stn-zero"       0 (string->number "0"))
(test-eqv "stn-zero-bin"   0 (string->number "#b0"))
(test-eqv "stn-zero-hex"   0 (string->number "#x0"))

; --- divide by zero rational -> #f ---
(test-false "stn-div-by-zero" (string->number "5/0"))
