; Conformance test for `(crab deflate)` — stdlib-modules iter 17.
;
; Split out of crab-compress.scm when flate2 moved to its own
; crate. gzip + deflate procs are unchanged; this exercises them
; via the cs-stdlib-deflate registration path.

(define (build-payload n)
  (let* ((bv (make-bytevector n 0)))
    (let loop ((i 0))
      (cond ((= i n) bv)
            (else (bytevector-u8-set! bv i (remainder i 256))
                  (loop (+ i 1)))))))

(define __payload__ (build-payload 1024))

(test-section "(crab deflate) — gzip round-trip")

(define __gz__ (gzip-compress __payload__))
(test-true "gzip-compress shrinks repeating data"
           (< (bytevector-length __gz__) (bytevector-length __payload__)))
(test-equal "gzip round-trip preserves bytes"
            __payload__
            (gzip-decompress __gz__))

(test-section "(crab deflate) — raw deflate round-trip")

(define __dfl__ (deflate-compress __payload__))
(test-equal "deflate round-trip preserves bytes"
            __payload__
            (deflate-decompress __dfl__))

(test-section "(crab deflate) — explicit level")

(test-equal "gzip level 9 still round-trips"
            __payload__
            (gzip-decompress (gzip-compress __payload__ 9)))

(test-section "(crab deflate) — decompression-bomb cap")

(test-true "gzip raises when output exceeds cap"
           (guard (e (#t #t)) (gzip-decompress __gz__ 100) #f))
(test-true "deflate raises when output exceeds cap"
           (guard (e (#t #t)) (deflate-decompress __dfl__ 100) #f))

(test-equal "gzip cap larger than output still decodes"
            __payload__
            (gzip-decompress __gz__ (* 1024 1024)))
