(test-section "R7RS read-bytevector!")

; --- read into a bytevector, fills the whole thing ---
(define bv1 (make-bytevector 5 0))
(define n1 (read-bytevector! bv1 (open-input-bytevector #u8(1 2 3 4 5))))
(test-eqv "rbvb-full-count" 5 n1)
(test-equal "rbvb-full-content" #u8(1 2 3 4 5) bv1)

; --- read into oversized bytevector, fills partial ---
(define bv2 (make-bytevector 10 0))
(define n2 (read-bytevector! bv2 (open-input-bytevector #u8(7 8 9))))
(test-eqv "rbvb-partial-count" 3 n2)
(test-equal "rbvb-partial-content" #u8(7 8 9 0 0 0 0 0 0 0) bv2)

; --- read with start offset ---
(define bv3 (make-bytevector 6 0))
(define n3 (read-bytevector! bv3 (open-input-bytevector #u8(11 12 13)) 2))
(test-eqv "rbvb-start-count" 3 n3)
(test-equal "rbvb-start-content" #u8(0 0 11 12 13 0) bv3)

; --- read with start + end ---
(define bv4 (make-bytevector 6 99))
(define n4 (read-bytevector! bv4 (open-input-bytevector #u8(20 21 22 23)) 1 3))
(test-eqv "rbvb-start-end-count" 2 n4)
; Only positions 1..3 modified; rest preserved
(test-equal "rbvb-start-end-content" #u8(99 20 21 99 99 99) bv4)

; --- read at EOF returns eof-object ---
(define bv5 (make-bytevector 3 0))
(define res5 (read-bytevector! bv5 (open-input-bytevector #u8())))
(test-true "rbvb-at-eof" (eof-object? res5))

; --- read into 0-length region returns 0 ---
(define bv6 (make-bytevector 4 0))
(define n6 (read-bytevector! bv6 (open-input-bytevector #u8(99 99 99)) 2 2))
(test-eqv "rbvb-empty-region" 0 n6)
(test-equal "rbvb-empty-unchanged" #u8(0 0 0 0) bv6)

; --- partial read leaves source positioned for more reads ---
(define p7 (open-input-bytevector #u8(1 2 3 4 5 6)))
(define bv7a (make-bytevector 3 0))
(define n7a (read-bytevector! bv7a p7))
(test-eqv "rbvb-pos-1" 3 n7a)
(test-equal "rbvb-pos-1-content" #u8(1 2 3) bv7a)
(define bv7b (make-bytevector 3 0))
(define n7b (read-bytevector! bv7b p7))
(test-eqv "rbvb-pos-2" 3 n7b)
(test-equal "rbvb-pos-2-content" #u8(4 5 6) bv7b)
(test-true "rbvb-pos-3-eof" (eof-object? (read-bytevector! (make-bytevector 1 0) p7)))

; --- non-bytevector arg: caught ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-bytevector! "not-bv" (open-input-bytevector #u8(1))))))))
(test-eqv "rbvb-non-bv" 'caught c1)

; --- start out of range: caught ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-bytevector! (make-bytevector 3 0)
                                     (open-input-bytevector #u8(1 2 3))
                                     99))))))
(test-eqv "rbvb-bad-start" 'caught c2)
