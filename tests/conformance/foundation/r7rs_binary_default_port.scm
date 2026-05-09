(test-section "R7RS binary I/O with optional port (default current-* port)")

; --- read-u8 without port via with-input-from-bytevector
;     (we don't have that; use direct port + parameterize alternative).
;     Foundation: install current-input-port via implementation-specific
;     param. Without with-input-from-bytevector, we test the explicit
;     port route works for binary I/O.

; --- explicit port forms (regression check) ---
(define ip1 (open-input-bytevector #u8(10 20 30)))
(test-eqv "ru8-explicit"   10 (read-u8 ip1))
(test-eqv "pu8-explicit"   20 (peek-u8 ip1))  ; doesn't consume
(test-eqv "ru8-after-peek" 20 (read-u8 ip1))
(test-eqv "ru8-explicit-3" 30 (read-u8 ip1))
(test-true "ru8-eof"       (eof-object? (read-u8 ip1)))

; --- u8-ready? on binary input port ---
(define ip2 (open-input-bytevector #u8(1 2)))
(test-true "u8r-explicit" (u8-ready? ip2))

; --- read-bytevector explicit port ---
(define ip3 (open-input-bytevector #u8(1 2 3 4 5)))
(test-equal "rbv-3"        #u8(1 2 3) (read-bytevector 3 ip3))
(test-equal "rbv-rest"     #u8(4 5)   (read-bytevector 99 ip3))
(test-true  "rbv-eof"      (eof-object? (read-bytevector 5 ip3)))

; --- write-u8 explicit port ---
(define op1 (open-output-bytevector))
(write-u8 1 op1)
(write-u8 2 op1)
(write-u8 3 op1)
(test-equal "wu8-explicit" #u8(1 2 3) (get-output-bytevector op1))

; --- write-bytevector explicit port + slicing ---
(define op2 (open-output-bytevector))
(write-bytevector #u8(10 20 30 40 50) op2 1 4)
(test-equal "wbv-explicit-slice" #u8(20 30 40) (get-output-bytevector op2))

; --- write-u8 / write-bytevector arity errors caught ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-u8))))))
(test-eqv "wu8-arity-0" 'caught c1)

(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-u8 1 2 3 4 5))))))
(test-eqv "wu8-too-many-args" 'caught c2)

; --- read-u8 arity error (too many) ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-u8 1 2 3))))))
(test-eqv "ru8-arity-3" 'caught c3)

; --- read-u8 on textual port: error ---
(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-u8 (open-input-string "abc")))))))
(test-eqv "ru8-on-string-port" 'caught c4)

; --- read-bytevector returns fresh bytevector each call ---
(define ip4 (open-input-bytevector #u8(1 2 3)))
(define bv1 (read-bytevector 1 ip4))
(define bv2 (read-bytevector 1 ip4))
(test-true "rbv-fresh-1" (not (eq? bv1 bv2)))
(test-equal "rbv-fresh-vals" '(1 2)
  (list (bytevector-u8-ref bv1 0) (bytevector-u8-ref bv2 0)))

; --- write-u8 byte 0 and 255 boundary ---
(define op3 (open-output-bytevector))
(write-u8 0 op3)
(write-u8 255 op3)
(test-equal "wu8-boundary" #u8(0 255) (get-output-bytevector op3))

; --- write-u8 byte too big: caught ---
(define c5
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-u8 256 (open-output-bytevector)))))))
(test-eqv "wu8-byte-too-big" 'caught c5)
