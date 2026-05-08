; CrabScheme example: Fibonacci with tail-recursive helper
(define (fib-loop a b k)
  (if (= k 0) a (fib-loop b (+ a b) (- k 1))))
(define (fib n) (fib-loop 0 1 n))
(fib 20)
