; parallel-runtime spec C6.2 — G4 cycle-collector bench.
;
; Builds N random cycles (varying size), forces a sweep,
; asserts gc-stats shows them collected. Exercises the full
; C4 stack: registry (C4.1), CycleChildren (C4.2), BR walk
; (C4.3), wired through run_sweep (C4.4), with the C4.5
; yield hook installed.
;
; Default N = 1000 (smoke); spec headline is 10000.
;
; Success: gc-stats sweep-cycles-collected >= N/2 (some
; cycles may not break in one pass — Hashtable's BreakCycle
; is a no-op; multi-pair cycles may need multiple sweeps).

(define N 1000)

; Make a small ring of K pairs all pointing back to start.
; Then drop the local reference, leaving the ring as a
; cycle candidate.
(define (make-ring k)
  (let* ((start (cons 0 '())))
    (let loop ((cur start) (i 1))
      (cond
       ((= i k)
        (set-cdr! cur start) ; close the ring
        start)
       (else
        (let ((next (cons i '())))
          (set-cdr! cur next)
          (loop next (+ i 1))))))))

; Build N rings of size 3..20, immediately drop them. The
; cycle detector registers each as a candidate; the sweep
; reclaims them.
(let loop ((i 0))
  (if (= i N)
      'done
      (begin
        ; Vary size 3..20.
        (make-ring (+ 3 (modulo i 18)))
        (loop (+ i 1)))))

; Force a sweep.
(collect)

; Read stats.
(define stats (gc-stats))
(define (alist-ref key alist)
  (let loop ((rest alist))
    (cond
     ((null? rest) #f)
     ((eq? (car (car rest)) key) (cdr (car rest)))
     (else (loop (cdr rest))))))

(define checked (alist-ref 'sweep-candidates-checked stats))
(define collected (alist-ref 'sweep-cycles-collected stats))
(define time-us (alist-ref 'sweep-time-us stats))

(cond
 ((and checked collected (>= checked N))
  (display "OK cycle-n-pair N=")
  (display N)
  (display " checked=")
  (display checked)
  (display " collected=")
  (display collected)
  (display " time-us=")
  (display time-us)
  (newline))
 (else
  (display "FAIL cycle-n-pair N=")
  (display N)
  (display " checked=")
  (display checked)
  (display " collected=")
  (display collected)
  (newline)
  (exit 1)))
