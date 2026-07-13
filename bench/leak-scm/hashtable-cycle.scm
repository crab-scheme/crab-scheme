; cs-i6p.3 leak harness, shape (b): hashtable self-cycle.
; Each iteration allocates a hashtable whose 'self key points back at
; itself, then drops it. Unlike shape (a), Hashtable has a real
; BreakCycle impl (crates/cs-core/src/value.rs:440) that demotes the
; first heap-bearing slot, so the layer-4 sweep fully reclaims this
; shape (see docs/measurements/2026-07-12-cycle-sweep-eval.md §3).
;
; Edit `n` below to change the iteration count (10,000 / 20,000 were
; the sizes used in the eval). Sampling is every 2,000 iterations;
; each run is single-shot.
(define n 10000)

(define (loop i)
  (if (< i n)
      (begin
        (let ((h (make-eq-hashtable)))
          (hashtable-set! h 'self h))
        (if (= 0 (modulo i 2000))
            (begin (display (gc-stats)) (newline)))
        (loop (+ i 1)))))

(loop 0)
(display "final: ") (display (gc-stats)) (newline)
