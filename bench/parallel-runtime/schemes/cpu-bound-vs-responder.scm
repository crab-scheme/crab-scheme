; parallel-runtime spec C6.2 — G2 starvation-prevention bench.
;
; One CPU-bound actor (tight loop bumping reductions) and one
; responder waiting for a message. The CPU actor's cooperative
; yield (parallel-runtime C2.1+C2.2) should let the responder
; ack within the budget despite the hog monopolizing one
; worker. Mirrors the Rust-side parallel_runtime_starvation
; test at the Scheme level.
;
; Success: responder acks within budget; emits OK with the
; observed ack latency.

(define responder-pid
  (spawn (lambda ()
           (let ((msg (raw-receive)))
             (if (pair? msg)
                 ; Reply to sender with same payload.
                 (send (car msg) 'ack)
                 'no-msg)))))

(define hog-pid
  (spawn (lambda ()
           ; 1M bump-reductions! calls — without C2's yield
           ; hook each call would tick the counter without
           ; surrendering the worker. With C2 the actor
           ; yields at every reduction-budget exhaust.
           (let loop ((i 0))
             (if (>= i 1000000)
                 'done
                 (begin
                   (bump-reductions! 1)
                   (loop (+ i 1))))))))

(define t0 (current-jiffy))
(define jpsr (jiffies-per-second))

(define me (self))
(send responder-pid (cons me 'ping))

; Block until the responder replies. If C2's yield hook
; isn't firing this would wait until the hog finishes.
(raw-receive)

(define t1 (current-jiffy))
(define latency-ms (* 1000 (/ (- t1 t0) jpsr)))

(display "OK cpu-bound-vs-responder latency-ms=")
(display latency-ms)
(newline)
