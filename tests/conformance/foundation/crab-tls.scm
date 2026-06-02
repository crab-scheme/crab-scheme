; Conformance test for `(crab tls)` — TLS client (rustls).
;
; Network-free + deterministic: a real handshake needs a server with a
; certificate the client trusts, so CI exercises only the handle
; mechanics + error paths (mirroring crab-net.scm's "avoid external
; network" convention). The happy path — tls-connect to a real HTTPS
; host + HTTP GET — is verified manually against the built binary.

(test-section "(crab tls) — bad-handle errors")

(test-true "tls-send on an unknown handle raises"
           (guard (e (#t #t))
             (tls-send 999999 (string->utf8 "hi"))
             #f))

(test-true "tls-recv on an unknown handle raises"
           (guard (e (#t #t))
             (tls-recv 999999 16)
             #f))

(test-true "tls-close on an unknown handle raises"
           (guard (e (#t #t))
             (tls-close 999999)
             #f))

(test-section "(crab tls) — argument validation")

(test-true "tls-connect with a non-string host raises"
           (guard (e (#t #t))
             (tls-connect 123 443)
             #f))

(test-true "tls-connect with a non-fixnum port raises"
           (guard (e (#t #t))
             (tls-connect "localhost" "443")
             #f))

;; max-len is validated before the handle lookup, so a bogus handle
;; still surfaces the positivity error — either way it must raise.
(test-true "tls-recv with a non-positive max-len raises"
           (guard (e (#t #t))
             (tls-recv 999999 0)
             #f))

(test-section "(crab tls) — connection refused")
;; Nothing listens on loopback port 1, so the TCP connect fails before
;; any TLS work — tls-connect must raise rather than return a handle.
(test-true "tls-connect to a closed loopback port raises"
           (guard (e (#t #t))
             (tls-connect "127.0.0.1" 1)
             #f))
