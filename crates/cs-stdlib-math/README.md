# `(crab math)` + `(crab math stats)` — Math extras

CrabScheme stdlib module. Iter 12 of the stdlib-modules spec.

R6RS covers the basic transcendentals (sin/cos/tan/log/exp/sqrt)
and the integer + rational tower. This module adds the "everyone
reaches for this eventually" extras + a tiny descriptive-stats
surface.

## Math extras

```
(math-erf  x)       ;-> flonum   ; error function
(math-erfc x)       ;-> flonum   ; complementary error function
(math-gamma x)      ;-> flonum   ; Γ
(math-lgamma x)     ;-> flonum   ; log Γ
(math-cbrt x)       ;-> flonum   ; cube root (faster than (expt x 1/3))
(math-hypot a b)    ;-> flonum   ; sqrt(a²+b²) without overflow
```

Backed by `libm` so the implementations match the C-library
reference for every platform.

## Statistics

```
(stats-mean lst)            ;-> flonum
(stats-median lst)          ;-> flonum
(stats-variance lst)        ;-> flonum   ; sample variance (n-1 denominator)
(stats-stddev lst)          ;-> flonum   ; sqrt of sample variance
(stats-percentile lst p)    ;-> flonum   ; p in [0, 1]; linear interpolation
```

Empty input raises. Procedures accept Scheme lists of numbers
(integer or flonum); arithmetic is f64 throughout.

## Example

```scheme
(import (crab math))

(define n 10)
(define ks (let loop ((i 1) (acc '())) (if (> i n) acc (loop (+ i 1) (cons i acc)))))
(display "mean = ")   (display (stats-mean   ks)) (newline)   ;; 5.5
(display "median = ") (display (stats-median ks)) (newline)   ;; 5.5
(display "stddev = ") (display (stats-stddev ks)) (newline)   ;; 3.0276...

;; Gamma satisfies Γ(n+1) = n!
(display (math-gamma 6.0)) (newline)   ;; 120.0
```
