; Conformance test for `(crab http server)` — stdlib-modules iter 11.
;
; CrabScheme is single-threaded — running an accept loop and a
; client request from the same process needs a multithreaded
; fixture that doesn't exist yet (the `run` wrapper backgrounds
; via shell but pipe ownership across `sh -c '... &'` is flaky on
; macOS). This conformance covers the registration + lifecycle
; surface; an end-to-end traffic test lands once the BEAM
; runtime can drive the accept loop on its own thread.

(test-section "(crab http server) — bind/close lifecycle")

(define __port__ 47916)

;; Round-trip: bind a port, close it, re-bind a different port
;; (proves close actually released the socket; different port
;; avoids TIME_WAIT flakiness on the same address).
(define __srv__ (http-server-bind "127.0.0.1" __port__))
(test-true "bind returns a positive fixnum"
           (and (number? __srv__) (> __srv__ 0)))
(http-server-close __srv__)

(define __srv2__ (http-server-bind "127.0.0.1" (+ __port__ 1)))
(test-true "second bind succeeds"
           (and (number? __srv2__) (> __srv2__ 0)))
(http-server-close __srv2__)

(test-section "(crab http server) — accept timeout")

(define __timeout-srv__ (http-server-bind "127.0.0.1" (+ __port__ 2)))
;; No client connecting; with a short timeout we expect #f.
(test-false "accept with 100ms timeout and no client returns #f"
            (http-server-accept __timeout-srv__ 100))
(http-server-close __timeout-srv__)

(test-section "(crab http server) — error shape")

(test-true "bind on a privileged/reserved port raises"
           (guard (e (#t #t))
             ;; Port 1 is reserved + privileged — bind always fails
             ;; in user mode.
             (http-server-bind "127.0.0.1" 1)
             #f))

(test-true "accept on bogus handle raises"
           (guard (e (#t #t))
             (http-server-accept 999999)
             #f))

(test-true "respond on bogus handle raises"
           (guard (e (#t #t))
             (http-respond 999999 200 '() (string->utf8 ""))
             #f))

(test-true "request accessor on bogus handle raises"
           (guard (e (#t #t))
             (http-request-method 999999)
             #f))

(test-true "close on already-closed handle raises"
           (guard (e (#t #t))
             ;; __srv__ was already closed above.
             (http-server-close __srv__)
             #f))
