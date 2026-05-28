# `(crab net …)` — TCP / UDP / DNS

CrabScheme stdlib module wrapping `std::net`. Iter 9 of the
stdlib-modules spec.

Sockets are represented as opaque **fixnum handles** that index
into a process-global slab — same approach `cs-actor` uses for
pids and `cs-stdlib-metrics` uses for the metric registry. A
typed `socket?` predicate + RAII drop semantics lands when
`Value::Opaque` does.

All operations are synchronous and block the calling thread. For
concurrent network IO, drive these from BEAM actors.

## DNS

```
(dns-resolve host)         ;-> list of strings
;; "host" may be "name", "name:port", "ip", or "ip:port".
;; Returns ips (no port) when no port was supplied; "ip:port" otherwise.
```

## TCP

```
(tcp-connect host port)            ;-> socket-handle
(tcp-listen  host port)            ;-> listener-handle
(tcp-accept  listener-handle)      ;-> socket-handle
(tcp-send    socket-handle bv)     ;-> unspec
(tcp-recv    socket-handle max)    ;-> bytevector  ; ≤ max bytes; empty bv on clean EOF
(tcp-close   handle)               ;-> unspec
```

## UDP

```
(udp-bind      host port)               ;-> socket-handle
(udp-send-to   handle bv host port)     ;-> unspec
(udp-recv-from handle max)              ;-> (bv source-host source-port)
(udp-close     handle)                  ;-> unspec
```

## Example — TCP echo client

```scheme
(import (crab net))
(import (crab string))

(define sock (tcp-connect "localhost" 8080))
(tcp-send sock (string->utf8 "ping\n"))
(define reply (utf8->string (tcp-recv sock 1024)))
(display "got: ") (display (string-trim reply)) (newline)
(tcp-close sock)
```

## Example — UDP server loop

```scheme
(import (crab net))

(define s (udp-bind "0.0.0.0" 9000))
(let loop ()
  (let* ((msg (udp-recv-from s 1500))
         (payload (car msg))
         (host    (car (cdr msg)))
         (port    (car (cdr (cdr msg)))))
    (udp-send-to s payload host port))    ; echo back
  (loop))
```

## WASM targets (#9 wasip2-networking)

On `wasm32-wasip2` with the cs-cli `wasm-stdlib-full` feature, the
**client** procs (`tcp-connect` / `tcp-send` / `tcp-recv` / `tcp-close`,
all UDP send-to / recv-from / close, `dns-resolve`) work via `std::net`
on top of `wasi:sockets 0.2` (Rust 1.83+). The **passive-socket** procs
(`tcp-listen` / `tcp-accept` / `udp-bind`) raise `FfiError::HostFailure`
at call time — `wasi:sockets 0.2` doesn't standardize socket creation;
use `wasi:http/incoming-handler` (cs-stdlib-http) for HTTP servers. On
`wasm32-wasip1` the crate isn't compiled (excluded from `wasm-stdlib`).
See ADR 0033.
