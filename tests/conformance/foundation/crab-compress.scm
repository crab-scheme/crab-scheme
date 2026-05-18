; Conformance test for `(crab compress)` — stdlib-modules iter 7.

(define (build-payload n)
  (let* ((bv (make-bytevector n 0)))
    (let loop ((i 0))
      (cond ((= i n) bv)
            (else (bytevector-u8-set! bv i (remainder i 256))
                  (loop (+ i 1)))))))

(define __payload__ (build-payload 1024))

(test-section "(crab compress) — gzip round-trip")

(define __gz__ (gzip-compress __payload__))
(test-true "gzip-compress shrinks repeating data"
           (< (bytevector-length __gz__) (bytevector-length __payload__)))
(test-equal "gzip round-trip preserves bytes"
            __payload__
            (gzip-decompress __gz__))

(test-section "(crab compress) — deflate round-trip")

(define __dfl__ (deflate-compress __payload__))
(test-equal "deflate round-trip preserves bytes"
            __payload__
            (deflate-decompress __dfl__))

(test-section "(crab compress) — zstd round-trip")

(define __zst__ (zstd-compress __payload__))
(test-true "zstd-compress shrinks repeating data"
           (< (bytevector-length __zst__) (bytevector-length __payload__)))
(test-equal "zstd round-trip preserves bytes"
            __payload__
            (zstd-decompress __zst__))

(test-section "(crab compress) — explicit level")

(test-equal "gzip level 9 still round-trips"
            __payload__
            (gzip-decompress (gzip-compress __payload__ 9)))
(test-equal "zstd level 1 still round-trips"
            __payload__
            (zstd-decompress (zstd-compress __payload__ 1)))
