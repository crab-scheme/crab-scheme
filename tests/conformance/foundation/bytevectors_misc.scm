(test-section "Bytevectors + current-port + gensym")

; Construction
(test-true  "bv-pred-of-bv"   (bytevector? (make-bytevector 0)))
(test-false "bv-pred-of-vec"  (bytevector? (vector 1 2 3)))
(test-eqv   "bv-len-empty"    0  (bytevector-length (make-bytevector 0)))
(test-eqv   "bv-len-5"        5  (bytevector-length (make-bytevector 5)))

; make-bytevector with fill
(define bv (make-bytevector 3 7))
(test-eqv "bv-fill-0"  7  (bytevector-u8-ref bv 0))
(test-eqv "bv-fill-1"  7  (bytevector-u8-ref bv 1))
(test-eqv "bv-fill-2"  7  (bytevector-u8-ref bv 2))

; bytevector constructor
(define bv2 (bytevector 65 66 67))
(test-eqv "bv-ref-0"  65  (bytevector-u8-ref bv2 0))
(test-eqv "bv-ref-1"  66  (bytevector-u8-ref bv2 1))
(test-eqv "bv-ref-2"  67  (bytevector-u8-ref bv2 2))

; Mutation
(define bv3 (make-bytevector 3 0))
(bytevector-u8-set! bv3 0 100)
(bytevector-u8-set! bv3 1 200)
(test-eqv "bv-after-set-0" 100  (bytevector-u8-ref bv3 0))
(test-eqv "bv-after-set-1" 200  (bytevector-u8-ref bv3 1))
(test-eqv "bv-untouched"   0    (bytevector-u8-ref bv3 2))

; copy
(define bv4 (bytevector 1 2 3 4))
(define bv5 (bytevector-copy bv4))
(bytevector-u8-set! bv4 0 99)
(test-eqv "bv-copy-isolated" 1  (bytevector-u8-ref bv5 0))

; <-> u8 list
(test-equal "bv-to-list"
  '(10 20 30)
  (bytevector->u8-list (bytevector 10 20 30)))
(test-equal "u8-list-to-bv"
  '(1 2 3)
  (bytevector->u8-list (u8-list->bytevector '(1 2 3))))

; equal? on bytevectors compares structurally
(test-true  "bv-equal-same-content"
  (equal? (bytevector 1 2 3) (bytevector 1 2 3)))
(test-false "bv-equal-diff-content"
  (equal? (bytevector 1 2 3) (bytevector 1 2 4)))

; gensym uniqueness
(test-true "gensym-distinct"
  (not (eq? (gensym) (gensym))))
(test-true "gensym-is-symbol"
  (symbol? (gensym)))

; current-output-port works inside with-output-to-string
(test-equal "current-port-inside-with"
  "captured"
  (with-output-to-string
    (lambda ()
      (display "captured" (current-output-port)))))
