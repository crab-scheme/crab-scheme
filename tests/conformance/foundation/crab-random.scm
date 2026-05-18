; Conformance test for `(crab random)` — stdlib-modules iter 5.

(test-section "(crab random) — bytes")

(define __bv0__ (random-bytes 0))
(test-eqv "random-bytes 0 returns empty bytevector"
          0 (bytevector-length __bv0__))

(define __bv32__ (random-bytes 32))
(test-eqv "random-bytes 32 length matches"
          32 (bytevector-length __bv32__))

; Sanity: two independent draws shouldn't match in 32 bytes
; (collision probability ≈ 2^-256). If you ever see this fail,
; physics broke first.
(define __bv32-second__ (random-bytes 32))
(test-false "two random-bytes 32 draws differ"
            (equal? __bv32__ __bv32-second__))

(test-section "(crab random) — integer")

; All draws of (random-integer 1) must be 0.
(test-eqv "random-integer 1 is always 0"
          0 (random-integer 1))

; Draws of (random-integer 100) must land in [0, 100).
(test-true "random-integer 100 within range"
           (let loop ((i 0) (ok #t))
             (cond ((= i 50) ok)
                   (else (let ((r (random-integer 100)))
                           (loop (+ i 1) (and ok (>= r 0) (< r 100))))))))

(test-section "(crab random) — flonum")

(test-true "random-flonum in [0,1)"
           (let ((f (random-flonum)))
             (and (>= f 0.0) (< f 1.0))))

(test-section "(crab random) — choice")

(test-true "random-choice picks an element from the list"
           (let ((picked (random-choice '(a b c d e))))
             (memq picked '(a b c d e))))
