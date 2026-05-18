# `(crab url)` — URL parse + percent-encode

CrabScheme stdlib module wrapping the `url` and `percent-encoding`
Rust crates. Iter 6 of the stdlib-modules spec.

```
(url-parse str)           ;-> alist          ; (("scheme" . …) ("host" . …) …)
(url-scheme str)          ;-> string         ; convenience accessor
(url-host str)            ;-> string or #f
(url-encode str)          ;-> string         ; percent-encode non-alphanumeric bytes
(url-decode str)          ;-> string         ; percent-decode UTF-8 payload
```

Parsed alist keys: `scheme`, `host`, `port`, `path`, `query`,
`fragment`, `username`, `password`. Missing components are `#f`
(port, query, fragment, host) or `""` (username when none was in
the URL).

## Example

```scheme
(import (crab url))

(define u (url-parse "https://api.example.com/users?id=42"))
(display (cdr (assoc "host" u))) (newline)       ; api.example.com
(display (cdr (assoc "query" u))) (newline)      ; id=42

(display (url-encode "hello world & friends"))   ; hello%20world%20%26%20friends
```
