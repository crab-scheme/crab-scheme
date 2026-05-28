# `(crab websocket)` — Synchronous WebSocket client

CrabScheme stdlib module wrapping `tungstenite` (TLS via rustls).
Iter 10 of the stdlib-modules spec.

Connections are opaque fixnum handles indexing a process-global
slab (same pattern as `cs-stdlib-net`). Blocking calls; for
concurrent connections, drive from BEAM actors. Typed `ws?`
predicate + RAII Drop lands when `Value::Opaque` does.

## Procedures

```
(ws-connect url)               ;-> handle
(ws-send-text   handle string) ;-> unspec
(ws-send-binary handle bv)     ;-> unspec
(ws-recv        handle)        ;-> (kind . payload)
(ws-close       handle)        ;-> unspec
```

`ws-recv` returns a pair where `kind` is one of:

- `"text"`   — payload is a string
- `"binary"` — payload is a bytevector
- `"ping"`   — payload is a bytevector (raw ping data)
- `"pong"`   — payload is a bytevector
- `"close"`  — payload is a string (close reason; empty if none)

## Example

```scheme
(import (crab websocket))

(define ws (ws-connect "wss://echo.websocket.org"))
(ws-send-text ws "hello")
(let ((m (ws-recv ws)))
  (display (car m)) (display ": ") (display (cdr m)) (newline))
(ws-close ws)
```

## WASM targets (#9 wasip2-networking)

On `wasm32-wasip2` with the cs-cli `wasm-stdlib-full` feature, the
client works as-is via tungstenite-on-`std::net`. The only build
adjustment is the rustls cert source: native uses
`rustls-tls-native-roots` (OS cert store), wasi uses
`rustls-tls-webpki-roots` (bundled Mozilla CAs) because
`rustls-native-certs` doesn't build on wasm32-wasip2. The crate is
client-only; a WS server lands when `wasi:sockets 0.2` adds socket
creation or `wasi:websocket` is standardized. Requires Wasmtime 28+.
See ADR 0033.
