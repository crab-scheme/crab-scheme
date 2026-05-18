; Conformance test for `(crab toml)` — stdlib-modules iter 6.

(test-section "(crab toml) — parse")

(define __t__ (toml-parse "title = \"hello\"\nport = 8080\nenabled = true\n"))

(test-equal "parse string"  "hello" (cdr (assoc "title"   __t__)))
(test-equal "parse integer" 8080    (cdr (assoc "port"    __t__)))
(test-equal "parse boolean" #t      (cdr (assoc "enabled" __t__)))

(test-section "(crab toml) — round-trip")

(define __out__ (toml-stringify '(("name" . "alice") ("score" . 42))))
(test-true "stringify returns a string" (string? __out__))

(define __r__ (toml-parse __out__))
(test-equal "round-trip preserves name"  "alice" (cdr (assoc "name"  __r__)))
(test-equal "round-trip preserves score" 42      (cdr (assoc "score" __r__)))
