; Conformance test for `(crab url)` — stdlib-modules iter 6.

(test-section "(crab url) — parse")

(define __u__ (url-parse "https://user:pw@example.com:8443/path?q=1#frag"))

(test-equal "scheme"   "https"           (cdr (assoc "scheme"   __u__)))
(test-equal "host"     "example.com"     (cdr (assoc "host"     __u__)))
(test-eqv   "port"     8443              (cdr (assoc "port"     __u__)))
(test-equal "path"     "/path"           (cdr (assoc "path"     __u__)))
(test-equal "query"    "q=1"             (cdr (assoc "query"    __u__)))
(test-equal "fragment" "frag"            (cdr (assoc "fragment" __u__)))
(test-equal "username" "user"            (cdr (assoc "username" __u__)))
(test-equal "password" "pw"              (cdr (assoc "password" __u__)))

(test-section "(crab url) — accessors")

(test-equal "url-scheme convenience"  "https"       (url-scheme "https://example.com/"))
(test-equal "url-host convenience"    "example.com" (url-host   "https://example.com/"))

(test-section "(crab url) — encode/decode")

(test-equal "encode special chars"
            "hello%20world%21"
            (url-encode "hello world!"))
(test-equal "decode special chars"
            "hello world!"
            (url-decode "hello%20world%21"))
(test-equal "encode then decode round-trip"
            "café & coffee"
            (url-decode (url-encode "café & coffee")))
