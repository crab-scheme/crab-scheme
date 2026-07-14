; cs-i6p.3 leak harness, shape (c): closure/letrec mutual-recursion
; cycle. Each iteration builds two letrec-bound mutually-recursive
; lambdas (each closing over the other via the shared letrec frame),
; calls one once, then drops both. Neither Procedure nor environment
; frames have a CycleChildren/BreakCycle impl anywhere in the tree, so
; this shape is structurally invisible to the layer-4 sweep regardless
; of the default-on decision (see
; docs/measurements/2026-07-12-cycle-sweep-eval.md §2 caveat / §3).
;
; Edit `n` below to change the iteration count. Sampling is every
; 2,000 iterations; each run is single-shot.
(define n 10000)

(define (loop i)
  (if (< i n)
      (begin
        (letrec ((even? (lambda (k) (if (= k 0) #t (odd? (- k 1)))))
                 (odd? (lambda (k) (if (= k 0) #f (even? (- k 1))))))
          (even? 4))
        (if (= 0 (modulo i 2000))
            (begin (display (gc-stats)) (newline)))
        (loop (+ i 1)))))

(loop 0)
(display "final: ") (display (gc-stats)) (newline)
