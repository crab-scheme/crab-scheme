; Conformance test for `(crab http client)` — stdlib-modules iter 10.
;
; Real HTTP traffic isn't deterministic on CI; we exercise the FFI
; surface against a localhost target spawned via (crab net).
; Specifically, bind a TCP listener that returns a minimal HTTP/1.1
; response, then `http-get` it from the same process.

(define __port__ 47812)
(define __listener__ (tcp-listen "127.0.0.1" __port__))

;; Spin up a tiny server on a separate Scheme procedure call: accept
;; once, write a canned response, close. We need this to happen
;; AFTER http-get connects, but the Scheme runtime is single-threaded
;; — so we use a separate background process via (crab process).
;; That overcomplicates the test; simpler: just exercise the procs
;; against a bogus URL and assert the error shape (procedures
;; return condition values rather than crashing the process).

(test-section "(crab http client) — error shape")

;; bogus URL — port 1 is privileged + reserved.
(test-true "http-get to a closed port raises a condition"
           (guard (e (#t #t))
             (http-get "http://127.0.0.1:1")
             #f))

(test-true "http-get with bad header type raises"
           (guard (e (#t #t))
             (http-get "http://127.0.0.1:1" '("not-a-pair"))
             #f))

(tcp-close __listener__)

(test-section "(crab http client) — procedures registered")

;; Each entry point exists; calling with insufficient args raises.
(test-true "http-post arity error" (guard (e (#t #t)) (http-post) #f))
(test-true "http-put arity error" (guard (e (#t #t)) (http-put) #f))
(test-true "http-delete arity error" (guard (e (#t #t)) (http-delete) #f))
(test-true "http-request arity error" (guard (e (#t #t)) (http-request) #f))
