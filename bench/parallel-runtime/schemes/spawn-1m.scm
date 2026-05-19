; parallel-runtime spec C6.2 — G1 spawn benchmark.
;
; Spawns N actors as fast as possible, measures throughput.
; Default N = 100k (smoke scale); the spec headline is 1M
; actors. The MVP scale validates the spawn-async path
; (parallel-runtime C1.1) doesn't hit the 4096-actor
; thread-per-actor ceiling from the legacy spawn_blocking
; path.
;
; Success: spawns finish without panic; emits OK with the
; achieved spawn rate.

(define N 100000)
(define t0 (current-jiffy))
(define jpsr (jiffies-per-second))

; Each actor's body is the smallest possible — exit
; immediately. The spawn primop returns a Pid; we discard.
(let loop ((i 0))
  (if (= i N)
      'done
      (begin
        (spawn (lambda () 'noop))
        (loop (+ i 1)))))

(define t1 (current-jiffy))
(define elapsed (/ (- t1 t0) jpsr))
(define rate (if (> elapsed 0.0) (/ N elapsed) 0.0))

(display "OK spawn-1m N=")
(display N)
(display " elapsed=")
(display elapsed)
(display "s rate=")
(display rate)
(display "/s")
(newline)
