(test-section "R7RS bytevector->list / list->bytevector")

; --- bytevector->list whole bytevector ---
(test-equal "bvtl-full" '(1 2 3 4) (bytevector->list #u8(1 2 3 4)))

; --- empty bytevector ---
(test-equal "bvtl-empty" '() (bytevector->list #u8()))

; --- with start ---
(test-equal "bvtl-start" '(3 4 5) (bytevector->list #u8(1 2 3 4 5) 2))

; --- with start + end ---
(test-equal "bvtl-start-end" '(2 3 4) (bytevector->list #u8(1 2 3 4 5) 1 4))

; --- empty range ---
(test-equal "bvtl-empty-range" '() (bytevector->list #u8(1 2 3) 1 1))

; --- boundary 0/0 ---
(test-equal "bvtl-zero-zero" '() (bytevector->list #u8(1 2 3) 0 0))

; --- boundary len/len ---
(test-equal "bvtl-len-len" '() (bytevector->list #u8(1 2 3) 3 3))

; --- list->bytevector ---
(test-equal "ltbv-empty" #u8() (list->bytevector '()))
(test-equal "ltbv-three" #u8(1 2 3) (list->bytevector '(1 2 3)))
(test-equal "ltbv-boundary" #u8(0 255) (list->bytevector '(0 255)))

; --- list->bytevector byte out-of-range ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (list->bytevector '(1 256 3)))))))
(test-eqv "ltbv-byte-too-big" 'caught c1)

; --- list->bytevector negative byte ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (list->bytevector '(1 -1 3)))))))
(test-eqv "ltbv-byte-negative" 'caught c2)

; --- bytevector->list out-of-range start ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector->list #u8(1 2 3) 99))))))
(test-eqv "bvtl-bad-start" 'caught c3)

; --- bytevector->list end < start ---
(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector->list #u8(1 2 3) 2 1))))))
(test-eqv "bvtl-bad-range" 'caught c4)

; --- round-trip ---
(test-equal "rt-bvtl-ltbv"
  #u8(10 20 30 40)
  (list->bytevector (bytevector->list #u8(10 20 30 40))))

; --- round-trip with slice ---
(test-equal "rt-slice"
  #u8(20 30)
  (list->bytevector (bytevector->list #u8(10 20 30 40) 1 3)))

; --- R6RS aliases still work ---
(test-equal "r6rs-bvtl"  '(1 2 3) (bytevector->u8-list #u8(1 2 3)))
(test-equal "r6rs-ltbv"  #u8(1 2 3) (u8-list->bytevector '(1 2 3)))
