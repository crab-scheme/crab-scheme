; fib(25) — recursion + integer arith.

(define (fib n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))

(realworld-bench
  "fib"
  '((n . 25))
  (lambda () (fib 25)))
