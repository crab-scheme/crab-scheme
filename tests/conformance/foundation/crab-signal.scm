; Conformance test for `(crab signal)` — stdlib-modules iter 13.
;
; Real signal delivery requires actually raising a signal, which
; is OK to do in-process. We use `(crab process)` to kill -USR1
; our own pid, then poll.
;
; SIGUSR1 is the test signal — safer than SIGINT/SIGTERM because
; the default disposition is "terminate" but we install a watcher
; first, which neutralizes the default.

(test-section "(crab signal) — bogus name raises")

(test-true "watch unknown signal raises"
           (guard (e (#t #t))
             (signal-watch! "SIGNOPE")
             #f))

(test-section "(crab signal) — poll without armed signals returns #f")

;; On a fresh process this should be #f. With prior tests in the
;; same `cargo test` process there may be pending entries from
;; previous runs; drain first.
(let drain ()
  (when (signal-poll) (drain)))

(test-false "signal-poll returns #f when empty" (signal-poll))

(test-section "(crab signal) — round-trip via self-kill")

(signal-watch! "SIGUSR1")
;; Use `kill -USR1 PID` to deliver to ourselves.
(define __pid__ (process-id))
(run "sh" (list "-c" (string-append "kill -USR1 " (number->string __pid__))))

;; The signal arrives asynchronously on a background thread; give
;; it a moment to land in the queue.
(sleep-ms 200)

(test-equal "signal-poll returns SIGUSR1 after self-kill"
            "SIGUSR1"
            (signal-poll))

;; Second poll returns #f since we drained.
(test-false "subsequent signal-poll returns #f after drain"
            (signal-poll))
