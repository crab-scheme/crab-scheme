(test-section "R7RS #u8(...) bytevector literal")

; --- empty bytevector literal ---
(define empty #u8())
(test-true  "bv-lit-empty-pred"   (bytevector? empty))
(test-eqv   "bv-lit-empty-len"  0 (bytevector-length empty))

; --- short bytevector literal ---
(define small #u8(0 1 2 3))
(test-true  "bv-lit-small-pred"   (bytevector? small))
(test-eqv   "bv-lit-small-len"  4 (bytevector-length small))
(test-eqv   "bv-lit-small-0"    0 (bytevector-u8-ref small 0))
(test-eqv   "bv-lit-small-3"    3 (bytevector-u8-ref small 3))

; --- boundary values: 0 and 255 ---
(define edge #u8(0 255 128))
(test-eqv "bv-lit-min" 0   (bytevector-u8-ref edge 0))
(test-eqv "bv-lit-max" 255 (bytevector-u8-ref edge 1))
(test-eqv "bv-lit-mid" 128 (bytevector-u8-ref edge 2))

; --- equal? on bytevector literals ---
(test-true  "bv-lit-equal"     (equal? #u8(1 2 3) #u8(1 2 3)))
(test-false "bv-lit-not-equal" (equal? #u8(1 2 3) #u8(1 2 4)))

; --- can be quoted (treated as self-evaluating) ---
(test-equal "bv-lit-quoted" #u8(7 8 9) '#u8(7 8 9))

; --- inside lists / data structures ---
(define xs (list #u8(1) #u8(2 3) #u8(4 5 6)))
(test-eqv "bv-lit-in-list-len" 3 (length xs))
(test-eqv "bv-lit-in-list-1st" 1 (bytevector-u8-ref (car xs) 0))
(test-eqv "bv-lit-in-list-2nd" 3 (bytevector-u8-ref (cadr xs) 1))

; --- vector containing bytevector ---
(define v (vector #u8(10) #u8(20)))
(test-eqv "bv-lit-in-vec" 10 (bytevector-u8-ref (vector-ref v 0) 0))
(test-eqv "bv-lit-in-vec-2" 20 (bytevector-u8-ref (vector-ref v 1) 0))
