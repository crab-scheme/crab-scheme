# `(crab http client|server)` — Synchronous HTTP

CrabScheme stdlib module. **Client** via `ureq` (iter 10);
**server** via `tiny_http` (iter 11). Both block the calling
thread — no Tokio dep, matches our BEAM-for-concurrency model.
Supersedes the example `cs-ffi-http` crate.

For concurrency, drive these from BEAM actors. Streaming body
variants on the client side land when `Value::Opaque` enables a
port wrapper over a `ureq::Response`.

## Client

```
(http-get url [headers])               ;-> response-alist
(http-post   url body [headers])       ;-> response-alist
(http-put    url body [headers])       ;-> response-alist
(http-delete url [headers])            ;-> response-alist
(http-request method url body headers) ;-> response-alist
```

## Server

```
(http-server-bind host port)            ;-> server-handle
(http-server-accept handle [timeout-ms]);-> request-handle or #f
(http-server-close server-handle)       ;-> unspec

(http-request-method  req)              ;-> string
(http-request-url     req)              ;-> string
(http-request-headers req)              ;-> alist
(http-request-body    req)              ;-> bytevector
(http-respond req status headers body)  ;-> unspec  ; consumes the request
```

`accept` blocks the calling thread until a request arrives or
the optional timeout (milliseconds) elapses; without a timeout
it blocks indefinitely. Each accepted request handle must be
passed to `http-respond` exactly once — that consumes the
handle and writes the response.

## Server example

```scheme
(import (crab http))

(define srv (http-server-bind "0.0.0.0" 8080))

(let loop ()
  (let ((req (http-server-accept srv)))
    (cond
      ((not req) (loop))               ; spurious wakeup; keep waiting
      (else
        (display "got ") (display (http-request-method req))
        (display " ")  (display (http-request-url req)) (newline)
        (http-respond req 200
                      '(("Content-Type" . "text/plain"))
                      (string->utf8 "hello\n"))
        (loop)))))
```

- `url`     — string.
- `body`    — bytevector. Pass an empty bytevector to send no body.
- `headers` — Scheme alist `(("Name" . "value") …)`. Optional on
  the convenience verbs.

## Response shape

```scheme
(("status"  . 200)
 ("headers" . (("Content-Type" . "application/json") …))
 ("body"    . #vu8(…)))
```

Non-2xx responses come back as the same alist (status reflects
the actual code). Errors that don't have a response (DNS,
connection refused, TLS handshake failure) raise as conditions.

## Example

```scheme
(import (crab http))
(import (crab json))
(import (crab base))

(define resp (http-get "https://api.example.com/users/42"
                       '(("Accept" . "application/json"))))
(if (= 200 (cdr (assoc "status" resp)))
    (let ((user (json-parse (utf8->string (cdr (assoc "body" resp))))))
      (display "name: ") (display (cdr (assoc "name" user))) (newline))
    (display "failed\n"))
```

## Caveats

- Response bodies are capped at 32 MB; larger responses are
  truncated silently. The streaming variant will lift this.
- Header decoding via `ureq::Response::headers_names` returns
  lowercased names per HTTP/2 norms; some servers' headers will
  appear in lowercase even if sent uppercase.
