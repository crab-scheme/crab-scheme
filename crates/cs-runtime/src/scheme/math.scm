;;; (crab math) — combinatorics and numeric helpers.
;;;
;;; A bundled Scheme extension of the Rust `(crab math)` module (which
;;; provides the special functions and statistics). Evaluated into the
;;; global environment at startup, so `(import (crab math))` is a no-op
;;; and pulls in both halves. The answer to the parts of Python's math
;;; and Clojure's math that the Rust side doesn't cover.
;;;
;;; All names here are fresh (they don't shadow built-ins).

;; n! — exact; promotes to a bignum automatically for large n.
(define (factorial n)
  (when (or (not (integer? n)) (negative? n))
    (error "factorial: expected a non-negative integer" n))
  (let loop ((i 2) (acc 1))
    (if (> i n) acc (loop (+ i 1) (* acc i)))))

;; Binomial coefficient "n choose k". Uses the multiplicative form, so
;; it never builds the full factorials; each step stays an exact integer.
(define (binomial n k)
  (if (or (negative? k) (> k n))
      0
      (let ((k (min k (- n k))))
        (let loop ((i 0) (acc 1))
          (if (= i k)
              acc
              (loop (+ i 1) (quotient (* acc (- n i)) (+ i 1))))))))

;; Clamp x into the inclusive range [lo, hi].
(define (clamp x lo hi)
  (cond ((< x lo) lo)
        ((> x hi) hi)
        (else x)))

;; Sum / product of a list of numbers.
(define (sum lst) (fold-left + 0 lst))
(define (product lst) (fold-left * 1 lst))

;; Sign of a number: -1, 0, or 1.
(define (sign x)
  (cond ((< x 0) -1)
        ((> x 0) 1)
        (else 0)))

;; Primality test by trial division (exact integers).
(define (prime? n)
  (cond
    ((or (not (integer? n)) (< n 2)) #f)
    ((= n 2) #t)
    ((even? n) #f)
    (else
     (let loop ((d 3))
       (cond ((> (* d d) n) #t)
             ((= 0 (modulo n d)) #f)
             (else (loop (+ d 2))))))))

;; --- bit operations (width-relative; Scheme integers are unbounded, so
;; rotates/leading-zeros take an explicit bit `width`). Popcount and
;; bit length already exist as R6RS `bitwise-bit-count`/`bitwise-length`.

;; Rotate the low `width` bits of `x` left by `n` positions.
(define (bit-rotate-left x n width)
  (let* ((mask (- (expt 2 width) 1))
         (x (bitwise-and x mask))
         (n (modulo n width)))
    (bitwise-and mask
                 (bitwise-or (bitwise-arithmetic-shift x n)
                             (bitwise-arithmetic-shift x (- n width))))))

;; Rotate the low `width` bits of `x` right by `n` positions.
(define (bit-rotate-right x n width)
  (bit-rotate-left x (- width (modulo n width)) width))

;; Leading zero bits in the `width`-bit representation of `x` (x >= 0).
(define (bit-leading-zeros x width)
  (- width (bitwise-length x)))

;; Trailing zero bits of `x` (the index of its lowest set bit); 0 has none.
(define (bit-trailing-zeros x)
  (if (= x 0)
      0
      (- (bitwise-length (bitwise-and x (- x))) 1)))
