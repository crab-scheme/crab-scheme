(test-section "bytevector input/output ports (R6RS binary I/O)")

; --- input port: get-u8, lookahead-u8, port-eof? ---
(define ip (open-bytevector-input-port (bytevector 65 66 67)))
(test-eqv "get-u8-1"        65 (get-u8 ip))
(test-eqv "lookahead-no-consume" 66 (lookahead-u8 ip))
(test-eqv "lookahead-twice" 66 (lookahead-u8 ip))
(test-eqv "get-u8-2"        66 (get-u8 ip))
(test-eqv "get-u8-3"        67 (get-u8 ip))
(test-true  "port-eof-after-drain" (port-eof? ip))
(test-true  "get-u8-eof"     (eof-object? (get-u8 ip)))
(test-true  "lookahead-eof"  (eof-object? (lookahead-u8 ip)))

; --- empty input port ---
(define empty (open-bytevector-input-port (bytevector)))
(test-true "empty-eof-immediately" (port-eof? empty))
(test-true "empty-get-u8-eof" (eof-object? (get-u8 empty)))

; --- output port: put-u8 + get-bytevector-output-port ---
(define op (open-bytevector-output-port))
(put-u8 op 1)
(put-u8 op 2)
(put-u8 op 3)
(test-equal "out-roundtrip"
  '(1 2 3) (bytevector->u8-list (get-bytevector-output-port op)))
; get-bytevector-output-port clears the buffer; the port stays usable.
(put-u8 op 99)
(test-equal "out-after-reset"
  '(99) (bytevector->u8-list (get-bytevector-output-port op)))
; Calling get on a freshly-emptied port yields the empty bytevector.
(test-eqv "out-empty-len"
  0 (bytevector-length (get-bytevector-output-port op)))

; --- get-bytevector-n: bulk read up to n bytes ---
(define ip2 (open-bytevector-input-port (bytevector 1 2 3 4 5)))
(test-equal "n-read-3"
  '(1 2 3) (bytevector->u8-list (get-bytevector-n ip2 3)))
(test-equal "n-read-rest-short"
  '(4 5) (bytevector->u8-list (get-bytevector-n ip2 10)))
(test-true "n-read-eof-after-drain"
  (eof-object? (get-bytevector-n ip2 1)))

; n=0 always succeeds (returns empty bytevector when bytes remain;
; EOF only when truly past end).
(define ip3 (open-bytevector-input-port (bytevector 1 2)))
(test-equal "n-read-zero"
  '() (bytevector->u8-list (get-bytevector-n ip3 0)))

; --- port classification predicates ---
(define sp (open-string-output-port))
(define bp (open-bytevector-output-port))
(test-true  "binary-port-on-bv"     (binary-port? bp))
(test-false "binary-port-on-string" (binary-port? sp))
(test-true  "textual-port-on-string" (textual-port? sp))
(test-false "textual-port-on-bv"    (textual-port? bp))
(test-true  "port-on-bv"             (port? bp))
(test-true  "port-on-string"         (port? sp))

; --- type errors raise proper conditions ---
(test-true "open-bv-input-rejects-string"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'open-bytevector-input-port)))
    (lambda () (open-bytevector-input-port "not a bv"))))
(test-true "put-u8-rejects-bad-byte"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (put-u8 op 256))))
(test-true "get-u8-on-output-port-fails"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (get-u8 op))))
