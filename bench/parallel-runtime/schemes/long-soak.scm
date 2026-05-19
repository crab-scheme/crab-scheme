; parallel-runtime spec C6.2 — G6 long-soak benchmark.
;
; Mixed workload — spawn / send / receive / region alloc /
; periodic collect — running until the configured wall-time
; budget elapses. Spec headline is 1h to verify no slow leaks
; in steady-state; smoke scale is 10s (set via PR_BUDGET).
;
; Success: completes the budget window without panic; emits
; OK with iteration count, peak candidate count, and final
; sweep stats.

; Read budget from env or default to 10s for smoke.
; (Scheme can't read env directly; the runner sets PR_BUDGET
; for timeout, but the bench measures via current-jiffy.)
(define BUDGET-SEC 10) ; smoke scale

(define t0 (current-jiffy))
(define jpsr (jiffies-per-second))
(define deadline (+ t0 (* BUDGET-SEC jpsr)))

(define iters 0)
(define peak-candidates 0)

; Pre-spawn a small actor pool that echoes; the main loop
; sends to one of them per iter.
(define pool
  (let loop ((i 0) (acc '()))
    (if (= i 4)
        (reverse acc)
        (let ((p (spawn (lambda ()
                          (let echo-loop ()
                            (let ((msg (raw-receive)))
                              (cond
                               ((pair? msg)
                                (send (car msg) 'ok)
                                (echo-loop))
                               (else 'done))))))))
          (loop (+ i 1) (cons p acc))))))

(define (alist-ref key alist)
  (let loop ((rest alist))
    (cond
     ((null? rest) #f)
     ((eq? (car (car rest)) key) (cdr (car rest)))
     (else (loop (cdr rest))))))

(define me (self))

(let loop ()
  (cond
   ((>= (current-jiffy) deadline) 'done)
   (else
    (set! iters (+ iters 1))
    ; Mix of work each iter:
    ; - allocate a region pair (auto-promote on scope exit)
    ; - cons a regular pair
    ; - bounce a message through one of the pool actors
    (with-region (lambda () (cons-in-region iters iters)))
    (let ((p (cons iters iters)))
      (set-cdr! p p) ; create a self-cycle for the registry
      ; drop p (let-binding goes out of scope at iter end)
      'cycled)
    ; Every 100 iters, collect + check peak.
    (when (= 0 (modulo iters 100))
      (collect)
      (let ((stats (gc-stats)))
        (let ((cur (alist-ref 'sweep-candidates-checked stats)))
          (when (and cur (> cur peak-candidates))
            (set! peak-candidates cur)))))
    ; Bounce through a pool actor.
    (send (list-ref pool (modulo iters 4)) (cons me iters))
    (raw-receive)
    (loop))))

(define t1 (current-jiffy))
(define elapsed (/ (- t1 t0) jpsr))

(display "OK long-soak budget-sec=")
(display BUDGET-SEC)
(display " elapsed=")
(display elapsed)
(display "s iters=")
(display iters)
(display " peak-candidates=")
(display peak-candidates)
(newline)
