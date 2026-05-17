; tak(18, 12, 6) — mutual recursion + integer arith.

(define (tak x y z)
  (if (not (< y x))
      z
      (tak (tak (- x 1) y z)
           (tak (- y 1) z x)
           (tak (- z 1) x y))))

(realworld-bench
  "tak"
  '((x . 18) (y . 12) (z . 6))
  (lambda () (tak 18 12 6)))
