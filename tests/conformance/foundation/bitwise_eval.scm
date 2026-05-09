(test-section "Bitwise + eval + error-object")

; bitwise-and / or / xor / not
(test-eqv "and-12-10"   8    (bitwise-and 12 10))
(test-eqv "and-empty"   -1   (bitwise-and))
(test-eqv "or-5-3"      7    (bitwise-or 5 3))
(test-eqv "or-empty"    0    (bitwise-or))
(test-eqv "xor-12-10"   6    (bitwise-xor 12 10))
(test-eqv "not-0"       -1   (bitwise-not 0))
(test-eqv "not-1"       -2   (bitwise-not 1))

; arithmetic shift
(test-eqv "shift-l-1-4" 16   (bitwise-arithmetic-shift-left 1 4))
(test-eqv "shift-l-3-2" 12   (bitwise-arithmetic-shift-left 3 2))
(test-eqv "shift-r-16-2" 4   (bitwise-arithmetic-shift-right 16 2))
(test-eqv "shift-pos-l" 32   (bitwise-arithmetic-shift 8 2))   ; positive count = left
(test-eqv "shift-neg-r" 2    (bitwise-arithmetic-shift 8 -2))  ; negative count = right

; bit-count / length / set?
(test-eqv "bit-count-7"  3   (bitwise-bit-count 7))
(test-eqv "bit-count-15" 4   (bitwise-bit-count 15))
(test-eqv "bit-count-0"  0   (bitwise-bit-count 0))
(test-eqv "length-0"     0   (bitwise-length 0))
(test-eqv "length-1"     1   (bitwise-length 1))
(test-eqv "length-7"     3   (bitwise-length 7))
(test-eqv "length-256"   9   (bitwise-length 256))
(test-true  "bit-set-yes" (bitwise-bit-set? 4 2))   ; 4 = b100, bit 2 set
(test-false "bit-set-no"  (bitwise-bit-set? 4 1))   ; 4 = b100, bit 1 not set

; exact-integer-sqrt returns (sqrt remainder) via 2 values (R6RS)
(call-with-values (lambda () (exact-integer-sqrt 16))
  (lambda (s r)
    (test-eqv "sqrt-16-s" 4 s)
    (test-eqv "sqrt-16-r" 0 r)))
(call-with-values (lambda () (exact-integer-sqrt 17))
  (lambda (s r)
    (test-eqv "sqrt-17-s" 4 s)
    (test-eqv "sqrt-17-r" 1 r)))
(call-with-values (lambda () (exact-integer-sqrt 100))
  (lambda (s r)
    (test-eqv "sqrt-100-s" 10 s)
    (test-eqv "sqrt-100-r" 0 r)))

; eval
(test-eqv   "eval-add"      10  (eval '(+ 1 2 3 4)))
(test-eqv   "eval-mult"     24  (eval '(* 2 3 4)))
(test-equal "eval-list"    '(1 2 3)  (eval '(list 1 2 3)))
(test-eqv   "eval-lambda"   25  (eval '((lambda (x) (* x x)) 5)))

; error-object accessors
(test-true  "err-obj?-of-error"
  (with-exception-handler
    (lambda (c) (error-object? c))
    (lambda () (error "boom"))))
(test-false "err-obj?-of-num" (error-object? 42))
(test-equal "err-obj-message"
  "test message"
  (with-exception-handler
    (lambda (c) (error-object-message c))
    (lambda () (error "test message"))))
(test-equal "err-obj-irritants"
  '(1 2 3)
  (with-exception-handler
    (lambda (c) (error-object-irritants c))
    (lambda () (error "msg" 1 2 3))))
