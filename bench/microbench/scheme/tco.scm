; Proper-tail-call regression guard (ADR 0019).
;
; Pre-fix, the Cranelift JIT only TCO'd self-recursion, so this
; program overflows the host stack and ABORTS the process on
; `--tier vm-jit`. The VM and walker tiers run it fine. Post-fix the
; JIT trampoline runs it in constant stack on every tier.
;
; The result is a single integer so it can be cross-checked across
; tiers: (tco) = 0 + 9000000 = 9000000.
;
; It exercises the two tail-call shapes the JIT failed on:
;   (1) mutual recursion (ping <-> pong), and
;   (2) nested named-let tail loops where the inner loop tail-calls
;       the outer (mandelbrot's col-loop -> row-loop shape).

; (1) Mutual tail recursion — 5,000,000 deep.
(define (ping n) (if (= n 0) 0 (pong (- n 1))))
(define (pong n) (if (= n 0) 1 (ping (- n 1))))

; (2) Nested named-let tail loops — n*n iterations, O(n) cross-call
;     nesting (col -> row) plus O(n) inner self-tail per row.
(define (grid n)
  (let row ((y 0) (acc 0))
    (if (= y n)
        acc
        (let col ((x 0) (a acc))
          (if (= x n)
              (row (+ y 1) a)
              (col (+ x 1) (+ a 1)))))))

(define (tco) (+ (ping 5000000) (grid 3000)))

(display "tco = ") (display (tco)) (newline)
