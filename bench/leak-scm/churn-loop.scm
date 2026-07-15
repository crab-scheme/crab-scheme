; cs-i6p.3 leak harness, shape (d): churn-loop (vector+closure
; composite). Each iteration builds a 2-slot vector "record" whose
; slot 0 is a closure closing over the record and whose slot 1 points
; back to the record itself — a composite vector+closure back-edge
; standing in for a connection-handler churn loop (a plain script
; can't easily drive real `spawn-source` actors in this harness). The
; back-edge is Vector-anchored, so this shape rides the same
; unimplemented Vector BreakCycle path as shape (a) — see
; docs/measurements/2026-07-12-cycle-sweep-eval.md §2/§3.
;
; Edit `n` below to change the iteration count. Sampling is every
; 2,000 iterations; each run is single-shot.
(define n 10000)

(define (make-record)
  (let ((r (make-vector 2 0)))
    (vector-set! r 0 (lambda () (vector-ref r 1)))
    (vector-set! r 1 r)
    r))

(define (loop i)
  (if (< i n)
      (begin
        (make-record)
        (if (= 0 (modulo i 2000))
            (begin (display (gc-stats)) (newline)))
        (loop (+ i 1)))))

(loop 0)
(display "final: ") (display (gc-stats)) (newline)
