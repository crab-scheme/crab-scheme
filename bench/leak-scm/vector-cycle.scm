; cs-i6p.3 leak harness, shape (a): vector self-cycle.
; Each iteration allocates a 1-slot vector that points at itself, then
; drops it. Layer-2's synchronous RC detector cannot break this (it is
; a genuine Rc cycle); only the layer-4 tracing sweep can reclaim it,
; and only if Vector has a real BreakCycle impl (it does not — see
; docs/measurements/2026-07-12-cycle-sweep-eval.md §3).
;
; Edit `n` below to change the iteration count (10,000 / 20,000 /
; 40,000 / 200,000 were the sizes used in the eval). Sampling is every
; 2,000 iterations; each run is single-shot (see doc §2/§3 note on
; run methodology).
(define n 10000)

(define (loop i)
  (if (< i n)
      (begin
        (let ((v (make-vector 1 0)))
          (vector-set! v 0 v))
        (if (= 0 (modulo i 2000))
            (begin (display (gc-stats)) (newline)))
        (loop (+ i 1)))))

(loop 0)
(display "final: ") (display (gc-stats)) (newline)
