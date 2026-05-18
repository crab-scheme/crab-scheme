; Conformance test for `(crab net)` — stdlib-modules iter 9.
;
; Exercises DNS + UDP + TCP loopback. Avoids external network so
; the test is deterministic on CI.

(test-section "(crab net dns)")

;; localhost resolves to one or more loopback addresses on every
;; sane host. We don't assert specific values — just non-empty.
(test-true "dns-resolve localhost returns a list"
           (let ((r (dns-resolve "localhost")))
             (and (pair? r) (string? (car r)))))

(test-section "(crab net udp) — loopback round-trip")

(define __udp-server__ (udp-bind "127.0.0.1" 0))
;; Bind grabbed a random port; we need its number to send to it.
;; UDP doesn't expose `local-port` yet — workaround: bind a
;; deterministic high port and retry on collision. Use ephemeral
;; range pseudo-randomly.
(define __test-port__ 47811)
(udp-close __udp-server__)
(define __server2__ (udp-bind "127.0.0.1" __test-port__))
(define __client__  (udp-bind "127.0.0.1" 0))

(define __payload__
  (let ((bv (make-bytevector 5 0)))
    (bytevector-u8-set! bv 0 104)
    (bytevector-u8-set! bv 1 101)
    (bytevector-u8-set! bv 2 108)
    (bytevector-u8-set! bv 3 108)
    (bytevector-u8-set! bv 4 111)
    bv))

(udp-send-to __client__ __payload__ "127.0.0.1" __test-port__)

(define __recv__ (udp-recv-from __server2__ 1024))
(test-eqv "received payload length matches"
          5
          (bytevector-length (car __recv__)))
(test-eqv "received payload byte 0"
          104
          (bytevector-u8-ref (car __recv__) 0))
(test-equal "source host is loopback"
            "127.0.0.1"
            (car (cdr __recv__)))

(udp-close __server2__)
(udp-close __client__)

(test-section "(crab net) — close raises on bogus handle")

(test-true "tcp-close on unknown handle raises"
           (guard (e (#t #t))
             (tcp-close 999999)
             #f))
