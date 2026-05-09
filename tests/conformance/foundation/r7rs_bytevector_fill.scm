(test-section "R7RS bytevector-fill! with optional [start [end]]")

; --- whole bytevector (R6RS-compatible 2-arg) ---
(define bv1 (make-bytevector 5 0))
(bytevector-fill! bv1 99)
(test-equal "bvfill-whole" #u8(99 99 99 99 99) bv1)

; --- with start ---
(define bv2 (make-bytevector 6 0))
(bytevector-fill! bv2 7 3)
(test-equal "bvfill-start" #u8(0 0 0 7 7 7) bv2)

; --- with start + end ---
(define bv3 (make-bytevector 6 0))
(bytevector-fill! bv3 88 1 4)
(test-equal "bvfill-start-end" #u8(0 88 88 88 0 0) bv3)

; --- empty range ---
(define bv4 (make-bytevector 4 1))
(bytevector-fill! bv4 99 2 2)
(test-equal "bvfill-empty-range" #u8(1 1 1 1) bv4)

; --- boundary 0/0 ---
(define bv5 (make-bytevector 3 5))
(bytevector-fill! bv5 9 0 0)
(test-equal "bvfill-zero-zero" #u8(5 5 5) bv5)

; --- boundary len/len ---
(define bv6 (make-bytevector 3 5))
(bytevector-fill! bv6 9 3 3)
(test-equal "bvfill-len-len" #u8(5 5 5) bv6)

; --- byte out of range ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector-fill! (make-bytevector 3 0) 256))))))
(test-eqv "bvfill-byte-out-of-range" 'caught c1)

; --- start out of range ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector-fill! (make-bytevector 3 0) 5 99))))))
(test-eqv "bvfill-start-out-of-range" 'caught c2)

; --- end < start ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector-fill! (make-bytevector 4 0) 5 3 1))))))
(test-eqv "bvfill-end-before-start" 'caught c3)

; --- bytevector-fill! with byte 0 (boundary) ---
(define bv7 (make-bytevector 3 99))
(bytevector-fill! bv7 0)
(test-equal "bvfill-zero-byte" #u8(0 0 0) bv7)

; --- bytevector-fill! with byte 255 (boundary) ---
(define bv8 (make-bytevector 3 0))
(bytevector-fill! bv8 255)
(test-equal "bvfill-255-byte" #u8(255 255 255) bv8)

; --- writes preserve before/after when start/end set ---
(define bv9 (make-bytevector 5 1))
(bytevector-fill! bv9 9 1 4)
(test-eqv  "bvfill-pre-preserved"  1 (bytevector-u8-ref bv9 0))
(test-eqv  "bvfill-mid-replaced"   9 (bytevector-u8-ref bv9 2))
(test-eqv  "bvfill-post-preserved" 1 (bytevector-u8-ref bv9 4))
