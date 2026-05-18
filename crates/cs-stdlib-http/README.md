# `(crab http client)` — Synchronous HTTP client

CrabScheme stdlib module wrapping `ureq` (TLS via rustls). Iter
10 of the stdlib-modules spec. Supersedes the example
`cs-ffi-http` crate.

All requests block the calling thread until the response is fully
received. For concurrency, drive these from BEAM actors. Streaming
body variants land when `Value::Opaque` enables a port wrapper
over a `ureq::Response`.

## Procedures

```
(http-get url [headers])           ;-> response-alist
(http-post url body [headers])     ;-> response-alist
(http-put  url body [headers])     ;-> response-alist
(http-delete url [headers])        ;-> response-alist
(http-request method url body headers) ;-> response-alist
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
