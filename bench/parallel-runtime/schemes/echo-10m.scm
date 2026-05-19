; parallel-runtime spec C6.2 — G1 echo benchmark.
;
; Two actors play ping-pong N times. Measures message
; throughput. Default N = 10k (smoke); spec headline is 10M.
;
; Exercises send + receive across actor boundaries — the
; full M1 stack (spawn-async, cs-actor mpsc, payload
; conversion).
;
; Success: every message round-trips, final count == N.

(define N 10000)
(define t0 (current-jiffy))
(define jpsr (jiffies-per-second))

; The ping actor's body: send self N messages to the
; partner, then receive N replies back. Uses raw-receive
; (the blocking primop the dispatch loop yields around).
(define partner-pid
  (spawn (lambda ()
           ; Echo loop: forward whatever we receive back to
           ; the sender. The message is a pair (sender . payload).
           (let loop ()
             (let ((msg (raw-receive)))
               (cond
                ((pair? msg)
                 (send (car msg) (cdr msg))
                 (loop))
                (else 'done)))))))

(define me (self))
(let loop ((i 0))
  (if (= i N)
      'sent
      (begin
        (send partner-pid (cons me i))
        (loop (+ i 1)))))

(let recv ((received 0))
  (if (= received N)
      'all-back
      (begin
        (raw-receive)
        (recv (+ received 1)))))

(define t1 (current-jiffy))
(define elapsed (/ (- t1 t0) jpsr))
(define rate (if (> elapsed 0.0) (/ (* 2 N) elapsed) 0.0))

(display "OK echo-10m N=")
(display N)
(display " elapsed=")
(display elapsed)
(display "s rate=")
(display rate)
(display "msgs/s")
(newline)
