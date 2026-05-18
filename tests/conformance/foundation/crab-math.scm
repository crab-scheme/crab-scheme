; Conformance test for `(crab math)` + `(crab math stats)` —
; stdlib-modules iter 12.

(define (close-enough? a b eps)
  (< (abs (- a b)) eps))

(test-section "(crab math) — extras")

(test-true "erf(0) ≈ 0" (close-enough? (math-erf 0.0) 0.0 1e-9))
(test-true "erf(1) ≈ 0.8427"
           (close-enough? (math-erf 1.0) 0.8427007929 1e-6))

(test-true "erf + erfc = 1"
           (close-enough? (+ (math-erf 0.5) (math-erfc 0.5)) 1.0 1e-12))

(test-true "gamma(5) = 24" (close-enough? (math-gamma 5.0) 24.0 1e-9))
(test-true "lgamma(11) ≈ log(10!)"
           (close-enough? (math-lgamma 11.0)
                          (log 3628800.0)
                          1e-9))

(test-true "cbrt(27) = 3" (close-enough? (math-cbrt 27.0) 3.0 1e-12))
(test-true "cbrt(-8) = -2" (close-enough? (math-cbrt -8.0) -2.0 1e-12))

(test-true "hypot(3,4) = 5" (close-enough? (math-hypot 3.0 4.0) 5.0 1e-12))

(test-section "(crab math stats) — descriptive")

(test-true "mean of 1..5 = 3.0"
           (close-enough? (stats-mean '(1 2 3 4 5)) 3.0 1e-12))

(test-true "median of 1..5 = 3"
           (close-enough? (stats-median '(1 2 3 4 5)) 3.0 1e-12))

(test-true "median of 1..4 = 2.5"
           (close-enough? (stats-median '(1 2 3 4)) 2.5 1e-12))

(test-true "variance of 1..5 = 2.5 (sample, n-1)"
           (close-enough? (stats-variance '(1 2 3 4 5)) 2.5 1e-12))

(test-true "stddev of 1..5 ≈ sqrt(2.5)"
           (close-enough? (stats-stddev '(1 2 3 4 5))
                          (sqrt 2.5)
                          1e-12))

(test-true "percentile 0 returns min"
           (close-enough? (stats-percentile '(10 20 30 40 50) 0.0)
                          10.0
                          1e-12))
(test-true "percentile 1 returns max"
           (close-enough? (stats-percentile '(10 20 30 40 50) 1.0)
                          50.0
                          1e-12))
(test-true "percentile 0.5 returns median (odd)"
           (close-enough? (stats-percentile '(10 20 30 40 50) 0.5)
                          30.0
                          1e-12))

(test-section "(crab math stats) — error cases")

(test-true "mean of empty list raises"
           (guard (e (#t #t))
             (stats-mean '())
             #f))

(test-true "percentile fraction out of range raises"
           (guard (e (#t #t))
             (stats-percentile '(1 2 3) 1.5)
             #f))
