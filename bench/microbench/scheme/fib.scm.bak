; Fibonacci — naive recursive (Benchmarks Game classic, not the
; "fasta" task itself but a standard recursion-heavy microbench).
; Tests function call overhead and tail-call dispatching of self-recursion.
;
; Default: fib(25) = 75025. The walker tier uses host stack for
; recursion so larger N risks stack overflow; the VM tier and a future
; JIT can comfortably go to 30+. Keep the same N across tiers for an
; apples-to-apples wall-time comparison.

(define (fib n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))

(define n 25)
(display "fib(") (display n) (display ") = ") (display (fib n)) (newline)
