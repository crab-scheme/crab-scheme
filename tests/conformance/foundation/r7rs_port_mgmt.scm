(test-section "R7RS port management: close-input-port, close-output-port, flush-output-port, *-open?")

; --- input-port-open? on a fresh string input port returns #t ---
(define ip1 (open-input-string "hello"))
(test-true  "input-open-fresh"        (input-port-open? ip1))
(test-false "input-open-on-output"    (input-port-open? (open-output-string)))
(test-false "input-open-on-non-port"  (input-port-open? 42))
(test-false "input-open-on-string"    (input-port-open? "hi"))

; --- output-port-open? on a fresh string output port returns #t ---
(define op1 (open-output-string))
(test-true  "output-open-fresh"       (output-port-open? op1))
(test-false "output-open-on-input"    (output-port-open? (open-input-string "x")))
(test-false "output-open-on-non-port" (output-port-open? 42))

; --- close-input-port on an input port doesn't error ---
(define ip2 (open-input-string "abc"))
(test-equal "close-input-no-error"  (if #f #f) (close-input-port ip2))

; --- close-input-port errors on an output port ---
(define ci-err
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (close-input-port (open-output-string)))))))
(test-eqv "close-input-on-output-errors" 'caught ci-err)

; --- close-output-port on an output port doesn't error ---
(define op2 (open-output-string))
(test-equal "close-output-no-error" (if #f #f) (close-output-port op2))

; --- close-output-port errors on an input port ---
(define co-err
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (close-output-port (open-input-string "x")))))))
(test-eqv "close-output-on-input-errors" 'caught co-err)

; --- flush-output-port on a string output port is a no-op (not error) ---
(define op3 (open-output-string))
(write-string "data" op3)
(test-equal "flush-noop-on-string" (if #f #f) (flush-output-port op3))
; Flushing must not destroy buffered data.
(test-equal "flush-preserves-data" "data" (get-output-string op3))

; --- flush-output-port with no arg is a no-op (R7RS allows no port arg) ---
(test-equal "flush-no-arg" (if #f #f) (flush-output-port))

; --- flush-output-port on a bytevector output port ---
(define bvo (open-output-bytevector))
(write-u8 1 bvo)
(write-u8 2 bvo)
(test-equal "flush-bytevector-noop" (if #f #f) (flush-output-port bvo))
(test-equal "flush-bytevector-preserved" #u8(1 2) (get-output-bytevector bvo))

; --- close-input-port + close-output-port aliases ---
; Both work consistently with their direction.
(test-true  "ip-still-input?"  (input-port? (open-input-string "z")))
(test-true  "op-still-output?" (output-port? (open-output-string)))

; --- Multiple closes on string ports should be tolerated ---
(define multi (open-output-string))
(close-output-port multi)
(test-equal "double-close" (if #f #f) (close-output-port multi))
