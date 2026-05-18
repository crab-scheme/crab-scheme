; Conformance test for `(crab websocket)` — stdlib-modules iter 10.
;
; WS testing requires a real WS server. We exercise the FFI surface
; without external network: error shape on connect, bad handle.

(test-section "(crab websocket) — procedures registered")

(test-true "ws-connect arity error" (guard (e (#t #t)) (ws-connect) #f))

(test-true "ws-connect to closed port raises"
           (guard (e (#t #t))
             (ws-connect "ws://127.0.0.1:1")
             #f))

(test-true "ws-send-text on bogus handle raises"
           (guard (e (#t #t))
             (ws-send-text 999999 "hi")
             #f))

(test-true "ws-close on bogus handle raises"
           (guard (e (#t #t))
             (ws-close 999999)
             #f))
