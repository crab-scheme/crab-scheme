; Conformance test for `(crab compress)` — stdlib-modules iter 7
; (slimmed in iter 17 when gzip+deflate moved to crab-deflate.scm).

(define (build-payload n)
  (let* ((bv (make-bytevector n 0)))
    (let loop ((i 0))
      (cond ((= i n) bv)
            (else (bytevector-u8-set! bv i (remainder i 256))
                  (loop (+ i 1)))))))

(define __payload__ (build-payload 1024))

(test-section "(crab compress) — zstd round-trip")

(define __zst__ (zstd-compress __payload__))
(test-true "zstd-compress shrinks repeating data"
           (< (bytevector-length __zst__) (bytevector-length __payload__)))
(test-equal "zstd round-trip preserves bytes"
            __payload__
            (zstd-decompress __zst__))

(test-section "(crab compress) — explicit level")

(test-equal "zstd level 1 still round-trips"
            __payload__
            (zstd-decompress (zstd-compress __payload__ 1)))

(test-section "(crab compress) — decompression-bomb cap")

(test-true "zstd raises when output exceeds cap"
           (guard (e (#t #t)) (zstd-decompress __zst__ 100) #f))
